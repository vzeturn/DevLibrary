using System.Data;
using System.Globalization;
using Microsoft.Data.SqlClient;
using MM.PdaOrder.Sync.Contracts.Wire;
using MM.PdaOrder.Sync.Mapping.Text;

namespace MM.PdaOrder.SyncAgent.Commands;

/// <summary>Read-side SQL of the order verbs: pre-checks and the read-your-writes check read (ADR 0104 §4).</summary>
internal static partial class OrderCommandSql
{
    internal const string CheckStatus =
        "SELECT TOP 1 ISNULL(Status, 0) FROM dbo.TmpInvoice WHERE CheckNo = @CheckNo AND RVC = @RVCNo;";

    internal const string ItemExists = "SELECT COUNT(*) FROM dbo.ItemDef WHERE ICode = @ItemCode;";

    internal const string FindLine =
        "SELECT t.TrnCode, CASE WHEN i.POrderTime IS NOT NULL OR ISNULL(i.PToOrder, 0) = 1 THEN 1 ELSE 0 END " +
        "FROM dbo.TmpTrn t OUTER APPLY (SELECT TOP 1 ti.POrderTime, ti.PToOrder FROM dbo.tmpTrnInfo ti " +
        "WHERE ti.TrnNo = t.TrnSeq AND ti.CheckNo = t.CheckNo) i " +
        "WHERE t.CheckNo = @CheckNo AND t.TrnSeq = @TrnSeq AND t.RVC = @RVCNo;";

    internal const string CgTrnExists = "SELECT CASE WHEN OBJECT_ID(N'dbo.CGTrn', N'U') IS NULL THEN 0 ELSE 1 END;";

    // The fingerprint columns are the ones CheckFingerprintAsync and the capture mirror take (ADR 0100 §4): every tmpTrn row
    // of the check, not only items. TrnDesc / CGName are TCVN3 bytes in a VARCHAR column: read raw and decoded like capture.
    internal const string ReadHeader =
        "SELECT CheckNo, TableNo, NGuest, SubTotal, TotalPaid FROM dbo.TmpInvoice WHERE CheckNo = @CheckNo AND RVC = @RVCNo;";

    private const string ReadLinesSelect =
        "SELECT t.TrnSeq, t.TrnCode, t.ItemCode, CAST(t.TrnDesc AS VARBINARY(510)), t.TrnQty, t.ItemPrice, t.BaseTTL, " +
        "t.MealPeriod, t.Cashier, CASE WHEN i.POrderTime IS NOT NULL OR ISNULL(i.PToOrder, 0) = 1 THEN 1 ELSE 0 END";

    private const string ReadLinesFrom =
        " FROM dbo.TmpTrn t OUTER APPLY (SELECT TOP 1 ti.POrderTime, ti.PToOrder FROM dbo.tmpTrnInfo ti " +
        "WHERE ti.TrnNo = t.TrnSeq AND ti.CheckNo = t.CheckNo) i WHERE t.CheckNo = @CheckNo ORDER BY t.TrnSeq;";

    internal const string ReadLines = ReadLinesSelect + ", CAST(NULL AS VARBINARY(300))" + ReadLinesFrom;

    internal const string ReadLinesWithNotes =
        ReadLinesSelect + ", (SELECT TOP 1 CAST(g.CGName AS VARBINARY(300)) FROM dbo.CGTrn g WHERE g.TrnNo = t.TrnSeq AND g.CGCode = 0)" +
        ReadLinesFrom;
}

/// <summary><see cref="IOrderCommandUnit"/> on the command's SQL connection and transaction, with the legacy parameter shapes.</summary>
internal sealed partial class SqlOrderCommandUnit(SqlConnection connection, SqlTransaction transaction) : IOrderCommandUnit
{
    /// <summary>SQL error numbers the legacy post batch THROWs before any write (open name / open price).</summary>
    internal const int OpenNameRequiredError = 51101;
    internal const int OpenPriceRequiredError = 51102;

    public async Task<int?> CheckStatusAsync(int rvcNo, long checkNo, CancellationToken ct)
    {
        await using var cmd = Command(OrderCommandSql.CheckStatus);
        Rvc(cmd, rvcNo);
        Check(cmd, checkNo);
        var value = await cmd.ExecuteScalarAsync(ct);
        return value is null or DBNull ? null : Convert.ToInt32(value, CultureInfo.InvariantCulture);
    }

