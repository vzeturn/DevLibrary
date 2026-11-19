//! `Tcvn3.Encode` on top of `dl_vn::tcvn3`: Unicode to the TCVN3 (ABC) bytes SMILE keeps
//! in its `varchar` name columns, one `char` per byte (U+0000..U+00FF), sent as `varchar`.
//!
//! Two rules keep the bytes identical to SMILE's own `dbo.UnicodeToTCVN` (and so to the
//! .NET agent), where `dl_vn` alone differs:
//! - an upper-case toned vowel uses the lower-case code (`Ấ` and `ấ` are both 202); only
//!   `Ă Â Ê Ô Ơ Ư Đ` have upper-case codes (161..=167);
//! - a character outside the table is folded to ASCII (`ü` → `u`, `ß` → `ss`), else `?`,
//!   one per code point, never kept (a Latin-1 `ü` is byte 252, which is TCVN3 `ự`).

const OWN_UPPER: [char; 7] = ['Ă', 'Â', 'Ê', 'Ô', 'Ơ', 'Ư', 'Đ'];

/// `Tcvn3.Encode`.
pub fn encode(s: &str) -> String {
    let composed: String = compose(s);
    let mut out = String::with_capacity(composed.len());
    for c in composed.chars() {
        if c.is_ascii() {
            out.push(c);
            continue;
        }
        let key = if c.is_uppercase() && !OWN_UPPER.contains(&c) {
            c.to_lowercase().next().unwrap_or(c)
        } else {
            c
        };
        match dl_vn::tcvn3::encode(key.encode_utf8(&mut [0; 4])) {
            Ok(b) if b.len() == 1 && b[0] >= 161 => out.push(char::from(b[0])),
            _ => out.push_str(&ascii_fallback(c)),
        }
    }
    out
}

/// `Tcvn3.Encode` of an optional value.
pub fn encode_opt(s: Option<&str>) -> Option<String> {
    s.map(encode)
}

/// NFC for Vietnamese: `dl_vn`'s encoder composes base + combining marks itself, so only
/// sequences it cannot compose stay apart; decomposed input is composed by encoding it
/// through `dl_vn` and decoding it back.
fn compose(s: &str) -> String {
    if !s.chars().any(|c| ('\u{300}'..='\u{36F}').contains(&c)) {
        return s.to_owned();
    }
    // Compose run by run so one foreign character does not stop the rest.
    let mut out = String::with_capacity(s.len());
    let mut run = String::new();
    let flush = |run: &mut String, out: &mut String| {
        if run.is_empty() {
            return;
        }
        match dl_vn::tcvn3::encode(run) {
            Ok(b) => out.push_str(&dl_vn::tcvn3::decode_bytes(&b)),
            Err(_) => out.push_str(run),
        }
        run.clear();
    };
    for c in s.chars() {
        let mark = ('\u{300}'..='\u{36F}').contains(&c);
        if c.is_whitespace() && !mark {
            flush(&mut run, &mut out);
            out.push(c);
        } else {
            run.push(c);
        }
    }
    flush(&mut run, &mut out);
    out
}

fn ascii_fallback(c: char) -> String {
    let folded = match c {
        'ß' => "ss",
        'æ' => "ae",
        'Æ' => "AE",
        'ø' => "o",
        'Ø' => "O",
        'œ' => "oe",
        'Œ' => "OE",
        'ł' => "l",
        'Ł' => "L",
        'À'..='Å' | 'Ā' | 'Ă' | 'Ą' => "A",
        'à'..='å' | 'ā' | 'ă' | 'ą' => "a",
        'Ç' | 'Ć' | 'Ĉ' | 'Ċ' | 'Č' => "C",
        'ç' | 'ć' | 'ĉ' | 'ċ' | 'č' => "c",
        'Ď' => "D",
        'ď' => "d",
        'È'..='Ë' | 'Ē' | 'Ĕ' | 'Ė' | 'Ę' | 'Ě' => "E",
        'è'..='ë' | 'ē' | 'ĕ' | 'ė' | 'ę' | 'ě' => "e",
        'Ĝ' | 'Ğ' | 'Ġ' | 'Ģ' => "G",
        'ĝ' | 'ğ' | 'ġ' | 'ģ' => "g",
        'Ĥ' => "H",
        'ĥ' => "h",
        'Ì'..='Ï' | 'Ĩ' | 'Ī' | 'Ĭ' | 'Į' | 'İ' => "I",
        'ì'..='ï' | 'ĩ' | 'ī' | 'ĭ' | 'į' => "i",
        'Ĵ' => "J",
        'ĵ' => "j",
        'Ķ' => "K",
        'ķ' => "k",
        'Ĺ' | 'Ļ' | 'Ľ' => "L",
        'ĺ' | 'ļ' | 'ľ' => "l",
        'Ñ' | 'Ń' | 'Ņ' | 'Ň' => "N",
        'ñ' | 'ń' | 'ņ' | 'ň' => "n",
        'Ò'..='Ö' | 'Ō' | 'Ŏ' | 'Ő' => "O",
        'ò'..='ö' | 'ō' | 'ŏ' | 'ő' => "o",
        'Ŕ' | 'Ŗ' | 'Ř' => "R",
        'ŕ' | 'ŗ' | 'ř' => "r",
        'Ś' | 'Ŝ' | 'Ş' | 'Š' => "S",
        'ś' | 'ŝ' | 'ş' | 'š' => "s",
        'Ţ' | 'Ť' => "T",
        'ţ' | 'ť' => "t",
        'Ù'..='Ü' | 'Ũ' | 'Ū' | 'Ŭ' | 'Ů' | 'Ű' | 'Ų' => "U",
        'ù'..='ü' | 'ũ' | 'ū' | 'ŭ' | 'ů' | 'ű' | 'ų' => "u",
        'Ŵ' => "W",
        'ŵ' => "w",
        'Ý' | 'Ŷ' | 'Ÿ' => "Y",
        'ý' | 'ÿ' | 'ŷ' => "y",
        'Ź' | 'Ż' | 'Ž' => "Z",
        'ź' | 'ż' | 'ž' => "z",
        _ => "?",
    };
    folded.to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bytes(s: &str) -> Vec<u32> {
        encode(s).chars().map(u32::from).collect()
    }

    #[test]
    fn vietnamese_letters_use_the_smile_table() {
        assert_eq!(bytes("ấ"), vec![202]);
        assert_eq!(bytes("Ấ"), vec![202]);
        assert_eq!(bytes("Á"), vec![184]);
        assert_eq!(bytes("Đ"), vec![167]);
        assert_eq!(bytes("đ"), vec![174]);
        assert_eq!(encode("Trần"), format!("Tr{}n", char::from(199)));
    }

    #[test]
    fn decomposed_input_is_composed_first() {
        assert_eq!(encode("Tra\u{302}\u{300}n"), encode("Trần"));
    }

    #[test]
    fn foreign_letters_fold_to_ascii_or_a_question_mark() {
        assert_eq!(encode("Müller"), "Muller");
        assert_eq!(encode("Straße"), "Strasse");
        assert_eq!(encode("李😀"), "??");
        assert_eq!(encode("a\u{A0}b"), "a?b");
    }
}
