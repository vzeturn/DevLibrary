//! The `cashier` master entity (port of `Staff/CashierMaster.cs`, ADR 0150): one POS
//! clerk across `dbo.CashierDef`, `dbo.CashierRVC` and the mapped `dbo.ClerkPermission`
//! rows, written back from the cloud with `master.upsert` on a `master_authority=cloud`
//! tenant. Byte for byte the .NET contract, so the agent and the cloud compute the same
//! row hash and open the same sealed PIN.

use super::StrMap;
use super::hashing::ordinal_cmp;
use super::staff::KEY_BYTES;
use super::utf16_len;
use super::wire::{StaffClerk, staff_function_ids, staff_sync_limits};
use crate::security::aes_gcm;
use crate::util::stj::eq_ignore_case;
use dl_hash::{Digest, Hmac, Sha256};

/// Wire entity name.
pub const ENTITY: &str = "cashier";
/// `CashierDef.Initial` is `varchar(10)`.
pub const MAX_KEY_LENGTH: usize = 10;
/// `CashierDef.FullName` is `varchar(50)` on every known build.
pub const MAX_NAME_LENGTH: usize = 50;
/// Highest `Security` level.
pub const MAX_SECURITY: i32 = 99;

/// Wire field names of `MasterUpsertPayload.fields` for this entity.
pub mod fields {
    /// Display name.
    pub const NAME: &str = "name";
    /// `"1"` or `"0"`.
    pub const ACTIVE: &str = "active";
    /// Security level.
    pub const SECURITY: &str = "security";
    /// Comma-separated RVC numbers.
    pub const RVCS: &str = "rvcs";
    /// Comma-separated mapped function ids.
    pub const FUNCTIONS: &str = "functions";
    /// The sealed new PIN.
    pub const PIN_SEALED: &str = "pin_sealed";
    /// Id of the PIN key the PIN was sealed under.
    pub const PIN_KEY_ID: &str = "pin_key_id";
    /// Every field, in contract order.
    pub const ALL: &[&str] = &[
        NAME, ACTIVE, SECURITY, RVCS, FUNCTIONS, PIN_SEALED, PIN_KEY_ID,
    ];
}

/// True for a clerk key the cloud may write (`IsWritableKey`): 1 to 10 printable ASCII
/// characters, no edge spaces, never `SML`.
pub fn is_writable_key(key: &str) -> bool {
    let n = utf16_len(key);
    (1..=MAX_KEY_LENGTH).contains(&n)
        && key.trim() == key
        && key.chars().all(|c| ('!'..='~').contains(&c) || c == ' ')
        && !key.eq_ignore_ascii_case("SML")
}

/// The mapped function ids among `functions` in contract spelling, distinct, ordinal
/// order (`CashierRowHash.Canonical`); unmapped values are dropped.
pub fn canonical<'a>(functions: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    let mut out: Vec<String> = functions
        .into_iter()
        .filter_map(|f| {
            let f = f.trim();
            staff_function_ids::MAPPED
                .iter()
                .find(|m| eq_ignore_case(m, f))
                .map(|m| (*m).to_owned())
        })
        .collect();
    out.sort_by(|a, b| ordinal_cmp(a, b));
    out.dedup();
    out
}

fn sorted_rvcs(rvcs: &[i32]) -> String {
    let mut r = rvcs.to_vec();
    r.sort_unstable();
    r.dedup();
    r.iter().map(i32::to_string).collect::<Vec<_>>().join(",")
}

/// The version of one clerk (`CashierRowHash.Of`): SHA-256, lowercase hex, over a
/// length-prefixed canonical text of its staff-sync projection. The PIN takes part only as
/// its lookup, so a POS-side PIN change is a conflict.
pub fn row_hash(clerk: &StaffClerk) -> String {
    let mut s = String::new();
    let mut append = |v: &str| {
        s.push_str(&v.len().to_string());
        s.push(':');
        s.push_str(v);
        s.push(';');
    };
    append("cashier/v1");
    append(clerk.key.trim());
    append(clerk.name.trim());
    append(if clerk.active { "1" } else { "0" });
    append(&clerk.pin_status);
    append(clerk.pin_lookup.as_deref().unwrap_or(""));
    append(&clerk.security.map(|v| v.to_string()).unwrap_or_default());
    append(&sorted_rvcs(&clerk.rvcs));
    append(&canonical(clerk.functions.iter().map(String::as_str)).join(","));
    dl_encoding::hex::encode(Sha256::digest(s.as_bytes()))
}

