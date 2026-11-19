//! What the voice says (port of `BuildPhrase` and `NormalizeLang` in the Go
//! `internal/tts/tts.go`).

/// Vietnamese.
pub const LANG_VI: &str = "vi";
/// English.
pub const LANG_EN: &str = "en";

/// `NormalizeLang`: `en`, `en-us`, `en-gb` (any case, trimmed) speak English; anything
/// else (`vi`, `auto`, empty, unknown) Vietnamese. Only these two values reach the
/// speech engine.
pub fn normalize_lang(lang: &str) -> &'static str {
    match lang.trim().to_lowercase().as_str() {
        "en" | "en-us" | "en-gb" => LANG_EN,
        _ => LANG_VI,
    }
}

/// The SAPI `Language` attributes (hex LCIDs) tried in order to pick a voice. The Go
/// agent took the first installed voice of any culture of the language
/// (`TwoLetterISOLanguageName`), so English tries US and UK first, then every other
/// English LCID Windows ships voices for; Vietnamese is `42A`.
pub fn voice_lcids(lang: &str) -> &'static [&'static str] {
    if normalize_lang(lang) == LANG_EN {
        &[
            "409", "809", "C09", "1009", "1409", "1809", "1C09", "2009", "2409", "2809", "2C09",
            "3009", "3409", "4009", "4409", "4809",
        ]
    } else {
        &["42A"]
    }
}

fn phrase_format(lang: &str, kind: &str) -> Option<&'static str> {
    Some(match (lang, kind) {
        (LANG_EN, "service_bill") => "Table %s requests the bill",
        (LANG_EN, "service_charge") => "Table %s charges to the room",
        (LANG_EN, "service_waiter") => "Table %s is calling a waiter",
        (LANG_EN, "order") => "Table %s placed an order",
        (_, "service_bill") => "Bàn %s yêu cầu thanh toán",
        (_, "service_charge") => "Bàn %s tính vào phòng",
        (_, "service_waiter") => "Bàn %s gọi phục vụ",
        (_, "order") => "Bàn %s đặt món",
        _ => return None,
    })
}

/// `BuildPhrase`: an empty table reads as "—"; an unknown kind uses the order phrase.
/// The table text is inserted once and never interpreted.
pub fn build_phrase(kind: &str, table: &str, lang: &str) -> String {
    let t = if table.is_empty() { "—" } else { table };
    let lang = normalize_lang(lang);
    let format = phrase_format(lang, kind)
        .or_else(|| phrase_format(lang, "order"))
        .unwrap_or("%s");
    format.replacen("%s", t, 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn voices_of_every_english_culture_are_tried_us_and_uk_first() {
        let en = voice_lcids("en");
        assert_eq!(en[..2], ["409", "809"]);
        for lcid in en {
            let v = u32::from_str_radix(lcid, 16).unwrap();
            assert_eq!(v & 0x3ff, 0x09, "{lcid} is not English");
        }
        assert!(en.contains(&"C09") && en.contains(&"4009"));
        assert_eq!(voice_lcids("vi"), ["42A"]);
        assert_eq!(voice_lcids(""), ["42A"]);
    }

    #[test]
    fn phrases_match_go() {
        for (kind, table, lang, want) in [
            ("order", "12", "vi", "Bàn 12 đặt món"),
            ("service_waiter", "7", "vi", "Bàn 7 gọi phục vụ"),
            ("service_bill", "3", "vi", "Bàn 3 yêu cầu thanh toán"),
            ("service_charge", "5", "vi", "Bàn 5 tính vào phòng"),
            ("order", "", "vi", "Bàn — đặt món"),
            ("unknown", "9", "vi", "Bàn 9 đặt món"),
            ("order", "12", "en", "Table 12 placed an order"),
            ("service_waiter", "7", "en", "Table 7 is calling a waiter"),
            ("service_bill", "3", "en", "Table 3 requests the bill"),
            ("service_charge", "5", "en", "Table 5 charges to the room"),
            ("unknown", "9", "en", "Table 9 placed an order"),
            ("order", "12", "", "Bàn 12 đặt món"),
            ("order", "12", "auto", "Bàn 12 đặt món"),
            ("order", "12", "ko", "Bàn 12 đặt món"),
            ("order", "%s", "en", "Table %s placed an order"),
        ] {
            assert_eq!(
                build_phrase(kind, table, lang),
                want,
                "{kind} {table} {lang}"
            );
        }
    }

    #[test]
    fn languages() {
        for (input, want) in [
            ("en", LANG_EN),
            ("EN", LANG_EN),
            (" en-US ", LANG_EN),
            ("en-gb", LANG_EN),
            ("vi", LANG_VI),
            ("", LANG_VI),
            ("auto", LANG_VI),
            ("zh", LANG_VI),
        ] {
            assert_eq!(normalize_lang(input), want, "{input:?}");
        }
    }
}
