use super::*;

fn url(s: &str) -> Url {
    Url::parse(s).unwrap()
}

fn via(rules: &ProxyRules, target: &str) -> Option<String> {
    rules
        .proxy_for(&url(target))
        .map(|u| format!("{}:{}", u.host(), u.port()))
}

#[test]
fn one_server_for_every_protocol() {
    let r = ProxyRules::from_windows("proxy.corp:8080", "").unwrap();
    assert_eq!(
        via(&r, "https://agent.example.com/v1"),
        Some("proxy.corp:8080".into())
    );
    assert_eq!(
        via(&r, "http://agent.example.com/"),
        Some("proxy.corp:8080".into())
    );
    assert_eq!(via(&r, "https://localhost/"), None);
    assert_eq!(via(&r, "https://127.0.0.1:8443/"), None);
    assert_eq!(
        r.describe(),
        "proxy.corp:8080; 0 bypass entries; credentials no"
    );
}

#[test]
fn per_protocol_lists_and_missing_protocols_go_direct() {
    let r =
        ProxyRules::from_windows("ftp=f:21;http=http://p1:3128;https=p2;socks=s:1080", "").unwrap();
    assert_eq!(via(&r, "http://a.example/"), Some("p1:3128".into()));
    assert_eq!(via(&r, "https://a.example/"), Some("p2:80".into()));
    let only_http = ProxyRules::from_windows("http=p1:3128", "").unwrap();
    assert_eq!(via(&only_http, "https://a.example/"), None);
    assert_eq!(
        only_http.describe(),
        "http=p1:3128 https=direct; 0 bypass entries; credentials no"
    );
    for bad in ["", "ftp=f:21;socks=s:1080", "https=https://p:443"] {
        assert!(ProxyRules::from_windows(bad, "").is_err(), "{bad}");
    }
}

#[test]
fn override_list_with_local_wildcards_and_ports() {
    let r = ProxyRules::from_windows(
        "p:8080",
        "<local>;*.corp.local; 10.*,192.168.1.5;api.example.com:8443;http://legacy.test",
    )
    .unwrap();
    for direct in [
        "https://intranet/",
        "https://SQL01/",
        "https://db.corp.local/",
        "https://a.b.CORP.local./",
        "http://10.1.2.3/",
        "http://192.168.1.5/",
        "https://api.example.com:8443/",
        "http://legacy.test/",
    ] {
        assert_eq!(via(&r, direct), None, "{direct} should be direct");
    }
    for proxied in [
        "https://corp.local.evil.com/",
        "https://hub.example.com/",
        "https://api.example.com/",
        "http://110.1.2.3/",
        "http://192.168.1.50/",
        "https://[2001:db8::1]/",
    ] {
        assert!(via(&r, proxied).is_some(), "{proxied} should use the proxy");
    }
    assert_eq!(
        r.describe(),
        "p:8080; 5 bypass entries + <local>; credentials no"
    );
    // Without <local>, a dotless name uses the proxy.
    let r = ProxyRules::from_windows("p:8080", "*.corp.local").unwrap();
    assert!(via(&r, "https://intranet/").is_some());
}

#[test]
fn glob_matches_runs_of_characters() {
    assert!(glob("*.corp.local", "a.corp.local"));
    assert!(!glob("*.corp.local", "corp.local"));
    assert!(glob("10.*", "10.0.0.1"));
    assert!(glob("a*b*c", "axxbyyc"));
    assert!(!glob("a*b*c", "axxbyy"));
    assert!(glob("*", "anything"));
    assert!(glob("exact", "exact") && !glob("exact", "exactly"));
}

#[test]
fn credentials_from_url_userinfo_or_added_later() {
    let r = ProxyRules::from_windows("http://u%40corp:p%3Ass@p:8080", "").unwrap();
    assert!(r.has_credentials());
    let p = r.proxy_for(&url("https://hub.example/")).unwrap();
    assert_eq!((p.username(), p.password()), ("u%40corp", Some("p%3Ass")));
    // Credentials in the URL win over the ones added.
    let kept = r.clone().with_credentials("other", "x").unwrap();
    assert_eq!(kept, r);
    let added = ProxyRules::from_windows("p:8080", "")
        .unwrap()
        .with_credentials("svc@corp", "p:a ss")
        .unwrap();
    let p = added.proxy_for(&url("https://hub.example/")).unwrap();
    assert_eq!(p.username(), "svc%40corp");
    assert_eq!(p.password(), Some("p%3Aa%20ss"));
    assert_eq!(
        added.describe(),
        "p:8080; 0 bypass entries; credentials yes"
    );
    assert!(!added.describe().contains("svc"), "{}", added.describe());
    assert!(!format!("{added:?}").contains("a%20ss"), "{added:?}");
    let none = ProxyRules::from_windows("p:8080", "").unwrap();
    assert_eq!(none.clone().with_credentials("", "x").unwrap(), none);
}

#[test]
fn environment_snapshot_keeps_no_proxy() {
    let env = |vars: &'static [(&'static str, &'static str)]| {
        move |n: &str| {
            vars.iter()
                .find(|(k, _)| *k == n)
                .map(|(_, v)| (*v).to_string())
        }
    };
    assert_eq!(ProxyRules::from_env(env(&[])).unwrap(), None);
    let r = ProxyRules::from_env(env(&[
        ("https_proxy", "http://p:3128"),
        ("NO_PROXY", ".corp.local,10.0.0.0/8"),
    ]))
    .unwrap()
    .unwrap();
    assert_eq!(via(&r, "https://hub.example/"), Some("p:3128".into()));
    assert_eq!(via(&r, "https://db.corp.local/"), None);
    assert_eq!(via(&r, "https://10.2.3.4/"), None);
    assert_eq!(via(&r, "http://hub.example/"), None);
    assert_eq!(
        r.describe(),
        "http=direct https=p:3128; NO_PROXY set; credentials no"
    );
    let cgi =
        ProxyRules::from_env(env(&[("HTTP_PROXY", "p:1"), ("REQUEST_METHOD", "GET")])).unwrap();
    assert_eq!(cgi, None);
    assert!(ProxyRules::from_env(env(&[("HTTPS_PROXY", "https://p:1")])).is_err());
}

#[test]
fn proxy_rules_variant_resolves() {
    let r = ProxyRules::from_windows("p:8080", "<local>").unwrap();
    let proxy = crate::Proxy::Rules(r);
    assert_eq!(
        proxy
            .resolve(&url("https://hub.example/"))
            .unwrap()
            .unwrap()
            .port(),
        8080
    );
    assert!(proxy.resolve(&url("https://intranet/")).unwrap().is_none());
}
