//! The JWT claims set (RFC 7519 section 4).

use alloc::string::String;
use alloc::vec::Vec;

use dl_json::{Error as JsonError, FromJson, Map, ToJson, Value};

use crate::{Error, Part};

/// The `aud` claim: one audience or a list (RFC 7519 section 4.1.3).
///
/// Both forms are kept as they were, so a decoded token re-encodes to the same JSON.
///
/// ```
/// use dl_jwt::Audience;
///
/// let aud = Audience::from(vec!["orders-api".to_string(), "billing".to_string()]);
/// assert!(aud.contains("billing"));
/// assert_eq!(aud.iter().collect::<Vec<_>>(), ["orders-api", "billing"]);
/// assert!(Audience::from("orders-api").contains("orders-api"));
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Audience {
    /// A single string: `"aud": "orders-api"`.
    Single(String),
    /// An array of strings: `"aud": ["orders-api", "billing"]`.
    Multiple(Vec<String>),
}

impl Audience {
    /// `true` if `audience` is one of the values.
    pub fn contains(&self, audience: &str) -> bool {
        self.iter().any(|a| a == audience)
    }

    /// Iterates over the values.
    pub fn iter(&self) -> impl Iterator<Item = &str> {
        let items: &[String] = match self {
            Audience::Single(s) => core::slice::from_ref(s),
            Audience::Multiple(v) => v,
        };
        items.iter().map(String::as_str)
    }
}

impl From<&str> for Audience {
    fn from(s: &str) -> Self {
        Audience::Single(s.into())
    }
}

impl From<String> for Audience {
    fn from(s: String) -> Self {
        Audience::Single(s)
    }
}

impl From<Vec<String>> for Audience {
    fn from(v: Vec<String>) -> Self {
        Audience::Multiple(v)
    }
}

impl ToJson for Audience {
    fn to_json(&self) -> Value {
        match self {
            Audience::Single(s) => Value::from(s.as_str()),
            Audience::Multiple(v) => v.to_json(),
        }
    }
}

impl FromJson for Audience {
    fn from_json(v: &Value) -> Result<Self, JsonError> {
        match v {
            Value::String(s) => Ok(Audience::Single(s.clone())),
            Value::Array(_) => Ok(Audience::Multiple(Vec::from_json(v)?)),
            other => Err(JsonError::type_mismatch(
                "string or array of strings",
                other.type_name(),
            )),
        }
    }
}

/// A JWT claims set: the registered claims of RFC 7519 section 4.1 plus any others.
///
/// Times (`exp`, `nbf`, `iat`) are NumericDate values: seconds since the Unix epoch, UTC.
/// Decoding accepts integers and fractional numbers (`1700000000.75`); fractions are
/// truncated to whole seconds, as PyJWT and golang-jwt do. Negative times are rejected.
///
/// Registered claims with the wrong JSON type (for example `"exp": "soon"` or
/// `"iss": null`) make the whole token invalid. Every other member goes to `extra`, in
/// token order.
///
/// Serialization writes `iss`, `sub`, `aud`, `exp`, `nbf`, `iat`, `jti`, then `extra`.
/// An `extra` entry whose name is a registered claim that is set is skipped, so the
/// output never has duplicate names.
///
/// ```
/// use dl_jwt::Claims;
/// use dl_json::ToJson;
///
/// let claims = Claims::new()
///     .issuer("https://auth.example.com")
///     .subject("user-42")
///     .audience("orders-api")
///     .issued_at(1_700_000_000)
///     .expires_at(1_700_000_900)
///     .claim("name", "Nguyễn Văn An")
///     .claim("roles", ["admin", "support"]);
///
/// assert_eq!(claims.get::<Vec<String>>("roles")?, ["admin", "support"]);
/// assert_eq!(claims.get::<u64>("exp")?, 1_700_000_900);
/// assert_eq!(claims.get::<Option<String>>("email")?, None);
/// assert_eq!(
///     claims.to_json().to_string(),
///     r#"{"iss":"https://auth.example.com","sub":"user-42","aud":"orders-api","exp":1700000900,"iat":1700000000,"name":"Nguyễn Văn An","roles":["admin","support"]}"#
/// );
/// # Ok::<(), dl_jwt::Error>(())
/// ```
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Claims {
    /// Issuer (`iss`): who created and signed the token.
    pub iss: Option<String>,
    /// Subject (`sub`): usually the user ID.
    pub sub: Option<String>,
    /// Audience (`aud`): who the token is meant for.
    pub aud: Option<Audience>,
    /// Expiration time (`exp`), Unix seconds. The token is rejected from this second on.
    pub exp: Option<u64>,
    /// Not before (`nbf`), Unix seconds. The token is rejected before this second.
    pub nbf: Option<u64>,
    /// Issued at (`iat`), Unix seconds.
    pub iat: Option<u64>,
    /// JWT ID (`jti`): a unique token ID, for example to deny-list a token.
    pub jti: Option<String>,
    /// Every other claim (private and public claims), in token order.
    pub extra: Map,
}

