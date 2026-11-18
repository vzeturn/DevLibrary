//! SSPI / SChannel declarations and safe wrappers. The only file of the Windows backend
//! that contains `unsafe`.
#![allow(unsafe_code)]

use core::ffi::c_void;
use core::ptr;

pub(crate) const SEC_E_OK: i32 = 0;
pub(crate) const SEC_I_CONTINUE_NEEDED: i32 = 0x0009_0312;
pub(crate) const SEC_I_CONTEXT_EXPIRED: i32 = 0x0009_0317;
pub(crate) const SEC_I_INCOMPLETE_CREDENTIALS: i32 = 0x0009_0320;
pub(crate) const SEC_I_RENEGOTIATE: i32 = 0x0009_0321;
pub(crate) const SEC_E_INCOMPLETE_MESSAGE: i32 = 0x8009_0318_u32 as i32;

const SECPKG_CRED_OUTBOUND: u32 = 2;
const SCHANNEL_CRED_VERSION: u32 = 4;
const SCH_CREDENTIALS_VERSION: u32 = 5;
const SCH_CRED_MANUAL_CRED_VALIDATION: u32 = 0x8;
const SCH_CRED_NO_DEFAULT_CREDS: u32 = 0x10;
const SCH_CRED_AUTO_CRED_VALIDATION: u32 = 0x20;
const SCH_USE_STRONG_CRYPTO: u32 = 0x0040_0000;
pub(crate) const SP_PROT_TLS1_0_CLIENT: u32 = 0x80;
pub(crate) const SP_PROT_TLS1_1_CLIENT: u32 = 0x200;
pub(crate) const SP_PROT_TLS1_2_CLIENT: u32 = 0x800;
pub(crate) const SP_PROT_TLS1_3_CLIENT: u32 = 0x2000;

const ISC_REQ_FLAGS: u32 = 0x0000_0008 // SEQUENCE_DETECT
    | 0x0000_0004 // REPLAY_DETECT
    | 0x0000_0010 // CONFIDENTIALITY
    | 0x0000_0100 // ALLOCATE_MEMORY
    | 0x0000_4000 // EXTENDED_ERROR
    | 0x0000_8000; // STREAM

const SECBUFFER_VERSION: u32 = 0;
const SECBUFFER_EMPTY: u32 = 0;
const SECBUFFER_DATA: u32 = 1;
const SECBUFFER_TOKEN: u32 = 2;
const SECBUFFER_EXTRA: u32 = 5;
const SECBUFFER_STREAM_TRAILER: u32 = 6;
const SECBUFFER_STREAM_HEADER: u32 = 7;
const SECBUFFER_ALERT: u32 = 17;
const SECBUFFER_APPLICATION_PROTOCOLS: u32 = 18;

const SECPKG_ATTR_STREAM_SIZES: u32 = 4;
const SECPKG_ATTR_APPLICATION_PROTOCOL: u32 = 35;
const SECPKG_ATTR_REMOTE_CERT_CONTEXT: u32 = 0x53;
const SECPKG_ATTR_CONNECTION_INFO: u32 = 0x5a;
const SCHANNEL_SHUTDOWN: u32 = 1;

#[repr(C)]
#[derive(Clone, Copy)]
struct SecHandle {
    lower: usize,
    upper: usize,
}

#[repr(C)]
struct SecBuffer {
    cb: u32,
    kind: u32,
    pv: *mut c_void,
}

#[repr(C)]
struct SecBufferDesc {
    version: u32,
    count: u32,
    buffers: *mut SecBuffer,
}

#[repr(C)]
struct SchannelCred {
    version: u32,
    creds: u32,
    pa_cred: *mut c_void,
    root_store: *mut c_void,
    mappers: u32,
    aph_mappers: *mut c_void,
    supported_algs: u32,
    palg_supported_algs: *mut u32,
    enabled_protocols: u32,
    minimum_cipher_strength: u32,
    maximum_cipher_strength: u32,
    session_lifespan: u32,
    flags: u32,
    cred_format: u32,
}

#[repr(C)]
struct TlsParameters {
    alpn_ids: u32,
    rgstr_alpn_ids: *mut c_void,
    disabled_protocols: u32,
    disabled_crypto: u32,
    p_disabled_crypto: *mut c_void,
    flags: u32,
}

#[repr(C)]
struct SchCredentials {
    version: u32,
    cred_format: u32,
    creds: u32,
    pa_cred: *mut c_void,
    root_store: *mut c_void,
    mappers: u32,
    aph_mappers: *mut c_void,
    session_lifespan: u32,
    flags: u32,
    tls_parameters_count: u32,
    tls_parameters: *mut TlsParameters,
}

