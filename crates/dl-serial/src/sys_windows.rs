//! Win32 serial ports: CreateFileW with overlapped I/O, DCB, COMMTIMEOUTS, and the port
//! list from the registry. This is the only Windows file in the crate with `unsafe`.
#![allow(unsafe_code)]

use core::ffi::c_void;
use core::ptr;
use core::time::Duration;
use std::collections::HashMap;
use std::fs::File;
use std::io;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle, RawHandle};

use crate::ports::natural_cmp;
use crate::win_encode::{
    comm_timeouts, dcb_config, device_desc_text, device_path, open_error, parse_usb_ids,
    strip_com_suffix, usb_serial,
};
use crate::{Buffers, Error, PortInfo, PortKind, Settings};

type Handle = *mut c_void;

const INVALID_HANDLE_VALUE: Handle = usize::MAX as Handle;
const GENERIC_READ: u32 = 0x8000_0000;
const GENERIC_WRITE: u32 = 0x4000_0000;
const OPEN_EXISTING: u32 = 3;
const FILE_FLAG_OVERLAPPED: u32 = 0x4000_0000;
const ERROR_IO_PENDING: i32 = 997;
const ERROR_INVALID_PARAMETER: i32 = 87;
const PURGE_TXCLEAR: u32 = 0x0004;
const PURGE_RXCLEAR: u32 = 0x0008;
const SETRTS: u32 = 3;
const CLRRTS: u32 = 4;
const SETDTR: u32 = 5;
const CLRDTR: u32 = 6;

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Dcb {
    dcb_length: u32,
    baud_rate: u32,
    flags: u32,
    w_reserved: u16,
    xon_lim: u16,
    xoff_lim: u16,
    byte_size: u8,
    parity: u8,
    stop_bits: u8,
    xon_char: u8,
    xoff_char: u8,
    error_char: u8,
    eof_char: u8,
    evt_char: u8,
    w_reserved1: u16,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct CommTimeouts {
    read_interval_timeout: u32,
    read_total_timeout_multiplier: u32,
    read_total_timeout_constant: u32,
    write_total_timeout_multiplier: u32,
    write_total_timeout_constant: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct ComStat {
    flags: u32,
    cb_in_que: u32,
    cb_out_que: u32,
}

/// `OVERLAPPED`; the Offset/OffsetHigh pair has the size and offset of the union on both
/// 32- and 64-bit Windows.
#[repr(C)]
struct Overlapped {
    internal: usize,
    internal_high: usize,
    offset: u32,
    offset_high: u32,
    h_event: Handle,
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn CreateFileW(
        name: *const u16,
        access: u32,
        share: u32,
        security: *mut c_void,
        disposition: u32,
        flags: u32,
        template: Handle,
    ) -> Handle;
    fn CreateEventW(security: *mut c_void, manual: i32, initial: i32, name: *const u16) -> Handle;
    fn GetCommState(h: Handle, dcb: *mut Dcb) -> i32;
    fn SetCommState(h: Handle, dcb: *const Dcb) -> i32;
    fn SetCommTimeouts(h: Handle, t: *const CommTimeouts) -> i32;
    fn SetupComm(h: Handle, in_queue: u32, out_queue: u32) -> i32;
    fn PurgeComm(h: Handle, flags: u32) -> i32;
    fn EscapeCommFunction(h: Handle, func: u32) -> i32;
    fn ClearCommError(h: Handle, errors: *mut u32, stat: *mut ComStat) -> i32;
    fn ReadFile(h: Handle, buf: *mut c_void, len: u32, read: *mut u32, ov: *mut Overlapped) -> i32;
    fn WriteFile(
        h: Handle,
        buf: *const c_void,
        len: u32,
        written: *mut u32,
        ov: *mut Overlapped,
    ) -> i32;
    fn GetOverlappedResult(h: Handle, ov: *const Overlapped, n: *mut u32, wait: i32) -> i32;
}

#[link(name = "advapi32")]
unsafe extern "system" {
    fn RegOpenKeyExW(
        key: Handle,
        sub_key: *const u16,
        options: u32,
        sam: u32,
        result: *mut Handle,
    ) -> i32;
    fn RegCloseKey(key: Handle) -> i32;
    fn RegEnumKeyExW(
        key: Handle,
        index: u32,
        name: *mut u16,
        name_len: *mut u32,
        reserved: *mut u32,
        class: *mut u16,
        class_len: *mut u32,
        last_write: *mut c_void,
    ) -> i32;
    fn RegEnumValueW(
        key: Handle,
        index: u32,
        name: *mut u16,
        name_len: *mut u32,
        reserved: *mut u32,
        kind: *mut u32,
        data: *mut u8,
        data_len: *mut u32,
    ) -> i32;
    fn RegQueryValueExW(
        key: Handle,
        name: *const u16,
        reserved: *mut u32,
        kind: *mut u32,
        data: *mut u8,
        data_len: *mut u32,
    ) -> i32;
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(core::iter::once(0)).collect()
}

fn last_error() -> i32 {
    io::Error::last_os_error().raw_os_error().unwrap_or(0)
}

fn timed_out(what: &str) -> io::Error {
    io::Error::new(io::ErrorKind::TimedOut, format!("serial {what} timed out"))
}

pub(crate) struct Port {
    file: File,
    event: OwnedHandle,
    read_timeout: Option<Duration>,
    write_timeout: Option<Duration>,
}

impl Port {
    fn handle(&self) -> Handle {
        self.file.as_raw_handle() as Handle
    }

    fn check(&self, ok: i32, op: &'static str) -> Result<(), Error> {
        if ok == 0 {
            Err(Error::Os {
                op,
                code: last_error(),
            })
        } else {
            Ok(())
        }
    }

    pub(crate) fn open(name: &str, s: &Settings) -> Result<Port, Error> {
        s.validate()?;
        let path = wide(&device_path(name)?);
        // SAFETY: `path` is NUL-terminated and outlives the call; the other pointers are
        // null, which CreateFileW accepts for the security attributes and template.
        let h = unsafe {
            CreateFileW(
                path.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                0,
                ptr::null_mut(),
                OPEN_EXISTING,
                FILE_FLAG_OVERLAPPED,
                ptr::null_mut(),
            )
        };
        if h == INVALID_HANDLE_VALUE {
            return Err(open_error(name, last_error() as u32));
        }
        // SAFETY: `h` is a valid handle that CreateFileW just returned and nothing else
        // owns; the OwnedHandle closes it exactly once.
        let file = File::from(unsafe { OwnedHandle::from_raw_handle(h as RawHandle) });
        let mut port = Port {
            file,
            event: new_event()?,
            read_timeout: s.read_timeout,
            write_timeout: s.write_timeout,
        };
        port.configure(name, s)?;
        port.set_timeouts(s.read_timeout, s.write_timeout)?;
        port.clear(Buffers::Both)?;
        Ok(port)
    }

    fn configure(&mut self, name: &str, s: &Settings) -> Result<(), Error> {
        let h = self.handle();
        // Queue sizes are a hint; virtual (Bluetooth, USB CDC) drivers may refuse them.
        // SAFETY: `h` is our open comm handle.
        let _ = unsafe { SetupComm(h, 4096, 2048) };
        let mut dcb = Dcb {
            dcb_length: size_of::<Dcb>() as u32,
            ..Dcb::default()
        };
        // SAFETY: `dcb` is a writable DCB with DCBlength set, valid for the call.
        let ok = unsafe { GetCommState(h, &mut dcb) };
        if ok == 0 {
            return Err(Error::NotASerialPort(name.to_string()));
        }
        let c = dcb_config(s, dcb.flags, true, true);
        dcb.dcb_length = size_of::<Dcb>() as u32;
        dcb.baud_rate = c.baud_rate;
        dcb.flags = c.flags;
        dcb.byte_size = c.byte_size;
        dcb.parity = c.parity;
        dcb.stop_bits = c.stop_bits;
        dcb.xon_char = c.xon_char;
        dcb.xoff_char = c.xoff_char;
        dcb.xon_lim = c.xon_lim;
        dcb.xoff_lim = c.xoff_lim;
        // SAFETY: `dcb` is a fully initialised DCB that outlives the call.
        let ok = unsafe { SetCommState(h, &dcb) };
        if ok == 0 {
            let code = last_error();
            return Err(if code == ERROR_INVALID_PARAMETER {
                Error::InvalidSettings("the driver rejected the line settings or baud rate")
            } else {
                Error::Os {
                    op: "SetCommState",
                    code,
                }
            });
        }
        Ok(())
    }

    /// Waits for an overlapped ReadFile/WriteFile started with `ov` and returns the count.
    fn finish(&self, started: i32, ov: &Overlapped) -> io::Result<u32> {
        if started == 0 {
            let code = last_error();
            if code != ERROR_IO_PENDING {
                return Err(io::Error::from_raw_os_error(code));
            }
        }
        let mut n = 0u32;
        // SAFETY: `ov` is the OVERLAPPED of an operation on this handle; bWait = TRUE
        // blocks until that operation completes, so the buffer and `ov` stay borrowed
        // for its whole lifetime.
        let ok = unsafe { GetOverlappedResult(self.handle(), ov, &mut n, 1) };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(n)
    }

    fn overlapped(&self) -> Overlapped {
        Overlapped {
            internal: 0,
            internal_high: 0,
            offset: 0,
            offset_high: 0,
            h_event: self.event.as_raw_handle() as Handle,
        }
    }

    pub(crate) fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        let len = u32::try_from(buf.len()).unwrap_or(u32::MAX);
        loop {
            let mut ov = self.overlapped();
            // SAFETY: `buf` is valid for `len` writable bytes and `ov` is a zeroed
            // OVERLAPPED with our event; `finish` waits for completion before either goes
            // out of scope.
            let started = unsafe {
                ReadFile(
                    self.handle(),
                    buf.as_mut_ptr().cast(),
                    len,
                    ptr::null_mut(),
                    &mut ov,
                )
            };
            let n = self.finish(started, &ov)?;
            if n > 0 {
                return Ok(n as usize);
            }
            if self.read_timeout.is_some() {
                return Err(timed_out("read"));
            }
            // No timeout: the ~49-day constant expired, wait again.
        }
    }

    pub(crate) fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        let len = u32::try_from(buf.len()).unwrap_or(u32::MAX);
        let mut ov = self.overlapped();
        // SAFETY: `buf` is valid for `len` readable bytes and `ov` is a zeroed OVERLAPPED
        // with our event; `finish` waits for completion before either goes out of scope.
        let started = unsafe {
            WriteFile(
                self.handle(),
                buf.as_ptr().cast(),
                len,
                ptr::null_mut(),
                &mut ov,
            )
        };
        let n = self.finish(started, &ov)?;
        if n == 0 {
            return Err(timed_out("write"));
        }
        Ok(n as usize)
    }

    pub(crate) fn flush(&mut self) -> io::Result<()> {
        // FlushFileBuffers: returns once the driver has transmitted everything written.
        self.file.sync_all()
    }

    pub(crate) fn clear(&self, b: Buffers) -> Result<(), Error> {
        let flags = match b {
            Buffers::Input => PURGE_RXCLEAR,
            Buffers::Output => PURGE_TXCLEAR,
            Buffers::Both => PURGE_RXCLEAR | PURGE_TXCLEAR,
        };
        // SAFETY: `handle` is our open comm handle; PurgeComm takes no pointers.
        let ok = unsafe { PurgeComm(self.handle(), flags) };
        self.check(ok, "PurgeComm")
    }

    pub(crate) fn set_timeouts(
        &mut self,
        read: Option<Duration>,
        write: Option<Duration>,
    ) -> Result<(), Error> {
        let [a, b, c, d, e] = comm_timeouts(read, write);
        let t = CommTimeouts {
            read_interval_timeout: a,
            read_total_timeout_multiplier: b,
            read_total_timeout_constant: c,
            write_total_timeout_multiplier: d,
            write_total_timeout_constant: e,
        };
        // SAFETY: `t` is an initialised COMMTIMEOUTS that outlives the call.
        let ok = unsafe { SetCommTimeouts(self.handle(), &t) };
        self.check(ok, "SetCommTimeouts")?;
        self.read_timeout = read;
        self.write_timeout = write;
        Ok(())
    }

    pub(crate) fn read_timeout(&self) -> Option<Duration> {
        self.read_timeout
    }

    pub(crate) fn write_timeout(&self) -> Option<Duration> {
        self.write_timeout
    }

    fn escape(&self, func: u32) -> Result<(), Error> {
        // SAFETY: `handle` is our open comm handle; EscapeCommFunction takes no pointers.
        let ok = unsafe { EscapeCommFunction(self.handle(), func) };
        self.check(ok, "EscapeCommFunction")
    }

    pub(crate) fn set_dtr(&self, on: bool) -> Result<(), Error> {
        self.escape(if on { SETDTR } else { CLRDTR })
    }

    pub(crate) fn set_rts(&self, on: bool) -> Result<(), Error> {
        self.escape(if on { SETRTS } else { CLRRTS })
    }

    pub(crate) fn bytes_available(&self) -> Result<u32, Error> {
        let mut errors = 0u32;
        let mut stat = ComStat::default();
        // SAFETY: both out pointers refer to live locals of the right type.
        let ok = unsafe { ClearCommError(self.handle(), &mut errors, &mut stat) };
        self.check(ok, "ClearCommError")?;
        Ok(stat.cb_in_que)
    }

    pub(crate) fn try_clone(&self) -> Result<Port, Error> {
        let file = self
            .file
            .try_clone()
            .map_err(|e| Error::os("DuplicateHandle", &e))?;
        Ok(Port {
            file,
            event: new_event()?,
            read_timeout: self.read_timeout,
            write_timeout: self.write_timeout,
        })
    }
}

