using Microsoft.Data.SqlClient;
using MM.PdaOrder.Sync.Capture.Source;
using MM.PdaOrder.Sync.Contracts.Wire;

namespace MM.PdaOrder.SyncAgent.Commands;

/// <summary>
/// <see cref="ITxnCounterUnit"/> on the command transaction (ADR 0161). The counter row is read with <c>UPDLOCK, HOLDLOCK</c>
/// so <c>GetRVCSeqNumber</c>'s single <c>UPDATE ... OUTPUT</c> waits for the commit and then continues above the raised value.
/// The backing-table reads mirror the procedure's self-heal floor (same tables, same <c>RVC * 100,000,000</c> prefix range)
/// and run only on tables that exist; table names come from the fixed list below, never from the payload.
/// </summary>
internal sealed class SqlTxnCounterUnit(SqlConnection connection, SqlTransaction transaction) : ITxnCounterUnit
{
    private const int TimeoutSeconds = 15;

    internal const string OwnObjectSql =
        "SELECT COUNT(*) FROM sys.objects WHERE schema_id = SCHEMA_ID(N'dbo') AND type IN ('SO', 'U') AND name = @Object;";

    internal const string LockSql =
        "IF NOT EXISTS (SELECT 1 FROM dbo.Para WITH (UPDLOCK, HOLDLOCK) WHERE RVC = @Rvc AND ParaName = @Name) " +
        "INSERT INTO dbo.Para (RVC, ParaName, ParaStr) VALUES (@Rvc, @Name, '0'); " +
        "SELECT MAX(ISNULL(TRY_CAST(NULLIF(RTRIM(ParaStr), '') AS bigint), 0)) FROM dbo.Para WITH (UPDLOCK, HOLDLOCK) " +
        "WHERE RVC = @Rvc AND ParaName = @Name;";

    internal const string RaiseSql =
        "UPDATE dbo.Para SET ParaStr = CAST(@Value AS varchar(20)) " +
        "WHERE RVC = @Rvc AND ParaName = @Name AND ISNULL(TRY_CAST(NULLIF(RTRIM(ParaStr), '') AS bigint), 0) < @Value;";

    /// <summary>(table, column, filtered by RVC, raw counter allowed) per counter: what <c>GetRVCSeqNumber</c> floors against.</summary>
    internal static readonly IReadOnlyDictionary<string, IReadOnlyList<(string Table, string Column, bool ByRvc, bool Raw)>> Backing =
        new Dictionary<string, IReadOnlyList<(string, string, bool, bool)>>(StringComparer.Ordinal)
        {
            [TxnCounterNames.LastCheck] = [("Invoice", "CheckNo", true, false), ("TmpInvoice", "CheckNo", true, false), ("IOrgInvoice", "CheckNo", false, false)],
            [TxnCounterNames.LastTrn] = [("Trn", "TrnSeq", true, false), ("TmpTrn", "TrnSeq", true, false)],
            [TxnCounterNames.LastPayment] = [("Payment", "PaymentNo", true, true), ("TmpPayment", "PaymentNo", true, true)],
            [TxnCounterNames.CloseNumber] = [("TaxTable", "Sub2", true, false), ("TmpTaxTable", "Sub2", true, false)],
        };

    /// <summary>The highest counter in one backing table: the composed range of the RVC, or (payments) the raw counter.</summary>
    internal static string InUseSql(string table, string column, bool byRvc, bool raw)
    {
        var c = SqlBuilder.Quote(column);
        var counter = raw
            ? $"CASE WHEN {c} >= @Prefix AND {c} < @Prefix + 100000000 THEN {c} - @Prefix WHEN {c} >= 1 AND {c} < 100000000 THEN {c} END"
            : $"CASE WHEN {c} >= @Prefix AND {c} < @Prefix + 100000000 THEN {c} - @Prefix END";
        var where = byRvc ? "WHERE RVC = @Rvc" : $"WHERE {c} >= @Prefix AND {c} < @Prefix + 100000000";
        return $"IF OBJECT_ID(N'dbo.{table}', N'U') IS NOT NULL " +
               $"SELECT CAST(MAX({counter}) AS bigint) FROM dbo.{SqlBuilder.Quote(table)} {where}; ELSE SELECT CAST(NULL AS bigint);";
    }

    public async Task<bool> HasOwnCounterObjectAsync(int rvcNo, string name, CancellationToken ct)
    {
        if (name is not (TxnCounterNames.LastCheck or TxnCounterNames.LastTrn)) return false;
        await using var cmd = Command(OwnObjectSql);
        cmd.Parameters.AddWithValue("@Object", name + rvcNo.ToString(System.Globalization.CultureInfo.InvariantCulture));
        return Convert.ToInt32(await cmd.ExecuteScalarAsync(ct), System.Globalization.CultureInfo.InvariantCulture) > 0;
    }

    public async Task<long> LockAsync(int rvcNo, string name, CancellationToken ct)
    {
        await using var cmd = Command(LockSql);
        AddKey(cmd, rvcNo, name);
        var value = await cmd.ExecuteScalarAsync(ct);
        return value is null or DBNull ? 0 : Convert.ToInt64(value, System.Globalization.CultureInfo.InvariantCulture);
    }

    public async Task RaiseAsync(int rvcNo, string name, long value, CancellationToken ct)
    {
        await using var cmd = Command(RaiseSql);
        AddKey(cmd, rvcNo, name);
        cmd.Parameters.Add("@Value", System.Data.SqlDbType.BigInt).Value = value;
        await cmd.ExecuteNonQueryAsync(ct);
    }

    public async Task<long?> InUseAsync(int rvcNo, string name, CancellationToken ct)
    {
        long? max = null;
        foreach (var (table, column, byRvc, raw) in Backing[name])
        {
            await using var cmd = Command(InUseSql(table, column, byRvc, raw));
            cmd.Parameters.Add("@Rvc", System.Data.SqlDbType.Int).Value = rvcNo;
            cmd.Parameters.Add("@Prefix", System.Data.SqlDbType.BigInt).Value = rvcNo * 100_000_000L;
            var v = await cmd.ExecuteScalarAsync(ct);
            if (v is not null and not DBNull)
            {
                var n = Convert.ToInt64(v, System.Globalization.CultureInfo.InvariantCulture);
                if (max is null || n > max) max = n;
            }
        }
        return max;
    }

    private static void AddKey(SqlCommand cmd, int rvcNo, string name)
    {
        cmd.Parameters.Add("@Rvc", System.Data.SqlDbType.Int).Value = rvcNo;
        cmd.Parameters.Add("@Name", System.Data.SqlDbType.VarChar, 50).Value = name;
    }

    private SqlCommand Command(string sql) =>
        new(sql, connection, transaction) { CommandTimeout = TimeoutSeconds };
}