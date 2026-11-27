//! System OpenSSL (libssl 3 or 1.1), loaded at run time with `dlopen`, so the crate builds
//! and links without OpenSSL installed. The only file of the Unix backend with `unsafe`;
//! client and server connections both run on memory BIOs.
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

/// `SSL_CTX_alpn_select_cb_func`.
type AlpnSelectCb =
    unsafe extern "C" fn(Ptr, *mut *const u8, *mut u8, *const u8, u32, Ptr) -> c_int;

api! {
    TLS_client_method: fn() -> Ptr;
    TLS_server_method: fn() -> Ptr;
    SSL_CTX_use_certificate_ASN1: fn(Ptr, c_int, *const u8) -> c_int;
    SSL_CTX_use_PrivateKey: fn(Ptr, Ptr) -> c_int;
    SSL_CTX_check_private_key: fn(Ptr) -> c_int;
    SSL_CTX_set_alpn_select_cb: fn(Ptr, Option<AlpnSelectCb>, Ptr) -> ();
    SSL_set_accept_state: fn(Ptr) -> ();
    d2i_X509: fn(*mut Ptr, *mut *const u8, c_long) -> Ptr;
    d2i_PKCS8_PRIV_KEY_INFO: fn(*mut Ptr, *mut *const u8, c_long) -> Ptr;
    EVP_PKCS82PKEY: fn(Ptr) -> Ptr;
    PKCS8_PRIV_KEY_INFO_free: fn(Ptr) -> ();
    EVP_PKEY_free: fn(Ptr) -> ();
    SSL_CTX_new: fn(Ptr) -> Ptr;
    SSL_CTX_free: fn(Ptr) -> ();
    SSL_CTX_ctrl: fn(Ptr, c_int, c_long, Ptr) -> c_long;
    SSL_CTX_set_default_verify_paths: fn(Ptr) -> c_int;
    SSL_CTX_set_verify: fn(Ptr, c_int, Ptr) -> ();
    SSL_CTX_set_security_level: fn(Ptr, c_int) -> ();
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
    BIO_free: fn(Ptr) -> c_int;
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
const SSL_CTRL_CHAIN_CERT: c_int = 89;
const SSL_TLSEXT_ERR_OK: c_int = 0;
const SSL_TLSEXT_ERR_ALERT_FATAL: c_int = 2;
const SSL_TLSEXT_ERR_NOACK: c_int = 3;
const SSL_CTRL_SET_MIN_PROTO_VERSION: c_int = 123;
pub(crate) const TLS1_VERSION: c_long = 0x301;
pub(crate) const TLS1_1_VERSION: c_long = 0x302;
pub(crate) const TLS1_2_VERSION: c_long = 0x303;
pub(crate) const TLS1_3_VERSION: c_long = 0x304;
const SSL_VERIFY_NONE: c_int = 0;
const SSL_VERIFY_PEER: c_int = 1;
const BIO_CTRL_PENDING: c_int = 10;

pub(crate) const SSL_ERROR_WANT_READ: c_int = 2;
pub(crate) const SSL_ERROR_WANT_WRITE: c_int = 3;
pub(crate) const SSL_ERROR_ZERO_RETURN: c_int = 6;

/// One connection: its SSL, the two memory BIOs the SSL owns and, for a client, its own
/// SSL_CTX.
pub(crate) struct Ssl {
    lib: &'static Lib,
    /// Owned by this connection (client), or NULL (server: owned by [`ServerCtx`]).
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
        // SAFETY: SSL_free also frees the BIOs attached with SSL_set_bio; a non-NULL ctx is
        // ours.
        unsafe {
            (self.lib.api.SSL_free)(self.ssl);
            if !self.ctx.is_null() {
                (self.lib.api.SSL_CTX_free)(self.ctx);
            }
        }
    }
}

