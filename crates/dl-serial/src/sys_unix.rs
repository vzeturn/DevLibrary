//! Serial ports on Linux (x86_64, aarch64; glibc and musl) and macOS: ttys opened with
//! O_NOCTTY | O_NONBLOCK, raw termios, poll() for timeouts. The libc declarations are
//! written out per target. This is the only Unix file in the crate with `unsafe`.
#![allow(unsafe_code)]

use core::ffi::{c_int, c_short, c_ulong};
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::time::{Duration, Instant};

use crate::ports::natural_cmp;
use crate::settings::millis_ceil;
use crate::{Buffers, DataBits, Error, Flow, Parity, PortInfo, PortKind, Settings, StopBits};

#[cfg(target_os = "linux")]
mod os {
    //! asm-generic values (x86_64, aarch64). The kernel `struct termios2` is used through
    //! TCGETS2 / TCSETS2, so the C library's termios layout and speed encoding never
    //! matter.
    use core::ffi::c_ulong;

    pub type Tcflag = u32;
    pub type Nfds = c_ulong;

    pub const O_NOCTTY: i32 = 0o400;
    pub const O_NONBLOCK: i32 = 0o4000;

    pub const IGNBRK: Tcflag = 0o1;
    pub const BRKINT: Tcflag = 0o2;
    pub const IGNPAR: Tcflag = 0o4;
    pub const PARMRK: Tcflag = 0o10;
    pub const INPCK: Tcflag = 0o20;
    pub const ISTRIP: Tcflag = 0o40;
    pub const INLCR: Tcflag = 0o100;
    pub const IGNCR: Tcflag = 0o200;
    pub const ICRNL: Tcflag = 0o400;
    pub const IXON: Tcflag = 0o2000;
    pub const IXANY: Tcflag = 0o4000;
    pub const IXOFF: Tcflag = 0o10000;
    pub const IMAXBEL: Tcflag = 0o20000;
    pub const OPOST: Tcflag = 0o1;
    pub const ISIG: Tcflag = 0o1;
    pub const ICANON: Tcflag = 0o2;
    pub const ECHO: Tcflag = 0o10;
    pub const ECHOE: Tcflag = 0o20;
    pub const ECHOK: Tcflag = 0o40;
    pub const ECHONL: Tcflag = 0o100;
    pub const IEXTEN: Tcflag = 0o100000;
    pub const CSIZE: Tcflag = 0o60;
    pub const CS5: Tcflag = 0;
    pub const CS6: Tcflag = 0o20;
    pub const CS7: Tcflag = 0o40;
    pub const CS8: Tcflag = 0o60;
    pub const CSTOPB: Tcflag = 0o100;
    pub const CREAD: Tcflag = 0o200;
    pub const PARENB: Tcflag = 0o400;
    pub const PARODD: Tcflag = 0o1000;
    pub const CLOCAL: Tcflag = 0o4000;
    pub const CMSPAR: Tcflag = 0o10000000000;
    pub const HAS_CMSPAR: bool = true;
    pub const CRTSCTS: Tcflag = 0o20000000000;
    pub const CBAUD: Tcflag = 0o10017;
    pub const BOTHER: Tcflag = 0o10000;
    pub const CIBAUD: Tcflag = CBAUD << 16;

    pub const VTIME: usize = 5;
    pub const VMIN: usize = 6;
    pub const VSTART: usize = 8;
    pub const VSTOP: usize = 9;

    pub const TCGETS2: c_ulong = 0x802C_542A;
    pub const TCSETS2: c_ulong = 0x402C_542B;
    pub const TCSBRK: c_ulong = 0x5409;
    pub const TCFLSH: c_ulong = 0x540B;
    pub const TIOCEXCL: c_ulong = 0x540C;
    pub const TIOCOUTQ: c_ulong = 0x5411;
    pub const TIOCMBIS: c_ulong = 0x5416;
    pub const TIOCMBIC: c_ulong = 0x5417;
    pub const FIONREAD: c_ulong = 0x541B;
    pub const TCIFLUSH: c_ulong = 0;
    pub const TCOFLUSH: c_ulong = 1;
    pub const TCIOFLUSH: c_ulong = 2;

    /// Kernel `struct termios2` (asm-generic/termbits.h, NCCS = 19).
    #[repr(C)]
    #[derive(Debug, Clone, Copy, Default)]
    pub struct Termios {
        pub c_iflag: Tcflag,
        pub c_oflag: Tcflag,
        pub c_cflag: Tcflag,
        pub c_lflag: Tcflag,
        pub c_line: u8,
        pub c_cc: [u8; 19],
        pub c_ispeed: u32,
        pub c_ospeed: u32,
    }

    /// Rates with a `Bnnn` constant; anything else uses BOTHER.
    pub const STANDARD_BAUDS: &[(u32, Tcflag)] = &[
        (50, 0o1),
        (75, 0o2),
        (110, 0o3),
        (134, 0o4),
        (150, 0o5),
        (200, 0o6),
        (300, 0o7),
        (600, 0o10),
        (1200, 0o11),
        (1800, 0o12),
        (2400, 0o13),
        (4800, 0o14),
        (9600, 0o15),
        (19200, 0o16),
        (38400, 0o17),
        (57600, 0o10001),
        (115_200, 0o10002),
        (230_400, 0o10003),
        (460_800, 0o10004),
        (500_000, 0o10005),
        (576_000, 0o10006),
        (921_600, 0o10007),
        (1_000_000, 0o10010),
        (1_152_000, 0o10011),
        (1_500_000, 0o10012),
        (2_000_000, 0o10013),
        (2_500_000, 0o10014),
        (3_000_000, 0o10015),
        (3_500_000, 0o10016),
        (4_000_000, 0o10017),
    ];
}

#[cfg(target_os = "macos")]
mod os {
    //! Darwin values (sys/termios.h, sys/ttycom.h, sys/filio.h).
    use core::ffi::{c_uint, c_ulong};

