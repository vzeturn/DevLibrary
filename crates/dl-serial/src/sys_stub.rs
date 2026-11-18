//! Platforms without serial port support (WebAssembly, other Unix targets): opening fails
//! with `Error::Unsupported`, so a `Port` value never exists.

use core::time::Duration;
use std::io;

use crate::{Buffers, Error, PortInfo, Settings};

pub(crate) enum Port {}

impl Port {
    pub(crate) fn open(_name: &str, s: &Settings) -> Result<Port, Error> {
        s.validate()?;
        Err(Error::Unsupported)
    }

    pub(crate) fn read(&mut self, _buf: &mut [u8]) -> io::Result<usize> {
        match *self {}
    }

    pub(crate) fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
        match *self {}
    }

    pub(crate) fn flush(&mut self) -> io::Result<()> {
        match *self {}
    }

    pub(crate) fn clear(&self, _b: Buffers) -> Result<(), Error> {
        match *self {}
    }

    pub(crate) fn set_timeouts(
        &mut self,
        _read: Option<Duration>,
        _write: Option<Duration>,
    ) -> Result<(), Error> {
        match *self {}
    }

    pub(crate) fn read_timeout(&self) -> Option<Duration> {
        match *self {}
    }

    pub(crate) fn write_timeout(&self) -> Option<Duration> {
        match *self {}
    }

    pub(crate) fn set_dtr(&self, _on: bool) -> Result<(), Error> {
        match *self {}
    }

    pub(crate) fn set_rts(&self, _on: bool) -> Result<(), Error> {
        match *self {}
    }

    pub(crate) fn bytes_available(&self) -> Result<u32, Error> {
        match *self {}
    }

    pub(crate) fn try_clone(&self) -> Result<Port, Error> {
        match *self {}
    }
}

pub(crate) fn available_ports() -> Result<Vec<PortInfo>, Error> {
    Err(Error::Unsupported)
}