impl Claims {
    /// An empty claims set.
    pub fn new() -> Claims {
        Claims::default()
    }

    /// Sets `iss`.
    pub fn issuer(mut self, iss: impl Into<String>) -> Claims {
        self.iss = Some(iss.into());
        self
    }

    /// Sets `sub`.
    pub fn subject(mut self, sub: impl Into<String>) -> Claims {
        self.sub = Some(sub.into());
        self
    }

    /// Sets `aud` to a single string.
    pub fn audience(mut self, aud: impl Into<String>) -> Claims {
        self.aud = Some(Audience::Single(aud.into()));
        self
    }

    /// Sets `aud` to an array of strings.
    pub fn audiences<I>(mut self, auds: I) -> Claims
    where
        I: IntoIterator,
        I::Item: Into<String>,
    {
        self.aud = Some(Audience::Multiple(
            auds.into_iter().map(Into::into).collect(),
        ));
        self
    }

    /// Sets `exp` (Unix seconds).
    pub fn expires_at(mut self, exp: u64) -> Claims {
        self.exp = Some(exp);
        self
    }

    /// Sets `nbf` (Unix seconds).
    pub fn not_before(mut self, nbf: u64) -> Claims {
        self.nbf = Some(nbf);
        self
    }

    /// Sets `iat` (Unix seconds).
    pub fn issued_at(mut self, iat: u64) -> Claims {
        self.iat = Some(iat);
        self
    }

    /// Sets `jti`.
    pub fn jwt_id(mut self, jti: impl Into<String>) -> Claims {
        self.jti = Some(jti.into());
        self
    }

    /// Adds a custom claim to `extra`. Registered claim names should be set with the
    /// typed methods instead; if both are set, the typed field wins on serialization.
    pub fn claim(mut self, name: impl Into<String>, value: impl ToJson) -> Claims {
        self.extra.insert(name, value.to_json());
        self
    }

    /// Reads any claim, registered or custom, as `T`.
    ///
    /// Works like [`Value::field`]: a missing claim is
    /// [`Error::MissingClaim`] unless `T` accepts `null` (`Option<T>` gives `None`), and a
    /// claim of the wrong type is [`Error::InvalidJson`] with a path such as `/role/1`.
    pub fn get<T: FromJson>(&self, name: &str) -> Result<T, Error> {
        let value = match name {
            "iss" => self.iss.as_deref().map(Value::from),
            "sub" => self.sub.as_deref().map(Value::from),
            "aud" => self.aud.as_ref().map(ToJson::to_json),
            "exp" => self.exp.map(Value::from),
            "nbf" => self.nbf.map(Value::from),
            "iat" => self.iat.map(Value::from),
            "jti" => self.jti.as_deref().map(Value::from),
            _ => self.extra.get(name).cloned(),
        };
        let Some(value) = value else {
            return T::from_json(&Value::Null).map_err(|_| Error::MissingClaim(name.into()));
        };
        let mut holder = Map::with_capacity(1);
        holder.insert(name, value);
        // Going through `field` puts the claim name into the error path.
        Value::Object(holder)
            .field(name)
            .map_err(|error| Error::InvalidJson {
                part: Part::Payload,
                error,
            })
    }

