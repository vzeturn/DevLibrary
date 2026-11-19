use dl_json::json;

use super::*;
use crate::testutil::{Recorded, Reply, Server, temp_dir};

const KEY: &str = "ZGwtYWdlbnQtYmFja3VwIHRlc3Qga2V5LCBub3QgYSBzZWNyZXQ=";

fn conn(endpoint: &str) -> Value {
    json!({
        "connectionString": format!("conn:AccountName=acct;AccountKey={KEY};BlobEndpoint={endpoint}/acct"),
        "bucketName": "bk",
    })
}

/// Recomputes the signature from what the server received.
fn signed_correctly(r: &Recorded) -> bool {
    let wanted = [
        "content-length",
        "content-type",
        "x-ms-blob-type",
        "x-ms-date",
        "x-ms-range",
        "x-ms-version",
    ];
    let headers: Vec<(&str, &str)> = r
        .headers
        .iter()
        .filter(|(k, _)| wanted.contains(&k.to_ascii_lowercase().as_str()))
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    let key = SharedKey::new("acct", KEY).unwrap();
    r.header("authorization")
        == Some(
            key.authorization(&r.method, &headers, r.path(), r.query())
                .as_str(),
        )
}

#[test]
fn credential_prefixes_like_go() {
    // Go TestAzureCredentialParsing_ConnPrefix / SASPrefix / NoPrefix and
    // TestAzureUpload_EmptyConnectionString.
    let a = Azure::from_tokens(&json!({"connectionString": format!("conn:DefaultEndpointsProtocol=https;AccountName=acct;AccountKey={KEY};EndpointSuffix=core.windows.net")})).unwrap();
    assert_eq!(
        a.cloud_path("x.bak"),
        "https://acct.blob.core.windows.net/backups/x.bak"
    );
    let a = Azure::from_tokens(&json!({"connectionString": "sas:https://acct.blob.core.windows.net/?sv=2022&sig=SECRET", "bucketName": "bk"})).unwrap();
    assert_eq!(
        a.cloud_path("x.bak"),
        "https://acct.blob.core.windows.net/bk/x.bak"
    );
    let e = Azure::from_tokens(&json!({"connectionString": "AccountName=a"})).unwrap_err();
    assert_eq!(
        e,
        Error::Upload("azure ConnectionString must start with 'conn:' or 'sas:'".into())
    );
    let e = Azure::from_tokens(&json!({})).unwrap_err();
    assert_eq!(
        e,
        Error::Upload(
            "azure blob: ConnectionString is empty — must start with 'conn:' or 'sas:'".into()
        )
    );
}

#[test]
fn bad_connection_strings_never_echo_the_secret() {
    for (raw, want) in [
        (
            "conn:AccountKey=SECRETKEY",
            "no AccountName or BlobEndpoint",
        ),
        ("conn:AccountName=a", "needs AccountName and AccountKey"),
        ("conn:AccountName=a;AccountKey=SECRET!!", "not valid base64"),
        ("conn:AccountName=a;SECRETKEY", "part without '='"),
        (
            "conn:AccountName=a;AccountKey=abc=;BlobEndpoint=http://blob.lan",
            "non-localhost",
        ),
        ("sas:https://acct.blob.core.windows.net/", "no token"),
        ("sas:not a url SECRET", "not a valid URL"),
    ] {
        let e = Azure::from_tokens(&json!({"connectionString": raw}))
            .unwrap_err()
            .to_string();
        assert!(e.contains(want), "{raw}: {e}");
        assert!(!e.contains("SECRET"), "{e}");
    }
}

#[test]
fn container_names() {
    // Go TestAzureContainerNameFromCreds, plus the container of a container-level SAS URL.
    let sas = |b: &str| json!({"connectionString": "sas:https://a.blob.core.windows.net/fromsas?sig=x", "bucketName": b});
    assert_eq!(Azure::from_tokens(&sas("")).unwrap().container(), "fromsas");
    assert_eq!(
        Azure::from_tokens(&sas("mine")).unwrap().container(),
        "mine"
    );
    let a = Azure::from_tokens(&json!({"connectionString": "conn:UseDevelopmentStorage=true"}))
        .unwrap();
    assert_eq!(a.container(), "backups");
    assert_eq!(
        a.cloud_path("db 1.bak"),
        "http://127.0.0.1:10000/devstoreaccount1/backups/db%201.bak"
    );
}

#[test]
fn cloud_paths_parse_like_go() {
    // Go TestDownloadAzureBlob_InvalidCloudPath and the blob URL helpers.
    let p = |s: &str| parse_cloud_path(s, "");
    assert_eq!(
        p("https://a.blob.core.windows.net/c/x.bak").unwrap(),
        ("c".into(), "x.bak".into())
    );
    assert_eq!(
        p("https://a.blob.core.windows.net/c/db%201.bak?sig=x").unwrap(),
        ("c".into(), "db 1.bak".into())
    );
    assert_eq!(
        parse_cloud_path(
            "http://127.0.0.1:10000/devstoreaccount1/c/x.bak",
            "/devstoreaccount1"
        )
        .unwrap(),
        ("c".into(), "x.bak".into())
    );
    for bad in [
        "https://a.blob.core.windows.net/c",
        "https://a.blob.core.windows.net/",
        "not a url",
    ] {
        assert!(p(bad).is_err(), "{bad}");
    }
    let e = p("https://a.blob.core.windows.net/c?sig=SECRET")
        .unwrap_err()
        .to_string();
    assert!(!e.contains("SECRET"), "{e}");
}

