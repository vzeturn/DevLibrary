//! The relay wire contract (`internal/egressrelay/wire.go`): the call the hub hands the
//! agent, the answer the agent posts back, and the limits both sides enforce.
//!
//! A relayed request carries the tenant's provider credential in its body, so neither
//! type prints its body in `Debug` output.

use std::fmt;

use dl_encoding::base64::STANDARD;
use dl_json::Value;

use crate::gojson::{self, Obj};

/// `MaxRequestBody`: the largest body the hub hands to an agent.
pub const MAX_REQUEST_BODY: usize = 4 << 20;
/// `MaxResponseBody`: the largest provider answer an agent returns (an invoice PDF fits).
pub const MAX_RESPONSE_BODY: usize = 8 << 20;

/// `egressrelay.Request`: one HTTP call the agent performs for the hub.
#[derive(Clone, Default, PartialEq)]
pub struct RelayRequest {
    /// Hub id of the call; the answer is posted under it.
    pub id: String,
    /// `POST` or `GET` (anything else is refused).
    pub method: String,
    /// The provider URL; it must pass [`crate::relay::allow::check_url`].
    pub url: String,
    /// Request headers as the hub sent them (only a few are forwarded).
    pub header: Vec<(String, String)>,
    /// Request body (base64 on the wire).
    pub body: Vec<u8>,
    /// Requested timeout; `<= 0` means the default, and it is capped.
    pub timeout_ms: i64,
}

impl fmt::Debug for RelayRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RelayRequest")
            .field("id", &self.id)
            .field("method", &self.method)
            .field("url", &self.url)
            .field("header", &self.header)
            .field("body", &format_args!("<{} bytes>", self.body.len()))
            .field("timeout_ms", &self.timeout_ms)
            .finish()
    }
}

fn str_field(v: &Value, k: &str) -> Result<String, String> {
    match v.get(k) {
        None | Some(Value::Null) => Ok(String::new()),
        Some(Value::String(s)) => Ok(s.clone()),
        Some(_) => Err(format!("field {k} is not a string")),
    }
}

impl RelayRequest {
    /// Decodes the `/relay/next` answer like Go's `json.Unmarshal` into the struct: absent
    /// or `null` fields stay zero, a field of the wrong type is an error.
    pub fn from_json(body: &[u8]) -> Result<RelayRequest, String> {
        let v = dl_json::parse_slice(body).map_err(|e| e.to_string())?;
        if !v.is_object() {
            return Err("relay request is not a JSON object".into());
        }
        let header = match v.get("header") {
            None | Some(Value::Null) => Vec::new(),
            Some(Value::Object(m)) => m
                .iter()
                .map(|(k, val)| match val {
                    Value::String(s) => Ok((k.to_string(), s.clone())),
                    _ => Err(format!("header {k} is not a string")),
                })
                .collect::<Result<_, _>>()?,
            Some(_) => return Err("field header is not an object".into()),
        };
        let body = match v.get("body") {
            None | Some(Value::Null) => Vec::new(),
            Some(Value::String(s)) => STANDARD
                .decode(s)
                .map_err(|e| format!("field body is not base64: {e}"))?,
            Some(_) => return Err("field body is not a string".into()),
        };
        let timeout_ms = match v.get("timeout_ms") {
            None | Some(Value::Null) => 0,
            Some(t) => t.as_i64().ok_or("field timeout_ms is not an integer")?,
        };
        Ok(RelayRequest {
            id: str_field(&v, "id")?,
            method: str_field(&v, "method")?,
            url: str_field(&v, "url")?,
            header,
            body,
            timeout_ms,
        })
    }
}

/// `egressrelay.Result`: the answer of one relayed call.
#[derive(Clone, Default, PartialEq)]
pub struct RelayResult {
    /// Provider status code (0 when the call was not made or failed).
    pub status: u16,
    /// The provider's `Content-Type`, when it sent one.
    pub content_type: Option<String>,
    /// Provider answer body.
    pub body: Vec<u8>,
    /// Why the call was refused or failed; empty on an answer.
    pub error: String,
}

impl fmt::Debug for RelayResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RelayResult")
            .field("status", &self.status)
            .field("content_type", &self.content_type)
            .field("body", &format_args!("<{} bytes>", self.body.len()))
            .field("error", &self.error)
            .finish()
    }
}

impl RelayResult {
    /// A result that carries only an error (refused, or the call failed).
    pub fn failed(error: impl Into<String>) -> RelayResult {
        RelayResult {
            error: error.into(),
            ..RelayResult::default()
        }
    }

    /// `json.Marshal(Result)`: `status`, then `header`, `body` and `error` with `omitempty`.
    pub fn to_json(&self) -> String {
        let mut out = String::new();
        let mut o = Obj::new(&mut out);
        o.int("status", i64::from(self.status));
        if let Some(ct) = &self.content_type {
            o.raw("header", |out| {
                out.push('{');
                gojson::string(out, "Content-Type");
                out.push(':');
                gojson::string(out, ct);
                out.push('}');
            });
        }
        o.str_omit("body", &STANDARD.encode(&self.body))
            .str_omit("error", &self.error)
            .end();
        out
    }
}

/// What one `/relay/next` poll returned.
#[derive(Debug, Clone, PartialEq)]
pub enum Poll {
    /// The hold ended with no call waiting (204).
    Empty,
    /// A call to perform.
    Call(RelayRequest),
    /// The hub has no relay route (404/405: a hub older than ADR 0016).
    Unsupported,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_decodes_like_go() {
        let r = RelayRequest::from_json(
            br#"{"id":"r1","method":"POST","url":"https://a.vnpt-invoice.com.vn/x","header":{"SOAPAction":"y"},"body":"PGEvPg==","timeout_ms":5000}"#,
        )
        .unwrap();
        assert_eq!(r.id, "r1");
        assert_eq!(r.header, vec![("SOAPAction".to_owned(), "y".to_owned())]);
        assert_eq!(r.body, b"<a/>");
        assert_eq!(r.timeout_ms, 5000);
        let sparse = RelayRequest::from_json(br#"{"id":"r2","header":null}"#).unwrap();
        assert_eq!((sparse.body.len(), sparse.timeout_ms), (0, 0));
        assert!(RelayRequest::from_json(br#"{"id":7}"#).is_err());
        assert!(RelayRequest::from_json(br#"{"body":"%%"}"#).is_err());
        assert!(RelayRequest::from_json(b"[]").is_err());
    }

    #[test]
    fn result_marshals_like_go() {
        assert_eq!(
            RelayResult::failed("agent từ chối: x").to_json(),
            r#"{"status":0,"error":"agent từ chối: x"}"#
        );
        let ok = RelayResult {
            status: 200,
            content_type: Some("text/xml".into()),
            body: b"<a/>".to_vec(),
            error: String::new(),
        };
        assert_eq!(
            ok.to_json(),
            r#"{"status":200,"header":{"Content-Type":"text/xml"},"body":"PGEvPg=="}"#
        );
    }

    #[test]
    fn debug_never_prints_the_body() {
        let r = RelayRequest {
            body: b"<password>secret</password>".to_vec(),
            ..Default::default()
        };
        assert!(!format!("{r:?}").contains("secret"));
        let res = RelayResult {
            body: b"secret".to_vec(),
            ..Default::default()
        };
        assert!(!format!("{res:?}").contains("secret"));
    }
}