    pub type Tcflag = c_ulong;
    pub type Speed = c_ulong;
    pub type Nfds = c_uint;

    pub const O_NOCTTY: i32 = 0x2_0000;
    pub const O_NONBLOCK: i32 = 0x4;

    pub const IGNBRK: Tcflag = 0x1;
    pub const BRKINT: Tcflag = 0x2;
    pub const IGNPAR: Tcflag = 0x4;
    pub const PARMRK: Tcflag = 0x8;
    pub const INPCK: Tcflag = 0x10;
    pub const ISTRIP: Tcflag = 0x20;
    pub const INLCR: Tcflag = 0x40;
    pub const IGNCR: Tcflag = 0x80;
    pub const ICRNL: Tcflag = 0x100;
    pub const IXON: Tcflag = 0x200;
    pub const IXOFF: Tcflag = 0x400;
    pub const IXANY: Tcflag = 0x800;
    pub const IMAXBEL: Tcflag = 0x2000;
    pub const OPOST: Tcflag = 0x1;
    pub const ECHOE: Tcflag = 0x2;
    pub const ECHOK: Tcflag = 0x4;
    pub const ECHO: Tcflag = 0x8;
    pub const ECHONL: Tcflag = 0x10;
    pub const ISIG: Tcflag = 0x80;
    pub const ICANON: Tcflag = 0x100;
    pub const IEXTEN: Tcflag = 0x400;
    pub const CSIZE: Tcflag = 0x300;
    pub const CS5: Tcflag = 0x0;
    pub const CS6: Tcflag = 0x100;
    pub const CS7: Tcflag = 0x200;
    pub const CS8: Tcflag = 0x300;
    pub const CSTOPB: Tcflag = 0x400;
    pub const CREAD: Tcflag = 0x800;
    pub const PARENB: Tcflag = 0x1000;
    pub const PARODD: Tcflag = 0x2000;
    pub const CLOCAL: Tcflag = 0x8000;
    pub const CMSPAR: Tcflag = 0;
    pub const HAS_CMSPAR: bool = false;
    pub const CRTSCTS: Tcflag = 0x3_0000; // CCTS_OFLOW | CRTS_IFLOW

    pub const VSTART: usize = 12;
    pub const VSTOP: usize = 13;
    pub const VMIN: usize = 16;
    pub const VTIME: usize = 17;

    pub const TCSANOW: core::ffi::c_int = 0;
    pub const TCIFLUSH: core::ffi::c_int = 1;
    pub const TCOFLUSH: core::ffi::c_int = 2;
    pub const TCIOFLUSH: core::ffi::c_int = 3;

    pub const TIOCEXCL: c_ulong = 0x2000_740D;
    pub const TIOCOUTQ: c_ulong = 0x4004_7473;
    pub const TIOCMBIS: c_ulong = 0x8004_746C;
    pub const TIOCMBIC: c_ulong = 0x8004_746B;
    pub const FIONREAD: c_ulong = 0x4004_667F;
    /// `IOSSIOSPEED`: set any input and output speed on a serial device.
    pub const IOSSIOSPEED: c_ulong = 0x8008_5402;

    /// `struct termios` (NCCS = 20).
    #[repr(C)]
    #[derive(Debug, Clone, Copy, Default)]
    pub struct Termios {
        pub c_iflag: Tcflag,
        pub c_oflag: Tcflag,
        pub c_cflag: Tcflag,
        pub c_lflag: Tcflag,
        pub c_cc: [u8; 20],
        pub c_ispeed: Speed,
        pub c_ospeed: Speed,
    }

    /// On Darwin speed_t is the rate itself; these are the ones with a `Bnnn` constant.
    pub const STANDARD_BAUDS: &[u32] = &[
        50, 75, 110, 134, 150, 200, 300, 600, 1200, 1800, 2400, 4800, 7200, 9600, 14400, 19200,
        28800, 38400, 57600, 76800, 115_200, 230_400,
    ];

    unsafe extern "C" {
        pub fn tcgetattr(fd: core::ffi::c_int, t: *mut Termios) -> core::ffi::c_int;
        pub fn tcsetattr(
            fd: core::ffi::c_int,
            action: core::ffi::c_int,
            t: *const Termios,
        ) -> core::ffi::c_int;
        pub fn cfsetispeed(t: *mut Termios, speed: Speed) -> core::ffi::c_int;
        pub fn cfsetospeed(t: *mut Termios, speed: Speed) -> core::ffi::c_int;
        pub fn tcflush(fd: core::ffi::c_int, queue: core::ffi::c_int) -> core::ffi::c_int;
        pub fn tcdrain(fd: core::ffi::c_int) -> core::ffi::c_int;
    }
}

use os::*;

const POLLIN: c_short = 0x1;
const POLLOUT: c_short = 0x4;
const POLLNVAL: c_short = 0x20;
const LOCK_EX: c_int = 2;
const LOCK_NB: c_int = 4;
const EPERM: i32 = 1;
const ENOENT: i32 = 2;
const ENXIO: i32 = 6;
const EBADF: i32 = 9;
const EACCES: i32 = 13;
const EBUSY: i32 = 16;
const ENODEV: i32 = 19;
const EINVAL: i32 = 22;
const ENOTTY: i32 = 25;

#[repr(C)]
struct PollFd {
    fd: c_int,
    events: c_short,
    revents: c_short,
}

unsafe extern "C" {
    fn ioctl(fd: c_int, request: c_ulong, ...) -> c_int;
    fn poll(fds: *mut PollFd, nfds: Nfds, timeout: c_int) -> c_int;
    fn flock(fd: c_int, op: c_int) -> c_int;
}

fn errno() -> i32 {
    io::Error::last_os_error().raw_os_error().unwrap_or(0)
}

