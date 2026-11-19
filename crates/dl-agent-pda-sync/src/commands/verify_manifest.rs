//! The cloud's side of `sync.verify` (`Commands/CloudManifestSource.cs`, ADR 0126/0128):
//! the per-bucket manifest of a table and the row keys of a few buckets, read over the
//! agent's signed transport.

use std::collections::HashMap;
use std::sync::Arc;

use dl_json::Value;

use crate::capture::shadow_manifest::ManifestBucket;
use crate::transport::{Error, SyncHttpClient, SyncResponse, TokenProvider};
use crate::util::ctx::Ctx;

/// `GET /sync/v1/manifest?table=&buckets=`.
pub const MANIFEST_PATH: &str = "/sync/v1/manifest";
/// `GET /sync/v1/manifest/keys?table=&buckets=&bucket=1,2,…`.
pub const KEYS_PATH: &str = "/sync/v1/manifest/keys";

/// The cloud's manifest of one table, or why it is missing: `unreachable`, `http_NNN` (the
/// cloud refused) or `malformed` (the answer could not be read).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CloudManifest {
    /// Non-empty buckets.
    pub buckets: Vec<ManifestBucket>,
    /// Why there is no manifest.
    pub error: Option<String>,
}

impl CloudManifest {
    /// A manifest.
    pub fn of(buckets: Vec<ManifestBucket>) -> CloudManifest {
        CloudManifest {
            buckets,
            error: None,
        }
    }

    /// No manifest, with the reason.
    pub fn failed(error: &str) -> CloudManifest {
        CloudManifest {
            buckets: Vec::new(),
            error: Some(error.to_owned()),
        }
    }
}

/// The cloud's row keys and hashes in some buckets of a table (ADR 0128), or why they are
/// missing (same errors as [`CloudManifest`]). `truncated`: the cloud held more keys than
/// it answers with.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CloudManifestKeys {
    /// Row key to row hash.
    pub rows: HashMap<String, String>,
    /// The cloud cut the answer short.
    pub truncated: bool,
    /// Why there are no keys.
    pub error: Option<String>,
}

impl CloudManifestKeys {
    /// No keys, with the reason.
    pub fn failed(error: &str) -> CloudManifestKeys {
        CloudManifestKeys {
            error: Some(error.to_owned()),
            ..CloudManifestKeys::default()
        }
    }
}

/// Reads the cloud's per-bucket manifest of a table (`sync.verify`, ADR 0126) and the keys
/// of a few buckets (ADR 0128), `ICloudManifestSource`. Only a cancelled `ctx` (or an
/// unexpected failure the C# code lets escape) is an `Err`.
pub trait CloudManifestSource: Send + Sync {
    /// The manifest of `table`.
    fn get(&self, table: &str, buckets: i32, ctx: &Ctx) -> Result<CloudManifest, Error>;

    /// The keys of `table` in `bucket_list`.
    fn get_keys(
        &self,
        table: &str,
        buckets: i32,
        bucket_list: &[i32],
        ctx: &Ctx,
    ) -> Result<CloudManifestKeys, Error>;
}

/// `Uri.EscapeDataString`: everything but the RFC 3986 unreserved characters, as `%XX`.
pub fn escape_data_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// The manifest over the agent's signed transport, with one retry after a 401/403 and a
/// refreshed token, like the site-event transport (`HttpCloudManifestSource`).
pub struct HttpCloudManifestSource {
    http: Arc<SyncHttpClient>,
    tokens: Arc<TokenProvider>,
}

impl std::fmt::Debug for HttpCloudManifestSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpCloudManifestSource")
            .finish_non_exhaustive()
    }
}

impl HttpCloudManifestSource {
    /// A source over `http` and `tokens`.
    pub fn new(http: Arc<SyncHttpClient>, tokens: Arc<TokenProvider>) -> HttpCloudManifestSource {
        HttpCloudManifestSource { http, tokens }
    }

    /// The request path of a manifest.
    pub fn manifest_path(table: &str, buckets: i32) -> String {
        format!(
            "{MANIFEST_PATH}?table={}&buckets={buckets}",
            escape_data_string(table)
        )
    }

    /// The request path of the keys of `bucket_list` (sent ascending).
    pub fn keys_path(table: &str, buckets: i32, bucket_list: &[i32]) -> String {
        let mut list = bucket_list.to_vec();
        list.sort_unstable();
        let list: Vec<String> = list.iter().map(i32::to_string).collect();
        format!(
            "{KEYS_PATH}?table={}&buckets={buckets}&bucket={}",
            escape_data_string(table),
            list.join(",")
        )
    }

