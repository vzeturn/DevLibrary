//! Text read from the store database (SMILE `char`/`nchar`/`varchar` columns), made safe
//! for JSON and PostgreSQL before it goes into a command result or a capture row
//! (RT-1309).
//!
//! SMILE pads `CHAR(n)` values with spaces, and some POS builds leave NUL (U+0000)
//! characters in names (`'C\0\0\0…'`). PostgreSQL `text` and `jsonb` refuse U+0000
//! (SQLSTATE 22P05): an `order.item_post` result that carried one was dropped by the cloud
//! and the command ended `lease_expired` although the agent had applied it. Every text the
//! agent reports from the store goes through [`clean`]; it only shapes what the agent
//! reports and is never applied to what the agent writes to the store.

/// `value` without any NUL character and without its trailing U+0020 padding. The store
/// ignores trailing spaces when it compares text, so `'100001'` and `'100001      '` are
/// one code there (PDAOrder ADR 0123) and the trimmed code still matches. Leading and inner
/// spaces, tabs and newlines stay; an all-space or all-NUL value becomes `""`, never NULL.
pub fn clean(mut value: String) -> String {
    if value.contains('\0') {
        value.retain(|c| c != '\0');
    }
    let len = value.trim_end_matches(' ').len();
    value.truncate(len);
    value
}

#[cfg(test)]
mod tests {
    use super::clean;

    #[test]
    fn nul_characters_inside_a_name_are_removed() {
        assert_eq!(clean("C\0\0\0\0\0".into()), "C");
        assert_eq!(clean("Cà\0 phê\0 sữa".into()), "Cà phê sữa");
        assert_eq!(clean("\0\0".into()), "");
    }

    #[test]
    fn a_padded_char_code_loses_only_its_trailing_spaces() {
        assert_eq!(clean("100001      ".into()), "100001");
        assert_eq!(clean("A01   \0\0  ".into()), "A01");
        assert_eq!(clean("  01 A\t".into()), "  01 A\t");
        assert_eq!(clean("     ".into()), "");
        assert_eq!(clean("100001".into()), "100001");
    }
}
