//! UTF-16 strings for Win32 and command-line quoting.

use crate::Error;

/// `s` as a NUL-terminated UTF-16 string. Rejects interior NUL characters, which would
/// silently truncate the value on the Windows side.
pub(crate) fn to_wide(s: &str) -> Result<Vec<u16>, Error> {
    if s.contains('\0') {
        return Err(Error::InvalidInput("string contains a NUL character"));
    }
    Ok(s.encode_utf16().chain(core::iter::once(0)).collect())
}

/// Quotes one command-line argument so `CommandLineToArgvW` (and the C runtime) reads it
/// back unchanged. Same rules and output as Go's `syscall.EscapeArg`.
pub fn escape_arg(s: &str) -> String {
    if s.is_empty() {
        return "\"\"".to_owned();
    }
    let has_space = s.bytes().any(|b| b == b' ' || b == b'\t');
    let needs_work = has_space || s.bytes().any(|b| b == b'"' || b == b'\\');
    if !needs_work {
        return s.to_owned();
    }
    let mut out = String::with_capacity(s.len() + 2);
    if has_space {
        out.push('"');
    }
    let mut slashes = 0usize;
    for c in s.chars() {
        match c {
            '\\' => {
                slashes += 1;
                out.push(c);
            }
            '"' => {
                for _ in 0..slashes {
                    out.push('\\');
                }
                slashes = 0;
                out.push('\\');
                out.push('"');
            }
            _ => {
                slashes = 0;
                out.push(c);
            }
        }
    }
    if has_space {
        for _ in 0..slashes {
            out.push('\\');
        }
        out.push('"');
    }
    out
}

/// The service command line for `exe` followed by `args`, each quoted with
/// [`escape_arg`]. This is the `binary_path_name` the SCM runs.
pub fn command_line(exe: &str, args: &[&str]) -> String {
    let mut s = escape_arg(exe);
    for a in args {
        s.push(' ');
        s.push_str(&escape_arg(a));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wide_rejects_nul() {
        assert_eq!(to_wide("ab").unwrap(), vec![97, 98, 0]);
        assert_eq!(to_wide("").unwrap(), vec![0]);
        assert_eq!(to_wide("đ").unwrap(), vec![0x111, 0]);
        assert!(matches!(to_wide("a\0b"), Err(Error::InvalidInput(_))));
    }

    #[test]
    fn escape_matches_go() {
        // Expected values produced by Go's syscall.EscapeArg.
        let cases = [
            ("", "\"\""),
            ("plain", "plain"),
            (
                r"C:\Program Files\App\a.exe",
                r#""C:\Program Files\App\a.exe""#,
            ),
            (r"C:\dir\", r"C:\dir\"),
            (r"C:\my dir\", r#""C:\my dir\\""#),
            (r#"say "hi""#, r#""say \"hi\"""#),
            (r#"a\"b"#, r#"a\\\"b"#),
            ("tab\there", "\"tab\there\""),
        ];
        for (input, want) in cases {
            assert_eq!(escape_arg(input), want, "input {input:?}");
        }
    }

    #[test]
    fn command_line_joins() {
        assert_eq!(
            command_line(r"C:\ProgramData\My App\agent.exe", &["-config", r"C:\x y"]),
            r#""C:\ProgramData\My App\agent.exe" -config "C:\x y""#
        );
        assert_eq!(command_line(r"C:\a.exe", &[]), r"C:\a.exe");
    }
}
