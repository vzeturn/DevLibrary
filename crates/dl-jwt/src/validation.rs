//! Claim validation rules.

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use dl_json::{Map, Value};

use crate::claims::Registered;
use crate::{Algorithm, Error};

/// Default for [`Validation::leeway_secs`]: 60 seconds.
pub const DEFAULT_LEEWAY_SECS: u64 = 60;

/// Default for [`Validation::max_token_len`]: 64 KiB. Also the limit used by
/// [`decode_header`](crate::decode_header) and [`decode_unverified`](crate::decode_unverified).
pub const DEFAULT_MAX_TOKEN_LEN: usize = 64 * 1024;

/// What [`decode`](crate::decode) accepts.
///
/// There is no `Default`: the accepted algorithms must always be chosen by the
/// application, never taken from the token (RFC 8725 section 3.1). Start from
/// [`Validation::new`] and override fields with struct update syntax:
///
/// ```
/// use dl_jwt::{Algorithm, Validation};
///
/// let validation = Validation {
///     issuer: Some(vec!["https://auth.example.com".into()]),
///     audience: Some(vec!["orders-api".into()]),
///     leeway_secs: 30,
///     ..Validation::new(Algorithm::HS256)
/// };
/// assert_eq!(validation.required_claims, ["exp"]);
/// ```
///
/// Time checks, with `now` and `leeway = leeway_secs` in Unix seconds:
///
/// | Claim | Rejected when | Error |
/// |-------|---------------|-------|
/// | `exp` | `now >= exp + leeway` | [`Error::Expired`] |
/// | `nbf` | `now + leeway < nbf` | [`Error::NotYetValid`] |
/// | `iat` with `max_age` | `now - iat > max_age + leeway` | [`Error::TooOld`] |
/// | `iat` with `max_age` | `iat > now + leeway` | [`Error::NotYetValid`] |
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Validation {
    /// Algorithms accepted in the `alg` header. Any other algorithm is
    /// [`Error::AlgorithmNotAllowed`]; an empty list rejects every token. `none` can
    /// never be accepted.
    pub algorithms: Vec<Algorithm>,
    /// Clock skew tolerated in the time checks, in seconds. Default 60.
    pub leeway_secs: u64,
    /// Check `exp` when present. Default `true`. Whether `exp` must be present is
    /// controlled by `required_claims`.
    pub validate_exp: bool,
    /// Check `nbf` when present. Default `true`.
    pub validate_nbf: bool,
    /// When `audience` is `None`, reject tokens that carry an `aud` claim, as RFC 7519
    /// section 4.1.3 requires. Default `true`. Set to `false` to accept any audience.
    pub validate_aud: bool,
    /// Claims that must be present and not `null`. Default `["exp"]`. Custom claim
    /// names work too.
    pub required_claims: Vec<String>,
    /// Accepted issuers. When set, `iss` is required and must equal one of them.
    pub issuer: Option<Vec<String>>,
    /// Accepted audiences. When set, `aud` is required and at least one of its values
    /// must equal one of them.
    pub audience: Option<Vec<String>>,
    /// Required subject. When set, `sub` is required and must equal it.
    pub subject: Option<String>,
    /// Maximum token age in seconds, measured from `iat`. When set, `iat` is required.
    pub max_age: Option<u64>,
    /// Longest token accepted, in bytes; checked before any decoding. Default 64 KiB.
    pub max_token_len: usize,
}

impl Validation {
    /// Accepts tokens signed with `alg` that carry a valid `exp`, with 60 seconds of
    /// leeway. Add more algorithms by pushing to [`algorithms`](Self::algorithms).
    pub fn new(alg: Algorithm) -> Validation {
        Validation {
            algorithms: vec![alg],
            leeway_secs: DEFAULT_LEEWAY_SECS,
            validate_exp: true,
            validate_nbf: true,
            validate_aud: true,
            required_claims: vec!["exp".into()],
            issuer: None,
            audience: None,
            subject: None,
            max_age: None,
            max_token_len: DEFAULT_MAX_TOKEN_LEN,
        }
    }