/// A clear PIN opened from a seal: never logged (`Debug` hides it), its buffer zeroed when
/// dropped.
pub struct ClearPin(String);

impl ClearPin {
    /// The PIN text.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for ClearPin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ClearPin(***)")
    }
}

impl Drop for ClearPin {
    fn drop(&mut self) {
        zero(std::mem::take(&mut self.0).into_bytes());
    }
}

/// Zeroes a buffer the optimizer may not elide.
fn zero(mut bytes: Vec<u8>) {
    bytes.fill(0);
    std::hint::black_box(&bytes);
}

const NONCE_BYTES: usize = 12;
const TAG_BYTES: usize = aes_gcm::TAG_LEN;
const SEAL_LABEL: &[u8] = b"cashier-pin-seal|v1";

fn seal_key(pin_key: &[u8]) -> Option<[u8; 32]> {
    (pin_key.len() == KEY_BYTES).then(|| Hmac::<Sha256>::mac(pin_key, SEAL_LABEL))
}

fn aad(clerk_key: &str) -> Vec<u8> {
    format!("cashier|{clerk_key}").into_bytes()
}

/// `CashierPinSeal.Open`: the PIN, or `None` when `sealed` is malformed, was sealed under
/// another key or for another clerk, or `pin_key` is not 32 bytes.
pub fn open_pin(pin_key: &[u8], clerk_key: &str, sealed: Option<&str>) -> Option<ClearPin> {
    let sealed = sealed.filter(|s| !s.is_empty())?;
    let data = dl_encoding::base64::STANDARD.decode(sealed).ok()?;
    if data.len() <= NONCE_BYTES + TAG_BYTES || data.len() > NONCE_BYTES + TAG_BYTES + 64 {
        return None;
    }
    let mut key = seal_key(pin_key)?;
    let opened = aes_gcm::open(
        &key,
        &data[..NONCE_BYTES],
        &aad(clerk_key),
        &data[NONCE_BYTES..],
    );
    key.fill(0);
    std::hint::black_box(&key);
    let plain = opened.ok()?;
    // C# Encoding.UTF8.GetString: invalid sequences become U+FFFD (then not a usable PIN).
    let pin = String::from_utf8_lossy(&plain).into_owned();
    zero(plain);
    Some(ClearPin(pin))
}

/// `CashierPinSeal.Seal` with a caller-chosen nonce: the cloud seals, the agent only opens;
/// this exists for tests and fixtures. `None` when `pin_key` is not 32 bytes.
pub fn seal_pin(pin_key: &[u8], clerk_key: &str, pin: &str, nonce: [u8; 12]) -> Option<String> {
    let mut key = seal_key(pin_key)?;
    let sealed = aes_gcm::seal(&key, &nonce, &aad(clerk_key), pin.as_bytes()).ok();
    key.fill(0);
    std::hint::black_box(&key);
    let mut out = nonce.to_vec();
    out.extend_from_slice(&sealed?);
    Some(dl_encoding::base64::STANDARD.encode(&out))
}

/// One cashier write as the agent applies it (`CashierWrite`), parsed from and rendered to
/// the string fields of `MasterUpsertPayload`. `functions` are the mapped ids to allow;
/// `pin_sealed` is `None` when the PIN does not change.
#[derive(Clone, PartialEq, Eq)]
pub struct CashierWrite {
    /// Trimmed display name.
    pub name: String,
    /// Active.
    pub active: bool,
    /// Security level 0..=99.
    pub security: i32,
    /// Distinct, ascending RVC numbers.
    pub rvcs: Vec<i32>,
    /// Canonical mapped function ids.
    pub functions: Vec<String>,
    /// The sealed new PIN (a secret: never logged).
    pub pin_sealed: Option<String>,
    /// Id of the PIN key of `pin_sealed`.
    pub pin_key_id: Option<String>,
}