    /// `true` if the claim is present (registered or custom) and not `null`.
    pub fn contains(&self, name: &str) -> bool {
        match name {
            "iss" => self.iss.is_some(),
            "sub" => self.sub.is_some(),
            "aud" => self.aud.is_some(),
            "exp" => self.exp.is_some(),
            "nbf" => self.nbf.is_some(),
            "iat" => self.iat.is_some(),
            "jti" => self.jti.is_some(),
            _ => self.extra.get(name).is_some_and(|v| !v.is_null()),
        }
    }
}

impl ToJson for Claims {
    fn to_json(&self) -> Value {
        let mut m = Map::with_capacity(7 + self.extra.len());
        if let Some(v) = &self.iss {
            m.insert("iss", v.as_str());
        }
        if let Some(v) = &self.sub {
            m.insert("sub", v.as_str());
        }
        if let Some(v) = &self.aud {
            m.insert("aud", v.to_json());
        }
        for (name, v) in [("exp", self.exp), ("nbf", self.nbf), ("iat", self.iat)] {
            if let Some(v) = v {
                m.insert(name, v);
            }
        }
        if let Some(v) = &self.jti {
            m.insert("jti", v.as_str());
        }
        for (name, value) in &self.extra {
            if !m.contains_key(name) {
                m.insert(name.as_str(), value.clone());
            }
        }
        Value::Object(m)
    }
}

impl FromJson for Claims {
    fn from_json(v: &Value) -> Result<Claims, JsonError> {
        let r = Registered::from_value(v)?;
        let mut extra = Map::new();
        if let Some(obj) = v.as_object() {
            for (name, value) in obj {
                if !is_registered(name) {
                    extra.insert(name.as_str(), value.clone());
                }
            }
        }
        Ok(Claims {
            iss: r.iss.map(String::from),
            sub: r.sub.map(String::from),
            aud: r.aud.map(Audience::from_json).transpose()?,
            exp: r.exp,
            nbf: r.nbf,
            iat: r.iat,
            jti: r.jti.map(String::from),
            extra,
        })
    }
}

fn is_registered(name: &str) -> bool {
    matches!(name, "iss" | "sub" | "aud" | "exp" | "nbf" | "iat" | "jti")
}

/// The registered claims of a payload, borrowed and type-checked. Used for validation
/// so that any claims type `T` gets the same checks.
#[derive(Debug)]
pub(crate) struct Registered<'a> {
    pub(crate) iss: Option<&'a str>,
    pub(crate) sub: Option<&'a str>,
    /// A string or an array of strings.
    pub(crate) aud: Option<&'a Value>,
    pub(crate) exp: Option<u64>,
    pub(crate) nbf: Option<u64>,
    pub(crate) iat: Option<u64>,
    pub(crate) jti: Option<&'a str>,
}

impl<'a> Registered<'a> {
    /// Type-checks the registered claims of a claims object.
    pub(crate) fn from_value(v: &'a Value) -> Result<Registered<'a>, JsonError> {
        let Some(obj) = v.as_object() else {
            return Err(JsonError::type_mismatch("object", v.type_name()));
        };
        let string = |name: &str| -> Result<Option<&'a str>, JsonError> {
            match obj.get(name) {
                None => Ok(None),
                Some(Value::String(s)) => Ok(Some(s.as_str())),
                Some(_) => Err(field_error::<String>(v, name)),
            }
        };
        let date = |name: &str| -> Result<Option<u64>, JsonError> {
            match obj.get(name) {
                None => Ok(None),
                Some(_) => v.field::<NumericDate>(name).map(|d| Some(d.0)),
            }
        };
        let aud = match obj.get("aud") {
            None => None,
            Some(a @ Value::String(_)) => Some(a),
            Some(a @ Value::Array(items)) if items.iter().all(Value::is_string) => Some(a),
            Some(_) => return Err(field_error::<Audience>(v, "aud")),
        };
        Ok(Registered {
            iss: string("iss")?,
            sub: string("sub")?,
            aud,
            exp: date("exp")?,
            nbf: date("nbf")?,
            iat: date("iat")?,
            jti: string("jti")?,
        })
    }

