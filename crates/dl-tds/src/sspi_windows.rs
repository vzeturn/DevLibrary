//! SSPI (secur32), computer name (kernel32) and canonical-name lookup (ws2_32)
//! declarations for integrated authentication. The only file of the crate with `unsafe`.
#![allow(unsafe_code)]

use core::ffi::c_void;
use core::ptr;

use crate::Error;

const SEC_E_OK: i32 = 0;
const SEC_I_CONTINUE_NEEDED: i32 = 0x0009_0312;
const SEC_I_COMPLETE_NEEDED: i32 = 0x0009_0313;
const SEC_I_COMPLETE_AND_CONTINUE: i32 = 0x0009_0314;
const SEC_E_INVALID_HANDLE: i32 = 0x8009_0301_u32 as i32;
const SEC_E_INVALID_TOKEN: i32 = 0x8009_0308_u32 as i32;

const SECPKG_CRED_OUTBOUND: u32 = 2;
const SECURITY_NETWORK_DREP: u32 = 0;
const SECBUFFER_VERSION: u32 = 0;
const SECBUFFER_TOKEN: u32 = 2;

/// The context requirements Microsoft.Data.SqlClient asks for (delegation, mutual
/// authentication, connection semantics, confidentiality); SSPI allocates the tokens.
const ISC_REQ_FLAGS: u32 = 0x0000_0001 // DELEGATE
    | 0x0000_0002 // MUTUAL_AUTH
    | 0x0000_0010 // CONFIDENTIALITY
    | 0x0000_0100 // ALLOCATE_MEMORY
    | 0x0000_0800; // CONNECTION

const COMPUTER_NAME_DNS_FULLY_QUALIFIED: i32 = 3;
const AI_CANONNAME: i32 = 0x2;
const SOCK_STREAM: i32 = 1;
/// Longest name read back from Windows (DNS names are at most 255 characters).
const MAX_NAME: usize = 1024;

#[repr(C)]
#[derive(Clone, Copy, Default)]
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

/// `ADDRINFOW`.
#[repr(C)]
struct AddrInfoW {
    flags: i32,
    family: i32,
    socktype: i32,
    protocol: i32,
    addrlen: usize,
    canonname: *mut u16,
    addr: *mut c_void,
    next: *mut AddrInfoW,
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
    fn CompleteAuthToken(ctx: *mut SecHandle, token: *mut SecBufferDesc) -> i32;
    fn DeleteSecurityContext(ctx: *mut SecHandle) -> i32;
    fn FreeContextBuffer(buf: *mut c_void) -> i32;
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetComputerNameExW(name_type: i32, buffer: *mut u16, size: *mut u32) -> i32;
}

#[link(name = "ws2_32")]
unsafe extern "system" {
    fn GetAddrInfoW(
        node: *const u16,
        service: *const u16,
        hints: *const AddrInfoW,
        result: *mut *mut AddrInfoW,
    ) -> i32;
    fn FreeAddrInfoW(info: *mut AddrInfoW);
}

/// NUL-terminated UTF-16, or `None` if `s` contains a NUL.
fn wide(s: &str) -> Option<Vec<u16>> {
    if s.contains('\0') {
        return None;
    }
    Some(s.encode_utf16().chain(Some(0)).collect())
}

/// A Negotiate client context and the credentials it uses.
pub(crate) struct Context {
    cred: SecHandle,
    ctx: Option<SecHandle>,
    target: Vec<u16>,
    done: bool,
}

impl Drop for Context {
    fn drop(&mut self) {
        if let Some(mut c) = self.ctx.take() {
            // SAFETY: the handle was created by InitializeSecurityContextW and is deleted
            // exactly once (taken out of the Option).
            unsafe { DeleteSecurityContext(&mut c) };
        }
        // SAFETY: the handle was returned by AcquireCredentialsHandleW (a Context only
        // exists after it succeeded) and is freed exactly once, here.
        unsafe { FreeCredentialsHandle(&mut self.cred) };
    }
}

impl Context {
    /// `AcquireCredentialsHandleW(NULL, "Negotiate", SECPKG_CRED_OUTBOUND)`: the logged-on
    /// user's credentials.
    pub(crate) fn acquire() -> Result<Context, Error> {
        let package: Vec<u16> = "Negotiate\0".encode_utf16().collect();
        let mut cred = SecHandle::default();
        let mut expiry = 0i64;
        // SAFETY: package is NUL-terminated and outlives the call; principal, logon id and
        // auth data are NULL (current user, default credentials); cred and expiry are
        // writable locals.
        let st = unsafe {
            AcquireCredentialsHandleW(
                ptr::null(),
                package.as_ptr(),
                SECPKG_CRED_OUTBOUND,
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null_mut(),
                &mut cred,
                &mut expiry,
            )
        };
        if st != SEC_E_OK {
            return Err(Error::Sspi {
                function: "AcquireCredentialsHandleW",
                status: st,
            });
        }
        Ok(Context {
            cred,
            ctx: None,
            target: Vec::new(),
            done: false,
        })
    }

