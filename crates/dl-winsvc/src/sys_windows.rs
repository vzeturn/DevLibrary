//! Win32 declarations and the safe wrappers the rest of the crate uses. This is the only
//! file in the crate that contains `unsafe`.
#![allow(unsafe_code)]

use core::ffi::c_void;
use core::mem::size_of;
use core::ptr;

use crate::Error;
use crate::process::Process;
use crate::raw::{HandlerFn, RawConfig, RawService, ServiceMainFn, ServiceStatus};

type Handle = *mut c_void;

const ERROR_INSUFFICIENT_BUFFER: u32 = 122;
const ERROR_MORE_DATA: u32 = 234;
const STATUS_INFO_LENGTH_MISMATCH: i32 = 0xC000_0004_u32 as i32;
const SERVICE_CONFIG_DESCRIPTION: u32 = 1;
const SC_ENUM_PROCESS_INFO: u32 = 0;
const SERVICE_WIN32: u32 = 0x30;
const SERVICE_STATE_ALL: u32 = 3;
const SERVICE_NO_CHANGE: u32 = 0xFFFF_FFFF;
const PROCESS_BASIC_INFORMATION_CLASS: u32 = 0;
const SYSTEM_PROCESS_INFORMATION_CLASS: u32 = 5;
/// Upper bound for the process list buffer; a real system needs well under 4 MiB.
const MAX_PROCESS_BUFFER: usize = 64 << 20;

