use std::sync::Mutex;

use dl_json::json;

use super::*;
use crate::testutil::{Reply, Server, temp_dir};

fn tokens(endpoint: &str) -> Value {
    json!({
        "accessKeyId": "AKTEST",
        "secretAccessKey": "SECRETTEST",
        "bucketName": "bk",
        "remoteFolderPath": "/site/a/",
        "endpoint": endpoint,
        "usePathStyle": true,
    })
}

#[test]
fn missing_credentials_and_bucket_fail_like_go() {
    // Go TestUploadS3_MissingCreds / TestS3Upload_MissingBucket.
    let e = S3::from_tokens(&json!({"bucketName": "b"})).unwrap_err();
    assert_eq!(
        e,
        Error::Upload("AWS S3 credentials incomplete (need accessKeyId, secretAccessKey)".into())
    );
    let e = S3::from_tokens(&json!({"accessKeyId": "a", "secretAccessKey": "s"})).unwrap_err();
    assert_eq!(e, Error::Upload("AWS S3 bucket name is required".into()));
}

#[test]
fn region_defaults() {
    // Go TestS3Upload_DefaultRegion / EndpointDefaultsRegionToAuto / EndpointExplicitRegionWins.
    let mut t = tokens("");
    let s = S3::from_tokens(&t).unwrap();
    assert_eq!(s.creds.region, "ap-southeast-1");
    assert_eq!(
        s.locate("bk", "k").unwrap().0,
        "https://bk.s3.ap-southeast-1.amazonaws.com"
    );
    t = tokens("https://acc.r2.cloudflarestorage.com");
    assert_eq!(S3::from_tokens(&t).unwrap().creds.region, "auto");
    if let Value::Object(m) = &mut t {
        m.insert("region", "eu-west-1");
    }
    assert_eq!(S3::from_tokens(&t).unwrap().creds.region, "eu-west-1");
}

#[test]
fn endpoint_validation_matches_go() {
    // Go TestValidateS3Endpoint.
    for ok in [
        "https://x.r2.cloudflarestorage.com",
        "http://localhost:9000",
        "http://127.0.0.1:9000",
        "http://[::1]:9000",
    ] {
        assert!(validate_endpoint(ok).is_ok(), "{ok}");
    }
    for bad in ["http://minio.lan:9000", "ftp://x", "not a url"] {
        assert!(validate_endpoint(bad).is_err(), "{bad}");
    }
    assert!(S3::from_tokens(&tokens("http://example.com")).is_err());
}

#[test]
fn cloud_paths_parse_like_go() {
    // Go TestParseS3CloudPath.
    assert_eq!(
        parse_cloud_path("s3://b/k/x.bak").unwrap(),
        ("b".into(), "k/x.bak".into())
    );
    for bad in ["b/k", "s3://", "s3://b", "s3://b/", "s3:///k"] {
        assert!(parse_cloud_path(bad).is_err(), "{bad}");
    }
}

#[test]
fn addressing_styles() {
    let mut t = tokens("https://acc.r2.cloudflarestorage.com");
    let s = S3::from_tokens(&t).unwrap();
    assert_eq!(
        s.locate("bk", "a b/x$.bak").unwrap(),
        (
            "https://acc.r2.cloudflarestorage.com".into(),
            "/bk/a%20b/x%24.bak".into()
        )
    );
    if let Value::Object(m) = &mut t {
        m.insert("usePathStyle", false);
    }
    let s = S3::from_tokens(&t).unwrap();
    assert_eq!(
        s.locate("bk", "x.bak").unwrap().0,
        "https://bk.acc.r2.cloudflarestorage.com"
    );
    assert!(s.locate("bk", "a/../x.bak").is_err());
    assert_eq!(s.key_for("x.bak"), "site/a/x.bak");
}

