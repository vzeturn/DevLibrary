//! Stand-ins for platforms without a Windows registry or print spooler: every call fails
//! with [`Error::Unsupported`] so callers compile and can fall back at run time.

use crate::Error;

// Never constructed: every function that would return one fails first.
#[allow(dead_code)]
pub(crate) struct RegKey(());

pub(crate) fn reg_open_hive(_hive: u32, _subkey: &[u16], _access: u32) -> Result<RegKey, Error> {
    Err(Error::Unsupported)
}

pub(crate) fn reg_open_sub(
    _parent: &RegKey,
    _subkey: &[u16],
    _access: u32,
) -> Result<RegKey, Error> {
    Err(Error::Unsupported)
}

pub(crate) fn reg_query_value(_k: &RegKey, _name: &[u16]) -> Result<Option<(u32, Vec<u8>)>, Error> {
    Err(Error::Unsupported)
}

pub(crate) fn reg_subkeys(_k: &RegKey) -> Result<Vec<String>, Error> {
    Err(Error::Unsupported)
}

pub(crate) fn reg_values(_k: &RegKey) -> Result<Vec<(String, u32, Vec<u8>)>, Error> {
    Err(Error::Unsupported)
}

pub(crate) fn expand_env(_src: &[u16]) -> Result<String, Error> {
    Err(Error::Unsupported)
}

pub(crate) fn enum_printers() -> Result<Vec<(String, String, String, u32)>, Error> {
    Err(Error::Unsupported)
}

pub(crate) fn default_printer() -> Result<Option<String>, Error> {
    Err(Error::Unsupported)
}

pub(crate) fn write_raw(_printer: &[u16], _document: &[u16], _data: &[u8]) -> Result<u32, Error> {
    Err(Error::Unsupported)
}
