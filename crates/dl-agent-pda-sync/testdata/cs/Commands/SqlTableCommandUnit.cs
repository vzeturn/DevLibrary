using System.Data;
using System.Globalization;
using System.Text;
using Microsoft.Data.SqlClient;
using MM.PdaOrder.Sync.Contracts.Wire;
using MM.PdaOrder.SyncAgent.Bootstrap;

namespace MM.PdaOrder.SyncAgent.Commands;

/// <summary>Read-side SQL of the table and multi-check verbs (OW1e, ADR 0112); reads only.</summary>
internal static partial class OrderCommandSql
{
    /// <summary>
    /// The money facts of one check: the paid amount on the header, any tender line (TrnCode 200 and up) and any discount line
    /// (TrnCode 101..189). Params: @CheckNo, @RVCNo.
    /// </summary>
    internal const string MoneyFacts =
        "SELECT ISNULL((SELECT TOP 1 TotalPaid FROM dbo.TmpInvoice WHERE CheckNo = @CheckNo AND RVC = @RVCNo), 0), " +
        "CASE WHEN EXISTS (SELECT 1 FROM dbo.TmpTrn WHERE CheckNo = @CheckNo AND RVC = @RVCNo AND TrnCode >= 200) THEN 1 ELSE 0 END, " +
        "CASE WHEN EXISTS (SELECT 1 FROM dbo.TmpTrn WHERE CheckNo = @CheckNo AND RVC = @RVCNo AND TrnCode BETWEEN 101 AND 189) " +
        "THEN 1 ELSE 0 END;";

    /// <summary>
    /// The facts the empty-bill cleanup guards on (ADR 0144). Params: @CheckNo, @RVCNo, @OpenedBefore (<c>yyyy-MM-ddTHH:mm:ss</c>,
    /// store local). One row: the trimmed table, 1 when <c>OpenTime</c> is earlier than the cutoff, the count of every
    /// <c>TmpTrn</c> line of the check (any code: an empty check has none); no row when the check is not in the RVC.
    /// </summary>
    internal const string EmptyBillFacts =
        "SELECT ISNULL(RTRIM(LTRIM(i.TableNo)), ''), " +
        "CASE WHEN i.OpenTime < CONVERT(datetime, @OpenedBefore, 126) THEN 1 ELSE 0 END, " +
        "(SELECT COUNT(*) FROM dbo.TmpTrn t WHERE t.CheckNo = @CheckNo AND t.RVC = @RVCNo) " +
        "FROM dbo.TmpInvoice i WHERE i.CheckNo = @CheckNo AND i.RVC = @RVCNo;";

    /// <summary>The prefix of the column probe; <see cref="SqlTableCommandUnit.MissingColumnsSql"/> adds one VALUES row per column.</summary>
    internal const string MissingColumnsHead = "SELECT need.t, need.c FROM (VALUES ";

    internal const string MissingColumnsTail =
        ") AS need(t, c) WHERE NOT EXISTS (SELECT 1 FROM INFORMATION_SCHEMA.COLUMNS ic " +
        "WHERE ic.TABLE_SCHEMA = N'dbo' AND ic.TABLE_NAME = need.t AND ic.COLUMN_NAME = need.c);";
}

/// <summary>The OW1e store writes of one command, on the connection and transaction of <see cref="SqlSiteCommandStore"/>.</summary>
internal sealed class SqlTableCommandUnit(SqlConnection connection, SqlTransaction transaction) : ITableCommandUnit
{
    internal const string AuditSavepoint = "ow1e_move_audit";

    public async Task<IReadOnlyList<string>> MissingColumnsAsync(IReadOnlyList<StoreColumn> columns, CancellationToken ct)
    {
        if (columns.Count == 0) return [];
        await using var cmd = Command(MissingColumnsSql(columns.Count));
        for (var i = 0; i < columns.Count; i++)
        {
            cmd.Parameters.Add($"@t{i}", SqlDbType.NVarChar, 128).Value = columns[i].Table;
            cmd.Parameters.Add($"@c{i}", SqlDbType.NVarChar, 128).Value = columns[i].Column;
        }
        var missing = new List<string>();
        await using var r = await cmd.ExecuteReaderAsync(ct);
        while (await r.ReadAsync(ct)) missing.Add(r.GetString(0) + "." + r.GetString(1));
        return missing;
    }

    /// <summary>The column probe for <paramref name="count"/> columns: parameter names only, never a value in the text.</summary>
    internal static string MissingColumnsSql(int count)
    {
        var sql = new StringBuilder(OrderCommandSql.MissingColumnsHead);
        for (var i = 0; i < count; i++)
            sql.Append(i == 0 ? "" : ", ").Append(CultureInfo.InvariantCulture, $"(@t{i}, @c{i})");
        return sql.Append(OrderCommandSql.MissingColumnsTail).ToString();
    }

