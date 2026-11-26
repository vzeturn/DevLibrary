namespace MM.PdaOrder.Sync.Contracts.Wire;

/// <summary>
/// Payload of <see cref="SiteCommandTypes.FoInHouseSearch"/> (RT-1296, ADR 0172 §7): find the guests in house in the Front Office
/// database of RVC <paramref name="Rvc"/> whose room, folio number or name matches <paramref name="Query"/>; at most
/// <paramref name="Limit"/> rows. Read on demand only: the cloud never keeps the answer (the result is scrubbed once read).
/// </summary>
public sealed record FoInHouseSearchPayload(int Rvc, string Query, int Limit)
{
    public const int MaxQuery = 40;
    public const int MaxLimit = 20;

    /// <summary>One aggregate per search (<paramref name="searchId"/>): a slow read never holds the next search back.</summary>
    public static string AggregateKey(int rvc, Guid searchId) =>
        string.Create(System.Globalization.CultureInfo.InvariantCulture, $"fo-inhouse:{rvc}:{searchId:N}");

    public string? Problem()
    {
        if (Rvc <= 0) return "rvc must be positive";
        var q = Query?.Trim() ?? "";
        if (q.Length is < 1 or > MaxQuery) return "query must be 1..40 characters";
        return Limit is < 1 or > MaxLimit ? "limit must be 1..20" : null;
    }
}

/// <summary>
/// One in-house folio of <see cref="FoInHouseSearchResult"/>: room, folio number, the display name <c>FirstName LastName</c> and the
/// stay dates (<c>yyyy-MM-dd</c>, null when the folio has none). Nothing else of the guest is read.
/// </summary>
public sealed record FoInHouseGuest(string RoomNo, long FolioNum, string GuestName, string? ArrivalDate, string? DepartureDate);

/// <summary>Result of <see cref="SiteCommandTypes.FoInHouseSearch"/>: the matching in-house folios, by room.</summary>
public sealed record FoInHouseSearchResult(int Rvc, IReadOnlyList<FoInHouseGuest> Guests);