    public async Task<bool> ItemExistsAsync(string itemCode, CancellationToken ct)
    {
        await using var cmd = Command(OrderCommandSql.ItemExists);
        Text(cmd, "@ItemCode", itemCode);
        return Convert.ToInt32(await cmd.ExecuteScalarAsync(ct), CultureInfo.InvariantCulture) > 0;
    }

    public async Task<long> OpenCheckAsync(OrderCheckOpenPayload payload, CancellationToken ct)
    {
        await using var cmd = Command(OrderCommandSql.OpenCheck);
        OpenParameters(cmd, payload);
        var value = await cmd.ExecuteScalarAsync(ct);
        return value is null or DBNull
            ? throw new InvalidOperationException("The open-check batch returned no check number.")
            : Convert.ToInt64(value, CultureInfo.InvariantCulture);
    }

    public async Task<OrderLineFacts?> FindLineAsync(int rvcNo, long checkNo, long trnSeq, CancellationToken ct)
    {
        await using var cmd = Command(OrderCommandSql.FindLine);
        Rvc(cmd, rvcNo);
        Check(cmd, checkNo);
        cmd.Parameters.Add("@TrnSeq", SqlDbType.BigInt).Value = trnSeq;
        await using var r = await cmd.ExecuteReaderAsync(ct);
        if (!await r.ReadAsync(ct)) return null;
        return new OrderLineFacts(ReadText(r, 0), Convert.ToInt32(r.GetValue(1), CultureInfo.InvariantCulture) == 1);
    }

    public async Task<OrderPostOutcome> PostItemAsync(OrderItemPostPayload payload, CancellationToken ct)
    {
        await using var cmd = Command(OrderCommandSql.PostItem);
        PostParameters(cmd, payload);
        try
        {
            // The batch's status row is its last result set (IsOut, MessageResult, TrnSeq, Seq, CheckNo, ItemCode); a vendor
            // proc it calls may emit sets of its own before it.
            await using var r = await cmd.ExecuteReaderAsync(ct);
            (int IsOut, long TrnSeq)? status = null;
            do
            {
                if (r.FieldCount < 3 || !string.Equals(r.GetName(0), "IsOut", StringComparison.OrdinalIgnoreCase)) continue;
                while (await r.ReadAsync(ct))
                    status = (Convert.ToInt32(r.GetValue(0), CultureInfo.InvariantCulture),
                        Convert.ToInt64(r.GetValue(2), CultureInfo.InvariantCulture));
            } while (await r.NextResultAsync(ct));

            if (status is not { } s) throw new InvalidOperationException("The post-item batch returned no status row.");
            if (s.IsOut != 0) return OrderPostOutcome.Refused(OrderCommandReasons.CheckNotFound);
            return s.TrnSeq > 0
                ? OrderPostOutcome.Posted(s.TrnSeq)
                : throw new InvalidOperationException("The post-item batch reported success without a line number.");
        }
        catch (SqlException ex) when (ex.Number == OpenNameRequiredError)
        {
            // THROW runs before any write and, with XACT_ABORT off, leaves the transaction committable for the log row.
            return OrderPostOutcome.Refused(OrderCommandReasons.OpenNameRequired);
        }
        catch (SqlException ex) when (ex.Number == OpenPriceRequiredError)
        {
            return OrderPostOutcome.Refused(OrderCommandReasons.OpenPriceRequired);
        }
    }

    public async Task<IReadOnlyList<long>> PendingSendSeqsAsync(int rvcNo, long checkNo, CancellationToken ct)
    {
        await using var cmd = Command(OrderCommandSql.PendingSendSeqs);
        Rvc(cmd, rvcNo);
        Check(cmd, checkNo);
        var seqs = new List<long>();
        await using var r = await cmd.ExecuteReaderAsync(ct);
        while (await r.ReadAsync(ct)) seqs.Add(Convert.ToInt64(r.GetValue(0), CultureInfo.InvariantCulture));
        return seqs;
    }

    public async Task SendAsync(OrderSendPayload payload, CancellationToken ct)
    {
        await using var cmd = Command(OrderCommandSql.SendOrder);
        SendParameters(cmd, payload);
        await DrainAsync(cmd, ct);
    }

