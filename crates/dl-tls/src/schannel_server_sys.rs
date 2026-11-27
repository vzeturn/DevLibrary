//! Certificates and CNG private keys for SChannel server credentials: crypt32 and ncrypt
//! declarations with safe wrappers. Contains `unsafe`, like `schannel_sys`.
#![allow(unsafe_code)]

use core::ffi::c_void;
use core::ptr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::schannel_sys::CertContext;

/// `X509_ASN_ENCODING | PKCS_7_ASN_ENCODING`.
const ENCODING: u32 = 0x0001_0001;
const CERT_KEY_PROV_INFO_PROP_ID: u32 = 2;
const CERT_NCRYPT_KEY_HANDLE_PROP_ID: u32 = 78;
const CERT_SET_PROPERTY_INHIBIT_PERSIST_FLAG: u32 = 0x4000_0000;
/// `CERT_STORE_PROV_SYSTEM_W`, an integer passed in place of a provider name.
const CERT_STORE_PROV_SYSTEM_W: usize = 10;
const CERT_SYSTEM_STORE_CURRENT_USER: u32 = 1 << 16;
const CERT_SYSTEM_STORE_LOCAL_MACHINE: u32 = 2 << 16;
const CERT_STORE_ADD_USE_EXISTING: u32 = 2;

const NCRYPTBUFFER_PKCS_KEY_NAME: u32 = 45;
const NCRYPT_SILENT_FLAG: u32 = 0x40;
const NCRYPT_DO_NOT_FINALIZE_FLAG: u32 = 0x400;

const MS_KEY_STORAGE_PROVIDER: &str = "Microsoft Software Key Storage Provider";
/// Name prefix of the temporary persisted keys; a crash can leave one behind.
pub(crate) const KEY_NAME_PREFIX: &str = "dl-tls-";

/// `NCryptBuffer`.
#[repr(C)]
struct NcryptBuffer {
    cb: u32,
    kind: u32,
    pv: *mut c_void,
}

/// `NCryptBufferDesc`.
#[repr(C)]
struct NcryptBufferDesc {
    version: u32,
    count: u32,
    buffers: *mut NcryptBuffer,
}

/// `CRYPT_KEY_PROV_INFO`.
#[repr(C)]
struct CryptKeyProvInfo {
    container_name: *const u16,
    prov_name: *const u16,
    prov_type: u32,
    flags: u32,
    prov_params: u32,
    rg_prov_param: *mut c_void,
    key_spec: u32,
}

#[link(name = "crypt32")]
unsafe extern "system" {
    fn CertCreateCertificateContext(
        encoding: u32,
        encoded: *const u8,
        len: u32,
    ) -> *const CertContext;
    fn CertFreeCertificateContext(cert: *const CertContext) -> i32;
    fn CertSetCertificateContextProperty(
        cert: *const CertContext,
        prop: u32,
        flags: u32,
        data: *const c_void,
    ) -> i32;
    fn CertOpenStore(
        provider: *const i8,
        encoding: u32,
        prov: usize,
        flags: u32,
        para: *const c_void,
    ) -> *mut c_void;
    fn CertAddEncodedCertificateToStore(
        store: *mut c_void,
        encoding: u32,
        encoded: *const u8,
        len: u32,
        disposition: u32,
        added: *mut *const CertContext,
    ) -> i32;
    fn CertCloseStore(store: *mut c_void, flags: u32) -> i32;
}

#[link(name = "ncrypt")]
unsafe extern "system" {
    fn NCryptOpenStorageProvider(provider: *mut usize, name: *const u16, flags: u32) -> i32;
    fn NCryptImportKey(
        provider: usize,
        import_key: usize,
        blob_type: *const u16,
        params: *const NcryptBufferDesc,
        key: *mut usize,
        data: *const u8,
        len: u32,
        flags: u32,
    ) -> i32;
    fn NCryptFinalizeKey(key: usize, flags: u32) -> i32;
    fn NCryptDeleteKey(key: usize, flags: u32) -> i32;
    fn NCryptFreeObject(object: usize) -> i32;
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(core::iter::once(0)).collect()
}

