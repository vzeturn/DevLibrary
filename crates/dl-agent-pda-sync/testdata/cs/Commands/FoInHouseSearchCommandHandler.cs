using System.Data;
using System.Globalization;
using Microsoft.Data.SqlClient;
using Microsoft.Extensions.Logging;
using MM.PdaOrder.Sync.Contracts.Wire;
using MM.PdaOrder.Sync.Mapping.Text;

namespace MM.PdaOrder.SyncAgent.Commands;

/// <summary>
/// What <see cref="IFoInHouseSource"/> read: the in-house folios found, or the <see cref="TxnReasons"/> value that refused the
/// read (<see cref="Refusal"/>), in which case nothing was read.
/// </summary>
public sealed record FoInHouseRead(string? Refusal, IReadOnlyList<FoInHouseGuest> Guests)
{
    public static FoInHouseRead Refused(string reason) => new(reason, []);
}

/// <summary>Read seam of <see cref="SiteCommandTypes.FoInHouseSearch"/>: in-house folios of one RVC's Front Office database.</summary>
public interface IFoInHouseSource
{
    /// <summary>Up to <paramref name="limit"/> in-house folios whose room, folio number or name matches <paramref name="query"/>, by room.</summary>
    Task<FoInHouseRead> SearchAsync(int rvc, string query, int limit, CancellationToken ct);
}

/// <summary>
/// Runs <see cref="SiteCommandTypes.FoInHouseSearch"/> (RT-1296, ADR 0172 §7): a SELECT-only read of <c>Folio</c> in the Front
/// Office database. Like <see cref="PosBillListCommandHandler"/> it opens no write transaction and writes no
/// <c>Pda_SyncCommandLog</c> row (a redelivery reads again). A Front Office database that is not usable is a rejection with
/// <see cref="TxnReasons.FolioDatabaseInvalid"/>; a cancelled read propagates, other SQL failures propagate to the channel's
/// failure count. The log carries the RVC and the row count only: never the query, a room, a folio number or a name.
/// </summary>
public sealed class FoInHouseSearchCommandHandler(IFoInHouseSource source, TimeProvider clock, ILogger<FoInHouseSearchCommandHandler> log)
{
    public async Task<SyncCommandResult> ExecuteAsync(SyncCommand command, CancellationToken ct)
    {
        var p = SiteCommandJson.FromElement(command.Payload, SyncJsonContext.Default.FoInHouseSearchPayload);
        var problem = p is null ? "payload is not a fo.inhouse_search object" : p.Problem();
        if (p is null || problem is not null)
            return Rejected(command, SiteCommandExecutor.Reasons.InvalidPayload + ": " + (problem ?? "invalid"));

        var read = await source.SearchAsync(p.Rvc, p.Query.Trim(), p.Limit, ct);
        if (read.Refusal is { } refusal)
        {
            log.LogWarning("fo.inhouse_search {Id}: rvc {Rvc} refused: {Reason}", command.CommandId, p.Rvc, refusal);
            return Rejected(command, refusal);
        }
        log.LogInformation("fo.inhouse_search {Id}: rvc {Rvc}: {Count} in-house folio(s)", command.CommandId, p.Rvc, read.Guests.Count);
        var result = new FoInHouseSearchResult(p.Rvc, read.Guests);
        return new(command.CommandId, CommandStatus.Applied, null, SiteCommandJson.ToElement(result), clock.GetUtcNow());
    }

    private SyncCommandResult Rejected(SyncCommand command, string reason) =>
        new(command.CommandId, CommandStatus.Rejected, reason, null, clock.GetUtcNow());
}

/// <summary>
/// <see cref="IFoInHouseSource"/> on the Front Office database as <c>pda_sync</c> (SELECT on <c>Folio</c>, granted by script 160 for
/// room charge). The database is the RVC's <c>dbo.Para InterfaceDB</c>, resolved and access-checked exactly as room charge does
/// (<see cref="SqlTxnRoomChargeUnit.ResolveDatabaseAsync"/>); it is the only interpolated text, bracketed, and every value is a
/// parameter. The connection never begins a transaction and nothing is written. Names are legacy varchar, read as bytes and
/// TCVN3-decoded like every other SMILE text column.
/// </summary>
public sealed class SqlFoInHouseSource(Func<string> connectionString) : IFoInHouseSource
{
    private const int TimeoutSeconds = 5;

    /// <summary>A hotel's in-house list is small; a name search reads at most this many folios and filters them here.</summary>
    internal const int InHouseCap = 500;

    // FolioStatus 2 = in house (the same filter as the room-charge lookup). A room or folio query (digits) matches the trimmed
    // room or the folio number exactly in SQL; a name query cannot match in SQL (names are TCVN3 bytes, the user types Unicode),
    // so it reads the in-house folios (TOP InHouseCap) and FoInHouseMatch filters and ranks the decoded names.
    internal const string Columns = """
        FolioNum, RTRIM(RoomCode) AS RoomCode,
               CAST(LTRIM(RTRIM(COALESCE(FirstName, '') + ' ' + COALESCE(LastName, ''))) AS varbinary(400)) AS GuestName,
               ArrivalDate, DepartureDate
        """;

    internal const string Sql = $$"""
        SELECT TOP (@Limit) {{Columns}}
          FROM {0}.dbo.Folio
         WHERE FolioStatus = 2
           AND (RTRIM(RoomCode) = @Q OR CAST(FolioNum AS varchar(20)) = @Q)
         ORDER BY RoomCode, FolioNum;
        """;

    internal const string SqlInHouse = $$"""
        SELECT TOP (@Limit) {{Columns}}
          FROM {0}.dbo.Folio
         WHERE FolioStatus = 2
         ORDER BY RoomCode, FolioNum;
        """;

    public async Task<FoInHouseRead> SearchAsync(int rvc, string query, int limit, CancellationToken ct)
    {
        await using var c = new SqlConnection(connectionString());
        await c.OpenAsync(ct);
        var fo = await new SqlTxnRoomChargeUnit(c, null).ResolveDatabaseAsync(rvc, ct);
        if (fo is null) return FoInHouseRead.Refused(TxnReasons.FolioDatabaseInvalid);
        var byNumber = FoInHouseMatch.IsNumber(query);
        var sql = (byNumber ? Sql : SqlInHouse).Replace("{0}", "[" + fo + "]", StringComparison.Ordinal);
        await using var cmd = new SqlCommand(sql, c) { CommandTimeout = TimeoutSeconds };
        cmd.Parameters.Add("@Limit", SqlDbType.Int).Value = byNumber ? limit : InHouseCap;
        if (byNumber) cmd.Parameters.Add("@Q", SqlDbType.VarChar, FoInHouseSearchPayload.MaxQuery).Value = query;
        var guests = new List<FoInHouseGuest>();
        await using var r = await cmd.ExecuteReaderAsync(ct);
        while (await r.ReadAsync(ct))
        {
            guests.Add(new FoInHouseGuest(
                r.GetValue(1) is string room ? room : "",
                Convert.ToInt64(r.GetValue(0), CultureInfo.InvariantCulture),
                r.GetValue(2) is byte[] name ? Tcvn3Decoder.Decode(name) ?? "" : "",
                Day(r.GetValue(3)),
                Day(r.GetValue(4))));
        }
        return new FoInHouseRead(null, byNumber ? guests : FoInHouseMatch.Filter(guests, query, limit));
    }

    private static string? Day(object value) =>
        value is DateTime d ? d.ToString("yyyy-MM-dd", CultureInfo.InvariantCulture) : null;
}