fn new_event() -> Result<OwnedHandle, Error> {
    // SAFETY: null security attributes and name are allowed; manual reset, non-signalled.
    let h = unsafe { CreateEventW(ptr::null_mut(), 1, 0, ptr::null()) };
    if h.is_null() {
        return Err(Error::Os {
            op: "CreateEventW",
            code: last_error(),
        });
    }
    // SAFETY: `h` is a fresh event handle that nothing else owns.
    Ok(unsafe { OwnedHandle::from_raw_handle(h as RawHandle) })
}

// ---- registry ----------------------------------------------------------------------

const HKEY_LOCAL_MACHINE: Handle = -2_147_483_646_isize as Handle; // 0x80000002 sign-extended
const KEY_READ: u32 = 0x2_0019;
const ERROR_FILE_NOT_FOUND: i32 = 2;
const ERROR_MORE_DATA: i32 = 234;
const ERROR_NO_MORE_ITEMS: i32 = 259;
const REG_SZ: u32 = 1;
const REG_EXPAND_SZ: u32 = 2;

struct RegKey(Handle);

impl Drop for RegKey {
    fn drop(&mut self) {
        // SAFETY: `self.0` is a key handle we opened and close exactly once.
        let _ = unsafe { RegCloseKey(self.0) };
    }
}

