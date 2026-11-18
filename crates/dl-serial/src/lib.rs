#![doc = include_str!("../README.md")]

mod error;
mod ports;
mod settings;
mod win_encode;

#[cfg(windows)]
#[path = "sys_windows.rs"]
mod sys;

#[cfg(any(
    all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64")
    ),
    target_os = "macos"
))]
#[path = "sys_unix.rs"]
mod sys;

#[cfg(not(any(
    windows,
    all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64")
    ),
    target_os = "macos"
)))]
#[path = "sys_stub.rs"]
mod sys;

use core::fmt;
use core::time::Duration;
use std::io;

pub use error::Error;
pub use ports::{PortInfo, PortKind};
pub use settings::{Buffers, DataBits, Flow, Parity, Settings, StopBits};

/// An open serial port with exclusive access.
///
/// Reads return as soon as at least one byte is available, with everything already
/// received up to the buffer size. A read that gets nothing within the read timeout, or a
/// write the port does not accept within the write timeout, fails with
/// [`io::ErrorKind::TimedOut`]. The port is closed on drop.
pub struct SerialPort {
    inner: sys::Port,
    name: String,
}

impl SerialPort {
    /// Opens and configures a port: `COM3` (any number) on Windows, `/dev/ttyUSB0` or
    /// `/dev/cu.usbserial-1410` on Unix (a name without `/` is looked up in `/dev`).
    ///
    /// DTR and RTS are raised on open (RTS follows the handshake with
    /// [`Flow::RtsCts`]); use [`set_dtr`](Self::set_dtr) / [`set_rts`](Self::set_rts) to
    /// change them. Pending input and output are discarded.
    pub fn open(name: &str, settings: &Settings) -> Result<SerialPort, Error> {
        Ok(SerialPort {
            inner: sys::Port::open(name, settings)?,
            name: name.to_string(),
        })
    }

    /// The name the port was opened with.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Discards received bytes not read yet, written bytes not sent yet, or both.
    pub fn clear(&self, buffers: Buffers) -> Result<(), Error> {
        self.inner.clear(buffers)
    }

    /// Changes the read and write timeouts (see [`Settings::read_timeout`]).
    ///
    /// On Windows the timeouts belong to the device, so they also change for clones made
    /// with [`try_clone`](Self::try_clone).
    pub fn set_timeouts(
        &mut self,
        read: Option<Duration>,
        write: Option<Duration>,
    ) -> Result<(), Error> {
        self.inner.set_timeouts(read, write)
    }

    /// The current read timeout.
    pub fn read_timeout(&self) -> Option<Duration> {
        self.inner.read_timeout()
    }

    /// The current write timeout.
    pub fn write_timeout(&self) -> Option<Duration> {
        self.inner.write_timeout()
    }

    /// Raises (`true`) or lowers the DTR line.
    pub fn set_dtr(&self, on: bool) -> Result<(), Error> {
        self.inner.set_dtr(on)
    }

    /// Raises (`true`) or lowers the RTS line. Fails on Windows while
    /// [`Flow::RtsCts`] controls the line.
    pub fn set_rts(&self, on: bool) -> Result<(), Error> {
        self.inner.set_rts(on)
    }

    /// The number of received bytes waiting to be read.
    pub fn bytes_available(&self) -> Result<u32, Error> {
        self.inner.bytes_available()
    }

    /// A second handle to the same port, for example to read in one thread and write in
    /// another. Line settings are shared.
    pub fn try_clone(&self) -> Result<SerialPort, Error> {
        Ok(SerialPort {
            inner: self.inner.try_clone()?,
            name: self.name.clone(),
        })
    }
}

impl io::Read for SerialPort {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.inner.read(buf)
    }
}

impl io::Write for SerialPort {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.inner.write(buf)
    }

    /// Waits until everything written has been transmitted (`FlushFileBuffers` /
    /// `tcdrain`). On Unix the wait for the output queue is bounded by the write timeout.
    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

impl fmt::Debug for SerialPort {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SerialPort")
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}

/// Lists the serial ports of the machine, sorted by name (`COM2` before `COM10`).
///
/// - Windows: the values of `HKLM\HARDWARE\DEVICEMAP\SERIALCOMM`; the kind and USB ids
///   come from the device nodes under `HKLM\SYSTEM\CurrentControlSet\Enum` (best effort).
/// - Linux: `/sys/class/tty/*` entries with a real device (USB, ACM, PCI and built-in
///   ports, but not the placeholder `ttyS*` of the 8250 driver) and `rfcomm*`.
/// - macOS: the `/dev/cu.*` call-out devices.
pub fn available_ports() -> Result<Vec<PortInfo>, Error> {
    sys::available_ports()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn traits() {
        fn send_sync<T: Send + Sync>() {}
        send_sync::<SerialPort>();
        send_sync::<Error>();
        send_sync::<Settings>();
    }

    #[cfg(not(any(
        windows,
        all(
            target_os = "linux",
            any(target_arch = "x86_64", target_arch = "aarch64")
        ),
        target_os = "macos"
    )))]
    #[test]
    fn stub_is_unsupported() {
        assert_eq!(
            SerialPort::open("COM1", &Settings::default()).err(),
            Some(Error::Unsupported)
        );
        assert_eq!(available_ports(), Err(Error::Unsupported));
    }
}
