//! System OpenSSL (libssl 3 or 1.1), loaded at run time with `dlopen`, so the crate builds
//! and links without OpenSSL installed. The only file of the Unix backend with `unsafe`.
#![allow(unsafe_code)]

use core::ffi::{c_char, c_int, c_long, c_void};
use std::sync::OnceLock;

type Ptr = *mut c_void;

unsafe extern "C" {
    fn dlopen(filename: *const c_char, flag: c_int) -> Ptr;
    fn dlsym(handle: Ptr, symbol: *const c_char) -> Ptr;
}

const RTLD_NOW: c_int = 2;

macro_rules! api {
    ($($name:ident : fn($($arg:ty),*) -> $ret:ty;)*) => {
        #[allow(non_snake_case)]
        struct Api { $($name: unsafe extern "C" fn($($arg),*) -> $ret,)* }

        impl Api {
            fn load(lib: Ptr, crypto: Ptr) -> Option<Api> {
                Some(Api { $($name: {
                    let sym = concat!(stringify!($name), "\0");
                    // SAFETY: `lib` and `crypto` are live dlopen handles; the names are
                    // NUL-terminated.
                    let mut p = unsafe { dlsym(lib, sym.as_ptr().cast()) };
                    if p.is_null() {
                        // SAFETY: as above.
                        p = unsafe { dlsym(crypto, sym.as_ptr().cast()) };
                    }
                    if p.is_null() {
                        return None;
                    }
                    // SAFETY: the symbol has this C signature in OpenSSL 1.1 and 3.x.
                    unsafe {
                        core::mem::transmute::<Ptr, unsafe extern "C" fn($($arg),*) -> $ret>(p)
                    }
                },)* })
            }
        }
    };
}

api! {
    TLS_client_method: fn() -> Ptr;
    SSL_CTX_new: fn(Ptr) -> Ptr;
    SSL_CTX_free: fn(Ptr) -> ();
    SSL_CTX_ctrl: fn(Ptr, c_int, c_long, Ptr) -> c_long;
    SSL_CTX_set_default_verify_paths: fn(Ptr) -> c_int;
    SSL_CTX_set_verify: fn(Ptr, c_int, Ptr) -> ();
    SSL_new: fn(Ptr) -> Ptr;
    SSL_free: fn(Ptr) -> ();
    SSL_ctrl: fn(Ptr, c_int, c_long, Ptr) -> c_long;
    SSL_set1_host: fn(Ptr, *const c_char) -> c_int;
    SSL_get0_param: fn(Ptr) -> Ptr;
    X509_VERIFY_PARAM_set1_ip_asc: fn(Ptr, *const c_char) -> c_int;
    SSL_set_alpn_protos: fn(Ptr, *const u8, u32) -> c_int;
    SSL_set_bio: fn(Ptr, Ptr, Ptr) -> ();
    SSL_set_connect_state: fn(Ptr) -> ();
    SSL_do_handshake: fn(Ptr) -> c_int;
    SSL_read: fn(Ptr, *mut c_void, c_int) -> c_int;
    SSL_write: fn(Ptr, *const c_void, c_int) -> c_int;
    SSL_shutdown: fn(Ptr) -> c_int;
    SSL_get_error: fn(Ptr, c_int) -> c_int;
    SSL_get_verify_result: fn(Ptr) -> c_long;
    SSL_version: fn(Ptr) -> c_int;
    SSL_get0_alpn_selected: fn(Ptr, *mut *const u8, *mut u32) -> ();
    BIO_s_mem: fn() -> Ptr;
    BIO_new: fn(Ptr) -> Ptr;
    BIO_read: fn(Ptr, *mut c_void, c_int) -> c_int;
    BIO_write: fn(Ptr, *const c_void, c_int) -> c_int;
    BIO_ctrl: fn(Ptr, c_int, c_long, Ptr) -> c_long;
    ERR_get_error: fn() -> u64;
    ERR_clear_error: fn() -> ();
    ERR_error_string_n: fn(u64, *mut c_char, usize) -> ();
    i2d_X509: fn(Ptr, *mut *mut u8) -> c_int;
    X509_free: fn(Ptr) -> ();
}

