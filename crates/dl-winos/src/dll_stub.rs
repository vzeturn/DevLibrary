//! Stand-ins for platforms without the Windows DLL loader: every call fails with
//! [`Error::Unsupported`].

use core::ffi::c_void;
use core::ptr::NonNull;
use std::path::PathBuf;

use crate::Error;

// Never constructed: every function that would return one fails first.
#[allow(dead_code)]
pub(crate) struct Module(());

impl Module {
    #[allow(dead_code)]
    pub(crate) fn addr(&self) -> usize {
        0
    }
}

pub(crate) fn load(_name: &[u16], _flags: u32) -> Result<Module, Error> {
    Err(Error::Unsupported)
}

pub(crate) fn leak(_m: Module) {}

pub(crate) fn proc_address(_m: &Module, _name: &[u8]) -> Result<NonNull<c_void>, Error> {
    Err(Error::Unsupported)
}

pub(crate) fn module_path(_m: &Module) -> Result<PathBuf, Error> {
    Err(Error::Unsupported)
}

pub(crate) fn acp() -> Result<u32, Error> {
    Err(Error::Unsupported)
}

pub(crate) fn to_acp(_wide: &[u16], _code_page: u32) -> Result<Option<Vec<u8>>, Error> {
    Err(Error::Unsupported)
}

pub(crate) fn from_acp(_bytes: &[u8]) -> Result<Vec<u16>, Error> {
    Err(Error::Unsupported)
}