fn last_error() -> i32 {
    std::io::Error::last_os_error().raw_os_error().unwrap_or(0)
}

/// A CNG key storage provider handle, freed on drop.
struct Provider(usize);

impl Provider {
    fn open() -> Result<Provider, i32> {
        let name = wide(MS_KEY_STORAGE_PROVIDER);
        let mut h = 0usize;
        // SAFETY: name is NUL-terminated; h is writable.
        let st = unsafe { NCryptOpenStorageProvider(&mut h, name.as_ptr(), 0) };
        if st == 0 { Ok(Provider(h)) } else { Err(st) }
    }

    /// Imports PKCS#8 DER; with `name` the key is persisted under that name.
    fn import(&self, pkcs8: &[u8], name: Option<&[u16]>) -> Result<usize, i32> {
        let blob_type = wide("PKCS8_PRIVATEKEY");
        let mut name_buf = name.map(<[u16]>::to_vec).unwrap_or_default();
        let mut buffers = [NcryptBuffer {
            cb: (name_buf.len() * 2) as u32,
            kind: NCRYPTBUFFER_PKCS_KEY_NAME,
            pv: name_buf.as_mut_ptr().cast(),
        }];
        let params = NcryptBufferDesc {
            version: 0,
            count: 1,
            buffers: buffers.as_mut_ptr(),
        };
        let params_ptr: *const NcryptBufferDesc =
            if name.is_some() { &params } else { ptr::null() };
        let mut key = 0usize;
        // SAFETY: the provider handle is open; blob_type and the key name are NUL-terminated
        // and outlive the call; pkcs8 holds len bytes; key is writable.
        let st = unsafe {
            NCryptImportKey(
                self.0,
                0,
                blob_type.as_ptr(),
                params_ptr,
                &mut key,
                pkcs8.as_ptr(),
                pkcs8.len() as u32,
                NCRYPT_SILENT_FLAG | NCRYPT_DO_NOT_FINALIZE_FLAG,
            )
        };
        if st != 0 {
            return Err(st);
        }
        // SAFETY: key is the handle just imported and not yet finalized.
        let st = unsafe { NCryptFinalizeKey(key, NCRYPT_SILENT_FLAG) };
        if st != 0 {
            if name.is_some() {
                // SAFETY: deletes the half-created key; this also frees the handle.
                if unsafe { NCryptDeleteKey(key, NCRYPT_SILENT_FLAG) } == 0 {
                    return Err(st);
                }
            }
            // SAFETY: key is a valid handle freed once.
            unsafe { NCryptFreeObject(key) };
            return Err(st);
        }
        Ok(key)
    }
}

impl Drop for Provider {
    fn drop(&mut self) {
        // SAFETY: the handle came from NCryptOpenStorageProvider and is freed once.
        unsafe { NCryptFreeObject(self.0) };
    }
}

/// A persisted CNG key that exists only for the life of a server credential: deleted from
/// the key store on drop.
pub(crate) struct TempKey {
    key: usize,
    name: Vec<u16>,
    _provider: Provider,
}

// SAFETY: CNG key handles may be used and freed from any thread.
unsafe impl Send for TempKey {}
// SAFETY: no method uses the handle through `&self`.
unsafe impl Sync for TempKey {}

impl TempKey {
    /// The key's name in the user's key store.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn name(&self) -> String {
        String::from_utf16_lossy(&self.name[..self.name.len().saturating_sub(1)])
    }
}

impl Drop for TempKey {
    fn drop(&mut self) {
        // SAFETY: deletes the key we created; on success this also frees the handle.
        if unsafe { NCryptDeleteKey(self.key, NCRYPT_SILENT_FLAG) } != 0 {
            // SAFETY: the deletion failed, so the handle is still ours to free, once.
            unsafe { NCryptFreeObject(self.key) };
        }
    }
}

