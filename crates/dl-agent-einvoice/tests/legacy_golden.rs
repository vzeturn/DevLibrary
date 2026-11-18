//! The SQL Server 2008 rewrite and the sealed DSN text against vectors exported from the
//! Go agent (`internal/agentcollector/legacy_golden_rust_test.go`).

use dl_agent_einvoice::dsn::Dsn;
use dl_agent_einvoice::install::{FoProbe, SCRIPTS, substitute_fo_tokens};
use dl_agent_einvoice::legacy::legacy_sql;
use dl_json::Value;

fn golden() -> Value {
    let text = include_str!("../testdata/legacy_golden.json");
    dl_json::parse(text).expect("legacy_golden.json")
}

fn s<'a>(v: &'a Value, k: &str) -> &'a str {
    v.get(k).and_then(Value::as_str).unwrap_or("")
}

fn check(c: &Value) {
    let (name, input) = (s(c, "name"), s(c, "input"));
    match legacy_sql(name, input) {
        Ok(got) => {
            assert_eq!(
                s(c, "error"),
                "",
                "{name}: Go failed, Rust did not:\n{input}"
            );
            assert_eq!(got, s(c, "output"), "{name}: rewrite differs from Go");
        }
        Err(e) => assert_eq!(e, s(c, "error"), "{name}: error differs from Go"),
    }
}

fn cases<'a>(g: &'a Value, k: &str) -> &'a [Value] {
    g.get(k)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
}

#[test]
fn every_embedded_script_rewrites_like_go() {
    let g = golden();
    let scripts = cases(&g, "scripts");
    assert!(!scripts.is_empty());
    for c in scripts {
        check(c);
    }
    // The Go inputs are the Rust token substitution of the same embedded files (line
    // endings aside: the vectors may come from a checkout with other ones).
    let lf = |t: &str| t.replace("\r\n", "\n");
    let all = FoProbe {
        has_trn_group: true,
        has_folio_balance: true,
        has_is_hk_post: true,
        has_tax_rate: true,
    };
    for (name, body) in SCRIPTS {
        let mine: Vec<String> = [FoProbe::default(), all]
            .iter()
            .map(|p| lf(&substitute_fo_tokens(body, *p)))
            .collect();
        let theirs: Vec<String> = scripts
            .iter()
            .filter(|c| s(c, "name") == *name)
            .map(|c| lf(s(c, "input")))
            .collect();
        assert!(!theirs.is_empty(), "{name}: no Go vector (regenerate)");
        for t in &theirs {
            assert!(
                mine.contains(t),
                "{name}: Go input differs from the embedded script"
            );
        }
    }
}

#[test]
fn edge_cases_rewrite_like_go() {
    let g = golden();
    let extra = cases(&g, "extra");
    assert!(!extra.is_empty());
    for c in extra {
        check(c);
    }
}

#[test]
fn dsn_text_matches_go() {
    let g = golden();
    let dsns = cases(&g, "dsns");
    assert!(!dsns.is_empty());
    for c in dsns {
        let d = c.get("dsn").expect("dsn");
        let dsn = Dsn {
            host: s(d, "Host").into(),
            port: s(d, "Port").into(),
            instance: s(d, "Instance").into(),
            database: s(d, "Database").into(),
            user: s(d, "User").into(),
            password: s(d, "Password").into(),
            tls_min: s(d, "TLSMin").into(),
            encrypt: s(d, "Encrypt").into(),
        };
        assert_eq!(dsn.to_string(), s(c, "text"), "{dsn:?}");
    }
}
