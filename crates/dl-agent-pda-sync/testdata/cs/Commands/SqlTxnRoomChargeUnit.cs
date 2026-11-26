using System.Data;
using System.Globalization;
using System.Text.RegularExpressions;
using Microsoft.Data.SqlClient;

namespace MM.PdaOrder.SyncAgent.Commands;

/// <summary>
/// <see cref="ITxnRoomChargeUnit"/> on the command transaction (ADR 0162). It posts through the vendor
/// <c>PostInterfaceTransactionToFolio</c> exactly as POS WIN does and reverses by the same rule (rows <c>'A'</c> to <c>'V'</c> plus
/// negated mirror rows), so a cloud bill and a POS WIN bill look the same in the folio. The Front Office database name is read
/// from <c>dbo.Para</c> (<c>InterfaceDB</c>, default <c>SMILE_FO</c>), must match <c>^[A-Za-z0-9_]+$</c> and must be usable by the login
/// (<see cref="DatabaseAccessSql"/>); it is the only interpolated text, bracketed, and every value is a parameter.
/// </summary>
internal sealed partial class SqlTxnRoomChargeUnit(SqlConnection connection, SqlTransaction? transaction) : ITxnRoomChargeUnit
{
    private const int TimeoutSeconds = 15;
    internal const string DefaultDatabase = "SMILE_FO";

    internal const string InterfaceDbSql =
        "SELECT TOP 1 LTRIM(RTRIM(ParaStr)) FROM dbo.Para WHERE ParaName = 'InterfaceDB' AND RVC = @Rvc;";

    /// <summary>
    /// <c>HAS_DBACCESS</c>, not <c>DB_ID</c> (G-03, as <see cref="Bootstrap.SqlFoSchemaProbe"/>): a login without
    /// <c>VIEW ANY DATABASE</c> (the CT server denies it, a hardened site may too) gets NULL from <c>DB_ID</c> for a database it
    /// does not own yet may use, which refused every room charge as <c>txn_folio_database_invalid</c>. Only 1 (usable) resolves;
    /// 0 (there but closed to this login, no <c>pda_sync</c> user: run grants, script 160) and NULL (no such database) keep the refusal.
    /// </summary>
    internal const string DatabaseAccessSql = "SELECT HAS_DBACCESS(@Name);";

    internal const string LinkSql =
        "SELECT TOP 1 LinkCode FROM dbo.TrnLinkByRVC WHERE RVC = @Rvc AND TrnCode = @TrnCode AND TrnSubCode = @TrnSub;";

    // The in-house check POS WIN's room-charge lookup applies (FolioStatus 2 = in house, no-post folios refused). The vendor
    // procedure itself checks nothing, so this is the only guard against posting to a checked-out room.
    internal const string FoliosByRoomSql =
        "SELECT FolioNum, RoomCode FROM {0}.dbo.Folio WHERE RTRIM(RoomCode) = @Room AND FolioStatus = 2 AND COALESCE(NoPostFlag, 0) = 0;";

    internal const string FolioByNumberSql =
        "SELECT FolioNum, RoomCode FROM {0}.dbo.Folio WHERE FolioNum = @FolioNum AND FolioStatus = 2 AND COALESCE(NoPostFlag, 0) = 0;";

    // UPDLOCK + HOLDLOCK takes a key-range lock until commit: two commands of one bill cannot both see "no active rows" and both post.
    internal const string ActiveSql =
        "SELECT TransactionID, TransactionAmount FROM {0}.dbo.FolioTransaction WITH (UPDLOCK, HOLDLOCK) " +
        "WHERE FolioNum = @FolioNum AND RefNumber = @Ref AND FolioBalanceCode = 'A' ORDER BY TransactionID;";