    fn send(&self, path: &str, ctx: &Ctx) -> Result<Result<SyncResponse, String>, Error> {
        for attempt in 1..=2 {
            let resp = match self
                .tokens
                .get_access_token(ctx)
                .and_then(|bearer| self.http.get(ctx, path, Some(&bearer)))
            {
                Ok(r) => r,
                Err(Error::Agent(_)) => return Ok(Err("unreachable".to_owned())),
                Err(e) => return Err(e),
            };
            if resp.is_success() {
                return Ok(Ok(resp));
            }
            if matches!(resp.status, 401 | 403) {
                self.tokens.invalidate();
                if attempt == 1 {
                    continue;
                }
            }
            return Ok(Err(format!("http_{}", resp.status)));
        }
        Ok(Err("http_401".to_owned()))
    }
}

impl CloudManifestSource for HttpCloudManifestSource {
    fn get(&self, table: &str, buckets: i32, ctx: &Ctx) -> Result<CloudManifest, Error> {
        Ok(
            match self.send(&Self::manifest_path(table, buckets), ctx)? {
                Ok(resp) => parse(&resp),
                Err(error) => CloudManifest::failed(&error),
            },
        )
    }

    fn get_keys(
        &self,
        table: &str,
        buckets: i32,
        bucket_list: &[i32],
        ctx: &Ctx,
    ) -> Result<CloudManifestKeys, Error> {
        Ok(
            match self.send(&Self::keys_path(table, buckets, bucket_list), ctx)? {
                Ok(resp) => parse_keys(&resp),
                Err(error) => CloudManifestKeys::failed(&error),
            },
        )
    }
}

/// The `data` object of an answer and its array member `name`.
fn data_array(resp: &SyncResponse, name: &str) -> Option<(Value, Vec<Value>)> {
    let data = resp.data().ok().flatten()?;
    let list = match &data {
        Value::Object(_) => crate::transport::data_member(&data, name).ok().flatten()?,
        _ => return None,
    };
    let items = list.as_array()?.clone();
    Some((data, items))
}

fn read_bucket(b: &Value) -> Result<ManifestBucket, Error> {
    use crate::transport::element::{get, int32, string};
    Ok(ManifestBucket {
        bucket: int32(get(b, "bucket")?)?,
        count: int32(get(b, "count")?)?,
        digest: string(get(b, "digest")?)?.ok_or_else(|| Error::Unexpected {
            type_name: "System.FormatException",
            message: "digest is null".to_owned(),
        })?,
    })
}

/// Reads `{ data: { buckets: [{ bucket, count, digest }] } }`; `malformed` when it cannot.
pub fn parse(resp: &SyncResponse) -> CloudManifest {
    let Some((_, items)) = data_array(resp, "buckets") else {
        return CloudManifest::failed("malformed");
    };
    match items.iter().map(read_bucket).collect::<Result<Vec<_>, _>>() {
        Ok(buckets) => CloudManifest::of(buckets),
        Err(_) => CloudManifest::failed("malformed"),
    }
}

fn read_key(k: &Value) -> Result<(String, String), Error> {
    use crate::transport::element::{get, string};
    let null = |what: &str| Error::Unexpected {
        type_name: "System.FormatException",
        message: format!("{what} is null"),
    };
    let key = string(get(k, "key")?)?.ok_or_else(|| null("key"))?;
    let hash = string(get(k, "hash")?)?.ok_or_else(|| null("hash"))?;
    Ok((key, hash))
}

/// Reads `{ data: { keys: [{ key, hash }], truncated } }`; `malformed` when it cannot. A
/// key listed twice keeps the last hash.
pub fn parse_keys(resp: &SyncResponse) -> CloudManifestKeys {
    let Some((data, items)) = data_array(resp, "keys") else {
        return CloudManifestKeys::failed("malformed");
    };
    let mut rows = HashMap::new();
    for k in &items {
        match read_key(k) {
            Ok((key, hash)) => {
                rows.insert(key, hash);
            }
            Err(_) => return CloudManifestKeys::failed("malformed"),
        }
    }
    let truncated = matches!(
        crate::transport::data_member(&data, "truncated"),
        Ok(Some(Value::Bool(true)))
    );
    CloudManifestKeys {
        rows,
        truncated,
        error: None,
    }
}