/// Why a [`ServerCtx`] could not be built; the OpenSSL error queue holds the details.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CtxError {
    /// OpenSSL could not create the context.
    Setup,
    /// A certificate is not DER that OpenSSL accepts.
    Certificate,
    /// The key is not PKCS#8 that OpenSSL accepts.
    Key,
    /// The key is not the leaf certificate's key.
    Mismatch,
}

/// A configured server SSL_CTX (certificates, key, ALPN), shared by every connection
/// accepted with it.
pub(crate) struct ServerCtx {
    lib: &'static Lib,
    ctx: Ptr,
    /// The ALPN protocols the select callback reads (from `Box::into_raw`, freed in Drop).
    alpn: *mut Vec<Vec<u8>>,
}

// SAFETY: after set-up the SSL_CTX is only read (SSL_new), which OpenSSL allows from any
// thread; its session cache has its own lock. `alpn` is never written after set-up.
unsafe impl Send for ServerCtx {}
// SAFETY: as above.
unsafe impl Sync for ServerCtx {}

impl Drop for ServerCtx {
    fn drop(&mut self) {
        // SAFETY: drops our context reference, then the list from `Box::into_raw`. Sessions
        // made from this context keep the ServerCtx alive, so no callback can still run.
        unsafe {
            (self.lib.api.SSL_CTX_free)(self.ctx);
            drop(Box::from_raw(self.alpn));
        }
    }
}

/// Server ALPN choice (RFC 7301 §3.2): the server's most preferred protocol the client offers,
/// or the fatal `no_application_protocol` alert.
unsafe extern "C" fn select_alpn(
    _ssl: Ptr,
    out: *mut *const u8,
    out_len: *mut u8,
    client: *const u8,
    client_len: u32,
    arg: Ptr,
) -> c_int {
    if arg.is_null() || client.is_null() || out.is_null() || out_len.is_null() {
        return SSL_TLSEXT_ERR_NOACK;
    }
    // SAFETY: `arg` is the boxed protocol list of a live ServerCtx (see its Drop); OpenSSL
    // passes the client's list as `client_len` readable bytes for the duration of the call.
    let (server, offered) = unsafe {
        (
            &*arg.cast::<Vec<Vec<u8>>>(),
            core::slice::from_raw_parts(client, client_len as usize),
        )
    };
    match crate::alpn::select(server, offered) {
        Some(p) => {
            // SAFETY: `out`/`out_len` are OpenSSL's out-parameters; `p` points into the
            // client's list, as OpenSSL requires, and is at most 255 bytes (one length byte).
            unsafe {
                *out = p.as_ptr();
                *out_len = p.len() as u8;
            }
            SSL_TLSEXT_ERR_OK
        }
        None => SSL_TLSEXT_ERR_ALERT_FATAL,
    }
}