/// A unique name for a temporary key: prefix, process id, a counter and the time.
fn temp_key_name() -> Vec<u16> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    wide(&format!(
        "{KEY_NAME_PREFIX}{}-{n}-{nanos}",
        std::process::id()
    ))
}

/// A certificate context, freed on drop.
pub(crate) struct Cert(*const CertContext);

// SAFETY: certificate contexts are reference-counted and thread-safe; this one is only freed
// on drop.
unsafe impl Send for Cert {}
// SAFETY: as above.
unsafe impl Sync for Cert {}

impl Cert {
    /// Parses one DER certificate; `None` when crypt32 cannot.
    pub(crate) fn from_der(der: &[u8]) -> Option<Cert> {
        let len = u32::try_from(der.len()).ok()?;
        // SAFETY: der holds len bytes; the context copies them.
        let c = unsafe { CertCreateCertificateContext(ENCODING, der.as_ptr(), len) };
        (!c.is_null()).then_some(Cert(c))
    }

    /// The context, valid while `self` lives.
    pub(crate) fn as_ptr(&self) -> *const CertContext {
        self.0
    }

    /// Imports `pkcs8` as an ephemeral CNG key (never written to disk) and attaches its
    /// handle to this certificate, which then owns it.
    pub(crate) fn attach_ephemeral_key(&self, pkcs8: &[u8]) -> Result<(), i32> {
        let provider = Provider::open()?;
        let key = provider.import(pkcs8, None)?;
        // SAFETY: self.0 is a valid context; the property data is a pointer to the handle.
        let ok = unsafe {
            CertSetCertificateContextProperty(
                self.0,
                CERT_NCRYPT_KEY_HANDLE_PROP_ID,
                CERT_SET_PROPERTY_INHIBIT_PERSIST_FLAG,
                (&raw const key).cast(),
            )
        };
        if ok == 0 {
            let err = last_error();
            // SAFETY: the certificate did not take the handle, so it is still ours.
            unsafe { NCryptFreeObject(key) };
            return Err(err);
        }
        Ok(())
    }

    /// Imports `pkcs8` as a persisted key under a unique temporary name in the user's key
    /// store and points this certificate at it. The key is deleted when the returned
    /// [`TempKey`] drops.
    pub(crate) fn attach_persisted_key(&self, pkcs8: &[u8]) -> Result<TempKey, i32> {
        let provider = Provider::open()?;
        let name = temp_key_name();
        let key = provider.import(pkcs8, Some(&name))?;
        let temp = TempKey {
            key,
            name,
            _provider: provider,
        };
        let prov_name = wide(MS_KEY_STORAGE_PROVIDER);
        let info = CryptKeyProvInfo {
            container_name: temp.name.as_ptr(),
            prov_name: prov_name.as_ptr(),
            // 0: a CNG key storage provider; dwKeySpec 0 likewise.
            prov_type: 0,
            flags: 0,
            prov_params: 0,
            rg_prov_param: ptr::null_mut(),
            key_spec: 0,
        };
        // SAFETY: self.0 is a valid context; info and its strings outlive the call (the
        // property is copied).
        let ok = unsafe {
            CertSetCertificateContextProperty(
                self.0,
                CERT_KEY_PROV_INFO_PROP_ID,
                0,
                (&raw const info).cast(),
            )
        };
        if ok == 0 {
            return Err(last_error());
        }
        Ok(temp)
    }
}

impl Drop for Cert {
    fn drop(&mut self) {
        // SAFETY: the context came from CertCreateCertificateContext and is freed once.
        unsafe { CertFreeCertificateContext(self.0) };
    }
}

/// Why intermediates could not be added.
pub(crate) enum StoreError {
    /// A certificate crypt32 cannot parse.
    Certificate,
    /// Neither the machine nor the user `CA` store accepted it (last error).
    Store(i32),
}