    public async Task<int?> LockCheckAsync(int rvcNo, long checkNo, CancellationToken ct)
    {
        await using var cmd = Command(OrderCommandSql.LockCheck);
        LockParameters(cmd, rvcNo, checkNo);
        await using var r = await cmd.ExecuteReaderAsync(ct);
        if (!await r.ReadAsync(ct)) return null;
        return await r.IsDBNullAsync(1, ct) ? 0 : Convert.ToInt32(r.GetValue(1), CultureInfo.InvariantCulture);
    }

    public async Task<CheckMoneyFacts> MoneyFactsAsync(int rvcNo, long checkNo, CancellationToken ct)
    {
        await using var cmd = Command(OrderCommandSql.MoneyFacts);
        LockParameters(cmd, rvcNo, checkNo);
        await using var r = await cmd.ExecuteReaderAsync(ct);
        if (!await r.ReadAsync(ct)) throw new InvalidOperationException("The money facts query returned no row.");
        var paid = await r.IsDBNullAsync(0, ct) ? 0m : Convert.ToDecimal(r.GetValue(0), CultureInfo.InvariantCulture);
        var tender = Convert.ToInt32(r.GetValue(1), CultureInfo.InvariantCulture) == 1;
        var discount = Convert.ToInt32(r.GetValue(2), CultureInfo.InvariantCulture) == 1;
        return new CheckMoneyFacts(tender || paid != 0m, discount);
    }

    public async Task<EmptyBillFacts?> EmptyBillFactsAsync(int rvcNo, long checkNo, string openedBefore, CancellationToken ct)
    {
        await using var cmd = Command(OrderCommandSql.EmptyBillFacts);
        EmptyBillFactsParameters(cmd, rvcNo, checkNo, openedBefore);
        await using var r = await cmd.ExecuteReaderAsync(ct);
        if (!await r.ReadAsync(ct)) return null;
        var table = await r.IsDBNullAsync(0, ct) ? "" : Convert.ToString(r.GetValue(0), CultureInfo.InvariantCulture)!.Trim();
        return new EmptyBillFacts(
            table,
            Convert.ToInt32(r.GetValue(1), CultureInfo.InvariantCulture) == 1,
            Convert.ToInt32(r.GetValue(2), CultureInfo.InvariantCulture));
    }

    public async Task CancelEmptyAsync(OrderCheckCancelEmptyPayload payload, CancellationToken ct)
    {
        await using var cancel = Command(OrderCommandSql.CancelOrder);
        CancelEmptyParameters(cancel, payload);
        await cancel.ExecuteNonQueryAsync(ct);
    }

    public async Task SetPoNumberAsync(OrderPoNumberPayload payload, CancellationToken ct)
    {
        await using var cmd = Command(OrderCommandSql.SetPoNumber);
        PoNumberParameters(cmd, payload);
        await cmd.ExecuteNonQueryAsync(ct);
    }

    public async Task ChangeTableAsync(OrderTableChangePayload payload, CancellationToken ct)
    {
        await using var cmd = Command(OrderCommandSql.TableInfo);
        TableInfoParameters(cmd, payload);
        await cmd.ExecuteNonQueryAsync(ct);
    }

    public async Task TransferAsync(OrderTableTransferPayload payload, CancellationToken ct)
    {
        await using (var header = Command(OrderCommandSql.TransferHeader))
        {
            TransferHeaderParameters(header, payload);
            await header.ExecuteNonQueryAsync(ct);
        }
        await using var info = Command(OrderCommandSql.TransferLineInfo);
        TransferLineInfoParameters(info, payload);
        await info.ExecuteNonQueryAsync(ct);
    }

    public async Task MergeAsync(OrderCheckMergePayload payload, CancellationToken ct)
    {
        await using (var lines = Command(OrderCommandSql.MergeLines))
        {
            MergeLinesParameters(lines, payload);
            await lines.ExecuteNonQueryAsync(ct);
        }
        await using (var info = Command(OrderCommandSql.MergeLineInfo))
        {
            MergeLineInfoParameters(info, payload);
            await info.ExecuteNonQueryAsync(ct);
        }
        await using (var cancel = Command(OrderCommandSql.CancelOrder))
        {
            CancelParameters(cancel, payload);
            await cancel.ExecuteNonQueryAsync(ct);
        }
        // Legacy order: the target (now holding every line), then the emptied source.
        await RecalcAsync(payload.TargetCheckNo, ct);
        await RecalcAsync(payload.SourceCheckNo, ct);
    }

