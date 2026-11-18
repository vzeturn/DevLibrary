//! `ConfigurationBinder.Bind` for `AgentOptions`: the TypeConverter rules for `string`,
//! `bool`, `int` and `TimeSpan`, and the exceptions a bad value raises at host start.

use super::dotnet::{DotnetError, trim};
use super::timespan::TimeSpanParseError;
use super::{AgentOptions, Configuration, LockOptions, SECTION, TimeSpan};

/// Binds `Agent:*` onto the C# defaults. Absent keys and `null` values keep the default;
/// unknown keys are ignored; the first value that does not convert fails, in property
/// declaration order, like the .NET binder.
pub fn bind_agent_options(c: &Configuration) -> Result<AgentOptions, DotnetError> {
    let mut o = AgentOptions::default();
    let b = Binder {
        c,
        section: SECTION.to_owned(),
    };
    b.string("ApiBaseUrl", &mut o.api_base_url);
    b.string("AgentId", &mut o.agent_id);
    b.string("TokenFile", &mut o.token_file);
    b.time_span("PollInterval", &mut o.poll_interval)?;
    b.time_span("CommandTimeout", &mut o.command_timeout)?;
    b.boolean("RunOnce", &mut o.run_once)?;
    b.string("Adapter", &mut o.adapter);
    bind_lock_options(
        &Binder {
            c,
            section: format!("{SECTION}:Lock"),
        },
        &mut o.lock,
    )?;
    b.string("DataDirectory", &mut o.data_directory);
    Ok(o)
}

fn bind_lock_options(b: &Binder<'_>, l: &mut LockOptions) -> Result<(), DotnetError> {
    b.string("LockKeyType", &mut l.lock_key_type);
    b.string("MfLockName", &mut l.mf_lock_name);
    b.string("MfLockServer", &mut l.mf_lock_server);
    b.int32("MfLockComPort", &mut l.mf_lock_com_port)?;
    b.int32("MfLockComBaud", &mut l.mf_lock_com_baud)?;
    b.string("MfLockMore", &mut l.mf_lock_more);
    b.int32("MfLockDoorType", &mut l.mf_lock_door_type)?;
    b.int32("HuneComPort", &mut l.hune_com_port)?;
    b.string("HuneCardCode", &mut l.hune_card_code);
    b.string("HuneSystemCode", &mut l.hune_system_code);
    b.string("HuneHotelCode", &mut l.hune_hotel_code);
    b.int32("OrbitaVersion", &mut l.orbita_version)?;
    b.string("TtLockClientId", &mut l.tt_lock_client_id);
    b.string("TtLockClientSecret", &mut l.tt_lock_client_secret);
    b.int32("TtLockCardType", &mut l.tt_lock_card_type)?;
    Ok(())
}

struct Binder<'a> {
    c: &'a Configuration,
    section: String,
}

impl Binder<'_> {
    fn value(&self, name: &str) -> Option<(&str, String)> {
        let path = format!("{}:{name}", self.section);
        self.c.get(&path).map(|v| (v, path))
    }

    fn string(&self, name: &str, slot: &mut String) {
        if let Some((v, _)) = self.value(name) {
            v.clone_into(slot);
        }
    }

    fn boolean(&self, name: &str, slot: &mut bool) -> Result<(), DotnetError> {
        if let Some((v, path)) = self.value(name) {
            *slot = convert_bool(v).map_err(|e| failed(v, &path, "System.Boolean", e))?;
        }
        Ok(())
    }

    fn int32(&self, name: &str, slot: &mut i32) -> Result<(), DotnetError> {
        if let Some((v, path)) = self.value(name) {
            *slot = convert_int32(v).map_err(|e| failed(v, &path, "System.Int32", e))?;
        }
        Ok(())
    }

    fn time_span(&self, name: &str, slot: &mut TimeSpan) -> Result<(), DotnetError> {
        if let Some((v, path)) = self.value(name) {
            *slot = convert_time_span(v).map_err(|e| failed(v, &path, "System.TimeSpan", e))?;
        }
        Ok(())
    }
}