fn utf16_until_nul(buf: &[u16]) -> String {
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..end])
}

impl RegKey {
    fn open(parent: Handle, path: &str) -> Result<RegKey, i32> {
        let p = wide(path);
        let mut key: Handle = ptr::null_mut();
        // SAFETY: `p` is NUL-terminated and `key` is a valid out pointer.
        let rc = unsafe { RegOpenKeyExW(parent, p.as_ptr(), 0, KEY_READ, &mut key) };
        if rc != 0 {
            return Err(rc);
        }
        Ok(RegKey(key))
    }

    fn subkey(&self, path: &str) -> Option<RegKey> {
        RegKey::open(self.0, path).ok()
    }

    fn subkey_names(&self) -> Vec<String> {
        let mut names = Vec::new();
        let mut buf = [0u16; 256]; // key names are at most 255 characters
        for index in 0u32.. {
            let mut len = buf.len() as u32;
            // SAFETY: `buf` holds `len` u16s; the optional out pointers are null.
            let rc = unsafe {
                RegEnumKeyExW(
                    self.0,
                    index,
                    buf.as_mut_ptr(),
                    &mut len,
                    ptr::null_mut(),
                    ptr::null_mut(),
                    ptr::null_mut(),
                    ptr::null_mut(),
                )
            };
            if rc != 0 {
                break; // ERROR_NO_MORE_ITEMS, or an error: best effort
            }
            let len = (len as usize).min(buf.len());
            names.push(String::from_utf16_lossy(&buf[..len]));
        }
        names
    }

