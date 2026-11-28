//! The value rules of the `Bool`, `Int`, `Tcvn3Text` and `Unicode` column kinds (RT-0831):
//! the same rules and problem texts as `MasterColumnValues.cs` of the .NET agent.

use std::collections::HashMap;
use std::sync::OnceLock;

use crate::contracts::mapping::tcvn3;

/// The SMILE_POS function a TCVN3 column is written through (`MasterColumn.Tcvn3Encode`);
/// an entity's constant update SQL writes the column with [`master_tcvn3_set!`](crate::master_tcvn3_set).
pub const TCVN3_ENCODE: &str = "dbo.Unicode2TCVN";

/// The longest text column: `dbo.Unicode2TCVN` takes `nvarchar(4000)` and returns `varchar(4000)`.
pub const MAX_TEXT_LENGTH: usize = 4000;

// Verbatim from dbo.Unicode2TCVN of SMILE_POS: the character with code @UNICODE[i] is stored as
// CHAR(@TCVN[i]); any other character is appended unconverted, so the database code page decides
// its byte. The first match wins (CHARINDEX).
const VENDOR_TCVN: &str = ",184 ,181 ,182 ,183 ,185 ,168 ,190 ,187 ,188 ,189 ,198 ,169 ,202 ,199 ,200 ,201 ,203 ,208 ,204 ,206 ,207 ,209 ,170 ,213 ,210 ,211 ,212 ,214 ,221 ,215 ,216 ,220 ,222 ,227 ,223 ,225 ,226 ,228 ,171 ,232 ,229 ,230 ,231 ,233 ,172 ,237 ,234 ,235 ,236 ,238 ,243 ,239 ,241 ,242 ,244 ,173 ,248 ,245 ,246 ,247 ,249 ,253 ,250 ,251 ,252 ,254 ,174 ,184 ,181 ,182 ,183 ,185 ,161 ,190 ,187 ,188 ,189 ,198 ,162 ,202 ,199 ,200 ,201 ,203 ,208 ,204 ,206 ,207 ,209 ,163 ,213 ,210 ,211 ,212 ,214 ,221 ,215 ,216 ,220 ,222 ,227 ,223 ,225 ,226 ,228 ,164 ,232 ,229 ,230 ,231 ,233 ,165 ,237 ,234 ,235 ,236 ,238 ,243 ,239 ,241 ,242 ,244 ,166 ,248 ,245 ,246 ,247 ,249 ,253 ,250 ,251 ,252 ,254 ,167 ,";
const VENDOR_UNICODE: &str = ",225 ,224 ,7843,227 ,7841,259 ,7855,7857,7859,7861,7863,226 ,7845,7847,7849,7851,7853,233 ,232 ,7867,7869,7865,234 ,7871,7873,7875,7877,7879,237 ,236 ,7881,297 ,7883,243 ,242 ,7887,245 ,7885,244 ,7889,7891,7893,7895,7897,417 ,7899,7901,7903,7905,7907,250 ,249 ,7911,361 ,7909,432 ,7913,7915,7917,7919,7921,253 ,7923,7927,7929,7925,273 ,193 ,192 ,7842,195 ,7840,258 ,7854,7856,7858,7860,7862,194 ,7844,7846,7848,7850,7852,201 ,200 ,7866,7868,7864,202 ,7870,7872,7874,7876,7878,205 ,204 ,7880,296 ,7882,211 ,210 ,7886,213 ,7884,212 ,7888,7890,7892,7894,7896,416 ,7898,7900,7902,7904,7906,218 ,217 ,7910,360 ,7908,431 ,7912,7914,7916,7918,7920,221 ,7922,7926,7928,7924,272 ,";

/// `master_tcvn3_set!("Col")` is the literal `"Col = dbo.Unicode2TCVN(@Col)"`, usable inside
/// `concat!` so an entity's update SQL stays a compile-time constant.
#[macro_export]
macro_rules! master_tcvn3_set {
    ($column:literal) => {
        concat!($column, " = dbo.Unicode2TCVN(@", $column, ")")
    };
}

/// Exactly `0`, `1`, `true` or `false`, lower case.
pub(super) fn bool_value(value: &str) -> Option<bool> {
    match value {
        "1" | "true" => Some(true),
        "0" | "false" => Some(false),
        _ => None,
    }
}

