//! The JOSE header.

use alloc::string::String;

use dl_json::{Error as JsonError, FromJson, Map, ToJson, Value};

use crate::Algorithm;

/// The JOSE header of a signed token (RFC 7515 section 4, RFC 7519 section 5).
///
/// `Header::default()` is `{"alg":"HS256","typ":"JWT"}`, the header every common JWT
/// library writes. Members are serialized in the order `alg`, `typ`, `kid`, `cty`, then
/// `extra`; an `extra` entry with the name of a typed field that is set is skipped, so
/// the output never contains duplicate names.
///
/// ```
/// use dl_jwt::{Algorithm, Header};
/// use dl_json::ToJson;
///
/// let header = Header::new(Algorithm::HS512).key_id("2024-06");
/// assert_eq!(header.to_json().to_string(), r#"{"alg":"HS512","typ":"JWT","kid":"2024-06"}"#);
/// ```
#[derive(Debug, Clone, PartialEq)]
pub struct Header {
    /// The signature algorithm (`alg`).
    pub alg: Algorithm,
    /// The media type of the whole token (`typ`), `"JWT"` by default.
    pub typ: Option<String>,
    /// The ID of the key that signed the token (`kid`), used to pick a key during
    /// rotation. Untrusted until the signature has been checked.
    pub kid: Option<String>,
    /// The content type of the payload (`cty`); only used for nested tokens.
    pub cty: Option<String>,
    /// Every other header parameter, in token order.
    pub extra: Map,
}

impl Header {
    /// A header for `alg` with `typ` set to `"JWT"`.
    pub fn new(alg: Algorithm) -> Header {
        Header {
            alg,
            typ: Some("JWT".into()),
            kid: None,
            cty: None,
            extra: Map::new(),
        }
    }

    /// Sets `kid`.
    pub fn key_id(mut self, kid: impl Into<String>) -> Header {
        self.kid = Some(kid.into());
        self
    }

    /// Sets `cty`.
    pub fn content_type(mut self, cty: impl Into<String>) -> Header {
        self.cty = Some(cty.into());
        self
    }

    /// Adds another header parameter.
    pub fn param(mut self, name: impl Into<String>, value: impl Into<Value>) -> Header {
        self.extra.insert(name, value);
        self
    }
}

impl Default for Header {
    fn default() -> Header {
        Header::new(Algorithm::HS256)
    }
}

impl ToJson for Header {
    fn to_json(&self) -> Value {
        let mut m = Map::with_capacity(4 + self.extra.len());
        m.insert("alg", self.alg.as_str());
        let optional = [("typ", &self.typ), ("kid", &self.kid), ("cty", &self.cty)];
        for (name, value) in optional {
            if let Some(v) = value {
                m.insert(name, v.as_str());
            }
        }
        for (name, value) in &self.extra {
            if !m.contains_key(name) {
                m.insert(name.as_str(), value.clone());
            }
        }
        Value::Object(m)
    }
}

/// Header parsing errors that are not JSON shape errors.
pub(crate) enum HeaderError {
    Json(JsonError),
    Alg(String),
    Crit(String),
}

impl From<JsonError> for HeaderError {
    fn from(e: JsonError) -> Self {
        HeaderError::Json(e)
    }
}

impl Header {
    /// Converts a parsed header, distinguishing unsupported algorithms and critical
    /// extensions from shape errors.
    pub(crate) fn from_value(v: &Value) -> Result<Header, HeaderError> {
        let Some(obj) = v.as_object() else {
            return Err(JsonError::type_mismatch("object", v.type_name()).into());
        };
        let alg: String = v.field("alg")?;
        let alg = alg
            .parse::<Algorithm>()
            .map_err(|_| HeaderError::Alg(alg))?;
        if obj.contains_key("crit") {
            // RFC 7515 section 4.1.11: a non-empty array of names the recipient must
            // understand. This crate implements no extensions, so any entry is fatal.
            let crit: alloc::vec::Vec<String> = v.field("crit")?;
            let first = crit
                .into_iter()
                .next()
                .ok_or_else(|| JsonError::custom("\"crit\" must not be empty"))?;
            return Err(HeaderError::Crit(first));
        }
        let mut extra = Map::new();
        for (name, value) in obj {
            if !matches!(name.as_str(), "alg" | "typ" | "kid" | "cty") {
                extra.insert(name.as_str(), value.clone());
            }
        }
        Ok(Header {
            alg,
            typ: v.field("typ")?,
            kid: v.field("kid")?,
            cty: v.field("cty")?,
            extra,
        })
    }
}

impl FromJson for Header {
    /// Converts a header object. Unsupported algorithms (including `none`) and any
    /// `crit` parameter are reported as conversion errors; [`decode`](crate::decode)
    /// reports them as [`Error::UnsupportedAlgorithm`](crate::Error::UnsupportedAlgorithm)
    /// and [`Error::UnsupportedCriticalHeader`](crate::Error::UnsupportedCriticalHeader).
    fn from_json(v: &Value) -> Result<Header, JsonError> {
        Header::from_value(v).map_err(|e| match e {
            HeaderError::Json(e) => e,
            HeaderError::Alg(alg) => {
                JsonError::custom(alloc::format!("unsupported algorithm {alg:?}"))
            }
            HeaderError::Crit(name) => JsonError::custom(alloc::format!(
                "unsupported critical header parameter {name:?}"
            )),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::ToString;
    use dl_json::json;

    #[test]
    fn serialize() {
        assert_eq!(
            Header::default().to_json().to_string(),
            r#"{"alg":"HS256","typ":"JWT"}"#
        );
        let h = Header {
            typ: None,
            ..Header::new(Algorithm::HS384)
        }
        .content_type("JWT")
        .param("x5t", "abc")
        .param("alg", "none") // ignored: the typed alg wins
        .param("typ", "at+jwt"); // used: typ is None
        assert_eq!(
            h.to_json().to_string(),
            r#"{"alg":"HS384","cty":"JWT","x5t":"abc","typ":"at+jwt"}"#
        );
    }

    #[test]
    fn parse() {
        let v = json!({"typ": "JWT", "alg": "HS256", "kid": "k1", "jku": "https://x"});
        let h = Header::from_json(&v).unwrap();
        assert_eq!(h.alg, Algorithm::HS256);
        assert_eq!(h.typ.as_deref(), Some("JWT"));
        assert_eq!(h.kid.as_deref(), Some("k1"));
        assert_eq!(h.cty, None);
        assert_eq!(h.extra.len(), 1);
        assert_eq!(h.extra["jku"], "https://x");

        let h = Header::from_json(&json!({"alg": "HS512"})).unwrap();
        assert_eq!(h.typ, None);
    }

    #[test]
    fn rejects() {
        for v in [
            json!([]),
            json!({}),
            json!({"alg": 1}),
            json!({"alg": "HS256", "kid": 5}),
            json!({"alg": "HS256", "typ": 1}),
            json!({"alg": "none"}),
            json!({"alg": "RS256"}),
            json!({"alg": "HS256", "crit": ["b64"], "b64": false}),
            json!({"alg": "HS256", "crit": []}),
            json!({"alg": "HS256", "crit": "b64"}),
        ] {
            assert!(Header::from_json(&v).is_err(), "{v}");
        }
        assert!(
            matches!(Header::from_value(&json!({"alg": "none"})), Err(HeaderError::Alg(a)) if a == "none")
        );
        assert!(matches!(
            Header::from_value(&json!({"alg": "HS256", "crit": ["exp"]})),
            Err(HeaderError::Crit(c)) if c == "exp"
        ));
    }
}