/// `SR.Error_FailedBinding` around the converter's exception.
fn failed(value: &str, path: &str, type_name: &str, inner: DotnetError) -> DotnetError {
    DotnetError::new(
        "System.InvalidOperationException",
        format!(
            "Failed to convert configuration value '{value}' at '{path}' to type '{type_name}'."
        ),
    )
    .with_inner(inner)
}

/// `BooleanConverter`: `bool.Parse` of the trimmed text (`true`/`false`, any casing).
fn convert_bool(value: &str) -> Result<bool, DotnetError> {
    let text = trim(value).trim_matches('\0');
    if text.eq_ignore_ascii_case("true") {
        Ok(true)
    } else if text.eq_ignore_ascii_case("false") {
        Ok(false)
    } else {
        Err(DotnetError::new(
            "System.FormatException",
            format!("{value} is not a valid value for Boolean."),
        )
        .with_inner(DotnetError::new(
            "System.FormatException",
            format!("String '{text}' was not recognized as a valid Boolean."),
        )))
    }
}

/// `Int32Converter`: trimmed text; `#hex`, `0xhex` or `&hhex` (two's complement), else
/// `int.Parse(text, NumberStyles.Integer, InvariantCulture)`.
fn convert_int32(value: &str) -> Result<i32, DotnetError> {
    let text = trim(value);
    let wrap = |inner: DotnetError| {
        DotnetError::new(
            "System.ArgumentException",
            format!("{text} is not a valid value for Int32. (Parameter 'value')"),
        )
        .with_inner(inner)
    };
    if text.is_empty() {
        return Err(wrap(DotnetError::new(
            "System.IndexOutOfRangeException",
            "Index was outside the bounds of the array.",
        )));
    }
    let hex = text.strip_prefix('#').or_else(|| {
        let head = text.get(..2)?;
        (head.eq_ignore_ascii_case("0x") || head.eq_ignore_ascii_case("&h")).then(|| &text[2..])
    });
    if let Some(digits) = hex {
        return parse_hex_int32(digits).map_err(wrap);
    }
    let (negative, digits) = match text.as_bytes()[0] {
        b'-' => (true, &text[1..]),
        b'+' => (false, &text[1..]),
        _ => (false, text),
    };
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return Err(wrap(DotnetError::new(
            "System.FormatException",
            format!("The input string '{text}' was not in a correct format."),
        )));
    }
    let overflow = || {
        wrap(DotnetError::new(
            "System.OverflowException",
            "Value was either too large or too small for an Int32.",
        ))
    };
    let significant = digits.trim_start_matches('0');
    if significant.len() > 10 {
        return Err(overflow());
    }
    let magnitude: i64 = if significant.is_empty() {
        0
    } else {
        significant.parse().map_err(|_| overflow())?
    };
    let signed = if negative { -magnitude } else { magnitude };
    i32::try_from(signed).map_err(|_| overflow())
}

/// `Convert.ToInt32(digits, 16)`: up to 8 hex digits, read as a 32-bit two's complement.
fn parse_hex_int32(digits: &str) -> Result<i32, DotnetError> {
    if digits.is_empty() {
        return Err(DotnetError::new(
            "System.ArgumentOutOfRangeException",
            "Specified argument was out of the range of valid values. (Parameter 'Index was out of range. Must be non-negative and less than the size of the collection.')",
        ));
    }
    let valid = digits.bytes().take_while(u8::is_ascii_hexdigit).count();
    if valid == 0 {
        return Err(DotnetError::new(
            "System.FormatException",
            "Could not find any recognizable digits.",
        ));
    }
    if valid < digits.len() {
        return Err(DotnetError::new(
            "System.FormatException",
            "Additional non-parsable characters are at the end of the string.",
        ));
    }
    let significant = digits.trim_start_matches('0');
    if significant.len() > 8 {
        return Err(DotnetError::new(
            "System.OverflowException",
            "Value was either too large or too small for an Int32.",
        ));
    }
    let v = if significant.is_empty() {
        0
    } else {
        u32::from_str_radix(significant, 16)
            .map_err(|e| DotnetError::new("System.FormatException", e.to_string()))?
    };
    Ok(v as i32)
}