/// `SSL_get1_peer_certificate` (3.x) or `SSL_get_peer_certificate` (1.1).
type PeerCertFn = unsafe extern "C" fn(Ptr) -> Ptr;

struct Lib {
    api: Api,
    peer_cert: Option<PeerCertFn>,
}

// SAFETY: the struct holds only function pointers into a library that is never unloaded.
unsafe impl Send for Lib {}
// SAFETY: as above.
unsafe impl Sync for Lib {}

fn lib() -> Option<&'static Lib> {
    static LIB: OnceLock<Option<Lib>> = OnceLock::new();
    LIB.get_or_init(|| {
        for (ssl, crypto) in [
            ("libssl.so.3\0", "libcrypto.so.3\0"),
            ("libssl.so\0", "libcrypto.so\0"),
            ("libssl.so.1.1\0", "libcrypto.so.1.1\0"),
        ] {
            // SAFETY: NUL-terminated library names; the handles are kept for the process.
            let l = unsafe { dlopen(ssl.as_ptr().cast(), RTLD_NOW) };
            if l.is_null() {
                continue;
            }
            // SAFETY: as above.
            let c = unsafe { dlopen(crypto.as_ptr().cast(), RTLD_NOW) };
            let c = if c.is_null() { l } else { c };
            if let Some(api) = Api::load(l, c) {
                let mut peer_cert = None;
                for name in ["SSL_get1_peer_certificate\0", "SSL_get_peer_certificate\0"] {
                    // SAFETY: live handle, NUL-terminated name.
                    let p = unsafe { dlsym(l, name.as_ptr().cast()) };
                    if !p.is_null() {
                        // SAFETY: both functions take an SSL* and return an owned X509*.
                        peer_cert = Some(unsafe { core::mem::transmute::<Ptr, PeerCertFn>(p) });
                        break;
                    }
                }
                return Some(Lib { api, peer_cert });
            }
        }
        None
    })
    .as_ref()
}

pub(crate) fn available() -> bool {
    lib().is_some()
}

const SSL_CTRL_SET_TLSEXT_HOSTNAME: c_int = 55;
const SSL_CTRL_SET_MIN_PROTO_VERSION: c_int = 123;
const TLS1_2_VERSION: c_long = 0x303;
const SSL_VERIFY_NONE: c_int = 0;
const SSL_VERIFY_PEER: c_int = 1;
const BIO_CTRL_PENDING: c_int = 10;

pub(crate) const SSL_ERROR_WANT_READ: c_int = 2;
pub(crate) const SSL_ERROR_WANT_WRITE: c_int = 3;
pub(crate) const SSL_ERROR_ZERO_RETURN: c_int = 6;

/// One client connection: SSL_CTX, SSL and the two memory BIOs it owns.
pub(crate) struct Ssl {
    lib: &'static Lib,
    ctx: Ptr,
    ssl: Ptr,
    /// Network -> OpenSSL.
    rbio: Ptr,
    /// OpenSSL -> network.
    wbio: Ptr,
}

// SAFETY: OpenSSL objects may move between threads; every method takes `&mut self`, so they
// are never used from two threads at once.
unsafe impl Send for Ssl {}
// SAFETY: no `&self` method touches OpenSSL state except read-only queries.
unsafe impl Sync for Ssl {}

impl Drop for Ssl {
    fn drop(&mut self) {
        // SAFETY: SSL_free also frees the BIOs attached with SSL_set_bio; ctx is ours.
        unsafe {
            (self.lib.api.SSL_free)(self.ssl);
            (self.lib.api.SSL_CTX_free)(self.ctx);
        }
    }
}

/// Error text from the OpenSSL error queue (cleared afterwards).
pub(crate) fn error_string() -> (u64, String) {
    let Some(l) = lib() else {
        return (0, String::new());
    };
    // SAFETY: plain calls on the thread-local error queue; the buffer is 256 bytes.
    unsafe {
        let code = (l.api.ERR_get_error)();
        let mut buf = [0 as c_char; 256];
        (l.api.ERR_error_string_n)(code, buf.as_mut_ptr(), buf.len());
        (l.api.ERR_clear_error)();
        let s = core::ffi::CStr::from_ptr(buf.as_ptr())
            .to_string_lossy()
            .into_owned();
        (code, s)
    }
}