    public async Task VoidUnsentAsync(OrderItemVoidPayload payload, CancellationToken ct)
    {
        await using var cmd = Command(OrderCommandSql.VoidUnsent);
        VoidUnsentParameters(cmd, payload);
        await DrainAsync(cmd, ct);
    }

    public async Task VoidSentAsync(OrderItemVoidPayload payload, CancellationToken ct)
    {
        await using var cmd = Command(OrderCommandSql.VoidSent);
        VoidParameters(cmd, payload);
        await DrainAsync(cmd, ct);
    }

    public async Task<bool> SetCoversAsync(OrderCoversPayload payload, CancellationToken ct)
    {
        await using var cmd = Command(OrderCommandSql.SetCovers);
        CoversParameters(cmd, payload);
        await using var r = await cmd.ExecuteReaderAsync(ct);
        int? isOut = null;
        do
        {
            if (r.FieldCount < 1 || !string.Equals(r.GetName(0), "IsOut", StringComparison.OrdinalIgnoreCase)) continue;
            while (await r.ReadAsync(ct)) isOut = Convert.ToInt32(r.GetValue(0), CultureInfo.InvariantCulture);
        } while (await r.NextResultAsync(ct));
        return isOut switch
        {
            0 => true,
            1 => false,
            _ => throw new InvalidOperationException(
                $"The covers batch returned no usable status row (IsOut={isOut?.ToString(CultureInfo.InvariantCulture) ?? "none"})."),
        };
    }

    public async Task<OrderCheckResult?> ReadCheckAsync(int rvcNo, long checkNo, CancellationToken ct)
    {
        CheckStateHeader header;
        await using (var cmd = Command(OrderCommandSql.ReadHeader))
        {
            Rvc(cmd, rvcNo);
            Check(cmd, checkNo);
            await using var r = await cmd.ExecuteReaderAsync(ct);
            if (!await r.ReadAsync(ct)) return null;
            header = new CheckStateHeader(Convert.ToInt64(r.GetValue(0), CultureInfo.InvariantCulture), ReadText(r, 1),
                (int)ReadNumber(r, 2), ReadNumber(r, 3), ReadNumber(r, 4));
        }

        bool notes;
        await using (var probe = Command(OrderCommandSql.CgTrnExists))
            notes = Convert.ToInt32(await probe.ExecuteScalarAsync(ct), CultureInfo.InvariantCulture) == 1;

        var lines = new List<OrderCheckLine>();
        await using (var cmd = Command(notes ? OrderCommandSql.ReadLinesWithNotes : OrderCommandSql.ReadLines))
        {
            Check(cmd, checkNo);
            await using var r = await cmd.ExecuteReaderAsync(ct);
            while (await r.ReadAsync(ct))
            {
                lines.Add(new OrderCheckLine(
                    LineSeq: Convert.ToInt64(r.GetValue(0), CultureInfo.InvariantCulture),
                    TransactionCode: ReadText(r, 1),
                    ItemCode: ReadText(r, 2)?.Trim(),
                    ItemName: Tcvn(r, 3),
                    Qty: ReadNumber(r, 4),
                    Price: ReadNumber(r, 5),
                    Amount: ReadNumber(r, 6),
                    SeatNo: (int)ReadNumber(r, 7),
                    Cashier: ReadText(r, 8)?.Trim(),
                    Sent: Convert.ToInt32(r.GetValue(9), CultureInfo.InvariantCulture) == 1,
                    Note: Tcvn(r, 10)));
            }
        }

        return new OrderCheckResult(rvcNo, header.CheckNo, header.TableNo?.Trim(), header.Guests, header.SubTotal, header.TotalPaid,
            lines, OrderCheckResult.Fingerprint(header, lines));
    }

    // The parameter shapes of the legacy batches (types as the legacy PDA repository bound them); internal so the tests can
    // hold each batch's free @variables against what is bound.
    internal static void OpenParameters(SqlCommand cmd, OrderCheckOpenPayload payload)
    {
        Rvc(cmd, payload.RvcNo);
        Text(cmd, "@TableNo", payload.TableNo);
        Text(cmd, "@Cashier", payload.Cashier);
        cmd.Parameters.Add("@NumGuest", SqlDbType.Int).Value = payload.NumGuest;
    }

