//! The opening handshake (RFC 6455 section 4): keys, the accept value and checks.

use dl_encoding::base64::STANDARD;
use dl_hash::{Digest, Sha1};

use crate::Error;

/// The GUID appended to the client key (RFC 6455 section 1.3).
const GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";

/// A fresh `Sec-WebSocket-Key`: 16 random bytes from the OS, base64.
///
/// # Errors
/// [`Error::Io`] if the OS random source fails.
pub fn generate_key() -> Result<String, Error> {
    let raw = dl_rand::secure_bytes::<16>()
        .map_err(|e| Error::Io(std::io::Error::other(e.to_string())))?;
    Ok(STANDARD.encode(raw))
}

/// The `Sec-WebSocket-Accept` value for a client key.
///
/// ```
/// assert_eq!(
///     dl_ws::accept_key("dGhlIHNhbXBsZSBub25jZQ=="),
///     "s3pPLMBiTxaQ9kYGzzhZRbK+xOo="
/// );
/// ```
pub fn accept_key(key: &str) -> String {
    let digest = Sha1::default().chain(key.trim()).chain(GUID).finalize();
    STANDARD.encode(digest)
}

/// The first value of a header field (names compare ASCII case-insensitively).
fn get<'a>(headers: &[(&'a str, &'a str)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(n, _)| n.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.trim())
}

/// Whether any value of a comma-separated header field contains `token`.
fn has_token(headers: &[(&str, &str)], name: &str, token: &str) -> bool {
    headers
        .iter()
        .filter(|(n, _)| n.eq_ignore_ascii_case(name))
        .any(|(_, v)| v.split(',').any(|t| t.trim().eq_ignore_ascii_case(token)))
}

/// Checks a server's `101` response fields against the request; returns the selected
/// subprotocol.
#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
pub(crate) fn check_response(
    key: &str,
    offered_protocols: &[String],
    headers: &[(&str, &str)],
) -> Result<Option<String>, Error> {
    if !has_token(headers, "upgrade", "websocket") {
        return Err(Error::Handshake("101 response without Upgrade: websocket"));
    }
    if !has_token(headers, "connection", "upgrade") {
        return Err(Error::Handshake("101 response without Connection: Upgrade"));
    }
    match get(headers, "sec-websocket-accept") {
        Some(v) if dl_hash::ct_eq(v.as_bytes(), accept_key(key).as_bytes()) => {}
        Some(_) => return Err(Error::Handshake("wrong Sec-WebSocket-Accept")),
        None => return Err(Error::Handshake("missing Sec-WebSocket-Accept")),
    }
    if headers
        .iter()
        .any(|(n, v)| n.eq_ignore_ascii_case("sec-websocket-extensions") && !v.trim().is_empty())
    {
        return Err(Error::Handshake(
            "server selected an extension that was not offered",
        ));
    }
    match get(headers, "sec-websocket-protocol") {
        None | Some("") => Ok(None),
        Some(p) if offered_protocols.iter().any(|o| o == p) => Ok(Some(p.to_owned())),
        Some(_) => Err(Error::Handshake(
            "server selected a subprotocol that was not offered",
        )),
    }
}