    internal const string ReverseSql = """
        UPDATE {0}.dbo.FolioTransaction SET FolioBalanceCode = 'V' WHERE TransactionID IN ({1});
        INSERT INTO {0}.dbo.FolioTransaction (
            FolioNum, TransactionCode, TransactionSubCode, TransactionDate, RoomCode, RefNumber,
            TransactionAmount, FolioBalanceCode, MealPeriodNumber, Comment, TaxAmount, ServiceCharge,
            SubAmount, SystemTime, OriginAmount, OriginCurrency, TaxCode, OriginFolioNumber, OriginRoom,
            PostingClerkID, Quantity, RptExRate, SrcCode, BillAmount, BillExRate,
            StsCode1, StsCode2, StsCode3, StsCode4, StsCode5, StsCode6, StsCode7, StsCode8,
            STaxCode, STaxAmount, NumberCover, GuestID, OrgGuestID)
        SELECT
            FolioNum, TransactionCode, TransactionSubCode, GETDATE(), RoomCode, RefNumber,
            TransactionAmount * -1, 'V', MealPeriodNumber, @Comment,
            TaxAmount * -1, ServiceCharge * -1, SubAmount * -1, GETDATE(), OriginAmount * -1,
            OriginCurrency, TaxCode, OriginFolioNumber, OriginRoom, PostingClerkID, Quantity * -1,
            RptExRate, SrcCode, BillAmount * -1, BillExRate,
            StsCode1, StsCode2, StsCode3, StsCode4, StsCode5, StsCode6, StsCode7, StsCode8,
            STaxCode, STaxAmount * -1, NumberCover * -1, GuestID, OrgGuestID
        FROM {0}.dbo.FolioTransaction WHERE TransactionID IN ({1});
        """;

    internal const string PostSql = """
        EXEC {0}.dbo.PostInterfaceTransactionToFolio
            @FolioNum = @FolioNum, @FolioBalanceCode = 'A',
            @TransactionCode = @TrnCode, @TransactionSubCode = @TrnSub,
            @RefNumber = @Ref, @TransactionAmount = @Amount,
            @Count = 0, @MealPeriodNumber = @MealPeriod, @Comment = @Comment,
            @ClerkID = @Clerk, @TransactionTime = @At, @StsCode8 = @Sts8;
        """;

    [GeneratedRegex("^[A-Za-z0-9_]+$")]
    private static partial Regex ValidName();

    public async Task<string?> ResolveDatabaseAsync(int rvc, CancellationToken ct)
    {
        await using var cmd = Command(InterfaceDbSql);
        cmd.Parameters.Add("@Rvc", SqlDbType.Int).Value = rvc;
        var configured = await cmd.ExecuteScalarAsync(ct) as string;
        var name = string.IsNullOrWhiteSpace(configured) ? DefaultDatabase : configured.Trim();
        if (!ValidName().IsMatch(name)) return null;
        await using var access = Command(DatabaseAccessSql);
        access.Parameters.Add("@Name", SqlDbType.NVarChar, 128).Value = name;
        return await access.ExecuteScalarAsync(ct) is int usable && usable == 1 ? name : null;
    }

    public async Task<int?> LinkCodeAsync(int rvc, int trnCode, int trnSub, CancellationToken ct)
    {
        await using var cmd = Command(LinkSql);
        cmd.Parameters.Add("@Rvc", SqlDbType.Int).Value = rvc;
        cmd.Parameters.Add("@TrnCode", SqlDbType.Int).Value = trnCode;
        cmd.Parameters.Add("@TrnSub", SqlDbType.Int).Value = trnSub;
        var v = await cmd.ExecuteScalarAsync(ct);
        return v is null or DBNull ? null : Convert.ToInt32(v, CultureInfo.InvariantCulture);
    }

