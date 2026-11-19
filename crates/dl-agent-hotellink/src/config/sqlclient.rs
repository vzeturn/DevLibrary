//! `connectionStringOverride` as sites write it: an ADO.NET (`SqlClient`) string such as
//! `Server=h;Database=SMILE_FO;User Id=sa;Password=…;Encrypt=false;TrustServerCertificate=true;`
//! (the SMILE-HLAPI install guide's example). `dl-tds` reads ADO strings with go-mssqldb's
//! rules, which differ from SqlClient in three places; this module rewrites only those keys
//! and leaves every other part exactly as written:
//!
//! | Written | SqlClient | go-mssqldb / dl-tds as is | Sent to dl-tds |
//! |---|---|---|---|
//! | `TrustServerCertificate=yes` / `no` (any case) | accepted | rejected | `TrustServerCertificate=true` / `false` |
//! | `Trust Server Certificate=…` | synonym | unknown key, ignored (certificate then checked) | `TrustServerCertificate=…` |
//! | `Encrypt=false` / `no` / `optional` / `0` without `TrustServerCertificate` | login packet encrypted, certificate never checked | certificate checked: a SMILE server's self-signed certificate fails | `TrustServerCertificate=true` added |
//!
//! `Encrypt=false` maps to `dl_tds::Encrypt::Off`, the closest mode: TLS for the LOGIN7
//! packet only when the server supports it, the whole session when the server requires it.
//! A written `TrustServerCertificate=false` keeps the certificate check on in that mode,
//! which is stricter than SqlClient for a login-only handshake. Values `dl-tds` does not know
//! (`TrustServerCertificate=maybe`) pass through so that it reports them. A URL
//! (`sqlserver://…`) is go-mssqldb's own format and is returned unchanged; so is a string
//! without an `Encrypt` key (the Go agent read it with go-mssqldb, which `dl-tds` matches).

/// The override rewritten for `dl-tds` (see the module docs).
pub fn to_tds(raw: &str) -> String {
    if raw.contains("://") {
        return raw.to_owned();
    }
    let mut out: Vec<String> = Vec::new();
    let mut login_only = false;
    let mut has_trust = false;
    for part in split(raw) {
        let Some((k, v)) = part.split_once('=') else {
            out.push(part);
            continue;
        };
        let key: String = k
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect::<String>()
            .to_ascii_lowercase();
        let value = v.trim();
        match key.as_str() {
            "trustservercertificate" => {
                has_trust = true;
                let norm = match value.to_ascii_lowercase().as_str() {
                    "true" | "yes" => "true".to_owned(),
                    "false" | "no" => "false".to_owned(),
                    _ => value.to_owned(),
                };
                out.push(format!("TrustServerCertificate={norm}"));
            }
            "encrypt" => {
                login_only = matches!(
                    value.to_ascii_lowercase().as_str(),
                    "false" | "no" | "optional" | "0"
                );
                out.push(part);
            }
            _ => out.push(part),
        }
    }
    if login_only && !has_trust {
        out.push("TrustServerCertificate=true".to_owned());
    }
    out.join(";")
}

/// The `;`-separated parts, empty ones dropped; a `;` inside a double-quoted value (with
/// `""` as an escaped quote) does not split.
fn split(raw: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    for c in raw.chars() {
        match c {
            '"' => {
                quoted = !quoted;
                cur.push(c);
            }
            ';' if !quoted => parts.push(std::mem::take(&mut cur)),
            _ => cur.push(c),
        }
    }
    parts.push(cur);
    parts.retain(|p| !p.trim().is_empty());
    parts
}

#[cfg(test)]
mod tests {
    use super::to_tds;
    use dl_tds::{Config, Encrypt};

    fn tds(raw: &str) -> Config {
        to_tds(raw).parse().unwrap_or_else(|e| panic!("{raw}: {e}"))
    }

    const BASE: &str = "Server=127.0.0.1,1433;Database=SMILE_FO;User Id=sa;Password=pw";

