//! `LegalVaultOps`: signed legal documents the hotel must keep stay on the hotel's own
//! disk, wrapped under the tenant master key; the cloud only asks for them by path.

/// `vault.put`.
pub const OP_PUT: &str = "vault.put";
/// `vault.get`.
pub const OP_GET: &str = "vault.get";
/// `vault.delete`.
pub const OP_DELETE: &str = "vault.delete";
/// Path parameter.
pub const PATH_PARAM: &str = "path";
/// Data parameter (base64).
pub const DATA_PARAM: &str = "data";
/// Expected SHA-256 (hex) of the data.
pub const SHA256_PARAM: &str = "sha256";
/// Result column.
pub const RESULT_COLUMN: &str = "value";
/// Largest file accepted.
pub const MAX_BYTES: usize = 6 * 1024 * 1024;

/// `LegalVaultOps.IsVaultOp`.
pub fn is_vault_op(operation: &str) -> bool {
    matches!(operation, OP_PUT | OP_GET | OP_DELETE)
}

/// `LegalVaultOps.Sha256Hex`.
pub fn sha256_hex(data: &[u8]) -> String {
    crate::crypto::sha256_hex(data)
}

/// `TryNormalize`: forward slashes, no empty, `.`, `..`, drive or control-character parts,
/// at most 260 UTF-16 units.
pub fn try_normalize(path: Option<&str>) -> Option<String> {
    let path = path?;
    if path.trim().is_empty() || path.encode_utf16().count() > 260 {
        return None;
    }
    let replaced = path.replace('\\', "/");
    let parts: Vec<&str> = replaced.split('/').filter(|p| !p.is_empty()).collect();
    let bad =
        |p: &&str| *p == "." || *p == ".." || p.contains(':') || p.chars().any(char::is_control);
    if parts.is_empty() || parts.iter().any(bad) {
        return None;
    }
    Some(parts.join("/"))
}