    fn string_value(&self, name: &str) -> Option<String> {
        let n = wide(name);
        let mut data = vec![0u16; 256];
        loop {
            let mut kind = 0u32;
            let mut bytes = (data.len() * 2) as u32;
            // SAFETY: `n` is NUL-terminated; `data` holds `bytes` writable bytes.
            let rc = unsafe {
                RegQueryValueExW(
                    self.0,
                    n.as_ptr(),
                    ptr::null_mut(),
                    &mut kind,
                    data.as_mut_ptr().cast(),
                    &mut bytes,
                )
            };
            if rc == ERROR_MORE_DATA && data.len() < 32 * 1024 {
                data.resize((bytes as usize).div_ceil(2).max(data.len() * 2), 0);
                continue;
            }
            if rc != 0 || (kind != REG_SZ && kind != REG_EXPAND_SZ) {
                return None;
            }
            let units = (bytes as usize / 2).min(data.len());
            return Some(utf16_until_nul(&data[..units]));
        }
    }

    /// All REG_SZ values as (name, data).
    fn string_values(&self) -> Result<Vec<(String, String)>, Error> {
        let mut out = Vec::new();
        let mut name = vec![0u16; 256];
        let mut data = vec![0u16; 256];
        let mut index = 0u32;
        loop {
            let mut name_len = name.len() as u32;
            let mut bytes = (data.len() * 2) as u32;
            let mut kind = 0u32;
            // SAFETY: `name` holds `name_len` u16s, `data` holds `bytes` bytes, and the
            // other out pointers are live locals.
            let rc = unsafe {
                RegEnumValueW(
                    self.0,
                    index,
                    name.as_mut_ptr(),
                    &mut name_len,
                    ptr::null_mut(),
                    &mut kind,
                    data.as_mut_ptr().cast(),
                    &mut bytes,
                )
            };
            match rc {
                0 => {}
                ERROR_NO_MORE_ITEMS => return Ok(out),
                ERROR_MORE_DATA if name.len() < 16_384 || data.len() < 32 * 1024 => {
                    name.resize(16_384, 0);
                    data.resize(32 * 1024, 0);
                    continue;
                }
                code => {
                    return Err(Error::Os {
                        op: "RegEnumValueW",
                        code,
                    });
                }
            }
            index += 1;
            if kind == REG_SZ || kind == REG_EXPAND_SZ {
                let n = String::from_utf16_lossy(&name[..(name_len as usize).min(name.len())]);
                let units = (bytes as usize / 2).min(data.len());
                out.push((n, utf16_until_nul(&data[..units])));
            }
        }
    }
}