#[test]
fn small_file_is_one_signed_put_blob() {
    // Go TestAzureUpload_RoundTrip / BlobNameFromLocalFile / ProgressCallback.
    let srv = Server::start(|_| Reply::new(201, ""));
    let dir = temp_dir("az-small");
    let file = dir.join("db_1.bak");
    std::fs::write(&file, b"hello").unwrap();
    let a = Azure::from_tokens(&conn(&srv.url())).unwrap();
    let mut last = (0, 0);
    let cloud = a
        .upload(&file, &CancelToken::new(), &mut |x, y| last = (x, y))
        .unwrap();
    assert_eq!(cloud, format!("{}/acct/bk/db_1.bak", srv.url()));
    assert_eq!(last, (5, 5));
    let reqs = srv.requests();
    let r = &reqs[0];
    assert_eq!(
        (r.method.as_str(), r.target.as_str()),
        ("PUT", "/acct/bk/db_1.bak")
    );
    assert_eq!(r.header("x-ms-blob-type"), Some("BlockBlob"));
    assert_eq!(r.body, b"hello");
    assert!(signed_correctly(r));
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn large_file_goes_up_in_blocks() {
    let srv = Server::start(|_| Reply::new(201, ""));
    let dir = temp_dir("az-large");
    let file = dir.join("big.bak");
    std::fs::write(&file, vec![7u8; BLOCK + 3]).unwrap();
    let a = Azure::from_tokens(&conn(&srv.url())).unwrap();
    let mut calls = Vec::new();
    a.upload(&file, &CancelToken::new(), &mut |x, y| calls.push((x, y)))
        .unwrap();
    let total = (BLOCK + 3) as u64;
    assert_eq!(calls, vec![(BLOCK as u64, total), (total, total)]);
    let reqs = srv.requests();
    assert_eq!(reqs.len(), 3);
    let id1 = STANDARD.encode("dl-agent-block-00000001");
    let id2 = STANDARD.encode("dl-agent-block-00000002");
    assert_eq!(
        reqs[0].query(),
        format!("comp=block&blockid={}", encode_query_component(&id1))
    );
    assert_eq!((reqs[0].body.len(), reqs[1].body.len()), (BLOCK, 3));
    assert_eq!(reqs[2].query(), "comp=blocklist");
    assert_eq!(
        String::from_utf8(reqs[2].body.clone()).unwrap(),
        format!(
            "<?xml version=\"1.0\" encoding=\"utf-8\"?><BlockList><Latest>{id1}</Latest><Latest>{id2}</Latest></BlockList>"
        )
    );
    assert!(reqs.iter().all(signed_correctly));
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn sas_requests_carry_the_token_but_the_cloud_path_does_not() {
    // Go TestAzureSasContainerURL kept the token in the cloud path; that leaked it.
    let srv = Server::start(|r| match r.method.as_str() {
        "PUT" => Reply::new(201, ""),
        "DELETE" => Reply::new(404, "<Error><Code>BlobNotFound</Code></Error>"),
        _ => Reply::new(
            403,
            "<Error><Code>AuthenticationFailed</Code><Message>bad sig</Message></Error>",
        ),
    });
    let dir = temp_dir("az-sas");
    let file = dir.join("x.bak");
    std::fs::write(&file, b"abc").unwrap();
    let t = json!({"connectionString": format!("sas:{}/?sv=2022-11-02&sig=abc%2Fdef", srv.url()), "bucketName": "bk"});
    let a = Azure::from_tokens(&t).unwrap();
    let cloud = a
        .upload(&file, &CancelToken::new(), &mut |_, _| {})
        .unwrap();
    assert_eq!(cloud, format!("{}/bk/x.bak", srv.url()));
    let reqs = srv.requests();
    assert_eq!(reqs[0].target, "/bk/x.bak?sv=2022-11-02&sig=abc%2Fdef");
    assert_eq!(reqs[0].header("authorization"), None);
    a.delete(&cloud).unwrap();
    let e = a
        .download(&cloud, &dir.join("r.bak"), &CancelToken::new())
        .unwrap_err();
    assert_eq!(
        e,
        Error::Upload("Azure get blob: HTTP 403 AuthenticationFailed bad sig".into())
    );
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn download_is_ranged_and_delete_accepts_202() {
    // Go TestDownloadAzureBlob_WritesFileContent.
    let srv = Server::start(|r| match r.method.as_str() {
        "GET" => {
            assert_eq!(r.header("x-ms-range"), Some("bytes=0-33554431"));
            Reply::new(206, "hello").with("content-range", "bytes 0-4/5")
        }
        "DELETE" => Reply::new(202, ""),
        _ => Reply::new(400, ""),
    });
    let dir = temp_dir("az-dl");
    let a = Azure::from_tokens(&conn(&srv.url())).unwrap();
    let cloud = format!("{}/acct/bk/x.bak", srv.url());
    assert_eq!(
        a.download(&cloud, &dir.join("r.bak"), &CancelToken::new())
            .unwrap(),
        5
    );
    assert_eq!(std::fs::read(dir.join("r.bak")).unwrap(), b"hello");
    a.delete(&cloud).unwrap();
    let reqs = srv.requests();
    assert_eq!(reqs[1].target, "/acct/bk/x.bak");
    assert!(reqs.iter().all(signed_correctly));
    std::fs::remove_dir_all(dir).unwrap();
}