/// `TimeSpanConverter`: `TimeSpan.Parse(text.Trim(), InvariantCulture)`; a format error is
/// wrapped once more, an overflow is not.
fn convert_time_span(value: &str) -> Result<TimeSpan, DotnetError> {
    let text = trim(value);
    TimeSpan::parse(text).map_err(|e| match e {
        TimeSpanParseError::Format => DotnetError::new("System.FormatException", format!("{value} is not a valid value for TimeSpan."))
            .with_inner(DotnetError::new("System.FormatException", format!("String '{text}' was not recognized as a valid TimeSpan."))),
        TimeSpanParseError::Overflow => DotnetError::new(
            "System.OverflowException",
            format!("The TimeSpan string '{text}' could not be parsed because at least one of the numeric components is out of range or contains too many digits."),
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bind(key: &str, value: Option<&str>) -> Result<AgentOptions, String> {
        let mut c = Configuration::new();
        c.set(key, value.map(str::to_owned));
        bind_agent_options(&c).map_err(|e| {
            e.to_string()
                .replace("\n   --- End of inner exception stack trace ---", "")
                .replace('\n', "")
        })
    }

    /// Messages measured with `ConfigurationBinder` on .NET 10 (inner exceptions joined).
    #[test]
    fn conversion_errors_match_dotnet() {
        let cases = [
            (
                "Agent:PollInterval",
                "abc",
                "System.InvalidOperationException: Failed to convert configuration value 'abc' at 'Agent:PollInterval' to type 'System.TimeSpan'. ---> System.FormatException: abc is not a valid value for TimeSpan. ---> System.FormatException: String 'abc' was not recognized as a valid TimeSpan.",
            ),
            (
                "Agent:PollInterval",
                "",
                "System.InvalidOperationException: Failed to convert configuration value '' at 'Agent:PollInterval' to type 'System.TimeSpan'. ---> System.FormatException:  is not a valid value for TimeSpan. ---> System.FormatException: String '' was not recognized as a valid TimeSpan.",
            ),
            (
                "Agent:PollInterval",
                "00:60:00",
                "System.InvalidOperationException: Failed to convert configuration value '00:60:00' at 'Agent:PollInterval' to type 'System.TimeSpan'. ---> System.OverflowException: The TimeSpan string '00:60:00' could not be parsed because at least one of the numeric components is out of range or contains too many digits.",
            ),
            (
                "Agent:RunOnce",
                "1",
                "System.InvalidOperationException: Failed to convert configuration value '1' at 'Agent:RunOnce' to type 'System.Boolean'. ---> System.FormatException: 1 is not a valid value for Boolean. ---> System.FormatException: String '1' was not recognized as a valid Boolean.",
            ),
            (
                "Agent:RunOnce",
                "",
                "System.InvalidOperationException: Failed to convert configuration value '' at 'Agent:RunOnce' to type 'System.Boolean'. ---> System.FormatException:  is not a valid value for Boolean. ---> System.FormatException: String '' was not recognized as a valid Boolean.",
            ),
            (
                "Agent:Lock:MfLockComPort",
                "3.0",
                "System.InvalidOperationException: Failed to convert configuration value '3.0' at 'Agent:Lock:MfLockComPort' to type 'System.Int32'. ---> System.ArgumentException: 3.0 is not a valid value for Int32. (Parameter 'value') ---> System.FormatException: The input string '3.0' was not in a correct format.",
            ),
            (
                "Agent:Lock:MfLockComPort",
                "",
                "System.InvalidOperationException: Failed to convert configuration value '' at 'Agent:Lock:MfLockComPort' to type 'System.Int32'. ---> System.ArgumentException:  is not a valid value for Int32. (Parameter 'value') ---> System.IndexOutOfRangeException: Index was outside the bounds of the array.",
            ),
            (
                "Agent:Lock:MfLockComPort",
                "2147483648",
                "System.InvalidOperationException: Failed to convert configuration value '2147483648' at 'Agent:Lock:MfLockComPort' to type 'System.Int32'. ---> System.ArgumentException: 2147483648 is not a valid value for Int32. (Parameter 'value') ---> System.OverflowException: Value was either too large or too small for an Int32.",
            ),
            (
                "Agent:Lock:MfLockComPort",
                "FFFFFFFF",
                "System.InvalidOperationException: Failed to convert configuration value 'FFFFFFFF' at 'Agent:Lock:MfLockComPort' to type 'System.Int32'. ---> System.ArgumentException: FFFFFFFF is not a valid value for Int32. (Parameter 'value') ---> System.FormatException: The input string 'FFFFFFFF' was not in a correct format.",
            ),
            (
                "Agent:Lock:MfLockComPort",
                "0x",
                "System.InvalidOperationException: Failed to convert configuration value '0x' at 'Agent:Lock:MfLockComPort' to type 'System.Int32'. ---> System.ArgumentException: 0x is not a valid value for Int32. (Parameter 'value') ---> System.ArgumentOutOfRangeException: Specified argument was out of the range of valid values. (Parameter 'Index was out of range. Must be non-negative and less than the size of the collection.')",
            ),
            (
                "Agent:Lock:MfLockComPort",
                "1,000",
                "System.InvalidOperationException: Failed to convert configuration value '1,000' at 'Agent:Lock:MfLockComPort' to type 'System.Int32'. ---> System.ArgumentException: 1,000 is not a valid value for Int32. (Parameter 'value') ---> System.FormatException: The input string '1,000' was not in a correct format.",
            ),
            (
                "Agent:Lock:MfLockComPort",
                "٣",
                "System.InvalidOperationException: Failed to convert configuration value '٣' at 'Agent:Lock:MfLockComPort' to type 'System.Int32'. ---> System.ArgumentException: ٣ is not a valid value for Int32. (Parameter 'value') ---> System.FormatException: The input string '٣' was not in a correct format.",
            ),
        ];
        for (key, value, expected) in cases {
            assert_eq!(
                bind(key, Some(value)).unwrap_err(),
                expected,
                "{key}={value:?}"
            );
        }
    }

    #[test]
    fn values_convert_like_dotnet() {
        for (v, ok) in [
            ("true", true),
            ("True", true),
            (" TRUE ", true),
            ("false", false),
        ] {
            assert_eq!(bind("Agent:RunOnce", Some(v)).unwrap().run_once, ok, "{v}");
        }
        for (v, n) in [
            ("3", 3),
            (" 3 ", 3),
            ("+3", 3),
            ("-3", -3),
            ("0x1F", 31),
            ("#1F", 31),
            ("&H1F", 31),
            ("0X1f", 31),
            ("#FFFFFFFF", -1),
            ("007", 7),
        ] {
            assert_eq!(
                bind("Agent:Lock:MfLockComPort", Some(v))
                    .unwrap()
                    .lock
                    .mf_lock_com_port,
                n,
                "{v}"
            );
        }
        assert_eq!(
            bind("Agent:PollInterval", Some("1"))
                .unwrap()
                .poll_interval
                .to_string(),
            "1.00:00:00"
        );
        assert_eq!(
            bind("agent:agentid", Some("FD01")).unwrap().agent_id,
            "FD01"
        );
        assert_eq!(bind("Agent:AgentId", None).unwrap().agent_id, "");
        assert_eq!(bind("Agent:Adapter", Some("")).unwrap().adapter, "");
        assert_eq!(
            bind("Agent:Lock", Some("x")).unwrap().lock,
            LockOptions::default()
        );
        assert_eq!(
            bind("Agent:CommandTimeout", Some("00:00:45"))
                .unwrap()
                .command_timeout,
            TimeSpan::from_secs(45)
        );
        assert_eq!(
            bind("Agent:Lock:TtLockClientSecret", Some("s"))
                .unwrap()
                .lock
                .tt_lock_client_secret,
            "s"
        );
        assert_eq!(
            bind("Agent:DataDirectory", Some("D:\\x"))
                .unwrap()
                .data_directory,
            "D:\\x"
        );
    }

    #[test]
    fn json_nulls_keep_the_defaults() {
        let c = super::super::reader::parse_json_settings(
            br#"{"Agent":{"AgentId":null,"RunOnce":null,"Lock":{},"TokenFile":"t"}}"#,
        )
        .unwrap();
        let o = bind_agent_options(&c).unwrap();
        assert_eq!(
            (o.agent_id.as_str(), o.run_once, o.token_file.as_str()),
            ("", false, "t")
        );
    }
}