    internal static void PostParameters(SqlCommand cmd, OrderItemPostPayload payload)
    {
        Rvc(cmd, payload.RvcNo);
        Check(cmd, payload.CheckNo);
        Text(cmd, "@Cashier", payload.Cashier);
        Text(cmd, "@ItemCode", payload.ItemCode);
        Money(cmd, "@Qty", payload.Qty);
        cmd.Parameters.Add("@SeatNo", SqlDbType.Int).Value = payload.SeatNo;
        Money(cmd, "@Price", payload.Price);
        Text(cmd, "@Note", payload.Note ?? "");
        Text(cmd, "@OpenName", payload.OpenName);
        cmd.Parameters.Add("@AllowZeroOpenPrice", SqlDbType.Bit).Value = payload.AllowZeroOpenPrice;
    }

    internal static void SendParameters(SqlCommand cmd, OrderSendPayload payload)
    {
        Rvc(cmd, payload.RvcNo);
        Check(cmd, payload.CheckNo);
        Text(cmd, "@Cashier", payload.Cashier);
    }

    internal static void VoidUnsentParameters(SqlCommand cmd, OrderItemVoidPayload payload)
    {
        VoidParameters(cmd, payload);
        Text(cmd, "@AuthorCashier", payload.AuthorCashier ?? payload.Cashier);
    }

    internal static void VoidParameters(SqlCommand cmd, OrderItemVoidPayload payload)
    {
        Rvc(cmd, payload.RvcNo);
        Check(cmd, payload.CheckNo);
        cmd.Parameters.Add("@TrnSeq", SqlDbType.BigInt).Value = payload.TrnSeq;
        Text(cmd, "@VoidBy", payload.Cashier);
        Text(cmd, "@VoidReason", payload.Reason);
    }

    internal static void CoversParameters(SqlCommand cmd, OrderCoversPayload payload)
    {
        Rvc(cmd, payload.RvcNo);
        Check(cmd, payload.CheckNo);
        cmd.Parameters.Add("@NumGuest", SqlDbType.Int).Value = payload.NumGuest;
        cmd.Parameters.Add("@NumChild", SqlDbType.Int).Value = payload.NumChild;
        Text(cmd, "@Cashier", payload.Cashier);
    }

    private SqlCommand Command(string sql) => SqlSiteCommandUnit.Command(connection, transaction, sql);

    /// <summary>Runs a write batch to its end, so an error in a later statement surfaces here instead of being lost.</summary>
    private static async Task DrainAsync(SqlCommand cmd, CancellationToken ct)
    {
        await using var r = await cmd.ExecuteReaderAsync(ct);
        do
        {
            while (await r.ReadAsync(ct)) { /* status grids are informational; a failure throws */ }
        } while (await r.NextResultAsync(ct));
    }

    private static void Rvc(SqlCommand cmd, int rvcNo) => cmd.Parameters.Add("@RVCNo", SqlDbType.Int).Value = rvcNo;

    private static void Check(SqlCommand cmd, long checkNo) => cmd.Parameters.Add("@CheckNo", SqlDbType.BigInt).Value = checkNo;

    // The legacy Dapper shapes: strings as NVARCHAR(4000), decimals as DECIMAL(19,4).
    private static void Text(SqlCommand cmd, string name, string? value) =>
        cmd.Parameters.Add(name, SqlDbType.NVarChar, 4000).Value = (object?)value ?? DBNull.Value;

    private static void Money(SqlCommand cmd, string name, decimal value)
    {
        var p = cmd.Parameters.Add(name, SqlDbType.Decimal);
        p.Precision = 19;
        p.Scale = 4;
        p.Value = value;
    }

    private static string? ReadText(SqlDataReader r, int i) =>
        r.IsDBNull(i) ? null : Convert.ToString(r.GetValue(i), CultureInfo.InvariantCulture);

    private static decimal ReadNumber(SqlDataReader r, int i) =>
        r.IsDBNull(i) ? 0m : Convert.ToDecimal(r.GetValue(i), CultureInfo.InvariantCulture);

    private static string? Tcvn(SqlDataReader r, int i) => r.IsDBNull(i) ? null : Tcvn3Decoder.Decode((byte[])r.GetValue(i))?.Trim();
}
