//! Hub URLs and the negotiate response (pure, available on every target).
//!
//! These follow the JavaScript client: `negotiate` is appended to the hub path, the
//! hub's query string is kept, and the connection token and access token are appended
//! to the WebSocket URL as `id` and `access_token`.
//!
//! ```
//! use dl_signalr::negotiate::{negotiate_url, websocket_url};
//!
//! assert_eq!(
//!     negotiate_url("https://pos.example.com/hubs/order")?,
//!     "https://pos.example.com/hubs/order/negotiate?negotiateVersion=1"
//! );
//! assert_eq!(
//!     websocket_url("https://pos.example.com/hubs/order", Some("tok+1"), Some("a b"))?,
//!     "wss://pos.example.com/hubs/order?id=tok%2B1&access_token=a%20b"
//! );
//! # Ok::<(), dl_signalr::Error>(())
//! ```

use dl_json::Value;

use crate::Error;

/// The hub URL split into its parts.
struct HubUrl<'a> {
    secure: bool,
    /// Host, port and path, without the scheme, query and fragment.
    authority_and_path: &'a str,
    /// The query without `?`; `None` when empty.
    query: Option<&'a str>,
}

fn split(hub_url: &str) -> Result<HubUrl<'_>, Error> {
    let (scheme, rest) = hub_url
        .split_once("://")
        .ok_or_else(|| Error::InvalidUrl(format!("{hub_url:?} has no scheme")))?;
    let secure = match scheme.to_ascii_lowercase().as_str() {
        "http" | "ws" => false,
        "https" | "wss" => true,
        _ => {
            return Err(Error::InvalidUrl(format!(
                "unsupported scheme {scheme:?} (use http, https, ws or wss)"
            )));
        }
    };
    let rest = rest.split('#').next().unwrap_or_default();
    let (path, query) = match rest.split_once('?') {
        Some((p, q)) => (p, Some(q).filter(|q| !q.is_empty())),
        None => (rest, None),
    };
    if path.is_empty() || path.starts_with('/') {
        return Err(Error::InvalidUrl(format!("{hub_url:?} has no host")));
    }
    let http = if secure { "https" } else { "http" };
    dl_http::Url::parse(&format!("{http}://{rest}"))
        .map_err(|e| Error::InvalidUrl(format!("{hub_url:?}: {e}")))?;
    Ok(HubUrl {
        secure,
        authority_and_path: path,
        query,
    })
}

/// Whether a query string already has the parameter `name`.
fn has_param(query: Option<&str>, name: &str) -> bool {
    query.is_some_and(|q| {
        q.split('&')
            .any(|kv| kv.split('=').next().unwrap_or_default() == name)
    })
}

/// The negotiate URL for a hub URL: `{hub}/negotiate?{hub query&}negotiateVersion=1`.
/// `ws`/`wss` are mapped to `http`/`https`.
///
/// # Errors
/// [`Error::InvalidUrl`] for a URL that is not `http`, `https`, `ws` or `wss`.
pub fn negotiate_url(hub_url: &str) -> Result<String, Error> {
    let u = split(hub_url)?;
    let scheme = if u.secure { "https" } else { "http" };
    let slash = if u.authority_and_path.ends_with('/') {
        ""
    } else {
        "/"
    };
    let mut url = format!("{scheme}://{}{slash}negotiate", u.authority_and_path);
    let mut params: Vec<&str> = u.query.into_iter().collect();
    if !has_param(u.query, "negotiateVersion") {
        params.push("negotiateVersion=1");
    }
    if !params.is_empty() {
        url.push('?');
        url.push_str(&params.join("&"));
    }
    Ok(url)
}

/// The WebSocket URL for a hub URL: the scheme mapped to `ws`/`wss`, the hub's query
/// kept, then `id=<connection token>` and `access_token=<token>` (each URL-encoded)
/// when given.
///
/// # Errors
/// [`Error::InvalidUrl`] for a URL that is not `http`, `https`, `ws` or `wss`.
pub fn websocket_url(
    hub_url: &str,
    connection_token: Option<&str>,
    access_token: Option<&str>,
) -> Result<String, Error> {
    let u = split(hub_url)?;
    let scheme = if u.secure { "wss" } else { "ws" };
    let mut url = format!("{scheme}://{}", u.authority_and_path);
    let mut params: Vec<String> = u.query.map(str::to_owned).into_iter().collect();
    if let Some(id) = connection_token {
        params.push(format!("id={}", dl_http::encode_query_component(id)));
    }
    if let Some(t) = access_token {
        params.push(format!(
            "access_token={}",
            dl_http::encode_query_component(t)
        ));
    }
    if !params.is_empty() {
        url.push('?');
        url.push_str(&params.join("&"));
    }
    Ok(url)
}