#[repr(C)]
#[derive(Clone, Copy)]
struct QueryServiceConfigW {
    service_type: u32,
    start_type: u32,
    error_control: u32,
    binary_path_name: *const u16,
    load_order_group: *const u16,
    tag_id: u32,
    dependencies: *const u16,
    service_start_name: *const u16,
    display_name: *const u16,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct ServiceDescriptionW {
    description: *const u16,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct ServiceStatusProcess {
    service_type: u32,
    current_state: u32,
    controls_accepted: u32,
    win32_exit_code: u32,
    service_specific_exit_code: u32,
    check_point: u32,
    wait_hint: u32,
    process_id: u32,
    service_flags: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct EnumServiceStatusProcessW {
    service_name: *const u16,
    display_name: *const u16,
    status: ServiceStatusProcess,
}

#[repr(C)]
struct ServiceTableEntryW {
    service_name: *const u16,
    service_proc: Option<ServiceMainFn>,
}

#[repr(C)]
struct DataBlob {
    cb_data: u32,
    pb_data: *mut u8,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct ProcessBasicInformation {
    exit_status: i32,
    peb_base_address: *mut c_void,
    affinity_mask: usize,
    base_priority: i32,
    unique_process_id: usize,
    inherited_from_unique_process_id: usize,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct UnicodeString {
    length: u16,
    maximum_length: u16,
    buffer: *const u16,
}

/// The leading fields of `SYSTEM_PROCESS_INFORMATION` (layout from winternl.h; the same
/// declaration gives the right offsets on 32- and 64-bit Windows).
#[repr(C)]
#[derive(Clone, Copy)]
struct SystemProcessInformation {
    next_entry_offset: u32,
    number_of_threads: u32,
    working_set_private_size: i64,
    hard_fault_count: u32,
    number_of_threads_high_watermark: u32,
    cycle_time: u64,
    create_time: i64,
    user_time: i64,
    kernel_time: i64,
    image_name: UnicodeString,
    base_priority: i32,
    unique_process_id: usize,
    inherited_from_unique_process_id: usize,
    handle_count: u32,
    session_id: u32,
}

#[link(name = "advapi32")]
unsafe extern "system" {
    fn OpenSCManagerW(machine: *const u16, database: *const u16, access: u32) -> Handle;
    fn CloseServiceHandle(h: Handle) -> i32;
    fn OpenServiceW(scm: Handle, name: *const u16, access: u32) -> Handle;
    fn CreateServiceW(
        scm: Handle,
        name: *const u16,
        display: *const u16,
        access: u32,
        service_type: u32,
        start_type: u32,
        error_control: u32,
        binary_path: *const u16,
        load_order_group: *const u16,
        tag_id: *mut u32,
        dependencies: *const u16,
        start_name: *const u16,
        password: *const u16,
    ) -> Handle;
    fn ChangeServiceConfigW(
        h: Handle,
        service_type: u32,
        start_type: u32,
        error_control: u32,
        binary_path: *const u16,
        load_order_group: *const u16,
        tag_id: *mut u32,
        dependencies: *const u16,
        start_name: *const u16,
        password: *const u16,
        display: *const u16,
    ) -> i32;
    fn ChangeServiceConfig2W(h: Handle, level: u32, info: *const c_void) -> i32;
    fn QueryServiceConfigW(h: Handle, config: *mut u8, size: u32, needed: *mut u32) -> i32;
    fn QueryServiceConfig2W(
        h: Handle,
        level: u32,
        buf: *mut u8,
        size: u32,
        needed: *mut u32,
    ) -> i32;
    fn QueryServiceStatus(h: Handle, status: *mut ServiceStatus) -> i32;
    fn StartServiceW(h: Handle, argc: u32, argv: *const *const u16) -> i32;
    fn ControlService(h: Handle, control: u32, status: *mut ServiceStatus) -> i32;
    fn DeleteService(h: Handle) -> i32;
    fn EnumServicesStatusExW(
        scm: Handle,
        level: u32,
        service_type: u32,
        state: u32,
        buf: *mut u8,
        size: u32,
        needed: *mut u32,
        returned: *mut u32,
        resume: *mut u32,
        group: *const u16,
    ) -> i32;
    fn StartServiceCtrlDispatcherW(table: *const ServiceTableEntryW) -> i32;
    fn RegisterServiceCtrlHandlerExW(
        name: *const u16,
        handler: HandlerFn,
        ctx: *mut c_void,
    ) -> Handle;
    fn SetServiceStatus(h: Handle, status: *const ServiceStatus) -> i32;
}

#[link(name = "crypt32")]
unsafe extern "system" {
    fn CryptProtectData(
        input: *const DataBlob,
        description: *const u16,
        entropy: *const DataBlob,
        reserved: *mut c_void,
        prompt: *const c_void,
        flags: u32,
        output: *mut DataBlob,
    ) -> i32;
    fn CryptUnprotectData(
        input: *const DataBlob,
        description: *mut *mut u16,
        entropy: *const DataBlob,
        reserved: *mut c_void,
        prompt: *const c_void,
        flags: u32,
        output: *mut DataBlob,
    ) -> i32;
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn LocalFree(mem: *mut c_void) -> *mut c_void;
}

#[link(name = "ntdll")]
unsafe extern "system" {
    fn NtQueryInformationProcess(
        process: Handle,
        class: u32,
        info: *mut c_void,
        len: u32,
        ret_len: *mut u32,
    ) -> i32;
    fn NtQuerySystemInformation(class: u32, info: *mut c_void, len: u32, ret_len: *mut u32) -> i32;
}

fn last_error(op: &'static str) -> Error {
    let code = std::io::Error::last_os_error().raw_os_error().unwrap_or(0) as u32;
    Error::Win32 { op, code }
}

fn last_code() -> u32 {
    std::io::Error::last_os_error().raw_os_error().unwrap_or(0) as u32
}

fn check(ok: i32, op: &'static str) -> Result<(), Error> {
    if ok == 0 { Err(last_error(op)) } else { Ok(()) }
}

fn len_u32(len: usize, what: &'static str) -> Result<u32, Error> {
    u32::try_from(len).map_err(|_| Error::InvalidInput(what))
}

/// A buffer for Win32 calls that return variable-size structures: allocated as `u64` so it
/// is 8-byte aligned, as those structures require.
struct AlignedBuf(Vec<u64>);

impl AlignedBuf {
    fn new(bytes: usize) -> AlignedBuf {
        AlignedBuf(vec![0; bytes.div_ceil(8).max(1)])
    }

    fn len(&self) -> usize {
        self.0.len() * 8
    }

    fn as_mut_ptr(&mut self) -> *mut u8 {
        self.0.as_mut_ptr().cast()
    }

    fn bytes(&self) -> &[u8] {
        // SAFETY: the pointer and length describe exactly the Vec's initialised allocation;
        // u8 has alignment 1 and every bit pattern is a valid u8.
        unsafe { core::slice::from_raw_parts(self.0.as_ptr().cast::<u8>(), self.len()) }
    }
}

/// Reads a `T` at byte offset `off` of `buf`, or `None` if it does not fit.
///
/// Only used with the `#[repr(C)]` structs above, whose fields are integers and raw
/// pointers: every bit pattern is a valid value for them.
fn read_pod<T: Copy>(buf: &[u8], off: usize) -> Option<T> {
    let end = off.checked_add(size_of::<T>())?;
    if end > buf.len() {
        return None;
    }
    // SAFETY: `off..end` is inside `buf` (checked above); read_unaligned has no alignment
    // requirement; T is a plain-data struct for which any bytes are a valid value.
    Some(unsafe { ptr::read_unaligned(buf.as_ptr().add(off).cast::<T>()) })
}

/// The NUL-terminated (or `max_units`-long) UTF-16 string that `p` points to, provided `p`
/// points inside `buf`. Win32 returns string pointers into the caller's buffer; anything
/// else (including NULL) gives an empty string. Reads through `buf`, never through `p`.
fn wide_in_buf(buf: &[u8], p: *const u16, max_units: Option<usize>) -> String {
    let base = buf.as_ptr() as usize;
    let addr = p as usize;
    if p.is_null() || addr < base || addr >= base + buf.len() {
        return String::new();
    }
    let mut units = Vec::new();
    for pair in buf[addr - base..].as_chunks::<2>().0 {
        if max_units.is_some_and(|m| units.len() >= m) {
            break;
        }
        let u = u16::from_ne_bytes([pair[0], pair[1]]);
        if u == 0 && max_units.is_none() {
            break;
        }
        units.push(u);
    }
    String::from_utf16_lossy(&units)
}

fn opt(s: Option<&[u16]>) -> *const u16 {
    s.map_or(ptr::null(), <[u16]>::as_ptr)
}

/// An owned SCM or service handle, closed on drop.
pub(crate) struct ScHandle(Handle);

// SAFETY: SCM and service handles are not tied to the thread that opened them; the SCM
// functions may be called with them from any thread.
unsafe impl Send for ScHandle {}
// SAFETY: as above; the SCM serialises concurrent calls on one handle itself.
unsafe impl Sync for ScHandle {}

impl Drop for ScHandle {
    fn drop(&mut self) {
        // SAFETY: self.0 is a valid handle owned by this value and closed exactly once.
        unsafe { CloseServiceHandle(self.0) };
    }
}

pub(crate) fn open_sc_manager(access: u32) -> Result<ScHandle, Error> {
    // SAFETY: NULL machine and database select the local active database.
    let h = unsafe { OpenSCManagerW(ptr::null(), ptr::null(), access) };
    if h.is_null() {
        Err(last_error("OpenSCManagerW"))
    } else {
        Ok(ScHandle(h))
    }
}

pub(crate) fn open_service(scm: &ScHandle, name: &[u16], access: u32) -> Result<ScHandle, Error> {
    // SAFETY: scm is a valid handle; name is NUL-terminated (built by wide::to_wide).
    let h = unsafe { OpenServiceW(scm.0, name.as_ptr(), access) };
    if h.is_null() {
        Err(last_error("OpenServiceW"))
    } else {
        Ok(ScHandle(h))
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn create_service(
    scm: &ScHandle,
    name: &[u16],
    display: &[u16],
    access: u32,
    service_type: u32,
    start_type: u32,
    error_control: u32,
    binary_path: &[u16],
) -> Result<ScHandle, Error> {
    // SAFETY: all string pointers are NUL-terminated buffers that outlive the call; NULL
    // optional arguments mean "none" (no group, no dependencies, LocalSystem account).
    let h = unsafe {
        CreateServiceW(
            scm.0,
            name.as_ptr(),
            display.as_ptr(),
            access,
            service_type,
            start_type,
            error_control,
            binary_path.as_ptr(),
            ptr::null(),
            ptr::null_mut(),
            ptr::null(),
            ptr::null(),
            ptr::null(),
        )
    };
    if h.is_null() {
        Err(last_error("CreateServiceW"))
    } else {
        Ok(ScHandle(h))
    }
}

pub(crate) fn change_service_config(
    h: &ScHandle,
    service_type: u32,
    start_type: u32,
    error_control: u32,
    binary_path: Option<&[u16]>,
    display: Option<&[u16]>,
) -> Result<(), Error> {
    // SAFETY: h is valid; strings are NUL-terminated or NULL (= no change).
    let ok = unsafe {
        ChangeServiceConfigW(
            h.0,
            service_type,
            start_type,
            error_control,
            opt(binary_path),
            ptr::null(),
            ptr::null_mut(),
            ptr::null(),
            ptr::null(),
            ptr::null(),
            opt(display),
        )
    };
    check(ok, "ChangeServiceConfigW")
}

pub(crate) const NO_CHANGE: u32 = SERVICE_NO_CHANGE;

pub(crate) fn set_description(h: &ScHandle, description: &[u16]) -> Result<(), Error> {
    let info = ServiceDescriptionW {
        description: description.as_ptr(),
    };
    // SAFETY: h is valid; info points to a SERVICE_DESCRIPTIONW whose string is
    // NUL-terminated and outlives the call.
    let ok =
        unsafe { ChangeServiceConfig2W(h.0, SERVICE_CONFIG_DESCRIPTION, (&raw const info).cast()) };
    check(ok, "ChangeServiceConfig2W")
}

pub(crate) fn query_config(h: &ScHandle) -> Result<RawConfig, Error> {
    let mut needed = 0u32;
    // SAFETY: a NULL buffer with size 0 only asks for the required size.
    let ok = unsafe { QueryServiceConfigW(h.0, ptr::null_mut(), 0, &mut needed) };
    if ok == 0 && last_code() != ERROR_INSUFFICIENT_BUFFER {
        return Err(last_error("QueryServiceConfigW"));
    }
    let mut buf = AlignedBuf::new((needed as usize).max(size_of::<QueryServiceConfigW>()));
    let size = len_u32(buf.len(), "buffer too large")?;
    // SAFETY: buf is writable for `size` bytes and 8-byte aligned.
    let ok = unsafe { QueryServiceConfigW(h.0, buf.as_mut_ptr(), size, &mut needed) };
    check(ok, "QueryServiceConfigW")?;
    let bytes = buf.bytes();
    let c: QueryServiceConfigW = read_pod(bytes, 0).ok_or(Error::Win32 {
        op: "QueryServiceConfigW",
        code: 0,
    })?;
    Ok(RawConfig {
        start_type: c.start_type,
        binary_path_name: wide_in_buf(bytes, c.binary_path_name, None),
        display_name: wide_in_buf(bytes, c.display_name, None),
        start_name: wide_in_buf(bytes, c.service_start_name, None),
    })
}

pub(crate) fn query_description(h: &ScHandle) -> Result<String, Error> {
    let mut needed = 0u32;
    // SAFETY: a NULL buffer with size 0 only asks for the required size.
    let ok = unsafe {
        QueryServiceConfig2W(
            h.0,
            SERVICE_CONFIG_DESCRIPTION,
            ptr::null_mut(),
            0,
            &mut needed,
        )
    };
    if ok == 0 && last_code() != ERROR_INSUFFICIENT_BUFFER {
        return Err(last_error("QueryServiceConfig2W"));
    }
    let mut buf = AlignedBuf::new((needed as usize).max(size_of::<ServiceDescriptionW>()));
    let size = len_u32(buf.len(), "buffer too large")?;
    // SAFETY: buf is writable for `size` bytes and 8-byte aligned.
    let ok = unsafe {
        QueryServiceConfig2W(
            h.0,
            SERVICE_CONFIG_DESCRIPTION,
            buf.as_mut_ptr(),
            size,
            &mut needed,
        )
    };
    check(ok, "QueryServiceConfig2W")?;
    let bytes = buf.bytes();
    let d: ServiceDescriptionW = read_pod(bytes, 0).ok_or(Error::Win32 {
        op: "QueryServiceConfig2W",
        code: 0,
    })?;
    Ok(wide_in_buf(bytes, d.description, None))
}

pub(crate) fn query_status(h: &ScHandle) -> Result<ServiceStatus, Error> {
    let mut st = ServiceStatus::default();
    // SAFETY: h is valid; st is a writable SERVICE_STATUS.
    let ok = unsafe { QueryServiceStatus(h.0, &mut st) };
    check(ok, "QueryServiceStatus")?;
    Ok(st)
}

pub(crate) fn start_service(h: &ScHandle, args: &[Vec<u16>]) -> Result<(), Error> {
    let ptrs: Vec<*const u16> = args.iter().map(|a| a.as_ptr()).collect();
    let argc = len_u32(ptrs.len(), "too many arguments")?;
    let argv = if ptrs.is_empty() {
        ptr::null()
    } else {
        ptrs.as_ptr()
    };
    // SAFETY: argv points to `argc` NUL-terminated strings that outlive the call, or is
    // NULL with argc 0.
    let ok = unsafe { StartServiceW(h.0, argc, argv) };
    check(ok, "StartServiceW")
}

pub(crate) fn control_service(h: &ScHandle, control: u32) -> Result<ServiceStatus, Error> {
    let mut st = ServiceStatus::default();
    // SAFETY: h is valid; st is a writable SERVICE_STATUS.
    let ok = unsafe { ControlService(h.0, control, &mut st) };
    check(ok, "ControlService")?;
    Ok(st)
}

pub(crate) fn delete_service(h: &ScHandle) -> Result<(), Error> {
    // SAFETY: h is a valid service handle.
    let ok = unsafe { DeleteService(h.0) };
    check(ok, "DeleteService")
}

pub(crate) fn enum_services(scm: &ScHandle) -> Result<Vec<RawService>, Error> {
    let mut out = Vec::new();
    let mut resume = 0u32;
    let mut buf = AlignedBuf::new(64 << 10);
    loop {
        let mut needed = 0u32;
        let mut returned = 0u32;
        let size = len_u32(buf.len(), "buffer too large")?;
        // SAFETY: buf is writable for `size` bytes and aligned; the out-parameters are valid.
        let ok = unsafe {
            EnumServicesStatusExW(
                scm.0,
                SC_ENUM_PROCESS_INFO,
                SERVICE_WIN32,
                SERVICE_STATE_ALL,
                buf.as_mut_ptr(),
                size,
                &mut needed,
                &mut returned,
                &mut resume,
                ptr::null(),
            )
        };
        let more = ok == 0 && last_code() == ERROR_MORE_DATA;
        if ok == 0 && !more {
            return Err(last_error("EnumServicesStatusExW"));
        }
        let bytes = buf.bytes();
        let step = size_of::<EnumServiceStatusProcessW>();
        for i in 0..returned as usize {
            let Some(e) = read_pod::<EnumServiceStatusProcessW>(bytes, i * step) else {
                break;
            };
            let s = e.status;
            out.push(RawService {
                name: wide_in_buf(bytes, e.service_name, None),
                display_name: wide_in_buf(bytes, e.display_name, None),
                status: ServiceStatus {
                    service_type: s.service_type,
                    current_state: s.current_state,
                    controls_accepted: s.controls_accepted,
                    win32_exit_code: s.win32_exit_code,
                    service_specific_exit_code: s.service_specific_exit_code,
                    check_point: s.check_point,
                    wait_hint: s.wait_hint,
                },
            });
        }
        if !more {
            return Ok(out);
        }
        if returned == 0 && (needed as usize) > buf.len() {
            buf = AlignedBuf::new(needed as usize);
        }
    }
}

pub(crate) fn crypt_protect(data: &[u8], flags: u32, entropy: &[u8]) -> Result<Vec<u8>, Error> {
    blob_call(data, flags, true, entropy)
}

pub(crate) fn crypt_unprotect(data: &[u8], flags: u32, entropy: &[u8]) -> Result<Vec<u8>, Error> {
    blob_call(data, flags, false, entropy)
}

fn blob_call(data: &[u8], flags: u32, protect: bool, entropy: &[u8]) -> Result<Vec<u8>, Error> {
    // DPAPI rejects a NULL data pointer even with length 0, so empty input points at a
    // dummy byte.
    let dummy = [0u8; 1];
    let src = if data.is_empty() { &dummy[..] } else { data };
    let input = DataBlob {
        cb_data: len_u32(data.len(), "data too large")?,
        pb_data: src.as_ptr().cast_mut(),
    };
    let entropy_blob = DataBlob {
        cb_data: len_u32(entropy.len(), "entropy too large")?,
        pb_data: entropy.as_ptr().cast_mut(),
    };
    // No entropy is a NULL pointer, as callers without entropy pass.
    let entropy_ptr: *const DataBlob = if entropy.is_empty() {
        ptr::null()
    } else {
        &entropy_blob
    };
    let mut output = DataBlob {
        cb_data: 0,
        pb_data: ptr::null_mut(),
    };
    // SAFETY: input (and entropy, when not NULL) describe `cb_data` readable bytes that
    // outlive the call (DPAPI does not write to them); output receives a LocalAlloc'ed
    // buffer, freed below; the other pointers are NULL.
    let ok = unsafe {
        if protect {
            CryptProtectData(
                &input,
                ptr::null(),
                entropy_ptr,
                ptr::null_mut(),
                ptr::null(),
                flags,
                &mut output,
            )
        } else {
            CryptUnprotectData(
                &input,
                ptr::null_mut(),
                entropy_ptr,
                ptr::null_mut(),
                ptr::null(),
                flags,
                &mut output,
            )
        }
    };
    if ok == 0 {
        return Err(last_error(if protect {
            "CryptProtectData"
        } else {
            "CryptUnprotectData"
        }));
    }
    let out = if output.pb_data.is_null() || output.cb_data == 0 {
        Vec::new()
    } else {
        // SAFETY: on success DPAPI returns `cb_data` initialised bytes at `pb_data`.
        unsafe { core::slice::from_raw_parts(output.pb_data, output.cb_data as usize) }.to_vec()
    };
    if !output.pb_data.is_null() {
        // SAFETY: pb_data was allocated by DPAPI with LocalAlloc and is freed exactly once.
        unsafe { LocalFree(output.pb_data.cast()) };
    }
    Ok(out)
}

/// Whether the parent of this process is `services.exe` in session 0: the check .NET's
/// `WindowsServiceHelpers.IsWindowsService` and Go's `svc.IsWindowsService` use.
pub(crate) fn parent_is_services() -> Result<bool, Error> {
    let mut pbi = ProcessBasicInformation {
        exit_status: 0,
        peb_base_address: ptr::null_mut(),
        affinity_mask: 0,
        base_priority: 0,
        unique_process_id: 0,
        inherited_from_unique_process_id: 0,
    };
    let mut ret = 0u32;
    // SAFETY: -1 is the current-process pseudo handle; pbi is a writable buffer of the
    // size passed.
    let st = unsafe {
        NtQueryInformationProcess(
            -1isize as Handle,
            PROCESS_BASIC_INFORMATION_CLASS,
            (&raw mut pbi).cast(),
            size_of::<ProcessBasicInformation>() as u32,
            &mut ret,
        )
    };
    if st < 0 {
        return Err(Error::Win32 {
            op: "NtQueryInformationProcess",
            code: st as u32,
        });
    }
    let parent = u32::try_from(pbi.inherited_from_unique_process_id).unwrap_or(u32::MAX);
    Ok(processes()?
        .iter()
        .find(|p| p.pid == parent)
        .is_some_and(|p| p.session_id == 0 && p.name.eq_ignore_ascii_case("services.exe")))
}

/// Every running process, from one `NtQuerySystemInformation(SystemProcessInformation)` call.
pub(crate) fn processes() -> Result<Vec<Process>, Error> {
    let mut buf = AlignedBuf::new(512 << 10);
    loop {
        let size = len_u32(buf.len(), "buffer too large")?;
        let mut needed = 0u32;
        // SAFETY: buf is writable for `size` bytes and 8-byte aligned.
        let st = unsafe {
            NtQuerySystemInformation(
                SYSTEM_PROCESS_INFORMATION_CLASS,
                buf.as_mut_ptr().cast(),
                size,
                &mut needed,
            )
        };
        if st == STATUS_INFO_LENGTH_MISMATCH {
            let next = (needed as usize).max(buf.len() * 2) + (64 << 10);
            if next > MAX_PROCESS_BUFFER {
                return Err(Error::Win32 {
                    op: "NtQuerySystemInformation",
                    code: st as u32,
                });
            }
            buf = AlignedBuf::new(next);
            continue;
        }
        if st < 0 {
            return Err(Error::Win32 {
                op: "NtQuerySystemInformation",
                code: st as u32,
            });
        }
        break;
    }
    let bytes = buf.bytes();
    let mut out = Vec::new();
    let mut off = 0usize;
    while let Some(e) = read_pod::<SystemProcessInformation>(bytes, off) {
        let units = usize::from(e.image_name.length / 2);
        out.push(Process {
            pid: u32::try_from(e.unique_process_id).unwrap_or(u32::MAX),
            parent_pid: u32::try_from(e.inherited_from_unique_process_id).unwrap_or(u32::MAX),
            session_id: e.session_id,
            name: wide_in_buf(bytes, e.image_name.buffer, Some(units)),
        });
        if e.next_entry_offset == 0 {
            break;
        }
        off = match off.checked_add(e.next_entry_offset as usize) {
            Some(o) => o,
            None => break,
        };
    }
    Ok(out)
}

const SERVICE_CONFIG_FAILURE_ACTIONS: u32 = 2;

#[repr(C)]
#[derive(Clone, Copy)]
struct ScAction {
    kind: u32,
    delay_ms: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct ServiceFailureActionsW {
    reset_period: u32,
    reboot_msg: *const u16,
    command: *const u16,
    actions_len: u32,
    actions: *const ScAction,
}

/// Sets `SERVICE_FAILURE_ACTIONSW`; `actions` are `(SC_ACTION_TYPE, delay ms)`.
pub(crate) fn set_failure_actions(
    h: &ScHandle,
    reset_seconds: u32,
    actions: &[(u32, u32)],
) -> Result<(), Error> {
    let list: Vec<ScAction> = actions
        .iter()
        .map(|&(kind, delay_ms)| ScAction { kind, delay_ms })
        .collect();
    let info = ServiceFailureActionsW {
        reset_period: reset_seconds,
        reboot_msg: ptr::null(),
        command: ptr::null(),
        actions_len: len_u32(list.len(), "too many actions")?,
        // Never NULL: NULL means "leave the actions unchanged", while a non-NULL pointer with
        // zero actions deletes them (an empty Vec's dangling pointer is non-NULL).
        actions: list.as_ptr(),
    };
    // SAFETY: h is valid (opened with SERVICE_ALL_ACCESS, which includes SERVICE_START as
    // restart actions need); info and the action array it points to outlive the call (with
    // zero actions the pointer is never read); NULL strings mean "unchanged".
    let ok = unsafe {
        ChangeServiceConfig2W(
            h.0,
            SERVICE_CONFIG_FAILURE_ACTIONS,
            (&raw const info).cast(),
        )
    };
    check(ok, "ChangeServiceConfig2W")
}

/// Reads `SERVICE_FAILURE_ACTIONSW`: reset period (s) and `(type, delay ms)` actions.
pub(crate) fn query_failure_actions(h: &ScHandle) -> Result<(u32, Vec<(u32, u32)>), Error> {
    let buf = query_config2(
        h,
        SERVICE_CONFIG_FAILURE_ACTIONS,
        size_of::<ServiceFailureActionsW>(),
    )?;
    let bytes = buf.bytes();
    let fa: ServiceFailureActionsW = read_pod(bytes, 0).ok_or(Error::Win32 {
        op: "QueryServiceConfig2W",
        code: 0,
    })?;
    let mut actions = Vec::new();
    let base = bytes.as_ptr() as usize;
    let at = fa.actions as usize;
    if !fa.actions.is_null() && at >= base {
        for i in 0..fa.actions_len as usize {
            let off = at - base + i * size_of::<ScAction>();
            let Some(a) = read_pod::<ScAction>(bytes, off) else {
                break;
            };
            actions.push((a.kind, a.delay_ms));
        }
    }
    Ok((fa.reset_period, actions))
}

/// Sets a `ChangeServiceConfig2W` level whose structure is a single BOOL
/// (`SERVICE_CONFIG_DELAYED_AUTO_START_INFO`, `SERVICE_CONFIG_FAILURE_ACTIONS_FLAG`).
pub(crate) fn set_bool_config(h: &ScHandle, level: u32, value: bool) -> Result<(), Error> {
    let v: i32 = value.into();
    // SAFETY: h is valid; both structures are one 32-bit BOOL, which `v` provides.
    let ok = unsafe { ChangeServiceConfig2W(h.0, level, (&raw const v).cast()) };
    check(ok, "ChangeServiceConfig2W")
}

pub(crate) fn query_bool_config(h: &ScHandle, level: u32) -> Result<bool, Error> {
    let buf = query_config2(h, level, 4)?;
    let v: i32 = read_pod(buf.bytes(), 0).ok_or(Error::Win32 {
        op: "QueryServiceConfig2W",
        code: 0,
    })?;
    Ok(v != 0)
}

/// `QueryServiceConfig2W` into an aligned buffer. Pointers in the structure point into
/// the same buffer; they are only used as offsets with `read_pod`.
fn query_config2(h: &ScHandle, level: u32, min: usize) -> Result<AlignedBuf, Error> {
    let mut needed = 0u32;
    // SAFETY: a NULL buffer with size 0 only asks for the required size.
    let ok = unsafe { QueryServiceConfig2W(h.0, level, ptr::null_mut(), 0, &mut needed) };
    if ok == 0 && last_code() != ERROR_INSUFFICIENT_BUFFER {
        return Err(last_error("QueryServiceConfig2W"));
    }
    let mut buf = AlignedBuf::new((needed as usize).max(min));
    let size = len_u32(buf.len(), "buffer too large")?;
    // SAFETY: buf is writable for `size` bytes and 8-byte aligned.
    let ok = unsafe { QueryServiceConfig2W(h.0, level, buf.as_mut_ptr(), size, &mut needed) };
    check(ok, "QueryServiceConfig2W")?;
    Ok(buf)
}

const SE_FILE_OBJECT: u32 = 1;
const OWNER_SECURITY_INFORMATION: u32 = 0x1;
const DACL_SECURITY_INFORMATION: u32 = 0x4;
const PROTECTED_DACL_SECURITY_INFORMATION: u32 = 0x8000_0000;
const SE_DACL_PROTECTED: u16 = 0x1000;
const INHERITED_ACE: u8 = 0x10;
const SDDL_REVISION_1: u32 = 1;
const WIN_BUILTIN_ADMINISTRATORS_SID: u32 = 26;

#[repr(C)]
#[derive(Clone, Copy)]
struct AclHeader {
    revision: u8,
    sbz1: u8,
    size: u16,
    ace_count: u16,
    sbz2: u16,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct AceHeader {
    kind: u8,
    flags: u8,
    size: u16,
}

#[link(name = "advapi32")]
unsafe extern "system" {
    fn GetNamedSecurityInfoW(
        name: *const u16,
        object_type: u32,
        info: u32,
        owner: *mut *mut c_void,
        group: *mut *mut c_void,
        dacl: *mut *mut c_void,
        sacl: *mut *mut c_void,
        sd: *mut *mut c_void,
    ) -> u32;
    fn SetNamedSecurityInfoW(
        name: *const u16,
        object_type: u32,
        info: u32,
        owner: *mut c_void,
        group: *mut c_void,
        dacl: *mut c_void,
        sacl: *mut c_void,
    ) -> u32;
    fn GetSecurityDescriptorControl(sd: *mut c_void, control: *mut u16, revision: *mut u32) -> i32;
    fn GetSecurityDescriptorDacl(
        sd: *mut c_void,
        present: *mut i32,
        dacl: *mut *mut c_void,
        defaulted: *mut i32,
    ) -> i32;
    fn ConvertSidToStringSidW(sid: *mut c_void, out: *mut *mut u16) -> i32;
    fn IsValidSid(sid: *mut c_void) -> i32;
    fn CreateWellKnownSid(kind: u32, domain: *mut c_void, sid: *mut c_void, size: *mut u32) -> i32;
    fn ConvertStringSecurityDescriptorToSecurityDescriptorW(
        sddl: *const u16,
        revision: u32,
        sd: *mut *mut c_void,
        size: *mut u32,
    ) -> i32;
}

/// Frees a LocalAlloc'ed pointer when dropped.
struct LocalBox(*mut c_void);

impl Drop for LocalBox {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: the pointer came from a Win32 call documented to LocalAlloc it, and is
            // freed exactly once.
            unsafe { LocalFree(self.0) };
        }
    }
}

fn sid_string(sid: *mut c_void) -> Result<String, Error> {
    let mut out: *mut u16 = ptr::null_mut();
    // SAFETY: sid points to a SID inside a security descriptor that the caller keeps
    // alive; out receives a LocalAlloc'ed NUL-terminated string.
    let ok = unsafe { ConvertSidToStringSidW(sid, &mut out) };
    check(ok, "ConvertSidToStringSidW")?;
    let guard = LocalBox(out.cast());
    let mut units = Vec::new();
    let mut i = 0;
    loop {
        // SAFETY: `out` is NUL-terminated; reading stops at the terminator.
        let u = unsafe { *out.add(i) };
        if u == 0 {
            break;
        }
        units.push(u);
        i += 1;
    }
    drop(guard);
    Ok(String::from_utf16_lossy(&units))
}

pub(crate) fn read_security(path: &[u16]) -> Result<crate::acl::Descriptor, Error> {
    let mut owner: *mut c_void = ptr::null_mut();
    let mut dacl: *mut c_void = ptr::null_mut();
    let mut sd: *mut c_void = ptr::null_mut();
    // SAFETY: path is NUL-terminated; the out-pointers point into the descriptor that is
    // returned in `sd` (LocalAlloc'ed, freed by the guard below).
    let rc = unsafe {
        GetNamedSecurityInfoW(
            path.as_ptr(),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            &mut owner,
            ptr::null_mut(),
            &mut dacl,
            ptr::null_mut(),
            &mut sd,
        )
    };
    if rc != 0 {
        return Err(Error::Win32 {
            op: "GetNamedSecurityInfoW",
            code: rc,
        });
    }
    let _guard = LocalBox(sd);
    let mut control = 0u16;
    let mut revision = 0u32;
    // SAFETY: sd is the valid descriptor returned above.
    let ok = unsafe { GetSecurityDescriptorControl(sd, &mut control, &mut revision) };
    check(ok, "GetSecurityDescriptorControl")?;
    let mut d = crate::acl::Descriptor {
        owner: String::new(),
        protected: control & SE_DACL_PROTECTED != 0,
        null_dacl: dacl.is_null(),
        aces: Vec::new(),
    };
    if !owner.is_null() {
        d.owner = sid_string(owner)?;
    }
    if dacl.is_null() {
        return Ok(d);
    }
    // SAFETY: dacl points to an ACL inside the descriptor; the header is 8 bytes.
    let header: AclHeader = unsafe { ptr::read_unaligned(dacl.cast::<AclHeader>()) };
    let acl_end = dacl as usize + usize::from(header.size);
    let mut at = dacl as usize + size_of::<AclHeader>();
    for _ in 0..header.ace_count {
        if at + size_of::<AceHeader>() > acl_end {
            break;
        }
        // SAFETY: `at` is inside the ACL (checked against its declared size).
        let ace: AceHeader = unsafe { ptr::read_unaligned(at as *const AceHeader) };
        let size = usize::from(ace.size);
        if size < size_of::<AceHeader>() || at + size > acl_end {
            break;
        }
        let mut entry = crate::acl::Ace {
            kind: ace.kind,
            sid: String::new(),
            mask: 0,
            inherited: ace.flags & INHERITED_ACE != 0,
        };
        // ACCESS_ALLOWED_ACE / ACCESS_DENIED_ACE: header, mask, then the SID.
        if (ace.kind == 0 || ace.kind == 1) && size >= 8 + 8 {
            // SAFETY: the mask is the 4 bytes after the 4-byte header, inside the ACE.
            entry.mask = unsafe { ptr::read_unaligned((at + 4) as *const u32) };
            let sid = (at + 8) as *mut c_void;
            // SAFETY: the SID starts inside the ACE; IsValidSid checks its structure.
            if unsafe { IsValidSid(sid) } != 0 {
                entry.sid = sid_string(sid)?;
            }
        }
        d.aces.push(entry);
        at += size;
    }
    Ok(d)
}

fn wide_lit(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(core::iter::once(0)).collect()
}

/// Owner Administrators; a protected DACL with full control for SYSTEM and
/// Administrators (inherited by children for a directory).
pub(crate) fn restrict_path(path: &[u16], is_dir: bool) -> Result<(), Error> {
    let sddl = if is_dir {
        "D:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)"
    } else {
        "D:P(A;;FA;;;SY)(A;;FA;;;BA)"
    };
    let sddl = wide_lit(sddl);
    let mut sd: *mut c_void = ptr::null_mut();
    // SAFETY: sddl is NUL-terminated; sd receives a LocalAlloc'ed descriptor.
    let ok = unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            SDDL_REVISION_1,
            &mut sd,
            ptr::null_mut(),
        )
    };
    check(ok, "ConvertStringSecurityDescriptorToSecurityDescriptorW")?;
    let _guard = LocalBox(sd);
    let mut present = 0i32;
    let mut defaulted = 0i32;
    let mut dacl: *mut c_void = ptr::null_mut();
    // SAFETY: sd is the valid descriptor built above; dacl points into it.
    let ok = unsafe { GetSecurityDescriptorDacl(sd, &mut present, &mut dacl, &mut defaulted) };
    check(ok, "GetSecurityDescriptorDacl")?;
    let mut sid = [0u64; 9]; // SECURITY_MAX_SID_SIZE is 68 bytes
    let mut size = (sid.len() * 8) as u32;
    // SAFETY: sid is a writable, aligned buffer of `size` bytes.
    let ok = unsafe {
        CreateWellKnownSid(
            WIN_BUILTIN_ADMINISTRATORS_SID,
            ptr::null_mut(),
            sid.as_mut_ptr().cast(),
            &mut size,
        )
    };
    check(ok, "CreateWellKnownSid")?;
    // SAFETY: path is NUL-terminated; owner and dacl are valid for the call's duration.
    let rc = unsafe {
        SetNamedSecurityInfoW(
            path.as_ptr(),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION
                | DACL_SECURITY_INFORMATION
                | PROTECTED_DACL_SECURITY_INFORMATION,
            sid.as_mut_ptr().cast(),
            ptr::null_mut(),
            dacl,
            ptr::null_mut(),
        )
    };
    if rc != 0 {
        return Err(Error::Win32 {
            op: "SetNamedSecurityInfoW",
            code: rc,
        });
    }
    Ok(())
}

/// Blocks in `StartServiceCtrlDispatcherW` until the service stops.
pub(crate) fn start_dispatcher(name: &[u16], main: ServiceMainFn) -> Result<(), Error> {
    let table = [
        ServiceTableEntryW {
            service_name: name.as_ptr(),
            service_proc: Some(main),
        },
        ServiceTableEntryW {
            service_name: ptr::null(),
            service_proc: None,
        },
    ];
    // SAFETY: the table is terminated by a NULL entry; `name` is NUL-terminated and both
    // outlive the call, which returns only after the service has stopped.
    check(
        unsafe { StartServiceCtrlDispatcherW(table.as_ptr()) },
        "StartServiceCtrlDispatcherW",
    )
}

/// A `SERVICE_STATUS_HANDLE`. It is not closed (the SCM owns it).
pub(crate) struct StatusHandle(Handle);

// SAFETY: SetServiceStatus may be called from any thread of the service process.
unsafe impl Send for StatusHandle {}

impl StatusHandle {
    pub(crate) fn set(&self, status: &ServiceStatus) -> Result<(), Error> {
        // SAFETY: self.0 came from RegisterServiceCtrlHandlerExW; status is a valid struct.
        check(
            unsafe { SetServiceStatus(self.0, status) },
            "SetServiceStatus",
        )
    }
}

pub(crate) fn register_handler(name: &[u16], handler: HandlerFn) -> Result<StatusHandle, Error> {
    // SAFETY: name is NUL-terminated; handler is an `extern "system"` function that lives
    // for the whole process; no context pointer is used.
    let h = unsafe { RegisterServiceCtrlHandlerExW(name.as_ptr(), handler, ptr::null_mut()) };
    if h.is_null() {
        Err(last_error("RegisterServiceCtrlHandlerExW"))
    } else {
        Ok(StatusHandle(h))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_matches_winternl() {
        use core::mem::offset_of;
        if size_of::<usize>() == 8 {
            assert_eq!(offset_of!(SystemProcessInformation, image_name), 56);
            assert_eq!(offset_of!(SystemProcessInformation, unique_process_id), 80);
            assert_eq!(
                offset_of!(SystemProcessInformation, inherited_from_unique_process_id),
                88
            );
            assert_eq!(offset_of!(SystemProcessInformation, session_id), 100);
            assert_eq!(size_of::<QueryServiceConfigW>(), 64);
            assert_eq!(size_of::<EnumServiceStatusProcessW>(), 56);
        }
        assert_eq!(size_of::<ServiceStatus>(), 28);
    }

    #[test]
    fn read_pod_and_wide_are_bounded() {
        let mut buf = AlignedBuf::new(16);
        assert!(read_pod::<u64>(buf.bytes(), 8).is_some());
        assert!(read_pod::<u64>(buf.bytes(), 9).is_none());
        assert!(read_pod::<u64>(buf.bytes(), usize::MAX).is_none());
        let units: [u16; 3] = [0x48, 0x69, 0];
        // SAFETY: test-only write of 6 bytes into a 16-byte buffer.
        unsafe { ptr::copy_nonoverlapping(units.as_ptr().cast::<u8>(), buf.as_mut_ptr(), 6) };
        let p = buf.bytes().as_ptr().cast::<u16>();
        assert_eq!(wide_in_buf(buf.bytes(), p, None), "Hi");
        assert_eq!(wide_in_buf(buf.bytes(), p, Some(1)), "H");
        assert_eq!(wide_in_buf(buf.bytes(), ptr::null(), None), "");
        let outside = units.as_ptr();
        assert_eq!(wide_in_buf(buf.bytes(), outside, None), "");
    }
}
