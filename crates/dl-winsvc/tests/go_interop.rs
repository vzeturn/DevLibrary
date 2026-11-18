//! DPAPI interoperability with the Go agent's sealing code (`internal/kernel/agentcrypto`).
//!
//! The Go side lives outside the repository. Run:
//!
//! ```text
//! $env:DL_WINSVC_GO_BLOB = "<file written by Go>"        # Go seals, Rust opens
//! $env:DL_WINSVC_RUST_BLOB = "<file for Go to open>"     # Rust seals, Go opens
//! cargo test -p dl-winsvc --test go_interop -- --ignored
//! ```
#![cfg(windows)]

use dl_winsvc::dpapi::{Scope, protect, unprotect};

/// The plaintext both sides use.
const PLAINTEXT: &str =
    r#"{"api_key":"k-123","sql_dsn":"sqlserver://smile:pw@./?database=SMILE_FO","note":"Hóa đơn"}"#;

#[test]
#[ignore = "needs DL_WINSVC_GO_BLOB from the Go helper"]
fn opens_a_blob_sealed_by_go() {
    let path = std::env::var("DL_WINSVC_GO_BLOB").expect("DL_WINSVC_GO_BLOB");
    let blob = std::fs::read(path).unwrap();
    let plain = unprotect(&blob, Scope::LocalMachine).unwrap();
    let (len, rest) = plain.split_at(4);
    assert_eq!(
        u32::from_be_bytes(len.try_into().unwrap()) as usize,
        rest.len()
    );
    assert_eq!(rest, PLAINTEXT.as_bytes());
}

#[test]
#[ignore = "writes DL_WINSVC_RUST_BLOB for the Go helper"]
fn seals_a_blob_for_go() {
    let path = std::env::var("DL_WINSVC_RUST_BLOB").expect("DL_WINSVC_RUST_BLOB");
    let mut framed = (PLAINTEXT.len() as u32).to_be_bytes().to_vec();
    framed.extend_from_slice(PLAINTEXT.as_bytes());
    std::fs::write(path, protect(&framed, Scope::LocalMachine).unwrap()).unwrap();
}