/// Strict integer text: an optional leading `-` and digits only (no blanks, `+` or
/// exponent), from `min` to `max`.
pub(super) fn int_value(value: &str, min: i32, max: i32) -> Option<i32> {
    let digits = value.strip_prefix('-').unwrap_or(value);
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let n = value.parse::<i32>().ok()?;
    (min..=max).contains(&n).then_some(n)
}

/// The bytes `dbo.Unicode2TCVN` stores for `value`, or `None` when a character is neither
/// printable ASCII nor in the function's map (the database code page would decide its byte).
/// Trailing blanks are kept here; the function drops them.
pub fn encode_tcvn3(value: &str) -> Option<Vec<u8>> {
    value
        .chars()
        .map(|ch| {
            if (' '..='~').contains(&ch) {
                u8::try_from(ch).ok()
            } else {
                tcvn3_bytes().get(&ch).copied()
            }
        })
        .collect()
}

/// Why a text value may not be written; `None` when it may.
pub(super) fn text_problem(
    name: &str,
    max_length: usize,
    tcvn3: bool,
    value: &str,
) -> Option<String> {
    if value.chars().any(char::is_control) {
        return Some(format!(
            "{name} must be valid Unicode text without control characters"
        ));
    }
    if tcvn3 {
        // dbo.Unicode2TCVN loops to LEN(@strInput), which drops trailing blanks, and returns
        // NULL for '' or an all-blank value.
        if value.is_empty() || value.ends_with(' ') {
            return Some(format!("{name} must not be empty or end with a blank"));
        }
        // Only what reads back unchanged: an upper-case toned letter is stored as its
        // lower-case byte, a character outside the map gets a code-page byte, and the decoder
        // folds an upper-case vowel followed by its own toned form into one letter.
        if !encode_tcvn3(value).is_some_and(|bytes| tcvn3::decode(&bytes) == value) {
            return Some(format!(
                "{name} must be ASCII or Vietnamese text that TCVN3 stores unchanged (no capital letter with a tone mark)"
            ));
        }
    }
    (value.encode_utf16().count() > max_length)
        .then(|| format!("{name} must be at most {max_length} characters"))
}

/// Why `update_sql` does not write the column the way its kind needs; `None` when it does. A
/// TCVN3 column is written only as `dbo.Unicode2TCVN(@Col)`; any other column never goes
/// through that function.
pub(super) fn sql_problem(name: &str, tcvn3: bool, update_sql: &str) -> Option<String> {
    let parameter = format!("@{name}");
    let wrapped = count(update_sql, &format!("{TCVN3_ENCODE}({parameter})"));
    let all = count(update_sql, &parameter);
    if tcvn3 {
        (wrapped == 0 || wrapped != all).then(|| {
            format!(
                "{name} is a TCVN3 column and must be written only as {TCVN3_ENCODE}({parameter})"
            )
        })
    } else {
        (wrapped != 0).then(|| {
            format!("{name} is not a TCVN3 column and must not be written through {TCVN3_ENCODE}")
        })
    }
}

/// Occurrences of `token` (ASCII case-insensitive) that end at an identifier boundary, so
/// `@Name` never counts inside `@NameX`.
fn count(sql: &str, token: &str) -> usize {
    let sql = sql.to_ascii_lowercase();
    let token = token.to_ascii_lowercase();
    let bytes = sql.as_bytes();
    sql.match_indices(&token)
        .filter(|(at, _)| {
            bytes.get(at + token.len()).is_none_or(|&b| {
                !(b.is_ascii_alphanumeric() || matches!(b, b'_' | b'@' | b'#' | b'$'))
            })
        })
        .count()
}

fn tcvn3_bytes() -> &'static HashMap<char, u8> {
    static MAP: OnceLock<HashMap<char, u8>> = OnceLock::new();
    MAP.get_or_init(|| {
        let mut map = HashMap::new();
        let entries = |list: &'static str| list.split(',').map(str::trim).filter(|s| !s.is_empty());
        for (code, byte) in entries(VENDOR_UNICODE).zip(entries(VENDOR_TCVN)) {
            let ch = code
                .parse::<u32>()
                .ok()
                .and_then(char::from_u32)
                .expect("dbo.Unicode2TCVN code list is constant");
            let byte = byte
                .parse::<u8>()
                .expect("dbo.Unicode2TCVN byte list is constant");
            map.entry(ch).or_insert(byte);
        }
        map
    })
}