/// What the server answered to a negotiate request.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum NegotiateResponse {
    /// Connect the WebSocket with this token.
    Connect {
        /// The connection id (for logging; the token is what the server checks).
        connection_id: Option<String>,
        /// The `id` query parameter for the WebSocket URL: `connectionToken` with
        /// negotiate version 1, `connectionId` with version 0.
        connection_token: String,
    },
    /// Negotiate again at another URL (Azure SignalR Service, for instance).
    Redirect {
        /// The new hub URL.
        url: String,
        /// The access token to use from now on, if the server sent one.
        access_token: Option<String>,
    },
}

fn str_member<'a>(obj: &'a dl_json::Map, key: &str) -> Option<&'a str> {
    obj.get(key).and_then(Value::as_str)
}

/// Parses the body of a `200` negotiate response.
///
/// # Errors
/// [`Error::Json`] for invalid JSON; [`Error::Negotiate`] when the response carries an
/// `error`, does not offer the WebSockets transport with the text format, or lacks a
/// connection token.
pub fn parse_negotiate_response(body: &[u8]) -> Result<NegotiateResponse, Error> {
    let value = dl_json::parse_slice(body)?;
    let Value::Object(obj) = value else {
        return Err(Error::Negotiate("the response is not a JSON object".into()));
    };
    if let Some(e) = str_member(&obj, "error") {
        return Err(Error::Negotiate(e.to_owned()));
    }
    if let Some(url) = str_member(&obj, "url") {
        return Ok(NegotiateResponse::Redirect {
            url: url.to_owned(),
            access_token: str_member(&obj, "accessToken").map(str::to_owned),
        });
    }
    if let Some(transports) = obj.get("availableTransports") {
        let offers_websockets = transports.as_array().is_some_and(|list| {
            list.iter().any(|t| {
                t.get("transport").and_then(Value::as_str) == Some("WebSockets")
                    && t.get("transferFormats")
                        .and_then(Value::as_array)
                        .is_none_or(|f| f.iter().any(|x| x.as_str() == Some("Text")))
            })
        });
        if !offers_websockets {
            return Err(Error::Negotiate(
                "the server does not offer the WebSockets transport".into(),
            ));
        }
    }
    let version = obj
        .get("negotiateVersion")
        .and_then(Value::as_i64)
        .unwrap_or(0);
    let connection_id = str_member(&obj, "connectionId").map(str::to_owned);
    let token = if version >= 1 {
        str_member(&obj, "connectionToken").map(str::to_owned)
    } else {
        connection_id.clone()
    };
    let connection_token =
        token.ok_or_else(|| Error::Negotiate("the response has no connection token".into()))?;
    Ok(NegotiateResponse::Connect {
        connection_id,
        connection_token,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn negotiate_urls() {
        for (hub, want) in [
            (
                "http://localhost:5000/hub",
                "http://localhost:5000/hub/negotiate?negotiateVersion=1",
            ),
            (
                "ws://localhost:5000/hub/",
                "http://localhost:5000/hub/negotiate?negotiateVersion=1",
            ),
            (
                "WSS://h/hubs/order?tenant=7#frag",
                "https://h/hubs/order/negotiate?tenant=7&negotiateVersion=1",
            ),
            (
                "https://x.service.signalr.net/client/?hub=chat&asrs.op=%2Fclient",
                "https://x.service.signalr.net/client/negotiate?hub=chat&asrs.op=%2Fclient&negotiateVersion=1",
            ),
            (
                "https://h/hub?negotiateVersion=1",
                "https://h/hub/negotiate?negotiateVersion=1",
            ),
            ("https://h", "https://h/negotiate?negotiateVersion=1"),
            (
                "https://h/hub?",
                "https://h/hub/negotiate?negotiateVersion=1",
            ),
        ] {
            assert_eq!(negotiate_url(hub).unwrap(), want, "{hub}");
        }
        for bad in [
            "ftp://h/hub",
            "localhost:5000/hub",
            "http:///hub",
            "http://",
        ] {
            assert!(
                matches!(negotiate_url(bad), Err(Error::InvalidUrl(_))),
                "{bad}"
            );
        }
    }

    #[test]
    fn websocket_urls() {
        assert_eq!(
            websocket_url("http://localhost:5000/hub", None, None).unwrap(),
            "ws://localhost:5000/hub"
        );
        assert_eq!(
            websocket_url("https://h/hub?x=1#f", Some("a/b+c=="), Some("e y.J&=")).unwrap(),
            "wss://h/hub?x=1&id=a%2Fb%2Bc%3D%3D&access_token=e%20y.J%26%3D"
        );
        assert_eq!(
            websocket_url("ws://h:81/hub/", None, Some("t")).unwrap(),
            "ws://h:81/hub/?access_token=t"
        );
        assert_eq!(
            websocket_url("wss://h/hub", Some("tok"), None).unwrap(),
            "wss://h/hub?id=tok"
        );
    }

    #[test]
    fn negotiate_responses() {
        // The documented version 1 response (aspnetcore TransportProtocols.md).
        let v1 = br#"{"connectionId":"807809a5-31bf-470d-9e23-afaee35d8a0d","connectionToken":"BpAw6qD-iFzxv4OD6wNkUg","negotiateVersion":1,"availableTransports":[{"transport":"WebSockets","transferFormats":["Text","Binary"]},{"transport":"ServerSentEvents","transferFormats":["Text"]},{"transport":"LongPolling","transferFormats":["Text","Binary"]}]}"#;
        assert_eq!(
            parse_negotiate_response(v1).unwrap(),
            NegotiateResponse::Connect {
                connection_id: Some("807809a5-31bf-470d-9e23-afaee35d8a0d".into()),
                connection_token: "BpAw6qD-iFzxv4OD6wNkUg".into()
            }
        );
        let v0 = br#"{"connectionId":"807809a5","availableTransports":[{"transport":"WebSockets","transferFormats":["Text"]}]}"#;
        assert_eq!(
            parse_negotiate_response(v0).unwrap(),
            NegotiateResponse::Connect {
                connection_id: Some("807809a5".into()),
                connection_token: "807809a5".into()
            }
        );
        let redirect = br#"{"url":"https://myapp.service.signalr.net/client/?hub=chat","accessToken":"eyJhbGciOiJIUzI1NiJ9.x.y"}"#;
        assert_eq!(
            parse_negotiate_response(redirect).unwrap(),
            NegotiateResponse::Redirect {
                url: "https://myapp.service.signalr.net/client/?hub=chat".into(),
                access_token: Some("eyJhbGciOiJIUzI1NiJ9.x.y".into())
            }
        );
        match parse_negotiate_response(br#"{"error":"This connection is not authorized."}"#) {
            Err(Error::Negotiate(e)) => assert_eq!(e, "This connection is not authorized."),
            other => panic!("{other:?}"),
        }
        let no_ws = br#"{"connectionToken":"t","negotiateVersion":1,"availableTransports":[{"transport":"LongPolling","transferFormats":["Text","Binary"]}]}"#;
        assert!(matches!(
            parse_negotiate_response(no_ws),
            Err(Error::Negotiate(_))
        ));
        let binary_only = br#"{"connectionToken":"t","negotiateVersion":1,"availableTransports":[{"transport":"WebSockets","transferFormats":["Binary"]}]}"#;
        assert!(parse_negotiate_response(binary_only).is_err());
        assert!(matches!(
            parse_negotiate_response(br#"{"negotiateVersion":1,"connectionId":"x"}"#),
            Err(Error::Negotiate(_))
        ));
        assert!(matches!(
            parse_negotiate_response(b"[]"),
            Err(Error::Negotiate(_))
        ));
        assert!(matches!(
            parse_negotiate_response(b"<html>"),
            Err(Error::Json(_))
        ));
    }
}
