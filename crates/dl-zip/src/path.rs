//! Entry name checks shared by extraction (zip-slip protection) and the writer.

/// Splits an entry name into path components, or returns `None` when the name could
/// escape the target directory.
///
/// Both `/` and `\` separate components (Windows PowerShell 5.1 writes `\`). Empty and
/// `.` components are dropped. Refused everywhere: a leading separator (absolute or UNC
/// path), a drive letter (`C:`), a `..` component, NUL and other control characters.
/// With `windows`, also refused: any `:` (alternate data streams), components ending in
/// `.` or space (Windows strips them, so `.. ` would become `..`) and reserved device
/// names (`CON`, `NUL`, `COM1`, ...).
pub(crate) fn components(name: &str, windows: bool) -> Option<Vec<&str>> {
    if name.starts_with(['/', '\\']) {
        return None;
    }
    let b = name.as_bytes();
    if b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':' {
        return None;
    }
    let mut out = Vec::new();
    for c in name.split(['/', '\\']) {
        if c.is_empty() || c == "." {
            continue;
        }
        if c == ".." || c.chars().any(char::is_control) {
            return None;
        }
        if windows && (c.contains(':') || c.ends_with(['.', ' ']) || is_device_name(c)) {
            return None;
        }
        out.push(c);
    }
    Some(out)
}

fn is_device_name(c: &str) -> bool {
    let stem = c.split('.').next().unwrap_or(c).trim_end_matches(' ');
    let upper = stem.to_ascii_uppercase();
    match upper.as_str() {
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$" => true,
        _ => {
            let (Some(head), Some(tail)) = (upper.get(..3), upper.get(3..)) else {
                return false;
            };
            (head == "COM" || head == "LPT")
                && matches!(
                    tail,
                    "0" | "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
                )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_ordinary_names() {
        assert_eq!(components("a/b.txt", true).unwrap(), ["a", "b.txt"]);
        assert_eq!(components("a/./b//c/", true).unwrap(), ["a", "b", "c"]);
        assert_eq!(
            components("a\\b\\c.txt", true).unwrap(),
            ["a", "b", "c.txt"]
        );
        assert_eq!(
            components("Tài liệu/hóa đơn.json", true).unwrap(),
            ["Tài liệu", "hóa đơn.json"]
        );
        assert_eq!(components("..a/b..c", true).unwrap(), ["..a", "b..c"]);
        assert_eq!(components("./", true).unwrap(), Vec::<&str>::new());
        assert_eq!(components("12:30:00.log", false).unwrap(), ["12:30:00.log"]);
        assert_eq!(components("console.log", true).unwrap(), ["console.log"]);
        assert_eq!(components("COM10", true).unwrap(), ["COM10"]);
        assert_eq!(components("ééé", true).unwrap(), ["ééé"]);
    }

    #[test]
    fn refuses_escaping_names() {
        for name in [
            "../evil",
            "a/../../evil",
            "a/..",
            "/etc/passwd",
            "\\evil",
            "\\\\server\\share\\x",
            "C:\\Windows\\evil.dll",
            "c:evil",
            "a\\..\\..\\evil",
            "a\0b",
            "a/\u{1}b",
        ] {
            assert!(components(name, false).is_none(), "{name:?}");
            assert!(components(name, true).is_none(), "{name:?}");
        }
        for name in [
            "a/b:stream",
            "a/.. ",
            "a/...",
            "x/CON",
            "nul.txt",
            "Com1.log",
            "lpt²",
            "aux /x",
        ] {
            assert!(components(name, false).is_some(), "{name:?}");
            assert!(components(name, true).is_none(), "{name:?}");
        }
    }
}