    public async Task<IReadOnlyList<FoFolio>> InHouseFoliosAsync(string database, string room, long folioNum, CancellationToken ct)
    {
        await using var cmd = Command(Fo(folioNum > 0 ? FolioByNumberSql : FoliosByRoomSql, database));
        if (folioNum > 0) cmd.Parameters.Add("@FolioNum", SqlDbType.BigInt).Value = folioNum;
        else cmd.Parameters.Add("@Room", SqlDbType.VarChar, 10).Value = room;
        var folios = new List<FoFolio>();
        await using var r = await cmd.ExecuteReaderAsync(ct);
        while (await r.ReadAsync(ct))
            folios.Add(new FoFolio(Convert.ToInt64(r.GetValue(0), CultureInfo.InvariantCulture), Convert.ToString(r.GetValue(1), CultureInfo.InvariantCulture)?.Trim() ?? ""));
        return folios;
    }

    public async Task<IReadOnlyList<FoActiveRow>> LockActiveAsync(string database, long folioNum, string refNumber, CancellationToken ct)
    {
        await using var cmd = Command(Fo(ActiveSql, database));
        cmd.Parameters.Add("@FolioNum", SqlDbType.BigInt).Value = folioNum;
        cmd.Parameters.Add("@Ref", SqlDbType.VarChar, 30).Value = refNumber;
        var rows = new List<FoActiveRow>();
        await using var r = await cmd.ExecuteReaderAsync(ct);
        while (await r.ReadAsync(ct))
            rows.Add(new FoActiveRow(Convert.ToInt64(r.GetValue(0), CultureInfo.InvariantCulture), Convert.ToDecimal(r.GetValue(1), CultureInfo.InvariantCulture)));
        return rows;
    }

    public async Task ReverseAsync(string database, IReadOnlyList<long> transactionIds, string comment, CancellationToken ct)
    {
        if (transactionIds.Count == 0) return;
        var names = transactionIds.Select((_, i) => "@Id" + i.ToString(CultureInfo.InvariantCulture)).ToList();
        await using var cmd = Command(Fo(ReverseSql, database).Replace("{1}", string.Join(", ", names), StringComparison.Ordinal));
        for (var i = 0; i < names.Count; i++) cmd.Parameters.Add(names[i], SqlDbType.BigInt).Value = transactionIds[i];
        cmd.Parameters.Add("@Comment", SqlDbType.NVarChar, 200).Value = comment;
        await cmd.ExecuteNonQueryAsync(ct);
    }

    public async Task PostAsync(string database, FoPostLine line, CancellationToken ct)
    {
        await using var cmd = Command(Fo(PostSql, database));
        cmd.Parameters.Add("@FolioNum", SqlDbType.VarChar, 10).Value = line.FolioNum.ToString(CultureInfo.InvariantCulture);
        cmd.Parameters.Add("@TrnCode", SqlDbType.VarChar, 10).Value = line.TransactionCode.ToString(CultureInfo.InvariantCulture);
        cmd.Parameters.Add("@TrnSub", SqlDbType.VarChar, 10).Value = line.TransactionSubCode.ToString(CultureInfo.InvariantCulture);
        cmd.Parameters.Add("@Ref", SqlDbType.VarChar, 30).Value = line.RefNumber;
        cmd.Parameters.Add("@Amount", SqlDbType.Decimal).Value = line.Amount;
        cmd.Parameters.Add("@MealPeriod", SqlDbType.Int).Value = line.MealPeriod;
        cmd.Parameters.Add("@Comment", SqlDbType.NVarChar, 200).Value = line.Comment;
        cmd.Parameters.Add("@Clerk", SqlDbType.VarChar, 10).Value = line.ClerkId;
        cmd.Parameters.Add("@At", SqlDbType.DateTime).Value = line.TransactionTime;
        cmd.Parameters.Add("@Sts8", SqlDbType.VarChar, 20).Value = line.StsCode8;
        await cmd.ExecuteNonQueryAsync(ct);
    }

    private static string Fo(string sql, string database) => sql.Replace("{0}", "[" + database + "]", StringComparison.Ordinal);

    private SqlCommand Command(string sql) => new(sql, connection, transaction) { CommandTimeout = TimeoutSeconds };
}
