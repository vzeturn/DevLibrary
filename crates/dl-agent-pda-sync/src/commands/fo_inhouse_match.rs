//! Name search of `fo.inhouse_search` on decoded names (`FoInHouseMatch.cs`, RT-1296).
//! Front Office stores names as TCVN3 bytes, so SQL cannot compare them with the Unicode
//! text staff type; the agent decodes the in-house folios and matches here. Both sides are
//! folded (accents and đ dropped, lower case, spaces collapsed), so "nguyen van an",
//! "Nguyễn Văn An" and "NGUYEN" all find "Nguyễn Văn An". Ranking: an exact room or folio
//! number first, then a name or word that starts with the text, then a name that contains
//! it; ties keep the room order of the read.

use crate::contracts::wire::FoInHouseGuest;

/// A room or folio number query (ASCII digits only): matched exactly in SQL, no name read
/// needed.
pub fn is_number(query: &str) -> bool {
    !query.is_empty() && query.bytes().all(|b| b.is_ascii_digit())
}

/// The guests matching `query`, best first, at most `limit`.
pub fn filter(guests: Vec<FoInHouseGuest>, query: &str, limit: i32) -> Vec<FoInHouseGuest> {
    let folded = fold(query);
    if folded.is_empty() {
        return Vec::new();
    }
    let raw = query.trim();
    let mut ranked: Vec<(u8, FoInHouseGuest)> = guests
        .into_iter()
        .filter_map(|g| rank(&g, raw, &folded).map(|r| (r, g)))
        .collect();
    // A stable sort keeps the read's room order within one rank.
    ranked.sort_by_key(|(r, _)| *r);
    ranked
        .into_iter()
        .take(usize::try_from(limit).unwrap_or(0))
        .map(|(_, g)| g)
        .collect()
}

fn upper(c: char) -> char {
    let mut u = c.to_uppercase();
    match (u.next(), u.next()) {
        (Some(one), None) => one,
        _ => c,
    }
}

/// `string.Equals(a, b, StringComparison.OrdinalIgnoreCase)`.
fn equals_ignore_case(a: &str, b: &str) -> bool {
    a.chars().map(upper).eq(b.chars().map(upper))
}

fn rank(g: &FoInHouseGuest, raw: &str, folded: &str) -> Option<u8> {
    if equals_ignore_case(g.room_no.trim(), raw) || g.folio_num.to_string() == raw {
        return Some(0);
    }
    let name = fold(&g.guest_name);
    if name.starts_with(folded) || name.contains(&format!(" {folded}")) {
        return Some(1);
    }
    name.contains(folded).then_some(2)
}

/// Accent-free lower-case text with single spaces: the form both the query and the names are
/// compared in (Vietnamese diacritics through [`dl_vn::text::remove_diacritics`]).
pub fn fold(text: &str) -> String {
    let plain = dl_vn::text::remove_diacritics(text);
    let mut out = String::with_capacity(plain.len());
    let mut space = false;
    for c in plain.chars() {
        if c.is_whitespace() {
            space = !out.is_empty();
            continue;
        }
        if space {
            out.push(' ');
            space = false;
        }
        let mut lower = c.to_lowercase();
        match (lower.next(), lower.next()) {
            (Some(one), None) => out.push(one),
            _ => out.push(c),
        }
    }
    out
}
