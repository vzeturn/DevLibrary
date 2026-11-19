using System.Data;
using System.Globalization;
using Microsoft.Data.SqlClient;
using MM.PdaOrder.Sync.Contracts.Wire;

namespace MM.PdaOrder.SyncAgent.Commands;

/// <summary>
/// <see cref="ISiteCommandStore"/> on the store's SQL Server through the least-privilege <c>pda_sync</c> login. Its write
/// grants are exactly the statements below (<c>Bootstrap/Sql/060_command_grants.sql</c>, checked by a test). All SQL is
/// parameterized; vendor tables and columns keep their names (docs/internal/pos-compat.md).
/// </summary>
public sealed class SqlSiteCommandStore(Func<string> connectionString) : ISiteCommandStore
{
    internal const string ReadLogSql =
        "SELECT Status, ResultJson, AtUtc FROM dbo.Pda_SyncCommandLog WHERE CommandId = @CommandId;";

    // UPDLOCK + HOLDLOCK takes a key-range lock even when the row is missing: a second delivery of the same id waits for
    // the first transaction and then reads its log row instead of executing again.
    internal const string LockLogSql =
        "SELECT Status, ResultJson, AtUtc FROM dbo.Pda_SyncCommandLog WITH (UPDLOCK, HOLDLOCK) WHERE CommandId = @CommandId;";

    internal const string InsertLogSql =
        "INSERT INTO dbo.Pda_SyncCommandLog (CommandId, Status, ResultJson, AtUtc) VALUES (@CommandId, @Status, @ResultJson, @AtUtc);";

    public async Task<SyncCommandResult?> FindRecordedAsync(Guid commandId, CancellationToken ct)
    {
        await using var c = await OpenAsync(ct);
        return await ReadLogAsync(c, null, ReadLogSql, commandId, ct);
    }

    public async Task<SyncCommandResult> RunOnceAsync(
        Guid commandId, Func<ISiteCommandUnit, CancellationToken, Task<SyncCommandResult>> work, CancellationToken ct)
    {
        await using var c = await OpenAsync(ct);
        // Disposing an uncommitted transaction rolls it back: an exception leaves neither the business write nor the log row.
        await using var tx = (SqlTransaction)await c.BeginTransactionAsync(IsolationLevel.ReadCommitted, ct);
        var recorded = await ReadLogAsync(c, tx, LockLogSql, commandId, ct);
        if (recorded is not null)
        {
            await tx.CommitAsync(ct);
            return recorded;
        }

        var result = await work(new SqlSiteCommandUnit(c, tx), ct);
        if (result.CommandId != commandId)
            throw new InvalidOperationException($"Command {commandId} produced the result of command {result.CommandId}.");

        await using (var insert = SqlSiteCommandUnit.Command(c, tx, InsertLogSql))
        {
            insert.Parameters.Add("@CommandId", SqlDbType.UniqueIdentifier).Value = commandId;
            insert.Parameters.Add("@Status", SqlDbType.NVarChar, 16).Value = result.Status.ToString();
            insert.Parameters.Add("@ResultJson", SqlDbType.NVarChar, -1).Value = SyncJson.Serialize(result);
            insert.Parameters.Add("@AtUtc", SqlDbType.DateTime2).Value = result.AtUtc.UtcDateTime;
            await insert.ExecuteNonQueryAsync(ct);
        }
        await tx.CommitAsync(ct);
        return result;
    }

    private static async Task<SyncCommandResult?> ReadLogAsync(
        SqlConnection c, SqlTransaction? tx, string sql, Guid commandId, CancellationToken ct)
    {
        await using var cmd = SqlSiteCommandUnit.Command(c, tx, sql);
        cmd.Parameters.Add("@CommandId", SqlDbType.UniqueIdentifier).Value = commandId;
        await using var r = await cmd.ExecuteReaderAsync(ct);
        if (!await r.ReadAsync(ct)) return null;

        var status = r.GetString(0);
        if (!await r.IsDBNullAsync(1, ct))
            return SyncJson.DeserializeCommandResult(r.GetString(1))
                ?? throw new InvalidOperationException($"Command log row {commandId} holds an unreadable result.");
        if (!Enum.TryParse<CommandStatus>(status, ignoreCase: true, out var parsed))
            throw new InvalidOperationException($"Command log row {commandId} has unknown status '{status}'.");
        var at = DateTime.SpecifyKind(r.GetDateTime(2), DateTimeKind.Utc);
        return new SyncCommandResult(commandId, parsed, null, null, new DateTimeOffset(at));
    }

    private async Task<SqlConnection> OpenAsync(CancellationToken ct)
    {
        var c = new SqlConnection(connectionString());
        try
        {
            await c.OpenAsync(ct);
            return c;
        }
        catch
        {
            await c.DisposeAsync();
            throw;
        }
    }
}