#[test]
fn small_file_is_one_signed_put() {
    // Go TestS3Upload_SmallFile / WithRemotePath / CustomEndpoint_R2.
    let srv = Server::start(|_| Reply::new(200, "").with("etag", "\"e\""));
    let dir = temp_dir("s3-small");
    let file = dir.join("db_1.bak");
    std::fs::write(&file, b"hello backup").unwrap();
    let s = S3::from_tokens(&tokens(&srv.url())).unwrap();
    let mut last = (0, 0);
    let cloud = s
        .upload(&file, &CancelToken::new(), &mut |a, b| last = (a, b))
        .unwrap();
    assert_eq!(cloud, "s3://bk/site/a/db_1.bak");
    assert_eq!(last, (12, 12));
    let reqs = srv.requests();
    assert_eq!(reqs.len(), 1);
    let r = &reqs[0];
    assert_eq!(
        (r.method.as_str(), r.target.as_str()),
        ("PUT", "/bk/site/a/db_1.bak")
    );
    assert_eq!(r.body, b"hello backup");
    assert_eq!(
        r.header("x-amz-content-sha256"),
        Some(sigv4::payload_hash(b"hello backup").as_str())
    );
    let auth = r.header("authorization").unwrap();
    assert!(
        auth.starts_with("AWS4-HMAC-SHA256 Credential=AKTEST/"),
        "{auth}"
    );
    assert!(auth.contains(
        "/auto/s3/aws4_request,SignedHeaders=content-type;host;x-amz-content-sha256;x-amz-date,"
    ));
    assert!(!auth.contains("SECRETTEST"));
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn large_file_goes_up_in_parts() {
    let srv = Server::start(|r| match (r.method.as_str(), r.query()) {
        ("POST", "uploads=") => Reply::new(
            200,
            "<InitiateMultipartUploadResult><UploadId>U1</UploadId></InitiateMultipartUploadResult>",
        ),
        ("PUT", q) if q.starts_with("partNumber=") => {
            let n = &q["partNumber=".len()..q.find('&').unwrap()];
            Reply::new(200, "").with("etag", &format!("\"etag-{n}\""))
        }
        ("POST", "uploadId=U1") => Reply::new(200, "<CompleteMultipartUploadResult/>"),
        _ => Reply::new(400, "<Error><Code>Unexpected</Code></Error>"),
    });
    let dir = temp_dir("s3-large");
    let file = dir.join("big.bak");
    std::fs::write(&file, vec![1u8; PART + 5]).unwrap();
    let s = S3::from_tokens(&tokens(&srv.url())).unwrap();
    let mut calls = Vec::new();
    s.upload(&file, &CancelToken::new(), &mut |a, b| calls.push((a, b)))
        .unwrap();
    let total = (PART + 5) as u64;
    assert_eq!(calls, vec![(PART as u64, total), (total, total)]);
    let reqs = srv.requests();
    let shape: Vec<(String, String, usize)> = reqs
        .iter()
        .map(|r| (r.method.clone(), r.query().to_owned(), r.body.len()))
        .collect();
    assert_eq!(
        shape,
        vec![
            ("POST".into(), "uploads=".into(), 0),
            ("PUT".into(), "partNumber=1&uploadId=U1".into(), PART),
            ("PUT".into(), "partNumber=2&uploadId=U1".into(), 5),
            ("POST".into(), "uploadId=U1".into(), reqs[3].body.len()),
        ]
    );
    let xml = String::from_utf8(reqs[3].body.clone()).unwrap();
    assert_eq!(
        xml,
        "<CompleteMultipartUpload><Part><PartNumber>1</PartNumber><ETag>\"etag-1\"</ETag></Part>\
         <Part><PartNumber>2</PartNumber><ETag>\"etag-2\"</ETag></Part></CompleteMultipartUpload>"
    );
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn failed_part_aborts_the_upload() {
    let srv = Server::start(|r| match (r.method.as_str(), r.query()) {
        ("POST", "uploads=") => Reply::new(200, "<UploadId>U2</UploadId>"),
        ("PUT", _) => Reply::new(
            403,
            "<Error><Code>AccessDenied</Code><Message>nope</Message></Error>",
        ),
        ("DELETE", "uploadId=U2") => Reply::new(204, ""),
        _ => Reply::new(400, ""),
    });
    let dir = temp_dir("s3-abort");
    let file = dir.join("big.bak");
    std::fs::write(&file, vec![1u8; PART + 1]).unwrap();
    let s = S3::from_tokens(&tokens(&srv.url())).unwrap();
    let err = s
        .upload(&file, &CancelToken::new(), &mut |_, _| {})
        .unwrap_err();
    assert_eq!(
        err,
        Error::Upload("S3 upload part 1: HTTP 403 AccessDenied nope".into())
    );
    assert_eq!(srv.requests().last().unwrap().method, "DELETE");
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn download_and_delete() {
    // Go TestDownloadS3_WritesFileContent / TestDeleteS3_RoundTrip / InvalidCloudPath.
    let seen = Mutex::new(0);
    let srv = Server::start(move |r| {
        *seen.lock().unwrap() += 1;
        match r.method.as_str() {
            "GET" => {
                assert_eq!(r.header("range"), Some("bytes=0-16777215"));
                Reply::new(206, "0123456789").with("content-range", "bytes 0-9/10")
            }
            "DELETE" if r.path().ends_with("gone.bak") => Reply::new(404, ""),
            "DELETE" => Reply::new(204, ""),
            _ => Reply::new(400, ""),
        }
    });
    let dir = temp_dir("s3-dl");
    let s = S3::from_tokens(&tokens(&srv.url())).unwrap();
    let dest = dir.join("restore.bak");
    assert_eq!(
        s.download("s3://bk/site/a/x.bak", &dest, &CancelToken::new())
            .unwrap(),
        10
    );
    assert_eq!(std::fs::read(&dest).unwrap(), b"0123456789");
    s.delete("s3://bk/site/a/x.bak").unwrap();
    s.delete("s3://bk/site/a/gone.bak").unwrap();
    assert!(s.delete("bk/x.bak").is_err());
    assert_eq!(srv.requests()[0].path(), "/bk/site/a/x.bak");
    std::fs::remove_dir_all(dir).unwrap();
}