    /// First `InitializeSecurityContextW` call for the SPN `target`; returns the token
    /// for LOGIN7.
    pub(crate) fn initialize(&mut self, target: &str) -> Result<Vec<u8>, Error> {
        self.target = wide(target).ok_or(Error::Config {
            key: "serverspn".into(),
            message: "the server SPN contains a NUL character".into(),
        })?;
        self.step(None)
    }

    /// Next `InitializeSecurityContextW` call with the server's token.
    pub(crate) fn next(&mut self, input: &[u8]) -> Result<Vec<u8>, Error> {
        self.step(Some(input))
    }

    /// `true` once SSPI returned `SEC_E_OK` (or `SEC_I_COMPLETE_NEEDED`).
    pub(crate) fn is_done(&self) -> bool {
        self.done
    }

    fn step(&mut self, input: Option<&[u8]>) -> Result<Vec<u8>, Error> {
        let isc_err = |status| Error::Sspi {
            function: "InitializeSecurityContextW",
            status,
        };
        if input.is_some() && self.ctx.is_none() {
            // A continuation without a context: the first call failed or never happened.
            return Err(isc_err(SEC_E_INVALID_HANDLE));
        }
        // SSPI takes the input buffer as mutable; give it a private copy.
        let mut in_data: Vec<u8> = input.map(<[u8]>::to_vec).unwrap_or_default();
        let mut in_buf = SecBuffer {
            cb: u32::try_from(in_data.len()).map_err(|_| isc_err(SEC_E_INVALID_TOKEN))?,
            kind: SECBUFFER_TOKEN,
            pv: in_data.as_mut_ptr().cast(),
        };
        let mut in_desc = SecBufferDesc {
            version: SECBUFFER_VERSION,
            count: 1,
            buffers: &raw mut in_buf,
        };
        let mut out_buf = SecBuffer {
            cb: 0,
            kind: SECBUFFER_TOKEN,
            pv: ptr::null_mut(),
        };
        let mut out_desc = SecBufferDesc {
            version: SECBUFFER_VERSION,
            count: 1,
            buffers: &raw mut out_buf,
        };
        let mut new_ctx = SecHandle::default();
        let mut attrs = 0u32;
        let mut expiry = 0i64;
        let existing: *mut SecHandle = match self.ctx.as_mut() {
            Some(c) => c,
            None => ptr::null_mut(),
        };
        let out_ctx: *mut SecHandle = if existing.is_null() {
            &raw mut new_ctx
        } else {
            existing
        };
        let input_desc: *mut SecBufferDesc = if input.is_some() {
            &raw mut in_desc
        } else {
            ptr::null_mut()
        };
        // SAFETY: cred is a valid credentials handle; existing is NULL (first call) or the
        // context created by the first call; target is NUL-terminated; the input
        // descriptor (or NULL) points to one TOKEN buffer over `in_data`, which outlives
        // the call; the output descriptor has one TOKEN buffer that SSPI fills with memory
        // it allocates (ISC_REQ_ALLOCATE_MEMORY), freed below with FreeContextBuffer.
        let st = unsafe {
            InitializeSecurityContextW(
                &mut self.cred,
                existing,
                self.target.as_ptr(),
                ISC_REQ_FLAGS,
                0,
                SECURITY_NETWORK_DREP,
                input_desc,
                0,
                out_ctx,
                &mut out_desc,
                &mut attrs,
                &mut expiry,
            )
        };
        if st >= 0 && self.ctx.is_none() {
            self.ctx = Some(new_ctx);
        }
        let mut st = st;
        if (st == SEC_I_COMPLETE_NEEDED || st == SEC_I_COMPLETE_AND_CONTINUE)
            && let Some(c) = self.ctx.as_mut()
        {
            // SAFETY: c is the live context; out_desc still describes the token SSPI just
            // produced (not freed yet).
            let cst = unsafe { CompleteAuthToken(c, &mut out_desc) };
            if cst < 0 {
                st = cst;
            }
        }
        let token = take_allocated(&out_buf);
        match st {
            SEC_E_OK | SEC_I_COMPLETE_NEEDED => {
                self.done = true;
                Ok(token)
            }
            SEC_I_CONTINUE_NEEDED | SEC_I_COMPLETE_AND_CONTINUE => Ok(token),
            other => Err(isc_err(other)),
        }
    }
}