/// `SecPkgContext_StreamSizes`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct StreamSizes {
    pub header: u32,
    pub trailer: u32,
    pub maximum_message: u32,
    pub buffers: u32,
    pub block_size: u32,
}

#[repr(C)]
#[derive(Default)]
struct ConnectionInfo {
    protocol: u32,
    cipher: u32,
    cipher_strength: u32,
    hash: u32,
    hash_strength: u32,
    exchange: u32,
    exchange_strength: u32,
}

#[repr(C)]
struct ApplicationProtocol {
    status: i32,
    ext: i32,
    size: u8,
    id: [u8; 255],
}

#[repr(C)]
struct CertContext {
    encoding_type: u32,
    encoded: *const u8,
    encoded_len: u32,
    info: *mut c_void,
    store: *mut c_void,
}

#[link(name = "secur32")]
unsafe extern "system" {
    fn AcquireCredentialsHandleW(
        principal: *const u16,
        package: *const u16,
        usage: u32,
        logon_id: *mut c_void,
        auth_data: *mut c_void,
        get_key_fn: *mut c_void,
        get_key_arg: *mut c_void,
        cred: *mut SecHandle,
        expiry: *mut i64,
    ) -> i32;
    fn FreeCredentialsHandle(cred: *mut SecHandle) -> i32;
    fn InitializeSecurityContextW(
        cred: *mut SecHandle,
        ctx: *mut SecHandle,
        target: *const u16,
        req: u32,
        reserved1: u32,
        data_rep: u32,
        input: *mut SecBufferDesc,
        reserved2: u32,
        new_ctx: *mut SecHandle,
        output: *mut SecBufferDesc,
        attrs: *mut u32,
        expiry: *mut i64,
    ) -> i32;
    fn DeleteSecurityContext(ctx: *mut SecHandle) -> i32;
    fn FreeContextBuffer(buf: *mut c_void) -> i32;
    fn QueryContextAttributesW(ctx: *mut SecHandle, attr: u32, buf: *mut c_void) -> i32;
    fn EncryptMessage(ctx: *mut SecHandle, qop: u32, msg: *mut SecBufferDesc, seq: u32) -> i32;
    fn DecryptMessage(ctx: *mut SecHandle, msg: *mut SecBufferDesc, seq: u32, qop: *mut u32)
    -> i32;
    fn ApplyControlToken(ctx: *mut SecHandle, input: *mut SecBufferDesc) -> i32;
}

#[link(name = "crypt32")]
unsafe extern "system" {
    fn CertFreeCertificateContext(cert: *const CertContext) -> i32;
}

fn buffer(kind: u32, data: Option<&mut [u8]>) -> SecBuffer {
    match data {
        Some(d) => SecBuffer {
            cb: d.len() as u32,
            kind,
            pv: d.as_mut_ptr().cast(),
        },
        None => SecBuffer {
            cb: 0,
            kind,
            pv: ptr::null_mut(),
        },
    }
}

fn desc(buffers: &mut [SecBuffer]) -> SecBufferDesc {
    SecBufferDesc {
        version: SECBUFFER_VERSION,
        count: buffers.len() as u32,
        buffers: buffers.as_mut_ptr(),
    }
}

/// An SChannel credentials handle.
pub(crate) struct Cred(SecHandle);

// SAFETY: SSPI handles are plain identifiers usable from any thread; the session that owns
// one only uses it through `&mut self`.
unsafe impl Send for Cred {}
// SAFETY: as above; no method takes `&self` and mutates through the handle.
unsafe impl Sync for Cred {}

impl Drop for Cred {
    fn drop(&mut self) {
        // SAFETY: the handle was returned by AcquireCredentialsHandleW and is freed once.
        unsafe { FreeCredentialsHandle(&mut self.0) };
    }
}