fn timed_out(what: &str) -> io::Error {
    io::Error::new(io::ErrorKind::TimedOut, format!("serial {what} timed out"))
}

fn deadline(timeout: Option<Duration>) -> Option<Instant> {
    timeout.and_then(|d| Instant::now().checked_add(d))
}

/// Waits until `fd` is ready for `events`. Returns `Ok(false)` when `until` passes first;
/// `None` waits forever.
fn wait_fd(fd: c_int, events: c_short, until: Option<Instant>) -> io::Result<bool> {
    loop {
        let timeout = match until {
            None => -1,
            Some(t) => {
                let left = t.saturating_duration_since(Instant::now());
                millis_ceil(left, i32::MAX as u64) as c_int
            }
        };
        let mut p = PollFd {
            fd,
            events,
            revents: 0,
        };
        // SAFETY: `p` is one live pollfd and nfds is 1.
        let rc = unsafe { poll(&mut p, 1, timeout) };
        if rc < 0 {
            let e = io::Error::last_os_error();
            if e.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(e);
        }
        if rc == 0 {
            if until.is_some_and(|t| Instant::now() < t) {
                continue;
            }
            return Ok(false);
        }
        if p.revents & POLLNVAL != 0 {
            return Err(io::Error::from_raw_os_error(EBADF));
        }
        // POLLHUP / POLLERR also count as ready: the next read or write reports them.
        return Ok(true);
    }
}

/// `ttyUSB0` -> `/dev/ttyUSB0`; absolute paths are used as given.
fn device_path(name: &str) -> Result<String, Error> {
    if name.is_empty() || name.contains('\0') {
        return Err(Error::InvalidName(name.to_string()));
    }
    if name.starts_with('/') {
        Ok(name.to_string())
    } else {
        Ok(format!("/dev/{name}"))
    }
}

fn open_error(name: &str, e: &io::Error) -> Error {
    match e.raw_os_error() {
        Some(ENOENT | ENXIO | ENODEV) => Error::NotFound(name.to_string()),
        Some(EBUSY) => Error::Busy(name.to_string()),
        Some(EACCES | EPERM) => Error::PermissionDenied(name.to_string()),
        _ => Error::os("open", e),
    }
}

/// cfmakeraw by hand, plus the line settings. Speed is set by the caller.
fn make_raw(t: &mut Termios, s: &Settings) -> Result<(), Error> {
    t.c_iflag &= !(IGNBRK
        | BRKINT
        | PARMRK
        | ISTRIP
        | INLCR
        | IGNCR
        | ICRNL
        | IXON
        | IXOFF
        | IXANY
        | INPCK
        | IGNPAR
        | IMAXBEL);
    t.c_oflag &= !OPOST;
    t.c_lflag &= !(ECHO | ECHOE | ECHOK | ECHONL | ICANON | ISIG | IEXTEN);
    t.c_cflag &= !(CSIZE | CSTOPB | PARENB | PARODD | CMSPAR | CRTSCTS);
    t.c_cflag |= CREAD
        | CLOCAL
        | match s.data_bits {
            DataBits::Five => CS5,
            DataBits::Six => CS6,
            DataBits::Seven => CS7,
            DataBits::Eight => CS8,
        };
    let mark_space = || {
        if HAS_CMSPAR {
            Ok(PARENB | CMSPAR)
        } else {
            Err(Error::InvalidSettings(
                "mark and space parity are not supported on this platform",
            ))
        }
    };
    t.c_cflag |= match s.parity {
        Parity::None => 0,
        Parity::Odd => PARENB | PARODD,
        Parity::Even => PARENB,
        Parity::Mark => mark_space()? | PARODD,
        Parity::Space => mark_space()?,
    };
    if s.parity != Parity::None {
        t.c_iflag |= INPCK;
    }
    // With CS5, CSTOPB gives 1.5 stop bits; `validate` refused the other 1.5 cases.
    if matches!(s.stop_bits, StopBits::Two | StopBits::OnePointFive) {
        t.c_cflag |= CSTOPB;
    }
    match s.flow {
        Flow::None => {}
        Flow::RtsCts => t.c_cflag |= CRTSCTS,
        Flow::XonXoff => {
            t.c_iflag |= IXON | IXOFF;
            t.c_cc[VSTART] = 0x11;
            t.c_cc[VSTOP] = 0x13;
        }
    }
    // Block (in the poll sense) for at least one byte, no inter-byte timer.
    t.c_cc[VMIN] = 1;
    t.c_cc[VTIME] = 0;
    Ok(())
}

#[cfg(target_os = "linux")]
fn get_attrs(fd: c_int) -> io::Result<Termios> {
    let mut t = Termios::default();
    // SAFETY: TCGETS2 writes one `struct termios2`, which `Termios` mirrors exactly.
    let rc = unsafe { ioctl(fd, TCGETS2, &mut t as *mut Termios) };
    if rc < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(t)
}