impl std::fmt::Debug for CashierWrite {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CashierWrite")
            .field("pin", &self.pin_sealed.is_some())
            .finish_non_exhaustive()
    }
}

fn digits(s: &str) -> Option<i32> {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse::<i32>().ok()
}

impl CashierWrite {
    /// `ToFields`.
    pub fn to_fields(&self) -> StrMap {
        let mut m = StrMap::new();
        m.insert(fields::NAME, Some(self.name.clone()));
        m.insert(
            fields::ACTIVE,
            Some(if self.active { "1" } else { "0" }.to_owned()),
        );
        m.insert(fields::SECURITY, Some(self.security.to_string()));
        m.insert(fields::RVCS, Some(sorted_rvcs(&self.rvcs)));
        m.insert(
            fields::FUNCTIONS,
            Some(canonical(self.functions.iter().map(String::as_str)).join(",")),
        );
        if let Some(sealed) = &self.pin_sealed {
            m.insert(fields::PIN_SEALED, Some(sealed.clone()));
            m.insert(fields::PIN_KEY_ID, self.pin_key_id.clone());
        }
        m
    }

    /// `Parse`: the write, or the problem. No field value is ever echoed (names are
    /// personal data, the sealed PIN a secret).
    pub fn parse(f: &StrMap) -> Result<CashierWrite, String> {
        if let Some(unknown) = f.keys().find(|k| !fields::ALL.contains(k)) {
            return Err(format!("field not allowed: {unknown}"));
        }
        let get = |name: &str| f.get(name).and_then(Option::as_deref);

        let name = get(fields::NAME).map(str::trim).unwrap_or("");
        if name.is_empty()
            || utf16_len(name) > MAX_NAME_LENGTH
            || name.chars().any(char::is_control)
        {
            return Err(format!("name must be 1 to {MAX_NAME_LENGTH} characters"));
        }
        let active = match get(fields::ACTIVE) {
            Some("1") => true,
            Some("0") => false,
            _ => return Err("active must be 1 or 0".to_owned()),
        };
        let security = match get(fields::SECURITY).and_then(digits) {
            Some(s) if s <= MAX_SECURITY => s,
            _ => return Err(format!("security must be an integer 0 to {MAX_SECURITY}")),
        };

        let mut rvcs: Vec<i32> = Vec::new();
        for part in get(fields::RVCS)
            .unwrap_or("")
            .split(',')
            .filter(|p| !p.is_empty())
        {
            match digits(part) {
                Some(r) if r > 0 => {
                    if !rvcs.contains(&r) {
                        rvcs.push(r);
                    }
                }
                _ => return Err("rvcs must be positive integers".to_owned()),
            }
        }
        if rvcs.len() > staff_sync_limits::MAX_RVCS {
            return Err("too many rvcs".to_owned());
        }

        let parts: Vec<&str> = get(fields::FUNCTIONS)
            .unwrap_or("")
            .split(',')
            .filter(|p| !p.is_empty())
            .collect();
        let functions = canonical(parts.iter().copied());
        let mut distinct: Vec<&str> = Vec::new();
        for p in &parts {
            if !distinct.iter().any(|d| eq_ignore_case(d, p)) {
                distinct.push(p);
            }
        }
        if functions.len() != distinct.len() {
            return Err("functions must be mapped function ids".to_owned());
        }

        let pin_sealed = get(fields::PIN_SEALED).map(str::to_owned);
        let pin_key_id = get(fields::PIN_KEY_ID).map(str::to_owned);
        if pin_sealed.is_some() != pin_key_id.is_some() {
            return Err("pin_sealed and pin_key_id go together".to_owned());
        }
        rvcs.sort_unstable();
        Ok(CashierWrite {
            name: name.to_owned(),
            active,
            security,
            rvcs,
            functions,
            pin_sealed,
            pin_key_id,
        })
    }
}