/// Acquires client credentials for the `SP_PROT_*_CLIENT` protocols in `enabled`.
/// `modern` uses `SCH_CREDENTIALS` (TLS 1.3 possible, Windows 10 1809+); otherwise
/// `SCHANNEL_CRED`, which cannot offer TLS 1.3.
pub(crate) fn acquire(verify: bool, modern: bool, enabled: u32) -> Result<Cred, i32> {
    let mut package: Vec<u16> = "Microsoft Unified Security Protocol Provider\0"
        .encode_utf16()
        .collect();
    let flags = SCH_CRED_NO_DEFAULT_CREDS
        | SCH_USE_STRONG_CRYPTO
        | if verify {
            SCH_CRED_AUTO_CRED_VALIDATION
        } else {
            SCH_CRED_MANUAL_CRED_VALIDATION
        };
    let mut params = TlsParameters {
        alpn_ids: 0,
        rgstr_alpn_ids: ptr::null_mut(),
        disabled_protocols: !enabled,
        disabled_crypto: 0,
        p_disabled_crypto: ptr::null_mut(),
        flags: 0,
    };
    let mut sch = SchCredentials {
        version: SCH_CREDENTIALS_VERSION,
        cred_format: 0,
        creds: 0,
        pa_cred: ptr::null_mut(),
        root_store: ptr::null_mut(),
        mappers: 0,
        aph_mappers: ptr::null_mut(),
        session_lifespan: 0,
        flags,
        tls_parameters_count: 1,
        tls_parameters: &mut params,
    };
    let mut legacy = SchannelCred {
        version: SCHANNEL_CRED_VERSION,
        creds: 0,
        pa_cred: ptr::null_mut(),
        root_store: ptr::null_mut(),
        mappers: 0,
        aph_mappers: ptr::null_mut(),
        supported_algs: 0,
        palg_supported_algs: ptr::null_mut(),
        enabled_protocols: enabled & !SP_PROT_TLS1_3_CLIENT,
        minimum_cipher_strength: 0,
        maximum_cipher_strength: 0,
        session_lifespan: 0,
        flags,
        cred_format: 0,
    };
    let auth: *mut c_void = if modern {
        (&raw mut sch).cast()
    } else {
        (&raw mut legacy).cast()
    };
    let mut h = SecHandle { lower: 0, upper: 0 };
    let mut expiry = 0i64;
    // SAFETY: package is NUL-terminated; auth points to a credentials struct (and its
    // TLS_PARAMETERS) that lives until the call returns; h and expiry are writable.
    let st = unsafe {
        AcquireCredentialsHandleW(
            ptr::null(),
            package.as_mut_ptr(),
            SECPKG_CRED_OUTBOUND,
            ptr::null_mut(),
            auth,
            ptr::null_mut(),
            ptr::null_mut(),
            &mut h,
            &mut expiry,
        )
    };
    if st == SEC_E_OK { Ok(Cred(h)) } else { Err(st) }
}

/// A security context (one TLS session).
pub(crate) struct Ctx(SecHandle);

// SAFETY: see `Cred`.
unsafe impl Send for Ctx {}
// SAFETY: see `Cred`.
unsafe impl Sync for Ctx {}

impl Drop for Ctx {
    fn drop(&mut self) {
        // SAFETY: the handle was created by InitializeSecurityContextW and is deleted once.
        unsafe { DeleteSecurityContext(&mut self.0) };
    }
}