    public async Task<IReadOnlyList<long>> LockItemLinesAsync(int rvcNo, long checkNo, CancellationToken ct)
    {
        await using var cmd = Command(OrderCommandSql.MoveSourceLines);
        LockParameters(cmd, rvcNo, checkNo);
        var seqs = new List<long>();
        await using var r = await cmd.ExecuteReaderAsync(ct);
        while (await r.ReadAsync(ct)) seqs.Add(Convert.ToInt64(r.GetValue(0), CultureInfo.InvariantCulture));
        return seqs;
    }

    public async Task<int?> MoveItemsAsync(OrderItemsMovePayload payload, IReadOnlyList<long> trnSeqs, CancellationToken ct)
    {
        await using (var line = Command(OrderCommandSql.MoveLine))
        {
            var seq = MoveLineParameters(line, payload);
            foreach (var trnSeq in trnSeqs)
            {
                seq.Value = trnSeq;
                if (await line.ExecuteNonQueryAsync(ct) == 0)
                    throw new InvalidOperationException(
                        $"Line {trnSeq} left check {payload.FromCheckNo} while it was locked for the move; nothing is committed.");
            }
        }
        await using (var info = Command(OrderCommandSql.MoveLineInfo))
        {
            var trnNo = MoveLineInfoParameters(info, payload);
            foreach (var trnSeq in trnSeqs)
            {
                trnNo.Value = trnSeq;
                await info.ExecuteNonQueryAsync(ct);
            }
        }
        var auditError = await AuditMoveAsync(payload, trnSeqs, ct);
        // Legacy order: the source, then the target.
        await RecalcAsync(payload.FromCheckNo, ct);
        await RecalcAsync(payload.ToCheckNo, ct);
        return auditError;
    }

    /// <summary>
    /// The legacy MOVE audit, best-effort as the legacy service had it (a build without <c>dbo.TrnActivities</c> or
    /// <c>STRING_SPLIT</c> still moves): a failure rolls back to a savepoint so the move's own writes stand. A missing grant is
    /// not swallowed: it fails the command so the grants are repaired instead of the audit being lost for good. Returns the SQL
    /// error number of a refused audit (the executor logs it); null when the audit was written.
    /// </summary>
    private async Task<int?> AuditMoveAsync(OrderItemsMovePayload payload, IReadOnlyList<long> trnSeqs, CancellationToken ct)
    {
        await transaction.SaveAsync(AuditSavepoint, ct);
        try
        {
            await using var audit = Command(OrderCommandSql.LogTrnMove);
            LogTrnMoveParameters(audit, payload, trnSeqs);
            await audit.ExecuteNonQueryAsync(ct);
            return null;
        }
        catch (SqlException ex) when (!SqlPermissionDenied.Is(ex))
        {
            await transaction.RollbackAsync(AuditSavepoint, ct);
            return ex.Number;
        }
    }

    /// <summary>Legacy recalculation of one check header; a reported failure throws so the command rolls back.</summary>
    private async Task RecalcAsync(long checkNo, CancellationToken ct)
    {
        await using var cmd = Command(OrderCommandSql.ReCalcBill);
        cmd.Parameters.Add("@CheckNo", SqlDbType.BigInt).Value = checkNo;
        int? failed = null;
        string? message = null;
        await using (var r = await cmd.ExecuteReaderAsync(ct))
        {
            // The status grid is the batch's last one; a grid the vendor procedure returns first is skipped.
            do
            {
                if (r.FieldCount < 3 || !string.Equals(r.GetName(0), "RecalcFailed", StringComparison.OrdinalIgnoreCase))
                {
                    while (await r.ReadAsync(ct)) { /* not the status grid */ }
                    continue;
                }
                if (await r.ReadAsync(ct))
                {
                    failed = Convert.ToInt32(r.GetValue(0), CultureInfo.InvariantCulture);
                    message = await r.IsDBNullAsync(2, ct) ? null : Convert.ToString(r.GetValue(2), CultureInfo.InvariantCulture);
                }
            } while (await r.NextResultAsync(ct));
        }
        if (failed is null) throw new InvalidOperationException($"The recalculation of check {checkNo} returned no status row.");
        if (failed != 0)
            throw new InvalidOperationException($"The store refused to recalculate check {checkNo}: {message}; nothing is committed.");
    }

    // ---- parameters: exactly the free variables of each legacy statement (a test pins every pairing) ----

    internal static void LockParameters(SqlCommand cmd, int rvcNo, long checkNo)
    {
        cmd.Parameters.Add("@CheckNo", SqlDbType.BigInt).Value = checkNo;
        cmd.Parameters.Add("@RVCNo", SqlDbType.Int).Value = rvcNo;
    }

    internal static void TableInfoParameters(SqlCommand cmd, OrderTableChangePayload payload)
    {
        LockParameters(cmd, payload.RvcNo, payload.CheckNo);
        Text(cmd, "@TableNo", payload.TableNo);
        cmd.Parameters.Add("@NumGuest", SqlDbType.Int).Value = payload.NumGuest;
    }