/// <summary>The store writes of one command, on the connection and transaction of <see cref="SqlSiteCommandStore"/>.</summary>
internal sealed class SqlSiteCommandUnit(SqlConnection connection, SqlTransaction transaction) : ISiteCommandUnit
{
    private const int TimeoutSeconds = 15;

    public IOrderCommandUnit Orders { get; } = new SqlOrderCommandUnit(connection, transaction);

    public ITableCommandUnit Tables { get; } = new SqlTableCommandUnit(connection, transaction);

    internal const string OpenCheckSql =
        "SELECT COUNT(*) FROM dbo.TmpInvoice WHERE CheckNo = @CheckNo AND LTRIM(RTRIM(CAST(RVC AS NVARCHAR(20)))) = @Rvc;";

    // ADR 0100 §4: the header row is update-locked so POS WIN cannot change the check between this read and the command's
    // writes. Legacy order batches X-lock the same row at their end (dbo.UpdateTmpInvoice); this only takes it earlier.
    internal const string CheckStateSql =
        "SELECT CheckNo, TableNo, NGuest, SubTotal, TotalPaid FROM dbo.TmpInvoice WITH (UPDLOCK) WHERE CheckNo = @CheckNo; " +
        "SELECT TrnSeq, TrnCode, ItemCode, TrnQty, ItemPrice FROM dbo.TmpTrn WHERE CheckNo = @CheckNo;";

    public async Task<string?> CheckFingerprintAsync(long checkNo, CancellationToken ct)
    {
        await using var cmd = Command(connection, transaction, CheckStateSql);
        cmd.Parameters.Add("@CheckNo", SqlDbType.BigInt).Value = checkNo;
        await using var r = await cmd.ExecuteReaderAsync(ct);
        if (!await r.ReadAsync(ct)) return null;
        var header = new CheckStateHeader(
            Convert.ToInt64(r.GetValue(0), CultureInfo.InvariantCulture), ReadText(r, 1), (int)ReadNumber(r, 2), ReadNumber(r, 3), ReadNumber(r, 4));
        if (await r.ReadAsync(ct))
            throw new InvalidOperationException($"Open check {checkNo} has more than one dbo.TmpInvoice row.");

        if (!await r.NextResultAsync(ct)) throw new InvalidOperationException("The check state query returned no line set.");
        var lines = new List<CheckStateLine>();
        while (await r.ReadAsync(ct))
            lines.Add(new CheckStateLine(Convert.ToInt64(r.GetValue(0), CultureInfo.InvariantCulture), ReadText(r, 1), ReadText(r, 2),
                ReadNumber(r, 3), ReadNumber(r, 4)));
        return CheckStateFingerprint.Compute(header, lines);
    }

    private static string? ReadText(SqlDataReader r, int i) =>
        r.IsDBNull(i) ? null : Convert.ToString(r.GetValue(i), CultureInfo.InvariantCulture);

    private static decimal ReadNumber(SqlDataReader r, int i) =>
        r.IsDBNull(i) ? 0m : Convert.ToDecimal(r.GetValue(i), CultureInfo.InvariantCulture);

    internal const string KdsAcceptColumnsSql =
        "SELECT CASE WHEN COL_LENGTH(N'dbo.tmpTrnInfo', N'KdsAcceptedTime') IS NOT NULL " +
        "AND COL_LENGTH(N'dbo.tmpTrnInfo', N'KdsAcceptedBy') IS NOT NULL THEN 1 ELSE 0 END;";

    /// <summary>Per action, ported from the legacy SRsp_Kds* procedures; a line already in the target state is not touched.</summary>
    internal static readonly IReadOnlyDictionary<string, string> KdsLineSql = new Dictionary<string, string>(StringComparer.Ordinal)
    {
        [KdsLineActions.Accept] =
            "UPDATE dbo.tmpTrnInfo SET KdsAcceptedTime = GETDATE(), KdsAcceptedBy = @Cashier " +
            "WHERE CheckNo = @CheckNo AND TrnNo = @TrnNo AND KdsAcceptedTime IS NULL;",
        [KdsLineActions.Unaccept] =
            "UPDATE dbo.tmpTrnInfo SET KdsAcceptedTime = NULL, KdsAcceptedBy = NULL " +
            "WHERE CheckNo = @CheckNo AND TrnNo = @TrnNo AND KdsAcceptedTime IS NOT NULL;",
        [KdsLineActions.Done] =
            "UPDATE dbo.tmpTrnInfo SET IsDone = 1, DoneTime = GETDATE() " +
            "WHERE CheckNo = @CheckNo AND TrnNo = @TrnNo AND ISNULL(IsDone, 0) = 0;",
        [KdsLineActions.Undone] =
            "UPDATE dbo.tmpTrnInfo SET IsDone = 0, DoneTime = NULL " +
            "WHERE CheckNo = @CheckNo AND TrnNo = @TrnNo AND ISNULL(IsDone, 0) = 1;",
    };

