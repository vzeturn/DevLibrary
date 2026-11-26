using System.Globalization;
using System.Text;
using MM.PdaOrder.Sync.Contracts.Wire;

namespace MM.PdaOrder.SyncAgent.Commands;

/// <summary>
/// Name search of <see cref="SiteCommandTypes.FoInHouseSearch"/> on decoded names (RT-1296). Front Office stores names as TCVN3
/// bytes, so SQL cannot compare them with the Unicode text staff type; the agent decodes the in-house folios and matches here.
/// Both sides are folded (accents and đ dropped, lower case, spaces collapsed), so "nguyen van an", "Nguyễn Văn An" and "NGUYEN"
/// all find "Nguyễn Văn An". Ranking: an exact room or folio number first, then a name or word that starts with the text, then a
/// name that contains it; ties keep the room order of the read.
/// </summary>
public static class FoInHouseMatch
{
    /// <summary>A room or folio number query (digits only): matched exactly in SQL, no name read needed.</summary>
    public static bool IsNumber(string query) => query.Length > 0 && query.All(char.IsAsciiDigit);

    public static IReadOnlyList<FoInHouseGuest> Filter(IReadOnlyList<FoInHouseGuest> guests, string query, int limit)
    {
        var q = Fold(query);
        if (q.Length == 0) return [];
        return guests
            .Select((g, i) => (Guest: g, Rank: Rank(g, query.Trim(), q), Order: i))
            .Where(x => x.Rank >= 0)
            .OrderBy(x => x.Rank).ThenBy(x => x.Order)
            .Take(limit)
            .Select(x => x.Guest)
            .ToList();
    }

    private static int Rank(FoInHouseGuest g, string raw, string folded)
    {
        if (string.Equals(g.RoomNo.Trim(), raw, StringComparison.OrdinalIgnoreCase)
            || g.FolioNum.ToString(CultureInfo.InvariantCulture) == raw) return 0;
        var name = Fold(g.GuestName);
        if (name.StartsWith(folded, StringComparison.Ordinal) || name.Contains(" " + folded, StringComparison.Ordinal)) return 1;
        return name.Contains(folded, StringComparison.Ordinal) ? 2 : -1;
    }

    /// <summary>Accent-free lower-case text with single spaces: the form both the query and the names are compared in.</summary>
    public static string Fold(string text)
    {
        var sb = new StringBuilder(text.Length);
        var space = false;
        foreach (var ch in text.Normalize(NormalizationForm.FormD))
        {
            if (CharUnicodeInfo.GetUnicodeCategory(ch) == UnicodeCategory.NonSpacingMark) continue;
            if (char.IsWhiteSpace(ch)) { space = sb.Length > 0; continue; }
            if (space) { sb.Append(' '); space = false; }
            sb.Append(ch is 'đ' or 'Đ' ? 'd' : char.ToLowerInvariant(ch));
        }
        return sb.ToString();
    }
}