    #[test]
    fn site_string_from_the_install_guide() {
        let raw = format!("{BASE};Encrypt=false;TrustServerCertificate=true;");
        let c = tds(&raw);
        assert_eq!(c.encrypt(), Encrypt::Off);
        assert!(c.trust_server_certificate());
        assert_eq!(c.database(), "SMILE_FO");
        assert_eq!(c.password(), "pw");
        dl_agent::sql::TdsConnector::parse(&to_tds(&raw)).expect("dl-agent accepts it");
    }

    #[test]
    fn encrypt_spellings_case_and_spaces() {
        for e in [
            "Encrypt=false",
            "Encrypt=False",
            "encrypt=FALSE",
            " Encrypt = false ",
            "Encrypt=no",
            "Encrypt=No",
            "Encrypt=Optional",
            "Encrypt=0",
        ] {
            for t in [
                "",
                ";TrustServerCertificate=true",
                ";TrustServerCertificate=True",
                "; trustservercertificate = yes ",
                ";TrustServerCertificate=YES",
                ";Trust Server Certificate=true",
                ";TRUST SERVER CERTIFICATE=Yes",
            ] {
                let c = tds(&format!("{BASE};{e}{t}"));
                assert_eq!(c.encrypt(), Encrypt::Off, "{e}{t}");
                assert!(c.trust_server_certificate(), "{e}{t}");
            }
            for t in [
                ";TrustServerCertificate=false",
                ";TrustServerCertificate=no",
                ";Trust Server Certificate=NO",
            ] {
                let c = tds(&format!("{BASE};{e}{t}"));
                assert_eq!(c.encrypt(), Encrypt::Off, "{e}{t}");
                assert!(!c.trust_server_certificate(), "{e}{t}");
            }
        }
    }

    #[test]
    fn other_encrypt_modes_are_kept() {
        let c = tds(&format!("{BASE};Encrypt=yes;TrustServerCertificate=yes"));
        assert_eq!(c.encrypt(), Encrypt::Required);
        assert!(c.trust_server_certificate());
        let c = tds(&format!("{BASE};Encrypt=True"));
        assert_eq!(c.encrypt(), Encrypt::Required);
        assert!(
            !c.trust_server_certificate(),
            "full encryption checks the certificate"
        );
        let c = tds(&format!(
            "{BASE};Encrypt=Mandatory;Trust Server Certificate=no"
        ));
        assert_eq!(c.encrypt(), Encrypt::Required);
        assert!(!c.trust_server_certificate());
        let c = tds(&format!("{BASE};encrypt=disable"));
        assert_eq!(c.encrypt(), Encrypt::Disabled);
        let c = tds(&format!("{BASE};Encrypt=Strict"));
        assert_eq!(c.encrypt(), Encrypt::Strict);
        // No Encrypt key: as written (go-mssqldb: login only, certificate not checked).
        assert_eq!(to_tds(BASE), BASE);
        let c = tds(BASE);
        assert_eq!(c.encrypt(), Encrypt::Off);
        assert!(c.trust_server_certificate());
    }

    #[test]
    fn other_parts_and_quotes_are_untouched() {
        let raw = "Server=h;Password=\"a;b\"\"Encrypt=true\";Encrypt=false;;Application Name=x";
        let got = to_tds(raw);
        assert_eq!(
            got,
            "Server=h;Password=\"a;b\"\"Encrypt=true\";Encrypt=false;Application Name=x;TrustServerCertificate=true"
        );
        let c = tds(raw);
        assert_eq!(c.password(), "a;b\"Encrypt=true");
        assert_eq!(c.app_name(), "x");
        assert!(c.trust_server_certificate());
    }

    #[test]
    fn urls_and_unknown_values_pass_through() {
        let url = "sqlserver://sa:pw@h:1433?database=SMILE_FO&encrypt=false";
        assert_eq!(to_tds(url), url);
        let bad = to_tds(&format!(
            "{BASE};Encrypt=false;TrustServerCertificate=maybe"
        ));
        assert!(bad.ends_with("TrustServerCertificate=maybe"), "{bad}");
        let e = bad.parse::<Config>().expect_err("dl-tds reports the value");
        assert!(e.to_string().contains("trust"), "{e}");
        assert!(!e.to_string().contains("pw"), "no secret in the error: {e}");
    }
}