/// Adds `certs` to the intermediate (`CA`) system store, of the machine when allowed and of
/// the user otherwise: SChannel sends a server's chain as it builds it from the system
/// stores, not from the credential. Certificates already present stay as they are.
pub(crate) fn add_intermediates(certs: &[Vec<u8>]) -> Result<(), StoreError> {
    if certs.is_empty() {
        return Ok(());
    }
    if certs.iter().any(|c| Cert::from_der(c).is_none()) {
        return Err(StoreError::Certificate);
    }
    let mut err = 0;
    for location in [
        CERT_SYSTEM_STORE_LOCAL_MACHINE,
        CERT_SYSTEM_STORE_CURRENT_USER,
    ] {
        match add_to(location, certs) {
            Ok(()) => return Ok(()),
            Err(e) => err = e,
        }
    }
    Err(StoreError::Store(err))
}

fn add_to(location: u32, certs: &[Vec<u8>]) -> Result<(), i32> {
    let name = wide("CA");
    // SAFETY: the provider "name" is the CERT_STORE_PROV_SYSTEM_W constant, as the API
    // defines; name is NUL-terminated.
    let store = unsafe {
        CertOpenStore(
            ptr::without_provenance(CERT_STORE_PROV_SYSTEM_W),
            0,
            0,
            location,
            name.as_ptr().cast(),
        )
    };
    if store.is_null() {
        return Err(last_error());
    }
    let mut result = Ok(());
    for c in certs {
        // SAFETY: store is open; c holds len bytes (checked when parsed above).
        let ok = unsafe {
            CertAddEncodedCertificateToStore(
                store,
                ENCODING,
                c.as_ptr(),
                c.len() as u32,
                CERT_STORE_ADD_USE_EXISTING,
                ptr::null_mut(),
            )
        };
        if ok == 0 {
            result = Err(last_error());
            break;
        }
    }
    // SAFETY: closes the store opened above, once.
    unsafe { CertCloseStore(store, 0) };
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::mem::size_of;

    const EC_CERT: &[u8] = include_bytes!("../testdata/ec-cert.der");
    const EC_KEY: &[u8] = include_bytes!("../testdata/ec-key.p8.der");

    #[link(name = "ncrypt")]
    unsafe extern "system" {
        fn NCryptOpenKey(
            provider: usize,
            key: *mut usize,
            name: *const u16,
            spec: u32,
            flags: u32,
        ) -> i32;
    }

    fn exists(name: &str) -> bool {
        let p = Provider::open().unwrap();
        let mut key = 0usize;
        let name = wide(name);
        // SAFETY: the provider is open; name is NUL-terminated; key is writable.
        let st = unsafe { NCryptOpenKey(p.0, &mut key, name.as_ptr(), 0, NCRYPT_SILENT_FLAG) };
        if st == 0 {
            // SAFETY: frees the handle just opened.
            unsafe { NCryptFreeObject(key) };
        }
        st == 0
    }

    #[test]
    fn layouts() {
        if size_of::<usize>() == 8 {
            assert_eq!(size_of::<CryptKeyProvInfo>(), 48);
            assert_eq!(size_of::<NcryptBuffer>(), 16);
        } else {
            assert_eq!(size_of::<CryptKeyProvInfo>(), 28);
            assert_eq!(size_of::<NcryptBuffer>(), 12);
        }
    }

    #[test]
    fn temporary_key_is_deleted_on_drop() {
        let cert = Cert::from_der(EC_CERT).unwrap();
        let key = cert.attach_persisted_key(EC_KEY).unwrap();
        let name = key.name();
        assert!(name.starts_with(KEY_NAME_PREFIX), "{name}");
        assert!(exists(&name), "{name} was not persisted");
        drop(key);
        assert!(!exists(&name), "{name} is still in the key store");
    }

    #[test]
    fn ephemeral_key_attaches_and_garbage_is_refused() {
        let cert = Cert::from_der(EC_CERT).unwrap();
        cert.attach_ephemeral_key(EC_KEY).unwrap();
        assert!(Cert::from_der(b"not a certificate").is_none());
        assert!(cert.attach_ephemeral_key(b"not a key").is_err());
    }
}