/// Checks a client's upgrade request (server side) and returns the
/// `Sec-WebSocket-Accept` value for the `101` response.
///
/// `headers` are the request header fields as `(name, value)` pairs in any case.
///
/// ```
/// let headers = [
///     ("Upgrade", "websocket"),
///     ("Connection", "Upgrade"),
///     ("Sec-WebSocket-Key", "dGhlIHNhbXBsZSBub25jZQ=="),
///     ("Sec-WebSocket-Version", "13"),
/// ];
/// assert_eq!(dl_ws::check_request("GET", &headers)?, "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=");
/// # Ok::<(), dl_ws::Error>(())
/// ```
///
/// # Errors
/// [`Error::Handshake`] when the request is not a valid version 13 WebSocket upgrade.
pub fn check_request(method: &str, headers: &[(&str, &str)]) -> Result<String, Error> {
    if method != "GET" {
        return Err(Error::Handshake("upgrade request must use GET"));
    }
    if !has_token(headers, "upgrade", "websocket") {
        return Err(Error::Handshake("missing Upgrade: websocket"));
    }
    if !has_token(headers, "connection", "upgrade") {
        return Err(Error::Handshake("missing Connection: Upgrade"));
    }
    if get(headers, "sec-websocket-version") != Some("13") {
        return Err(Error::Handshake("Sec-WebSocket-Version must be 13"));
    }
    let key =
        get(headers, "sec-websocket-key").ok_or(Error::Handshake("missing Sec-WebSocket-Key"))?;
    match STANDARD.decode(key) {
        Ok(raw) if raw.len() == 16 => Ok(accept_key(key)),
        _ => Err(Error::Handshake(
            "Sec-WebSocket-Key is not 16 bytes of base64",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &str = "dGhlIHNhbXBsZSBub25jZQ==";
    const ACCEPT: &str = "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=";

    #[test]
    fn rfc6455_accept_example() {
        assert_eq!(accept_key(KEY), ACCEPT);
        assert_eq!(accept_key(" dGhlIHNhbXBsZSBub25jZQ== "), ACCEPT);
    }

    #[test]
    fn keys_are_random_16_bytes() {
        let a = generate_key().unwrap();
        let b = generate_key().unwrap();
        assert_ne!(a, b);
        assert_eq!(STANDARD.decode(&a).unwrap().len(), 16);
    }

    #[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
    #[test]
    fn response_checks() {
        let ok = [
            ("Upgrade", "WebSocket"),
            ("Connection", "keep-alive, Upgrade"),
            ("Sec-WebSocket-Accept", ACCEPT),
        ];
        assert_eq!(check_response(KEY, &[], &ok).unwrap(), None);

        let with_proto = [
            ("upgrade", "websocket"),
            ("connection", "Upgrade"),
            ("sec-websocket-accept", ACCEPT),
            ("Sec-WebSocket-Protocol", "chat"),
        ];
        let offered = ["superchat".to_owned(), "chat".to_owned()];
        assert_eq!(
            check_response(KEY, &offered, &with_proto)
                .unwrap()
                .as_deref(),
            Some("chat")
        );
        assert!(check_response(KEY, &[], &with_proto).is_err());

        let wrong = [
            ("Upgrade", "websocket"),
            ("Connection", "Upgrade"),
            ("Sec-WebSocket-Accept", "AAAAAAAAAAAAAAAAAAAAAAAAAAA="),
        ];
        assert!(matches!(
            check_response(KEY, &[], &wrong),
            Err(Error::Handshake("wrong Sec-WebSocket-Accept"))
        ));
        assert!(check_response(KEY, &[], &[("Connection", "Upgrade")]).is_err());
        let ext = [
            ("Upgrade", "websocket"),
            ("Connection", "Upgrade"),
            ("Sec-WebSocket-Accept", ACCEPT),
            ("Sec-WebSocket-Extensions", "permessage-deflate"),
        ];
        assert!(check_response(KEY, &[], &ext).is_err());
    }

    #[test]
    fn request_checks() {
        let ok = [
            ("Host", "server.example.com"),
            ("Upgrade", "websocket"),
            ("Connection", "Upgrade"),
            ("Sec-WebSocket-Key", KEY),
            ("Sec-WebSocket-Version", "13"),
        ];
        assert_eq!(check_request("GET", &ok).unwrap(), ACCEPT);
        assert!(check_request("POST", &ok).is_err());
        let v8 = [
            ("Upgrade", "websocket"),
            ("Connection", "Upgrade"),
            ("Sec-WebSocket-Key", KEY),
            ("Sec-WebSocket-Version", "8"),
        ];
        assert!(check_request("GET", &v8).is_err());
        let short_key = [
            ("Upgrade", "websocket"),
            ("Connection", "Upgrade"),
            ("Sec-WebSocket-Key", "c2hvcnQ="),
            ("Sec-WebSocket-Version", "13"),
        ];
        assert!(check_request("GET", &short_key).is_err());
        assert!(check_request("GET", &[]).is_err());
    }
}