/// Input to one `InitializeSecurityContextW` step.
#[derive(Clone, Copy)]
pub(crate) enum Input<'a> {
    /// First call, no data.
    None,
    /// First call, offering ALPN (a `SEC_APPLICATION_PROTOCOLS` buffer).
    Alpn(&'a [u8]),
    /// Handshake bytes received from the server.
    Token(&'a [u8]),
}

/// The result of one handshake step.
pub(crate) struct Step {
    pub status: i32,
    /// Bytes to send to the server.
    pub token: Vec<u8>,
    /// Number of input bytes at the end of `Input::Token` that were not consumed.
    pub extra: usize,
}

/// Takes a buffer SChannel allocated (ALLOCATE_MEMORY), copies it and frees it.
fn take_allocated(b: &SecBuffer) -> Vec<u8> {
    if b.pv.is_null() {
        return Vec::new();
    }
    // SAFETY: SChannel returned `cb` initialised bytes at `pv`.
    let v = unsafe { core::slice::from_raw_parts(b.pv.cast::<u8>(), b.cb as usize) }.to_vec();
    // SAFETY: the buffer was allocated by the security package and is freed exactly once.
    unsafe { FreeContextBuffer(b.pv) };
    v
}

pub(crate) fn initialize(
    cred: &mut Cred,
    ctx: &mut Option<Ctx>,
    target: &[u16],
    input: Input<'_>,
) -> Step {
    let mut owned: Vec<u8> = match input {
        Input::None => Vec::new(),
        Input::Alpn(a) | Input::Token(a) => a.to_vec(),
    };
    let mut in_bufs = match input {
        Input::None => Vec::new(),
        Input::Alpn(_) => vec![buffer(SECBUFFER_APPLICATION_PROTOCOLS, Some(&mut owned))],
        Input::Token(_) => {
            vec![
                buffer(SECBUFFER_TOKEN, Some(&mut owned)),
                buffer(SECBUFFER_EMPTY, None),
            ]
        }
    };
    let mut in_desc = desc(&mut in_bufs);
    let mut out_bufs = [
        buffer(SECBUFFER_TOKEN, None),
        buffer(SECBUFFER_ALERT, None),
        buffer(SECBUFFER_EMPTY, None),
    ];
    let mut out_desc = desc(&mut out_bufs);
    let mut new_ctx = SecHandle { lower: 0, upper: 0 };
    let mut attrs = 0u32;
    let mut expiry = 0i64;
    let existing: *mut SecHandle = match ctx.as_mut() {
        Some(c) => &mut c.0,
        None => ptr::null_mut(),
    };
    let input_ptr: *mut SecBufferDesc = if in_bufs_is_empty(&in_desc) {
        ptr::null_mut()
    } else {
        &mut in_desc
    };
    // SAFETY: cred is valid; existing is NULL or a valid context; target is NUL-terminated;
    // the descriptors point to buffers that outlive the call; output buffers are allocated
    // by SChannel (ALLOCATE_MEMORY) and freed by take_allocated below.
    let status = unsafe {
        InitializeSecurityContextW(
            &mut cred.0,
            existing,
            target.as_ptr(),
            ISC_REQ_FLAGS,
            0,
            0,
            input_ptr,
            0,
            if existing.is_null() {
                &mut new_ctx
            } else {
                existing
            },
            &mut out_desc,
            &mut attrs,
            &mut expiry,
        )
    };
    if ctx.is_none() && (new_ctx.lower != 0 || new_ctx.upper != 0) {
        *ctx = Some(Ctx(new_ctx));
    }
    let token = take_allocated(&out_bufs[0]);
    let _alert = take_allocated(&out_bufs[1]);
    let _ = take_allocated(&out_bufs[2]);
    let extra = match input {
        Input::Token(_) => in_bufs
            .get(1)
            .filter(|b| b.kind == SECBUFFER_EXTRA)
            .map_or(0, |b| b.cb as usize),
        _ => 0,
    };
    Step {
        status,
        token,
        extra,
    }
}

fn in_bufs_is_empty(d: &SecBufferDesc) -> bool {
    d.count == 0
}

/// Result of decrypting the start of a buffer in place.
pub(crate) struct Decrypted {
    pub status: i32,
    /// Where the plaintext is inside the buffer that was passed.
    pub plain: core::ops::Range<usize>,
    /// Number of bytes at the end of the buffer that belong to the next record(s).
    pub extra: usize,
}

pub(crate) fn decrypt(ctx: &mut Ctx, data: &mut [u8]) -> Decrypted {
    let base = data.as_ptr() as usize;
    let len = data.len();
    let mut bufs = [
        buffer(SECBUFFER_DATA, Some(data)),
        buffer(SECBUFFER_EMPTY, None),
        buffer(SECBUFFER_EMPTY, None),
        buffer(SECBUFFER_EMPTY, None),
    ];
    let mut d = desc(&mut bufs);
    // SAFETY: ctx is valid; the DATA buffer is our writable slice, decrypted in place; the
    // EMPTY buffers receive pointers into it.
    let status = unsafe { DecryptMessage(&mut ctx.0, &mut d, 0, ptr::null_mut()) };
    let mut plain = 0..0;
    let mut extra = 0;
    for b in &bufs {
        match b.kind {
            SECBUFFER_DATA if status == SEC_E_OK || status == SEC_I_RENEGOTIATE => {
                let start = (b.pv as usize).wrapping_sub(base);
                let end = start.saturating_add(b.cb as usize);
                if !b.pv.is_null() && start <= len && end <= len {
                    plain = start..end;
                }
            }
            SECBUFFER_EXTRA => extra = (b.cb as usize).min(len),
            _ => {}
        }
    }
    Decrypted {
        status,
        plain,
        extra,
    }
}

pub(crate) fn encrypt(ctx: &mut Ctx, sizes: &StreamSizes, data: &[u8]) -> Result<Vec<u8>, i32> {
    let h = sizes.header as usize;
    let t = sizes.trailer as usize;
    let mut msg = vec![0u8; h + data.len() + t];
    msg[h..h + data.len()].copy_from_slice(data);
    let (head, rest) = msg.split_at_mut(h);
    let (body, tail) = rest.split_at_mut(data.len());
    let mut bufs = [
        buffer(SECBUFFER_STREAM_HEADER, Some(head)),
        buffer(SECBUFFER_DATA, Some(body)),
        buffer(SECBUFFER_STREAM_TRAILER, Some(tail)),
        buffer(SECBUFFER_EMPTY, None),
    ];
    let mut d = desc(&mut bufs);
    // SAFETY: ctx is valid; the three buffers are disjoint parts of `msg`, sized from the
    // stream sizes SChannel reported.
    let status = unsafe { EncryptMessage(&mut ctx.0, 0, &mut d, 0) };
    if status != SEC_E_OK {
        return Err(status);
    }
    let total = (bufs[0].cb + bufs[1].cb + bufs[2].cb) as usize;
    msg.truncate(total.min(h + data.len() + t));
    Ok(msg)
}

pub(crate) fn apply_shutdown(ctx: &mut Ctx) -> i32 {
    let mut token = SCHANNEL_SHUTDOWN.to_le_bytes();
    let mut bufs = [buffer(SECBUFFER_TOKEN, Some(&mut token))];
    let mut d = desc(&mut bufs);
    // SAFETY: ctx is valid; the token buffer holds the SCHANNEL_SHUTDOWN DWORD.
    unsafe { ApplyControlToken(&mut ctx.0, &mut d) }
}

pub(crate) fn stream_sizes(ctx: &mut Ctx) -> Result<StreamSizes, i32> {
    let mut s = StreamSizes::default();
    // SAFETY: the attribute writes a SecPkgContext_StreamSizes into `s`.
    let st = unsafe {
        QueryContextAttributesW(&mut ctx.0, SECPKG_ATTR_STREAM_SIZES, (&raw mut s).cast())
    };
    if st == SEC_E_OK { Ok(s) } else { Err(st) }
}

pub(crate) fn protocol(ctx: &mut Ctx) -> Option<u32> {
    let mut c = ConnectionInfo::default();
    // SAFETY: the attribute writes a SecPkgContext_ConnectionInfo into `c`.
    let st = unsafe {
        QueryContextAttributesW(&mut ctx.0, SECPKG_ATTR_CONNECTION_INFO, (&raw mut c).cast())
    };
    (st == SEC_E_OK).then_some(c.protocol)
}

pub(crate) fn application_protocol(ctx: &mut Ctx) -> Option<Vec<u8>> {
    let mut a = ApplicationProtocol {
        status: 0,
        ext: 0,
        size: 0,
        id: [0; 255],
    };
    // SAFETY: the attribute writes a SecPkgContext_ApplicationProtocol into `a`.
    let st = unsafe {
        QueryContextAttributesW(
            &mut ctx.0,
            SECPKG_ATTR_APPLICATION_PROTOCOL,
            (&raw mut a).cast(),
        )
    };
    // ProtoNegoStatus 1 = SecApplicationProtocolNegotiationStatus_Success.
    (st == SEC_E_OK && a.status == 1 && a.size > 0).then(|| a.id[..usize::from(a.size)].to_vec())
}

pub(crate) fn remote_cert_der(ctx: &mut Ctx) -> Option<Vec<u8>> {
    let mut cert: *const CertContext = ptr::null();
    // SAFETY: the attribute writes a PCCERT_CONTEXT that we must free.
    let st = unsafe {
        QueryContextAttributesW(
            &mut ctx.0,
            SECPKG_ATTR_REMOTE_CERT_CONTEXT,
            (&raw mut cert).cast(),
        )
    };
    if st != SEC_E_OK || cert.is_null() {
        return None;
    }
    // SAFETY: cert is a valid CERT_CONTEXT; pbCertEncoded holds cbCertEncoded bytes.
    let der = unsafe {
        let c = &*cert;
        if c.encoded.is_null() {
            Vec::new()
        } else {
            core::slice::from_raw_parts(c.encoded, c.encoded_len as usize).to_vec()
        }
    };
    // SAFETY: frees the reference QueryContextAttributesW gave us.
    unsafe { CertFreeCertificateContext(cert) };
    Some(der)
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::mem::size_of;

    #[test]
    fn layouts() {
        if size_of::<usize>() == 8 {
            assert_eq!(size_of::<SecBuffer>(), 16);
            assert_eq!(size_of::<SchannelCred>(), 80);
            assert_eq!(size_of::<SchCredentials>(), 72);
            assert_eq!(size_of::<TlsParameters>(), 40);
        }
        assert_eq!(size_of::<StreamSizes>(), 20);
        assert_eq!(size_of::<ApplicationProtocol>(), 264);
    }
}