    // Ported from pda.SRsp_SaveReservation.
    internal const string InsertReservationSql =
        "INSERT INTO dbo.TableReservation (GuestInfo, NumAdult, NumChild, FromDateTime, ToDateTime, TableNum, TelNum, Booker, " +
        "CompanyName, VATCode, Address, RVC) VALUES (@GuestInfo, @NumAdult, @NumChild, @FromDateTime, @ToDateTime, @TableNum, " +
        "@TelNum, @Booker, @CompanyName, @VATCode, @Address, @Rvc); SELECT CAST(SCOPE_IDENTITY() AS INT);";

    internal const string DeleteReservationSql =
        "DELETE FROM dbo.TableReservation WHERE nID = @Id AND LTRIM(RTRIM(ISNULL(RVC, N''))) = @Rvc;";

    public async Task<bool> OpenCheckBelongsToRvcAsync(int rvcNo, long checkNo, CancellationToken ct)
    {
        await using var cmd = Command(connection, transaction, OpenCheckSql);
        cmd.Parameters.Add("@CheckNo", SqlDbType.BigInt).Value = checkNo;
        cmd.Parameters.Add("@Rvc", SqlDbType.NVarChar, 20).Value = Rvc(rvcNo);
        return Convert.ToInt32(await cmd.ExecuteScalarAsync(ct), CultureInfo.InvariantCulture) > 0;
    }

    public async Task<bool> KdsAcceptColumnsExistAsync(CancellationToken ct)
    {
        await using var cmd = Command(connection, transaction, KdsAcceptColumnsSql);
        return Convert.ToInt32(await cmd.ExecuteScalarAsync(ct), CultureInfo.InvariantCulture) == 1;
    }

    public async Task<int> ApplyKdsLineStateAsync(
        long checkNo, IReadOnlyList<long> lineSeqs, string action, string cashier, CancellationToken ct)
    {
        if (!KdsLineSql.TryGetValue(action, out var sql)) throw new ArgumentOutOfRangeException(nameof(action), action, "Unknown KDS action.");
        await using var cmd = Command(connection, transaction, sql);
        cmd.Parameters.Add("@CheckNo", SqlDbType.BigInt).Value = checkNo;
        var trnNo = cmd.Parameters.Add("@TrnNo", SqlDbType.BigInt);
        if (action == KdsLineActions.Accept) Text(cmd, "@Cashier", cashier);

        var changed = 0;
        foreach (var seq in lineSeqs)
        {
            trnNo.Value = seq;
            changed += await cmd.ExecuteNonQueryAsync(ct);
        }
        return changed;
    }

    public async Task<int> InsertReservationAsync(ReservationSavePayload reservation, CancellationToken ct)
    {
        await using var cmd = Command(connection, transaction, InsertReservationSql);
        Text(cmd, "@GuestInfo", reservation.GuestInfo);
        cmd.Parameters.Add("@NumAdult", SqlDbType.Int).Value = reservation.NumAdult;
        cmd.Parameters.Add("@NumChild", SqlDbType.Int).Value = reservation.NumChild;
        cmd.Parameters.Add("@FromDateTime", SqlDbType.DateTime).Value = reservation.FromDateTime;
        cmd.Parameters.Add("@ToDateTime", SqlDbType.DateTime).Value = reservation.ToDateTime;
        Text(cmd, "@TableNum", reservation.TableNum);
        Text(cmd, "@TelNum", reservation.TelNum);
        Text(cmd, "@Booker", reservation.Booker);
        Text(cmd, "@CompanyName", reservation.CompanyName);
        Text(cmd, "@VATCode", reservation.VatCode);
        Text(cmd, "@Address", reservation.Address);
        Text(cmd, "@Rvc", Rvc(reservation.RvcNo));
        var id = await cmd.ExecuteScalarAsync(ct);
        return id is null or DBNull
            ? throw new InvalidOperationException("dbo.TableReservation insert returned no identity.")
            : Convert.ToInt32(id, CultureInfo.InvariantCulture);
    }

    public async Task<bool> DeleteReservationAsync(int rvcNo, int id, CancellationToken ct)
    {
        await using var cmd = Command(connection, transaction, DeleteReservationSql);
        cmd.Parameters.Add("@Id", SqlDbType.Int).Value = id;
        cmd.Parameters.Add("@Rvc", SqlDbType.NVarChar, 20).Value = Rvc(rvcNo);
        return await cmd.ExecuteNonQueryAsync(ct) > 0;
    }

    internal static SqlCommand Command(SqlConnection c, SqlTransaction? tx, string sql) =>
        new(sql, c, tx) { CommandTimeout = TimeoutSeconds };

    private static string Rvc(int rvcNo) => rvcNo.ToString(CultureInfo.InvariantCulture);

    private static void Text(SqlCommand cmd, string name, string? value) =>
        cmd.Parameters.Add(name, SqlDbType.NVarChar, -1).Value = (object?)value ?? DBNull.Value;
}