impl ServerCtx {
    /// `chain` is the leaf certificate then its intermediates (DER), `key` the leaf's
    /// PKCS#8 DER key, `alpn` the accepted protocols (empty: no ALPN), `min_version` a
    /// `TLS1_*_VERSION` constant.
    pub(crate) fn new(
        chain: &[Vec<u8>],
        key: &[u8],
        alpn: Vec<Vec<u8>>,
        min_version: c_long,
    ) -> Result<ServerCtx, CtxError> {
        let lib = lib().ok_or(CtxError::Setup)?;
        let a = &lib.api;
        let (leaf, intermediates) = chain.split_first().ok_or(CtxError::Certificate)?;
        let leaf_len = c_int::try_from(leaf.len()).map_err(|_| CtxError::Certificate)?;
        let key_len = c_long::try_from(key.len()).map_err(|_| CtxError::Key)?;
        // SAFETY: standard OpenSSL server set-up. Every pointer is checked for NULL; the
        // context is owned by `me` from creation on, so early returns free it; d2i_* read at
        // most the given lengths; add0_chain_cert takes ownership of the X509 on success and
        // the key reference we created is released after SSL_CTX_use_PrivateKey took its own.
        unsafe {
            (a.ERR_clear_error)();
            let ctx = (a.SSL_CTX_new)((a.TLS_server_method)());
            if ctx.is_null() {
                return Err(CtxError::Setup);
            }
            let has_alpn = !alpn.is_empty();
            let me = ServerCtx {
                lib,
                ctx,
                alpn: Box::into_raw(Box::new(alpn)),
            };
            (a.SSL_CTX_ctrl)(
                ctx,
                SSL_CTRL_SET_MIN_PROTO_VERSION,
                min_version,
                core::ptr::null_mut(),
            );
            if min_version < TLS1_2_VERSION {
                (a.SSL_CTX_set_security_level)(ctx, 0);
            }
            if (a.SSL_CTX_use_certificate_ASN1)(ctx, leaf_len, leaf.as_ptr()) != 1 {
                return Err(CtxError::Certificate);
            }
            for der in intermediates {
                let len = c_long::try_from(der.len()).map_err(|_| CtxError::Certificate)?;
                let mut p = der.as_ptr();
                let x = (a.d2i_X509)(core::ptr::null_mut(), &mut p, len);
                if x.is_null() {
                    return Err(CtxError::Certificate);
                }
                // SSL_CTX_add0_chain_cert.
                if (a.SSL_CTX_ctrl)(ctx, SSL_CTRL_CHAIN_CERT, 0, x) != 1 {
                    (a.X509_free)(x);
                    return Err(CtxError::Certificate);
                }
            }
            let mut p = key.as_ptr();
            let p8 = (a.d2i_PKCS8_PRIV_KEY_INFO)(core::ptr::null_mut(), &mut p, key_len);
            if p8.is_null() {
                return Err(CtxError::Key);
            }
            let pkey = (a.EVP_PKCS82PKEY)(p8);
            (a.PKCS8_PRIV_KEY_INFO_free)(p8);
            if pkey.is_null() {
                return Err(CtxError::Key);
            }
            let used = (a.SSL_CTX_use_PrivateKey)(ctx, pkey);
            (a.EVP_PKEY_free)(pkey);
            if used != 1 || (a.SSL_CTX_check_private_key)(ctx) != 1 {
                return Err(CtxError::Mismatch);
            }
            if has_alpn {
                (a.SSL_CTX_set_alpn_select_cb)(ctx, Some(select_alpn), me.alpn.cast());
            }
            Ok(me)
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
    /// `min_version` is a `TLS1_*_VERSION` constant; below TLS 1.2 the security level is
    /// lowered to 0, since OpenSSL 3 refuses TLS 1.0/1.1 at level 1 and above.
    pub(crate) fn new(
        host: &str,
        is_ip: bool,
        verify: bool,
        alpn: &[u8],
        min_version: c_long,
    ) -> Option<Ssl> {
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
                min_version,
                core::ptr::null_mut(),
            );
            if min_version < TLS1_2_VERSION {
                (a.SSL_CTX_set_security_level)(ctx, 0);
            }
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

    /// A server connection on `server`'s SSL_CTX.
    pub(crate) fn server(server: &ServerCtx) -> Option<Ssl> {
        let lib = server.lib;
        let a = &lib.api;
        // SAFETY: SSL_new takes its own reference on the configured ctx; the BIOs are checked
        // and handed to the SSL with SSL_set_bio.
        unsafe {
            (a.ERR_clear_error)();
            let ssl = (a.SSL_new)(server.ctx);
            if ssl.is_null() {
                return None;
            }
            let rbio = (a.BIO_new)((a.BIO_s_mem)());
            let wbio = (a.BIO_new)((a.BIO_s_mem)());
            if rbio.is_null() || wbio.is_null() {
                for bio in [rbio, wbio] {
                    if !bio.is_null() {
                        (a.BIO_free)(bio);
                    }
                }
                (a.SSL_free)(ssl);
                return None;
            }
            (a.SSL_set_bio)(ssl, rbio, wbio);
            (a.SSL_set_accept_state)(ssl);
            Some(Ssl {
                lib,
                ctx: core::ptr::null_mut(),
                ssl,
                rbio,
                wbio,
            })
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