impl Ssl {
    /// `host` must be NUL-free. `alpn` is the encoded protocol list (may be empty).
    pub(crate) fn new(host: &str, is_ip: bool, verify: bool, alpn: &[u8]) -> Option<Ssl> {
        let lib = lib()?;
        let a = &lib.api;
        let host_c = std::ffi::CString::new(host).ok()?;
        // SAFETY: standard OpenSSL client set-up; every pointer is checked for NULL, and
        // ownership of the BIOs passes to the SSL with SSL_set_bio.
        unsafe {
            (a.ERR_clear_error)();
            let ctx = (a.SSL_CTX_new)((a.TLS_client_method)());
            if ctx.is_null() {
                return None;
            }
            (a.SSL_CTX_ctrl)(
                ctx,
                SSL_CTRL_SET_MIN_PROTO_VERSION,
                TLS1_2_VERSION,
                core::ptr::null_mut(),
            );
            if verify {
                (a.SSL_CTX_set_default_verify_paths)(ctx);
            }
            (a.SSL_CTX_set_verify)(
                ctx,
                if verify {
                    SSL_VERIFY_PEER
                } else {
                    SSL_VERIFY_NONE
                },
                core::ptr::null_mut(),
            );
            let ssl = (a.SSL_new)(ctx);
            if ssl.is_null() {
                (a.SSL_CTX_free)(ctx);
                return None;
            }
            let rbio = (a.BIO_new)((a.BIO_s_mem)());
            let wbio = (a.BIO_new)((a.BIO_s_mem)());
            let me = Ssl {
                lib,
                ctx,
                ssl,
                rbio,
                wbio,
            };
            if rbio.is_null() || wbio.is_null() {
                return None;
            }
            (a.SSL_set_bio)(ssl, rbio, wbio);
            if !is_ip && !host.is_empty() {
                (a.SSL_ctrl)(
                    ssl,
                    SSL_CTRL_SET_TLSEXT_HOSTNAME,
                    0,
                    host_c.as_ptr().cast_mut().cast(),
                );
            }
            if verify {
                let ok = if is_ip {
                    (a.X509_VERIFY_PARAM_set1_ip_asc)((a.SSL_get0_param)(ssl), host_c.as_ptr())
                } else {
                    (a.SSL_set1_host)(ssl, host_c.as_ptr())
                };
                if ok != 1 {
                    return None;
                }
            }
            if !alpn.is_empty()
                && (a.SSL_set_alpn_protos)(ssl, alpn.as_ptr(), alpn.len() as u32) != 0
            {
                return None;
            }
            (a.SSL_set_connect_state)(ssl);
            Some(me)
        }
    }

    /// `SSL_do_handshake`: 1 when done, else the `SSL_get_error` code.
    pub(crate) fn handshake(&mut self) -> Result<(), c_int> {
        // SAFETY: self.ssl is a valid SSL.
        let r = unsafe { (self.lib.api.SSL_do_handshake)(self.ssl) };
        if r == 1 { Ok(()) } else { Err(self.error(r)) }
    }

    fn error(&self, r: c_int) -> c_int {
        // SAFETY: self.ssl is valid; r is the return value of the last SSL call.
        unsafe { (self.lib.api.SSL_get_error)(self.ssl, r) }
    }

    /// Feeds bytes received from the network.
    pub(crate) fn feed(&mut self, data: &[u8]) -> bool {
        let Ok(len) = c_int::try_from(data.len()) else {
            return false;
        };
        // SAFETY: memory BIO write of `len` readable bytes.
        unsafe { (self.lib.api.BIO_write)(self.rbio, data.as_ptr().cast(), len) == len }
    }