    /// The `aud` values.
    pub(crate) fn audiences(&self) -> impl Iterator<Item = &'a str> {
        let items: &'a [Value] = match self.aud {
            None => &[],
            Some(Value::Array(items)) => items,
            Some(single) => core::slice::from_ref(single),
        };
        items.iter().filter_map(Value::as_str)
    }
}

/// The error `v.field::<T>(name)` reports for a member already known not to convert,
/// which carries the claim name in its path.
fn field_error<T: FromJson>(v: &Value, name: &str) -> JsonError {
    match v.field::<T>(name) {
        Err(e) => e,
        Ok(_) => JsonError::custom(alloc::format!("invalid \"{name}\" claim")),
    }
}

/// A NumericDate (RFC 7519 section 2): non-negative seconds, fractions truncated.
struct NumericDate(u64);

impl FromJson for NumericDate {
    fn from_json(v: &Value) -> Result<Self, JsonError> {
        let Value::Number(n) = v else {
            return Err(JsonError::type_mismatch("NumericDate", v.type_name()));
        };
        if let Some(secs) = n.as_u64() {
            return Ok(NumericDate(secs));
        }
        if n.as_i64().is_some() {
            return Err(JsonError::type_mismatch("NumericDate", "negative number"));
        }
        match n.as_f64() {
            // 2^64 as f64; `as` truncates toward zero, which is floor for these values.
            Some(f) if (0.0..18_446_744_073_709_551_616.0).contains(&f) => {
                Ok(NumericDate(f as u64))
            }
            Some(f) if f < 0.0 => Err(JsonError::type_mismatch("NumericDate", "negative number")),
            _ => Err(JsonError::type_mismatch(
                "NumericDate",
                "out-of-range number",
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::ToString;
    use alloc::vec;
    use dl_json::{ErrorKind, json};

    #[test]
    fn round_trip() {
        let c = Claims::new()
            .issuer("i")
            .subject("s")
            .audiences(["a", "b"])
            .expires_at(10)
            .not_before(5)
            .issued_at(4)
            .jwt_id("j")
            .claim("n", 1)
            .claim("o", json!({"x": [true]}));
        let text = c.to_json().to_string();
        assert_eq!(
            text,
            r#"{"iss":"i","sub":"s","aud":["a","b"],"exp":10,"nbf":5,"iat":4,"jti":"j","n":1,"o":{"x":[true]}}"#
        );
        let back: Claims = dl_json::from_str(&text).unwrap();
        assert_eq!(back, c);
        assert_eq!(Claims::new().to_json().to_string(), "{}");
    }

    #[test]
    fn typed_fields_win_over_extra() {
        let c = Claims::new()
            .expires_at(10)
            .claim("exp", "x")
            .claim("nbf", 3);
        assert_eq!(c.to_json().to_string(), r#"{"exp":10,"nbf":3}"#);
    }

    #[test]
    fn numeric_dates() {
        let c: Claims =
            dl_json::from_str(r#"{"exp": 1700000000.75, "nbf": 1.0e9, "iat": 0}"#).unwrap();
        assert_eq!(
            (c.exp, c.nbf, c.iat),
            (Some(1_700_000_000), Some(1_000_000_000), Some(0))
        );
        let c: Claims = dl_json::from_str(r#"{"exp": 18446744073709551615}"#).unwrap();
        assert_eq!(c.exp, Some(u64::MAX));
        for bad in [
            r#"{"exp": "1700000000"}"#,
            r#"{"exp": null}"#,
            r#"{"exp": -1}"#,
            r#"{"exp": -0.5}"#,
            r#"{"exp": 1e20}"#,
            r#"{"exp": 18446744073709551616}"#,
            r#"{"nbf": true}"#,
            r#"{"iat": [1]}"#,
        ] {
            let err = dl_json::from_str::<Claims>(bad).unwrap_err();
            assert!(err.path().starts_with('/'), "{bad}: {err}");
        }
    }

    #[test]
    fn audience_forms() {
        let c: Claims = dl_json::from_str(r#"{"aud": "x"}"#).unwrap();
        assert_eq!(c.aud, Some(Audience::Single("x".into())));
        let c: Claims = dl_json::from_str(r#"{"aud": ["x", "y"]}"#).unwrap();
        assert_eq!(
            c.aud,
            Some(Audience::Multiple(vec!["x".into(), "y".into()]))
        );
        assert_eq!(c.to_json().to_string(), r#"{"aud":["x","y"]}"#);
        let c: Claims = dl_json::from_str(r#"{"aud": []}"#).unwrap();
        assert_eq!(c.aud, Some(Audience::Multiple(vec![])));
        for bad in [
            r#"{"aud": 1}"#,
            r#"{"aud": ["x", 1]}"#,
            r#"{"aud": null}"#,
            r#"{"aud": {}}"#,
        ] {
            assert!(dl_json::from_str::<Claims>(bad).is_err(), "{bad}");
        }
        let err = dl_json::from_str::<Claims>(r#"{"aud": ["x", 1]}"#).unwrap_err();
        assert_eq!(err.path(), "/aud/1");
    }

    #[test]
    fn string_claims_are_strict() {
        for bad in [
            r#"{"iss": 1}"#,
            r#"{"sub": null}"#,
            r#"{"jti": []}"#,
            "[]",
            "1",
            "null",
        ] {
            assert!(dl_json::from_str::<Claims>(bad).is_err(), "{bad}");
        }
        let err = dl_json::from_str::<Claims>(r#"{"sub": 42}"#).unwrap_err();
        assert_eq!(err.path(), "/sub");
    }

    #[test]
    fn get() {
        let c: Claims = dl_json::from_str(
            r#"{"sub": "7", "aud": ["a"], "exp": 9, "role": ["Admin", "User"], "n": null}"#,
        )
        .unwrap();
        assert_eq!(c.get::<String>("sub").unwrap(), "7");
        assert_eq!(c.get::<Vec<String>>("aud").unwrap(), ["a"]);
        assert_eq!(c.get::<u64>("exp").unwrap(), 9);
        assert_eq!(c.get::<Vec<String>>("role").unwrap(), ["Admin", "User"]);
        assert_eq!(c.get::<Option<String>>("n").unwrap(), None);
        assert_eq!(c.get::<Option<u64>>("nbf").unwrap(), None);
        assert_eq!(
            c.get::<String>("email").unwrap_err(),
            Error::MissingClaim("email".into())
        );
        let Error::InvalidJson { part, error } = c.get::<Vec<u8>>("role").unwrap_err() else {
            panic!("expected InvalidJson");
        };
        assert_eq!(part, Part::Payload);
        assert_eq!(error.path(), "/role/0");
        assert!(matches!(error.kind(), ErrorKind::Type { .. }));
        assert!(c.contains("role") && c.contains("exp") && !c.contains("n") && !c.contains("iss"));
    }

    #[test]
    fn registered_view() {
        let v = json!({"iss": "i", "aud": ["a", "b"], "exp": 3, "x": 1});
        let r = Registered::from_value(&v).unwrap();
        assert_eq!(r.iss, Some("i"));
        assert_eq!(r.audiences().collect::<Vec<_>>(), ["a", "b"]);
        let v = json!({"aud": "only"});
        assert_eq!(
            Registered::from_value(&v)
                .unwrap()
                .audiences()
                .collect::<Vec<_>>(),
            ["only"]
        );
        let v = json!({});
        assert_eq!(Registered::from_value(&v).unwrap().audiences().count(), 0);
    }
}