    /// Runs the claim checks on a verified payload.
    pub(crate) fn check(&self, claims: &Registered<'_>, obj: &Map, now: u64) -> Result<(), Error> {
        for name in &self.required_claims {
            if obj.get(name).is_none_or(Value::is_null) {
                return Err(Error::MissingClaim(name.clone()));
            }
        }

        // i128 cannot overflow for u64 inputs.
        let now = i128::from(now);
        let leeway = i128::from(self.leeway_secs);
        if self.validate_exp
            && let Some(exp) = claims.exp
            && now >= i128::from(exp) + leeway
        {
            return Err(Error::Expired);
        }
        if self.validate_nbf
            && let Some(nbf) = claims.nbf
            && now + leeway < i128::from(nbf)
        {
            return Err(Error::NotYetValid);
        }
        if let Some(max_age) = self.max_age {
            let iat = i128::from(
                claims
                    .iat
                    .ok_or_else(|| Error::MissingClaim("iat".into()))?,
            );
            if iat > now + leeway {
                return Err(Error::NotYetValid);
            }
            if now - iat > i128::from(max_age) + leeway {
                return Err(Error::TooOld);
            }
        }

        if let Some(issuers) = &self.issuer {
            let iss = claims
                .iss
                .ok_or_else(|| Error::MissingClaim("iss".into()))?;
            if !issuers.iter().any(|i| i == iss) {
                return Err(Error::InvalidIssuer);
            }
        }
        match &self.audience {
            Some(accepted) => {
                if claims.aud.is_none() {
                    return Err(Error::MissingClaim("aud".into()));
                }
                if !claims.audiences().any(|a| accepted.iter().any(|x| x == a)) {
                    return Err(Error::InvalidAudience);
                }
            }
            None if self.validate_aud && claims.aud.is_some() => {
                return Err(Error::InvalidAudience);
            }
            None => {}
        }
        if let Some(subject) = &self.subject {
            let sub = claims
                .sub
                .ok_or_else(|| Error::MissingClaim("sub".into()))?;
            if sub != subject {
                return Err(Error::InvalidSubject);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dl_json::json;

    fn check(v: &Validation, payload: Value, now: u64) -> Result<(), Error> {
        let r = Registered::from_value(&payload).unwrap();
        v.check(&r, payload.as_object().unwrap(), now)
    }

    fn base() -> Validation {
        Validation::new(Algorithm::HS256)
    }

    #[test]
    fn exp_edges() {
        let v = Validation {
            leeway_secs: 0,
            ..base()
        };
        let p = json!({"exp": 1000});
        assert_eq!(check(&v, p.clone(), 999), Ok(()));
        assert_eq!(check(&v, p.clone(), 1000), Err(Error::Expired));
        let v = base(); // 60 s leeway
        assert_eq!(check(&v, p.clone(), 1059), Ok(()));
        assert_eq!(check(&v, p.clone(), 1060), Err(Error::Expired));
        let v = Validation {
            validate_exp: false,
            ..base()
        };
        assert_eq!(check(&v, p, u64::MAX), Ok(()));
        // Extreme values do not overflow.
        let v = Validation {
            leeway_secs: u64::MAX,
            ..base()
        };
        assert_eq!(check(&v, json!({"exp": u64::MAX}), u64::MAX), Ok(()));
        assert_eq!(check(&base(), json!({"exp": 0}), 0), Ok(()));
        assert_eq!(check(&base(), json!({"exp": 0}), 60), Err(Error::Expired));
    }

    #[test]
    fn nbf_edges() {
        let v = Validation {
            leeway_secs: 0,
            required_claims: vec![],
            ..base()
        };
        let p = json!({"nbf": 1000});
        assert_eq!(check(&v, p.clone(), 999), Err(Error::NotYetValid));
        assert_eq!(check(&v, p.clone(), 1000), Ok(()));
        let v = Validation {
            required_claims: vec![],
            ..base()
        };
        assert_eq!(check(&v, p.clone(), 940), Ok(()));
        assert_eq!(check(&v, p.clone(), 939), Err(Error::NotYetValid));
        let v = Validation {
            validate_nbf: false,
            required_claims: vec![],
            ..base()
        };
        assert_eq!(check(&v, p, 0), Ok(()));
    }

    #[test]
    fn required_claims() {
        assert_eq!(
            check(&base(), json!({}), 0),
            Err(Error::MissingClaim("exp".into()))
        );
        let v = Validation {
            required_claims: vec!["exp".into(), "tenant".into()],
            ..base()
        };
        assert_eq!(
            check(&v, json!({"exp": 9, "tenant": null}), 0),
            Err(Error::MissingClaim("tenant".into()))
        );
        assert_eq!(check(&v, json!({"exp": 9, "tenant": "t1"}), 0), Ok(()));
        let v = Validation {
            required_claims: vec![],
            ..base()
        };
        assert_eq!(check(&v, json!({}), 0), Ok(()));
    }

    #[test]
    fn max_age() {
        let v = Validation {
            max_age: Some(300),
            leeway_secs: 0,
            required_claims: vec![],
            ..base()
        };
        assert_eq!(
            check(&v, json!({}), 1000),
            Err(Error::MissingClaim("iat".into()))
        );
        assert_eq!(check(&v, json!({"iat": 700}), 1000), Ok(()));
        assert_eq!(check(&v, json!({"iat": 699}), 1000), Err(Error::TooOld));
        assert_eq!(check(&v, json!({"iat": 1000}), 1000), Ok(()));
        assert_eq!(
            check(&v, json!({"iat": 1001}), 1000),
            Err(Error::NotYetValid)
        );
        let v = Validation {
            max_age: Some(300),
            required_claims: vec![],
            ..base()
        };
        assert_eq!(check(&v, json!({"iat": 640}), 1000), Ok(()));
        assert_eq!(check(&v, json!({"iat": 639}), 1000), Err(Error::TooOld));
        assert_eq!(check(&v, json!({"iat": 1060}), 1000), Ok(()));
        assert_eq!(
            check(&v, json!({"iat": 1061}), 1000),
            Err(Error::NotYetValid)
        );
    }

    #[test]
    fn issuer() {
        let v = Validation {
            issuer: Some(vec!["a".into(), "b".into()]),
            required_claims: vec![],
            ..base()
        };
        assert_eq!(check(&v, json!({"iss": "b"}), 0), Ok(()));
        assert_eq!(check(&v, json!({"iss": "c"}), 0), Err(Error::InvalidIssuer));
        assert_eq!(check(&v, json!({"iss": "A"}), 0), Err(Error::InvalidIssuer));
        assert_eq!(
            check(&v, json!({}), 0),
            Err(Error::MissingClaim("iss".into()))
        );
        let v = Validation {
            issuer: Some(vec![]),
            required_claims: vec![],
            ..base()
        };
        assert_eq!(check(&v, json!({"iss": "a"}), 0), Err(Error::InvalidIssuer));
    }

    #[test]
    fn audience() {
        let v = Validation {
            audience: Some(vec!["api".into(), "web".into()]),
            required_claims: vec![],
            ..base()
        };
        assert_eq!(check(&v, json!({"aud": "api"}), 0), Ok(()));
        assert_eq!(check(&v, json!({"aud": ["x", "web"]}), 0), Ok(()));
        assert_eq!(
            check(&v, json!({"aud": ["x", "y"]}), 0),
            Err(Error::InvalidAudience)
        );
        assert_eq!(
            check(&v, json!({"aud": []}), 0),
            Err(Error::InvalidAudience)
        );
        assert_eq!(
            check(&v, json!({"aud": "API"}), 0),
            Err(Error::InvalidAudience)
        );
        assert_eq!(
            check(&v, json!({}), 0),
            Err(Error::MissingClaim("aud".into()))
        );

        // No audience configured: a token addressed to someone is rejected by default.
        let v = Validation {
            required_claims: vec![],
            ..base()
        };
        assert_eq!(
            check(&v, json!({"aud": "api"}), 0),
            Err(Error::InvalidAudience)
        );
        assert_eq!(check(&v, json!({}), 0), Ok(()));
        let v = Validation {
            validate_aud: false,
            required_claims: vec![],
            ..base()
        };
        assert_eq!(check(&v, json!({"aud": "api"}), 0), Ok(()));
    }

    #[test]
    fn subject() {
        let v = Validation {
            subject: Some("42".into()),
            required_claims: vec![],
            ..base()
        };
        assert_eq!(check(&v, json!({"sub": "42"}), 0), Ok(()));
        assert_eq!(
            check(&v, json!({"sub": "43"}), 0),
            Err(Error::InvalidSubject)
        );
        assert_eq!(
            check(&v, json!({}), 0),
            Err(Error::MissingClaim("sub".into()))
        );
    }
}