#[cfg(target_os = "linux")]
fn configure(fd: c_int, name: &str, s: &Settings) -> Result<(), Error> {
    let mut t = get_attrs(fd).map_err(|e| match e.raw_os_error() {
        Some(ENOTTY | EINVAL) => Error::NotASerialPort(name.to_string()),
        _ => Error::os("ioctl(TCGETS2)", &e),
    })?;
    make_raw(&mut t, s)?;
    let standard = STANDARD_BAUDS
        .iter()
        .find(|(rate, _)| *rate == s.baud_rate)
        .map(|&(_, code)| code);
    t.c_cflag &= !(CBAUD | CIBAUD);
    t.c_cflag |= standard.unwrap_or(BOTHER);
    t.c_ispeed = s.baud_rate;
    t.c_ospeed = s.baud_rate;
    // SAFETY: TCSETS2 reads one `struct termios2` from the live `t`.
    let rc = unsafe { ioctl(fd, TCSETS2, &t as *const Termios) };
    if rc < 0 {
        let e = io::Error::last_os_error();
        return Err(match e.raw_os_error() {
            Some(EINVAL) if standard.is_none() => Error::UnsupportedBaudRate(s.baud_rate),
            Some(EINVAL) => Error::InvalidSettings("the driver rejected the line settings"),
            _ => Error::os("ioctl(TCSETS2)", &e),
        });
    }
    if standard.is_none() {
        // Drivers round a custom rate to what their divisor allows; refuse it when the
        // result is more than 2 % off, the usual tolerance of a UART.
        let got = get_attrs(fd).map_err(|e| Error::os("ioctl(TCGETS2)", &e))?;
        let want = u64::from(s.baud_rate);
        if u64::from(got.c_ospeed).abs_diff(want) * 50 > want {
            return Err(Error::UnsupportedBaudRate(s.baud_rate));
        }
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn configure(fd: c_int, name: &str, s: &Settings) -> Result<(), Error> {
    let mut t = Termios::default();
    // SAFETY: tcgetattr writes one `struct termios` into the live `t`.
    let rc = unsafe { tcgetattr(fd, &mut t) };
    if rc < 0 {
        let e = io::Error::last_os_error();
        return Err(match e.raw_os_error() {
            Some(ENOTTY | EINVAL) => Error::NotASerialPort(name.to_string()),
            _ => Error::os("tcgetattr", &e),
        });
    }
    make_raw(&mut t, s)?;
    let standard = STANDARD_BAUDS.contains(&s.baud_rate);
    let speed = if standard {
        Speed::from(s.baud_rate)
    } else {
        9600
    };
    // SAFETY: `t` is a live termios; the setters only write its speed fields.
    let rc = unsafe { cfsetispeed(&mut t, speed) | cfsetospeed(&mut t, speed) };
    if rc < 0 {
        return Err(Error::UnsupportedBaudRate(s.baud_rate));
    }
    // SAFETY: tcsetattr reads one `struct termios` from the live `t`.
    let rc = unsafe { tcsetattr(fd, TCSANOW, &t) };
    if rc < 0 {
        let e = io::Error::last_os_error();
        return Err(match e.raw_os_error() {
            Some(EINVAL) => Error::InvalidSettings("the driver rejected the line settings"),
            _ => Error::os("tcsetattr", &e),
        });
    }
    if !standard {
        let rate = Speed::from(s.baud_rate);
        // SAFETY: IOSSIOSPEED reads one speed_t from the live `rate`.
        let rc = unsafe { ioctl(fd, IOSSIOSPEED, &rate as *const Speed) };
        if rc < 0 {
            return Err(Error::UnsupportedBaudRate(s.baud_rate));
        }
    }
    Ok(())
}

pub(crate) struct Port {
    file: File,
    read_timeout: Option<Duration>,
    write_timeout: Option<Duration>,
}

impl Port {
    fn fd(&self) -> c_int {
        self.file.as_raw_fd()
    }

    pub(crate) fn open(name: &str, s: &Settings) -> Result<Port, Error> {
        s.validate()?;
        let path = device_path(name)?;
        // O_NONBLOCK keeps open() from waiting for carrier detect and stays set, so reads
        // and writes never block past their timeouts (poll() does the waiting).
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(O_NOCTTY | O_NONBLOCK)
            .open(&path)
            .map_err(|e| open_error(name, &e))?;
        let fd = file.as_raw_fd();
        // SAFETY: TIOCEXCL takes no argument.
        let rc = unsafe { ioctl(fd, TIOCEXCL) };
        if rc < 0 {
            return Err(match errno() {
                ENOTTY | EINVAL => Error::NotASerialPort(name.to_string()),
                code => Error::Os {
                    op: "ioctl(TIOCEXCL)",
                    code,
                },
            });
        }
        // TIOCEXCL does not stop root; an advisory lock catches a second open by any
        // cooperating program. Errors other than "held" are ignored (no lock support).
        // SAFETY: flock takes only integers.
        let rc = unsafe { flock(fd, LOCK_EX | LOCK_NB) };
        if rc < 0 && io::Error::last_os_error().kind() == io::ErrorKind::WouldBlock {
            return Err(Error::Busy(name.to_string()));
        }
        configure(fd, name, s)?;
        let port = Port {
            file,
            read_timeout: s.read_timeout,
            write_timeout: s.write_timeout,
        };
        port.clear(Buffers::Both)?;
        Ok(port)
    }

    pub(crate) fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        let until = deadline(self.read_timeout);
        loop {
            match (&self.file).read(buf) {
                Ok(n) => return Ok(n),
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
                Err(e) => return Err(e),
            }
            if !wait_fd(self.fd(), POLLIN, until)? {
                return Err(timed_out("read"));
            }
        }
    }

    pub(crate) fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        let until = deadline(self.write_timeout);
        loop {
            match (&self.file).write(buf) {
                Ok(n) => return Ok(n),
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
                Err(e) => return Err(e),
            }
            if !wait_fd(self.fd(), POLLOUT, until)? {
                return Err(timed_out("write"));
            }
        }
    }

    fn out_queue(&self) -> Option<c_int> {
        let mut n: c_int = 0;
        // SAFETY: TIOCOUTQ writes one int into the live `n`.
        let rc = unsafe { ioctl(self.fd(), TIOCOUTQ, &mut n as *mut c_int) };
        (rc >= 0).then_some(n)
    }

    fn drain(&self) -> io::Result<()> {
        loop {
            #[cfg(target_os = "linux")]
            // SAFETY: TCSBRK with a non-zero argument is tcdrain; it takes an integer.
            let rc = unsafe { ioctl(self.fd(), TCSBRK, 1 as c_ulong) };
            #[cfg(target_os = "macos")]
            // SAFETY: tcdrain takes only the descriptor.
            let rc = unsafe { tcdrain(self.fd()) };
            if rc == 0 {
                return Ok(());
            }
            let e = io::Error::last_os_error();
            if e.kind() != io::ErrorKind::Interrupted {
                return Err(e);
            }
        }
    }

    pub(crate) fn flush(&mut self) -> io::Result<()> {
        // tcdrain cannot time out, so first wait (bounded) for the output queue to empty.
        if let Some(until) = deadline(self.write_timeout) {
            while let Some(n) = self.out_queue() {
                if n <= 0 {
                    break;
                }
                if Instant::now() >= until {
                    return Err(timed_out("flush"));
                }
                std::thread::sleep(Duration::from_millis(2));
            }
        }
        self.drain()
    }

    pub(crate) fn clear(&self, b: Buffers) -> Result<(), Error> {
        let queue = match b {
            Buffers::Input => TCIFLUSH,
            Buffers::Output => TCOFLUSH,
            Buffers::Both => TCIOFLUSH,
        };
        #[cfg(target_os = "linux")]
        // SAFETY: TCFLSH takes an integer queue selector.
        let rc = unsafe { ioctl(self.fd(), TCFLSH, queue) };
        #[cfg(target_os = "macos")]
        // SAFETY: tcflush takes only integers.
        let rc = unsafe { tcflush(self.fd(), queue) };
        if rc < 0 {
            return Err(Error::Os {
                op: "tcflush",
                code: errno(),
            });
        }
        Ok(())
    }

    pub(crate) fn set_timeouts(
        &mut self,
        read: Option<Duration>,
        write: Option<Duration>,
    ) -> Result<(), Error> {
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

    fn modem_bit(&self, bit: c_int, on: bool) -> Result<(), Error> {
        let request = if on { TIOCMBIS } else { TIOCMBIC };
        // SAFETY: TIOCMBIS / TIOCMBIC read one int from the live `bit`.
        let rc = unsafe { ioctl(self.fd(), request, &bit as *const c_int) };
        if rc < 0 {
            return Err(Error::Os {
                op: if on {
                    "ioctl(TIOCMBIS)"
                } else {
                    "ioctl(TIOCMBIC)"
                },
                code: errno(),
            });
        }
        Ok(())
    }

    pub(crate) fn set_dtr(&self, on: bool) -> Result<(), Error> {
        self.modem_bit(0x002, on) // TIOCM_DTR
    }

    pub(crate) fn set_rts(&self, on: bool) -> Result<(), Error> {
        self.modem_bit(0x004, on) // TIOCM_RTS
    }

    pub(crate) fn bytes_available(&self) -> Result<u32, Error> {
        let mut n: c_int = 0;
        // SAFETY: FIONREAD writes one int into the live `n`.
        let rc = unsafe { ioctl(self.fd(), FIONREAD, &mut n as *mut c_int) };
        if rc < 0 {
            return Err(Error::Os {
                op: "ioctl(FIONREAD)",
                code: errno(),
            });
        }
        Ok(u32::try_from(n).unwrap_or(0))
    }

    pub(crate) fn try_clone(&self) -> Result<Port, Error> {
        let file = self
            .file
            .try_clone()
            .map_err(|e| Error::os("fcntl(F_DUPFD_CLOEXEC)", &e))?;
        Ok(Port {
            file,
            read_timeout: self.read_timeout,
            write_timeout: self.write_timeout,
        })
    }
}

// ---- port list ---------------------------------------------------------------------

#[cfg(target_os = "linux")]
fn read_text(path: &std::path::Path) -> Option<String> {
    let s = std::fs::read_to_string(path).ok()?;
    let s = s.trim();
    (!s.is_empty()).then(|| s.to_string())
}

/// Classifies a tty's canonical `device` directory by walking up the sysfs tree.
#[cfg(target_os = "linux")]
fn classify_device(dev: &std::path::Path) -> PortKind {
    use crate::ports::parse_hex_u16;
    for p in dev.ancestors().take(8) {
        let vid = read_text(&p.join("idVendor")).and_then(|s| parse_hex_u16(&s));
        let pid = read_text(&p.join("idProduct")).and_then(|s| parse_hex_u16(&s));
        if let (Some(vid), Some(pid)) = (vid, pid) {
            return PortKind::Usb {
                vid,
                pid,
                serial: read_text(&p.join("serial")),
                product: read_text(&p.join("product")),
            };
        }
    }
    for p in dev.ancestors().take(8) {
        let subsystem = std::fs::read_link(p.join("subsystem")).ok();
        match subsystem.as_deref().and_then(|s| s.file_name()?.to_str()) {
            Some("pci") => return PortKind::Pci,
            Some("bluetooth") => return PortKind::Bluetooth,
            _ => {}
        }
    }
    PortKind::Unknown
}

/// The kind of `/sys/class/tty/<name>`, or `None` when it is not a real serial port.
#[cfg(target_os = "linux")]
fn sysfs_kind(name: &str, class_entry: &std::path::Path) -> Option<PortKind> {
    if name.starts_with("rfcomm") {
        return Some(PortKind::Bluetooth);
    }
    let dev = std::fs::canonicalize(class_entry.join("device")).ok()?;
    // The 8250 driver registers placeholder ttyS ports under its platform device.
    if dev.file_name().is_some_and(|f| f == "serial8250") {
        return None;
    }
    Some(classify_device(&dev))
}

#[cfg(target_os = "linux")]
pub(crate) fn available_ports() -> Result<Vec<PortInfo>, Error> {
    let entries = match std::fs::read_dir("/sys/class/tty") {
        Ok(d) => d,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(Error::os("read_dir(/sys/class/tty)", &e)),
    };
    let mut ports = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(kind) = sysfs_kind(&name, &entry.path()) else {
            continue;
        };
        let dev = format!("/dev/{name}");
        if std::path::Path::new(&dev).exists() {
            ports.push(PortInfo { name: dev, kind });
        }
    }
    ports.sort_by(|a, b| natural_cmp(&a.name, &b.name));
    Ok(ports)
}