    /// Takes every byte OpenSSL wants to send.
    pub(crate) fn drain(&mut self) -> Vec<u8> {
        let mut out = Vec::new();
        let mut buf = [0u8; 16 * 1024];
        loop {
            // SAFETY: BIO_ctrl(PENDING) only reads the BIO state.
            let pending = unsafe {
                (self.lib.api.BIO_ctrl)(self.wbio, BIO_CTRL_PENDING, 0, core::ptr::null_mut())
            };
            if pending <= 0 {
                return out;
            }
            // SAFETY: reads at most buf.len() bytes into buf.
            let n = unsafe {
                (self.lib.api.BIO_read)(self.wbio, buf.as_mut_ptr().cast(), buf.len() as c_int)
            };
            if n <= 0 {
                return out;
            }
            out.extend_from_slice(&buf[..n as usize]);
        }
    }

    /// Whether OpenSSL still holds unprocessed network bytes.
    pub(crate) fn input_pending(&self) -> bool {
        // SAFETY: read-only BIO query.
        unsafe {
            (self.lib.api.BIO_ctrl)(self.rbio, BIO_CTRL_PENDING, 0, core::ptr::null_mut()) > 0
        }
    }

    /// `SSL_read`: bytes read, or the `SSL_get_error` code.
    pub(crate) fn read(&mut self, buf: &mut [u8]) -> Result<usize, c_int> {
        let len = buf.len().min(c_int::MAX as usize) as c_int;
        // SAFETY: buf is writable for `len` bytes.
        let r = unsafe { (self.lib.api.SSL_read)(self.ssl, buf.as_mut_ptr().cast(), len) };
        if r > 0 {
            Ok(r as usize)
        } else {
            Err(self.error(r))
        }
    }

    /// `SSL_write`: bytes accepted, or the `SSL_get_error` code.
    pub(crate) fn write(&mut self, buf: &[u8]) -> Result<usize, c_int> {
        let len = buf.len().min(c_int::MAX as usize) as c_int;
        // SAFETY: buf is readable for `len` bytes.
        let r = unsafe { (self.lib.api.SSL_write)(self.ssl, buf.as_ptr().cast(), len) };
        if r > 0 {
            Ok(r as usize)
        } else {
            Err(self.error(r))
        }
    }

    pub(crate) fn shutdown(&mut self) {
        // SAFETY: self.ssl is valid; the result only says whether the peer answered.
        unsafe { (self.lib.api.SSL_shutdown)(self.ssl) };
    }

    pub(crate) fn verify_result(&self) -> c_long {
        // SAFETY: read-only query.
        unsafe { (self.lib.api.SSL_get_verify_result)(self.ssl) }
    }

    pub(crate) fn version(&self) -> c_int {
        // SAFETY: read-only query.
        unsafe { (self.lib.api.SSL_version)(self.ssl) }
    }

    pub(crate) fn alpn_selected(&self) -> Option<Vec<u8>> {
        let mut p: *const u8 = core::ptr::null();
        let mut len = 0u32;
        // SAFETY: OpenSSL sets p/len to memory owned by the SSL (or NULL/0).
        unsafe { (self.lib.api.SSL_get0_alpn_selected)(self.ssl, &mut p, &mut len) };
        if p.is_null() || len == 0 {
            return None;
        }
        // SAFETY: p points to `len` bytes owned by the live SSL.
        Some(unsafe { core::slice::from_raw_parts(p, len as usize) }.to_vec())
    }

    pub(crate) fn peer_certificate_der(&self) -> Option<Vec<u8>> {
        let f = self.lib.peer_cert?;
        // SAFETY: returns an owned X509* (or NULL) that we free below.
        let x = unsafe { f(self.ssl) };
        if x.is_null() {
            return None;
        }
        // SAFETY: i2d_X509 with a NULL output pointer returns the DER length; with a pointer
        // to our buffer it writes that many bytes; X509_free releases our reference.
        unsafe {
            let len = (self.lib.api.i2d_X509)(x, core::ptr::null_mut());
            let der = if len > 0 {
                let mut v = vec![0u8; len as usize];
                let mut p = v.as_mut_ptr();
                (self.lib.api.i2d_X509)(x, &mut p);
                Some(v)
            } else {
                None
            };
            (self.lib.api.X509_free)(x);
            der
        }
    }
}
