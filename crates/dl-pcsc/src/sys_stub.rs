//! Platforms without a PC/SC library (WebAssembly and others): every call returns
//! `Error::Unsupported`.

use crate::{Error, ReaderStatus, ReaderWait};

/// Placeholder for `SCARDCONTEXT` / `SCARDHANDLE`.
pub(crate) type Handle = usize;

/// `INFINITE` timeout of `SCardGetStatusChange`.
pub(crate) const INFINITE: u32 = 0xFFFF_FFFF;

fn unsupported() -> Error {
    Error::Unsupported("no PC/SC library exists on this platform".to_owned())
}

pub(crate) fn load_library() -> Result<(), Error> {
    Err(unsupported())
}

pub(crate) fn establish(_scope: u32) -> Result<Handle, Error> {
    Err(unsupported())
}

pub(crate) fn release(_ctx: Handle) {}

pub(crate) fn cancel(_ctx: Handle) -> Result<(), Error> {
    Err(unsupported())
}

pub(crate) fn list_readers(_ctx: Handle) -> Result<Vec<String>, Error> {
    Err(unsupported())
}

pub(crate) fn get_status_change(
    _ctx: Handle,
    _timeout_ms: u32,
    _readers: &[ReaderWait],
) -> Result<Vec<ReaderStatus>, Error> {
    Err(unsupported())
}

pub(crate) fn connect(
    _ctx: Handle,
    _reader: &str,
    _share: u32,
    _protocols: u32,
) -> Result<(Handle, u32), Error> {
    Err(unsupported())
}

pub(crate) fn disconnect(_card: Handle, _disposition: u32) -> Result<(), Error> {
    Err(unsupported())
}

pub(crate) fn begin_transaction(_card: Handle) -> Result<(), Error> {
    Err(unsupported())
}

pub(crate) fn end_transaction(_card: Handle, _disposition: u32) -> Result<(), Error> {
    Err(unsupported())
}

pub(crate) fn transmit(
    _card: Handle,
    _protocol: u32,
    _apdu: &[u8],
    _capacity: usize,
) -> Result<Vec<u8>, Error> {
    Err(unsupported())
}

pub(crate) fn status(_card: Handle) -> Result<(String, u32, u32, Vec<u8>), Error> {
    Err(unsupported())
}