/// Port name (upper case) -> kind, from the device nodes under `Enum\{USB,FTDIBUS,...}`.
fn device_kinds() -> HashMap<String, PortKind> {
    let mut map = HashMap::new();
    let Ok(enum_root) = RegKey::open(HKEY_LOCAL_MACHINE, r"SYSTEM\CurrentControlSet\Enum") else {
        return map;
    };
    for bus in ["USB", "FTDIBUS", "BTHENUM", "PCI"] {
        let Some(bus_key) = enum_root.subkey(bus) else {
            continue;
        };
        for device_id in bus_key.subkey_names() {
            let Some(dev_key) = bus_key.subkey(&device_id) else {
                continue;
            };
            for instance_id in dev_key.subkey_names() {
                let Some(inst) = dev_key.subkey(&instance_id) else {
                    continue;
                };
                let Some(port) = inst
                    .subkey("Device Parameters")
                    .and_then(|p| p.string_value("PortName"))
                else {
                    continue;
                };
                let kind = match bus {
                    "BTHENUM" => PortKind::Bluetooth,
                    "PCI" => PortKind::Pci,
                    _ => match parse_usb_ids(&device_id) {
                        Some((vid, pid)) => {
                            let product = inst
                                .string_value("FriendlyName")
                                .map(|f| strip_com_suffix(&f).to_string())
                                .or_else(|| {
                                    inst.string_value("DeviceDesc")
                                        .map(|d| device_desc_text(&d).to_string())
                                })
                                .filter(|p| !p.is_empty());
                            PortKind::Usb {
                                vid,
                                pid,
                                serial: usb_serial(bus, &device_id, &instance_id),
                                product,
                            }
                        }
                        None => PortKind::Unknown,
                    },
                };
                map.insert(port.trim().to_ascii_uppercase(), kind);
            }
        }
    }
    map
}