    internal static void PoNumberParameters(SqlCommand cmd, OrderPoNumberPayload payload)
    {
        LockParameters(cmd, payload.RvcNo, payload.CheckNo);
        Text(cmd, "@PONumber", payload.PoNumber);
    }

    internal static void TransferHeaderParameters(SqlCommand cmd, OrderTableTransferPayload payload)
    {
        Text(cmd, "@ToTableNo", payload.ToTableNo.Trim());
        LockParameters(cmd, payload.RvcNo, payload.CheckNo);
    }

    internal static void TransferLineInfoParameters(SqlCommand cmd, OrderTableTransferPayload payload)
    {
        Text(cmd, "@ToTableNo", payload.ToTableNo.Trim());
        cmd.Parameters.Add("@CheckNo", SqlDbType.BigInt).Value = payload.CheckNo;
    }

    internal static void MergeLinesParameters(SqlCommand cmd, OrderCheckMergePayload payload)
    {
        MergeLineInfoParameters(cmd, payload);
        cmd.Parameters.Add("@RVCNo", SqlDbType.Int).Value = payload.RvcNo;
    }

    internal static void MergeLineInfoParameters(SqlCommand cmd, OrderCheckMergePayload payload)
    {
        cmd.Parameters.Add("@Target", SqlDbType.BigInt).Value = payload.TargetCheckNo;
        cmd.Parameters.Add("@Source", SqlDbType.BigInt).Value = payload.SourceCheckNo;
    }

    internal static void EmptyBillFactsParameters(SqlCommand cmd, int rvcNo, long checkNo, string openedBefore)
    {
        LockParameters(cmd, rvcNo, checkNo);
        Text(cmd, "@OpenedBefore", openedBefore);
    }

    /// <summary>The cancel batch of the empty-bill cleanup: the same three free variables as the merge's cancel, <c>@CancelBy</c> is the cashier.</summary>
    internal static void CancelEmptyParameters(SqlCommand cmd, OrderCheckCancelEmptyPayload payload)
    {
        LockParameters(cmd, payload.RvcNo, payload.CheckNo);
        Text(cmd, "@CancelBy", payload.Cashier);
    }

    internal static void CancelParameters(SqlCommand cmd, OrderCheckMergePayload payload)
    {
        LockParameters(cmd, payload.RvcNo, payload.SourceCheckNo);
        Text(cmd, "@CancelBy", payload.Cashier);
    }

    /// <summary>Binds the move statement and returns its <c>@TrnSeq</c> parameter, set per line.</summary>
    internal static SqlParameter MoveLineParameters(SqlCommand cmd, OrderItemsMovePayload payload)
    {
        cmd.Parameters.Add("@ToCheckNo", SqlDbType.BigInt).Value = payload.ToCheckNo;
        var seq = cmd.Parameters.Add("@TrnSeq", SqlDbType.BigInt);
        cmd.Parameters.Add("@FromCheckNo", SqlDbType.BigInt).Value = payload.FromCheckNo;
        cmd.Parameters.Add("@RVCNo", SqlDbType.Int).Value = payload.RvcNo;
        return seq;
    }

    /// <summary>Binds the line-info move statement and returns its <c>@TrnNo</c> parameter, set per line.</summary>
    internal static SqlParameter MoveLineInfoParameters(SqlCommand cmd, OrderItemsMovePayload payload)
    {
        cmd.Parameters.Add("@ToCheckNo", SqlDbType.BigInt).Value = payload.ToCheckNo;
        var trnNo = cmd.Parameters.Add("@TrnNo", SqlDbType.BigInt);
        cmd.Parameters.Add("@FromCheckNo", SqlDbType.BigInt).Value = payload.FromCheckNo;
        return trnNo;
    }

    internal static void LogTrnMoveParameters(SqlCommand cmd, OrderItemsMovePayload payload, IReadOnlyList<long> trnSeqs)
    {
        cmd.Parameters.Add("@RVC", SqlDbType.Int).Value = payload.RvcNo;
        cmd.Parameters.Add("@FromCheckNo", SqlDbType.BigInt).Value = payload.FromCheckNo;
        cmd.Parameters.Add("@ToCheckNo", SqlDbType.BigInt).Value = payload.ToCheckNo;
        Text(cmd, "@Cashier", payload.Cashier);
        Text(cmd, "@TrnSeqCsv", string.Join(',', trnSeqs.Select(s => s.ToString(CultureInfo.InvariantCulture))));
    }

    private SqlCommand Command(string sql) => SqlSiteCommandUnit.Command(connection, transaction, sql);

    // The legacy Dapper shape: strings as NVARCHAR(4000).
    private static void Text(SqlCommand cmd, string name, string? value) =>
        cmd.Parameters.Add(name, SqlDbType.NVarChar, 4000).Value = (object?)value ?? DBNull.Value;
}