/// Copies and frees a buffer SSPI allocated.
fn take_allocated(b: &SecBuffer) -> Vec<u8> {
    if b.pv.is_null() {
        return Vec::new();
    }
    // SAFETY: SSPI returned `cb` initialised bytes at the non-NULL `pv`.
    let v = unsafe { core::slice::from_raw_parts(b.pv.cast::<u8>(), b.cb as usize) }.to_vec();
    // SAFETY: the buffer was allocated by the security package and is freed exactly once.
    unsafe { FreeContextBuffer(b.pv) };
    v
}

/// The computer's fully qualified DNS name (`GetComputerNameExW
/// (ComputerNameDnsFullyQualified)`), e.g. `pc1.corp.example`, or just the host name on a
/// machine outside a domain.
pub(crate) fn local_dns_name() -> Option<String> {
    let mut size = 0u32;
    // SAFETY: a NULL buffer with size 0 only asks for the required size (in characters,
    // including the terminator), written to `size`.
    let _ = unsafe {
        GetComputerNameExW(
            COMPUTER_NAME_DNS_FULLY_QUALIFIED,
            ptr::null_mut(),
            &mut size,
        )
    };
    if size == 0 || size as usize > MAX_NAME {
        return None;
    }
    let mut buf = vec![0u16; size as usize];
    let mut n = size;
    // SAFETY: buf holds `n` writable u16s; on success `n` becomes the name's length
    // without the terminator.
    let ok =
        unsafe { GetComputerNameExW(COMPUTER_NAME_DNS_FULLY_QUALIFIED, buf.as_mut_ptr(), &mut n) };
    if ok == 0 || n as usize >= buf.len() {
        return None;
    }
    String::from_utf16(&buf[..n as usize])
        .ok()
        .filter(|s| !s.is_empty())
}

/// The canonical DNS name of `host` (`GetAddrInfoW` with `AI_CANONNAME`), as
/// SqlClient's `Dns.GetHostEntry(host).HostName`. Called after the TCP connection exists,
/// so Winsock is initialised.
pub(crate) fn canonical_name(host: &str) -> Option<String> {
    let node = wide(host)?;
    let hints = AddrInfoW {
        flags: AI_CANONNAME,
        family: 0,
        socktype: SOCK_STREAM,
        protocol: 0,
        addrlen: 0,
        canonname: ptr::null_mut(),
        addr: ptr::null_mut(),
        next: ptr::null_mut(),
    };
    let mut res: *mut AddrInfoW = ptr::null_mut();
    // SAFETY: node is NUL-terminated; the service is NULL; hints is a valid ADDRINFOW with
    // only flags/socktype set; res receives a list we free with FreeAddrInfoW.
    let rc = unsafe { GetAddrInfoW(node.as_ptr(), ptr::null(), &hints, &mut res) };
    if rc != 0 || res.is_null() {
        return None;
    }
    // SAFETY: res is the non-NULL list GetAddrInfoW returned, valid until FreeAddrInfoW;
    // ai_canonname of the first entry is NULL or a NUL-terminated string inside it, read
    // up to MAX_NAME characters.
    let name = unsafe {
        let p = (*res).canonname;
        if p.is_null() {
            None
        } else {
            let mut n = 0;
            while n < MAX_NAME && *p.add(n) != 0 {
                n += 1;
            }
            String::from_utf16(core::slice::from_raw_parts(p, n)).ok()
        }
    };
    // SAFETY: frees the list GetAddrInfoW allocated, exactly once.
    unsafe { FreeAddrInfoW(res) };
    name.filter(|n| !n.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::mem::size_of;

    #[test]
    fn layouts() {
        let p = size_of::<usize>();
        assert_eq!(size_of::<SecHandle>(), 2 * p);
        assert_eq!(size_of::<SecBuffer>(), 8 + p);
        assert_eq!(size_of::<SecBufferDesc>(), 8 + p);
        // ADDRINFOW: 4 ints, size_t, 3 pointers.
        assert_eq!(size_of::<AddrInfoW>(), 16 + 4 * p);
    }

    #[test]
    fn wide_strings() {
        assert_eq!(wide("ab"), Some(vec![0x61, 0x62, 0]));
        assert_eq!(wide("a\0b"), None);
    }

    #[test]
    fn canonical_name_of_localhost() {
        // Make sure Winsock is initialised, as it is after Client::connect dialled.
        let _ = std::net::ToSocketAddrs::to_socket_addrs(&("localhost", 1));
        let n = canonical_name("localhost");
        assert!(n.as_deref().is_none_or(|n| !n.is_empty()));
        assert_eq!(canonical_name("bad\0name"), None);
    }
}