pub(crate) fn available_ports() -> Result<Vec<PortInfo>, Error> {
    let key = match RegKey::open(HKEY_LOCAL_MACHINE, r"HARDWARE\DEVICEMAP\SERIALCOMM") {
        Ok(k) => k,
        // The key only exists once a serial driver has registered a port.
        Err(ERROR_FILE_NOT_FOUND) => return Ok(Vec::new()),
        Err(code) => {
            return Err(Error::Os {
                op: "RegOpenKeyExW",
                code,
            });
        }
    };
    let values = key.string_values()?;
    let kinds = device_kinds();
    let mut ports: Vec<PortInfo> = values
        .into_iter()
        .map(|(device, port)| {
            let name = port.trim().to_string();
            let kind = kinds
                .get(&name.to_ascii_uppercase())
                .cloned()
                .unwrap_or_else(|| {
                    if device.to_ascii_lowercase().contains("bthmodem") {
                        PortKind::Bluetooth
                    } else {
                        PortKind::Unknown
                    }
                });
            PortInfo { name, kind }
        })
        .filter(|p| !p.name.is_empty())
        .collect();
    ports.sort_by(|a, b| natural_cmp(&a.name, &b.name));
    ports.dedup_by(|a, b| a.name == b.name);
    Ok(ports)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn struct_layouts() {
        assert_eq!(size_of::<Dcb>(), 28);
        assert_eq!(size_of::<CommTimeouts>(), 20);
        assert_eq!(size_of::<ComStat>(), 12);
        assert_eq!(
            size_of::<Overlapped>(),
            if cfg!(target_pointer_width = "64") {
                32
            } else {
                20
            }
        );
    }

    #[test]
    fn registry_reader_matches_reg_exe() {
        let ours: Vec<String> = available_ports()
            .unwrap()
            .into_iter()
            .map(|p| p.name.to_ascii_uppercase())
            .collect();
        let out = std::process::Command::new("reg")
            .args(["query", r"HKLM\HARDWARE\DEVICEMAP\SERIALCOMM"])
            .output()
            .expect("run reg.exe");
        let text = String::from_utf8_lossy(&out.stdout);
        let mut theirs: Vec<String> = text
            .lines()
            .filter(|l| l.contains("REG_SZ"))
            .filter_map(|l| l.split_whitespace().last())
            .map(str::to_ascii_uppercase)
            .collect();
        theirs.sort_by(|a, b| natural_cmp(a, b));
        theirs.dedup();
        assert_eq!(ours, theirs);
    }

    #[test]
    fn expected_port_is_listed() {
        // Set DL_SERIAL_EXPECT_PORT=COM3 on a machine that has that port.
        let Ok(want) = std::env::var("DL_SERIAL_EXPECT_PORT") else {
            return;
        };
        let ports = available_ports().unwrap();
        eprintln!("{ports:?}");
        assert!(
            ports.iter().any(|p| p.name.eq_ignore_ascii_case(&want)),
            "{want} not in {ports:?}"
        );
    }

    #[test]
    fn missing_port_is_not_found() {
        let e = Port::open("COM250", &Settings::default()).err().unwrap();
        assert_eq!(e, Error::NotFound("COM250".into()));
        let e = Port::open(r"\\.\DL_SERIAL_NO_SUCH_DEVICE", &Settings::default())
            .err()
            .unwrap();
        assert!(matches!(e, Error::NotFound(_)), "{e:?}");
    }

    #[test]
    fn invalid_names_and_settings() {
        for bad in ["", "COM3\0", "COM 3", "x/y"] {
            let e = Port::open(bad, &Settings::default()).err().unwrap();
            assert_eq!(e, Error::InvalidName(bad.into()));
        }
        let e = Port::open("COM250", &Settings::new(0)).err().unwrap();
        assert!(matches!(e, Error::InvalidSettings(_)), "{e:?}");
    }

    #[test]
    fn non_serial_device_is_rejected() {
        // NUL opens fine but has no comm state.
        let e = Port::open("NUL", &Settings::default()).err().unwrap();
        assert_eq!(e, Error::NotASerialPort("NUL".into()));
    }
}