#[cfg(target_os = "macos")]
pub(crate) fn available_ports() -> Result<Vec<PortInfo>, Error> {
    let entries = std::fs::read_dir("/dev").map_err(|e| Error::os("read_dir(/dev)", &e))?;
    let mut ports: Vec<PortInfo> = entries
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            name.starts_with("cu.").then(|| PortInfo {
                kind: if name.to_ascii_lowercase().contains("bluetooth") {
                    PortKind::Bluetooth
                } else {
                    PortKind::Unknown
                },
                name: format!("/dev/{name}"),
            })
        })
        .collect();
    ports.sort_by(|a, b| natural_cmp(&a.name, &b.name));
    Ok(ports)
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::ffi::c_char;
    use std::os::fd::FromRawFd;
    use std::sync::Mutex;

    unsafe extern "C" {
        fn posix_openpt(flags: c_int) -> c_int;
        fn grantpt(fd: c_int) -> c_int;
        fn unlockpt(fd: c_int) -> c_int;
        fn ptsname(fd: c_int) -> *mut c_char;
    }

    static PTSNAME: Mutex<()> = Mutex::new(());

    /// A pseudo-terminal pair: the master end and the slave's path (the "port").
    fn pty() -> (File, String) {
        const O_RDWR: c_int = 2;
        // SAFETY: posix_openpt takes only integers.
        let fd = unsafe { posix_openpt(O_RDWR | O_NOCTTY) };
        assert!(fd >= 0, "posix_openpt: {}", io::Error::last_os_error());
        // SAFETY: `fd` is a fresh descriptor that nothing else owns.
        let master = unsafe { File::from_raw_fd(fd) };
        // SAFETY: `fd` is a valid pty master.
        assert_eq!(unsafe { grantpt(fd) }, 0);
        // SAFETY: `fd` is a valid pty master.
        assert_eq!(unsafe { unlockpt(fd) }, 0);
        let _guard = PTSNAME.lock().unwrap();
        // SAFETY: ptsname returns a pointer into a static buffer, valid until the next
        // ptsname call, which the mutex excludes while it is copied.
        let p = unsafe { ptsname(fd) };
        assert!(!p.is_null());
        // SAFETY: `p` is a NUL-terminated string (see above).
        let name = unsafe { core::ffi::CStr::from_ptr(p) }
            .to_string_lossy()
            .into_owned();
        (master, name)
    }

    fn read_master(master: &File, n: usize) -> Vec<u8> {
        let until = Instant::now() + Duration::from_secs(5);
        let mut out = Vec::new();
        let mut buf = [0u8; 256];
        while out.len() < n {
            assert!(
                wait_fd(master.as_raw_fd(), POLLIN, Some(until)).unwrap(),
                "master read timed out after {out:?}"
            );
            let k = (&*master).read(&mut buf).unwrap();
            out.extend_from_slice(&buf[..k]);
        }
        out
    }

    fn quick() -> Settings {
        Settings {
            read_timeout: Some(Duration::from_secs(2)),
            ..Settings::default()
        }
    }

    const BINARY: &[u8] = b"\x00\x01\x03\x04\x11\x13\x1b@\r\n\x7f\xffDL";

    #[test]
    fn write_and_read_both_directions() {
        let (master, slave) = pty();
        let mut port = Port::open(&slave, &quick()).unwrap();
        assert_eq!(port.write(BINARY).unwrap(), BINARY.len());
        port.flush().unwrap();
        assert_eq!(read_master(&master, BINARY.len()), BINARY);

        (&master).write_all(BINARY).unwrap();
        let mut got = Vec::new();
        let mut buf = [0u8; 64];
        while got.len() < BINARY.len() {
            let n = port.read(&mut buf).unwrap();
            assert!(n > 0);
            got.extend_from_slice(&buf[..n]);
        }
        assert_eq!(got, BINARY);
    }

    #[test]
    fn read_returns_what_is_available() {
        let (master, slave) = pty();
        let mut port = Port::open(&slave, &quick()).unwrap();
        (&master).write_all(b"abc").unwrap();
        std::thread::sleep(Duration::from_millis(100));
        let mut buf = [0u8; 100];
        let n = port.read(&mut buf).unwrap();
        assert_eq!(&buf[..n], b"abc");
    }

    #[test]
    fn read_times_out() {
        let (_master, slave) = pty();
        let mut port = Port::open(&slave, &quick()).unwrap();
        port.set_timeouts(Some(Duration::from_millis(200)), None)
            .unwrap();
        let start = Instant::now();
        let e = port.read(&mut [0u8; 8]).unwrap_err();
        let took = start.elapsed();
        assert_eq!(e.kind(), io::ErrorKind::TimedOut);
        assert!(took >= Duration::from_millis(190), "{took:?}");
        assert!(took < Duration::from_secs(2), "{took:?}");
        port.set_timeouts(Some(Duration::ZERO), None).unwrap();
        let e = port.read(&mut [0u8; 8]).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::TimedOut);
        assert_eq!(port.read_timeout(), Some(Duration::ZERO));
        assert_eq!(port.write_timeout(), None);
    }

    #[test]
    fn write_times_out_when_nobody_reads() {
        let (_master, slave) = pty();
        let mut port = Port::open(&slave, &quick()).unwrap();
        port.set_timeouts(None, Some(Duration::from_millis(200)))
            .unwrap();
        let chunk = [0x55u8; 4096];
        let start = Instant::now();
        let mut total = 0usize;
        let e = loop {
            match port.write(&chunk) {
                Ok(n) => total += n,
                Err(e) => break e,
            }
            assert!(start.elapsed() < Duration::from_secs(10), "never filled");
        };
        assert_eq!(e.kind(), io::ErrorKind::TimedOut, "after {total} bytes");
        assert!(total > 0);
    }

    #[test]
    fn settings_are_applied() {
        let (_master, slave) = pty();
        let s = Settings {
            baud_rate: 19200,
            data_bits: DataBits::Seven,
            parity: Parity::Even,
            stop_bits: StopBits::Two,
            flow: Flow::XonXoff,
            ..quick()
        };
        let port = Port::open(&slave, &s).unwrap();
        let t = current(&port);
        assert_eq!(t.c_lflag & (ICANON | ECHO | ISIG | IEXTEN), 0);
        assert_eq!(t.c_oflag & OPOST, 0);
        assert_eq!(t.c_iflag & (ICRNL | INLCR | IGNCR | ISTRIP), 0);
        assert_eq!(t.c_iflag & (IXON | IXOFF), IXON | IXOFF);
        assert_ne!(t.c_cflag & CSTOPB, 0);
        assert_ne!(t.c_cflag & CREAD, 0);
        assert_eq!(t.c_cc[VMIN], 1);
        assert_eq!(t.c_cc[VTIME], 0);
        assert_eq!(t.c_cc[VSTART], 0x11);
        assert_eq!(t.c_cc[VSTOP], 0x13);
        // ptys force CS8 and clear PARENB in the kernel, so data bits and parity are
        // checked on the encoded termios in `raw_mode_flags` instead.
        assert_speed(&t, 19200);
    }

    #[cfg(target_os = "linux")]
    fn current(port: &Port) -> Termios {
        get_attrs(port.fd()).unwrap()
    }

    #[cfg(target_os = "macos")]
    fn current(port: &Port) -> Termios {
        let mut t = Termios::default();
        // SAFETY: tcgetattr writes one termios into the live `t`.
        assert_eq!(unsafe { tcgetattr(port.fd(), &mut t) }, 0);
        t
    }

    #[cfg(target_os = "linux")]
    fn assert_speed(t: &Termios, baud: u32) {
        let code = STANDARD_BAUDS.iter().find(|(r, _)| *r == baud).unwrap().1;
        assert_eq!(t.c_cflag & CBAUD, code);
        assert_eq!(t.c_ospeed, baud);
    }

    #[cfg(target_os = "macos")]
    fn assert_speed(t: &Termios, baud: u32) {
        assert_eq!(t.c_ospeed, Speed::from(baud));
    }

    /// The same flags read back through the C library's tcgetattr.
    #[cfg(target_os = "linux")]
    #[test]
    fn libc_tcgetattr_sees_raw_mode() {
        #[repr(C)]
        struct LibcTermios {
            c_iflag: u32,
            c_oflag: u32,
            c_cflag: u32,
            c_lflag: u32,
            c_line: u8,
            c_cc: [u8; 32],
            c_ispeed: u32,
            c_ospeed: u32,
        }
        unsafe extern "C" {
            fn tcgetattr(fd: c_int, t: *mut LibcTermios) -> c_int;
        }
        let (_master, slave) = pty();
        let s = Settings {
            stop_bits: StopBits::Two,
            flow: Flow::RtsCts,
            ..quick()
        };
        let port = Port::open(&slave, &s).unwrap();
        let mut t = LibcTermios {
            c_iflag: 0,
            c_oflag: 0,
            c_cflag: 0,
            c_lflag: 0,
            c_line: 0,
            c_cc: [0; 32],
            c_ispeed: 0,
            c_ospeed: 0,
        };
        // SAFETY: tcgetattr writes one glibc/musl termios into the live `t`.
        assert_eq!(unsafe { tcgetattr(port.fd(), &mut t) }, 0);
        assert_eq!(t.c_lflag & (ICANON | ECHO | ISIG), 0);
        assert_eq!(t.c_oflag & OPOST, 0);
        assert_ne!(t.c_cflag & CSTOPB, 0);
        assert_ne!(t.c_cflag & CRTSCTS, 0);
        assert_eq!(t.c_cflag & CBAUD, 0o15, "B9600");
        assert_eq!((t.c_cc[VMIN], t.c_cc[VTIME]), (1, 0));
        let _ = (t.c_line, t.c_iflag, t.c_ispeed, t.c_ospeed);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn custom_baud_uses_bother() {
        let (_master, slave) = pty();
        let port = Port::open(&slave, &Settings::new(250_000)).unwrap();
        let t = current(&port);
        assert_eq!(t.c_cflag & CBAUD, BOTHER);
        assert_eq!(t.c_ospeed, 250_000);
        assert_eq!(t.c_ispeed, 250_000);
    }

    #[test]
    fn raw_mode_flags() {
        let mut t = Termios {
            c_iflag: ICRNL | IXON | ISTRIP,
            c_oflag: OPOST,
            c_lflag: ICANON | ECHO | ISIG | IEXTEN,
            c_cflag: CS8 | PARODD,
            ..Termios::default()
        };
        let s = Settings {
            data_bits: DataBits::Seven,
            parity: Parity::Even,
            stop_bits: StopBits::Two,
            flow: Flow::RtsCts,
            ..Settings::default()
        };
        make_raw(&mut t, &s).unwrap();
        assert_eq!(t.c_iflag, INPCK);
        assert_eq!(t.c_oflag, 0);
        assert_eq!(t.c_lflag, 0);
        assert_eq!(t.c_cflag, CS7 | PARENB | CSTOPB | CREAD | CLOCAL | CRTSCTS);
        assert_eq!((t.c_cc[VMIN], t.c_cc[VTIME]), (1, 0));

        let five = Settings {
            data_bits: DataBits::Five,
            stop_bits: StopBits::OnePointFive,
            parity: Parity::Odd,
            ..Settings::default()
        };
        make_raw(&mut t, &five).unwrap();
        assert_eq!(t.c_cflag, CS5 | PARENB | PARODD | CSTOPB | CREAD | CLOCAL);

        for (parity, extra) in [(Parity::Mark, PARODD), (Parity::Space, 0)] {
            let s = Settings {
                parity,
                ..Settings::default()
            };
            let r = make_raw(&mut t, &s);
            if HAS_CMSPAR {
                r.unwrap();
                assert_eq!(t.c_cflag, CS8 | PARENB | CMSPAR | extra | CREAD | CLOCAL);
            } else {
                assert!(matches!(r, Err(Error::InvalidSettings(_))));
            }
        }
    }

    #[test]
    fn clear_and_bytes_available() {
        let (master, slave) = pty();
        let mut port = Port::open(&slave, &quick()).unwrap();
        (&master).write_all(b"12345").unwrap();
        let until = Instant::now() + Duration::from_secs(2);
        while port.bytes_available().unwrap() < 5 {
            assert!(Instant::now() < until);
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(port.bytes_available().unwrap(), 5);
        port.clear(Buffers::Input).unwrap();
        assert_eq!(port.bytes_available().unwrap(), 0);
        port.set_timeouts(Some(Duration::from_millis(50)), None)
            .unwrap();
        assert_eq!(
            port.read(&mut [0u8; 8]).unwrap_err().kind(),
            io::ErrorKind::TimedOut
        );
        port.clear(Buffers::Output).unwrap();
        port.clear(Buffers::Both).unwrap();
    }

    #[test]
    fn second_open_is_busy() {
        let (_master, slave) = pty();
        let _first = Port::open(&slave, &quick()).unwrap();
        let e = Port::open(&slave, &quick()).err().unwrap();
        assert_eq!(e, Error::Busy(slave.clone()));
    }

    #[test]
    fn clone_shares_the_port() {
        let (master, slave) = pty();
        let mut port = Port::open(&slave, &quick()).unwrap();
        let mut other = port.try_clone().unwrap();
        other.write(b"hi").unwrap();
        assert_eq!(read_master(&master, 2), b"hi");
        (&master).write_all(b"yo").unwrap();
        let mut buf = [0u8; 2];
        let mut n = 0;
        while n < 2 {
            n += port.read(&mut buf[n..]).unwrap();
        }
        assert_eq!(&buf, b"yo");
    }

    #[test]
    fn open_errors() {
        assert_eq!(
            Port::open("/dev/dl-serial-does-not-exist", &quick()).err(),
            Some(Error::NotFound("/dev/dl-serial-does-not-exist".into()))
        );
        assert_eq!(
            Port::open("dl-serial-does-not-exist", &quick()).err(),
            Some(Error::NotFound("dl-serial-does-not-exist".into()))
        );
        assert_eq!(
            Port::open("", &quick()).err(),
            Some(Error::InvalidName(String::new()))
        );
        assert_eq!(
            Port::open("tty\0", &quick()).err(),
            Some(Error::InvalidName("tty\0".into()))
        );
        assert_eq!(
            Port::open("/dev/null", &quick()).err(),
            Some(Error::NotASerialPort("/dev/null".into()))
        );
        assert!(matches!(
            Port::open("/dev/null", &Settings::new(0)).err(),
            Some(Error::InvalidSettings(_))
        ));
        assert_eq!(device_path("ttyUSB0").unwrap(), "/dev/ttyUSB0");
        assert_eq!(device_path("/dev/cu.x").unwrap(), "/dev/cu.x");
    }

    #[test]
    fn listing_works() {
        let ports = available_ports().unwrap();
        for p in &ports {
            assert!(p.name.starts_with("/dev/"), "{p:?}");
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn sysfs_classification() {
        let root = std::env::temp_dir().join(format!("dl-serial-sysfs-{}", std::process::id()));
        let usb_dev = root.join("devices/pci0000:00/0000:00:14.0/usb1/1-2");
        let tty = usb_dev.join("1-2:1.0/ttyUSB0");
        std::fs::create_dir_all(&tty).unwrap();
        std::fs::write(usb_dev.join("idVendor"), "1a86\n").unwrap();
        std::fs::write(usb_dev.join("idProduct"), "7523\n").unwrap();
        std::fs::write(usb_dev.join("product"), "USB Serial\n").unwrap();
        assert_eq!(
            classify_device(&tty),
            PortKind::Usb {
                vid: 0x1a86,
                pid: 0x7523,
                serial: None,
                product: Some("USB Serial".into())
            }
        );
        let pci = root.join("devices/pci0000:00/0000:03:00.0");
        std::fs::create_dir_all(&pci).unwrap();
        let bus = root.join("bus/pci");
        std::fs::create_dir_all(&bus).unwrap();
        std::os::unix::fs::symlink(&bus, pci.join("subsystem")).unwrap();
        assert_eq!(classify_device(&pci), PortKind::Pci);
        let plain = root.join("devices/pnp0/00:04");
        std::fs::create_dir_all(&plain).unwrap();
        assert_eq!(classify_device(&plain), PortKind::Unknown);

        let class = root.join("class/tty");
        let placeholder = root.join("devices/platform/serial8250");
        std::fs::create_dir_all(class.join("ttyS4")).unwrap();
        std::fs::create_dir_all(class.join("ttyS0")).unwrap();
        std::fs::create_dir_all(&placeholder).unwrap();
        std::os::unix::fs::symlink(&placeholder, class.join("ttyS4/device")).unwrap();
        std::os::unix::fs::symlink(&plain, class.join("ttyS0/device")).unwrap();
        assert_eq!(sysfs_kind("ttyS4", &class.join("ttyS4")), None);
        assert_eq!(
            sysfs_kind("ttyS0", &class.join("ttyS0")),
            Some(PortKind::Unknown)
        );
        assert_eq!(
            sysfs_kind("rfcomm0", &class.join("rfcomm0")),
            Some(PortKind::Bluetooth)
        );
        assert_eq!(sysfs_kind("tty0", &class.join("tty0")), None);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn termios2_layout() {
        assert_eq!(size_of::<Termios>(), 44);
    }
}
