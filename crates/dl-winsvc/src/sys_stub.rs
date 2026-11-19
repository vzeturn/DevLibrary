//! Stand-ins for platforms without Windows services: every call fails with
//! [`Error::Unsupported`] so callers compile and can fall back at run time.

use crate::Error;
use crate::process::Process;
use crate::raw::{HandlerFn, RawConfig, RawService, ServiceMainFn, ServiceStatus};

// Never constructed: every function that would return one fails first.
#[allow(dead_code)]
pub(crate) struct ScHandle(());

#[allow(dead_code)]
pub(crate) struct StatusHandle(());

pub(crate) const NO_CHANGE: u32 = 0xFFFF_FFFF;

pub(crate) fn open_sc_manager(_access: u32) -> Result<ScHandle, Error> {
    Err(Error::Unsupported)
}

pub(crate) fn open_service(
    _scm: &ScHandle,
    _name: &[u16],
    _access: u32,
) -> Result<ScHandle, Error> {
    Err(Error::Unsupported)
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn create_service(
    _scm: &ScHandle,
    _name: &[u16],
    _display: &[u16],
    _access: u32,
    _service_type: u32,
    _start_type: u32,
    _error_control: u32,
    _binary_path: &[u16],
) -> Result<ScHandle, Error> {
    Err(Error::Unsupported)
}

pub(crate) fn change_service_config(
    _h: &ScHandle,
    _service_type: u32,
    _start_type: u32,
    _error_control: u32,
    _binary_path: Option<&[u16]>,
    _display: Option<&[u16]>,
) -> Result<(), Error> {
    Err(Error::Unsupported)
}

pub(crate) fn set_description(_h: &ScHandle, _description: &[u16]) -> Result<(), Error> {
    Err(Error::Unsupported)
}

pub(crate) fn query_config(_h: &ScHandle) -> Result<RawConfig, Error> {
    Err(Error::Unsupported)
}

pub(crate) fn query_description(_h: &ScHandle) -> Result<String, Error> {
    Err(Error::Unsupported)
}

pub(crate) fn query_status(_h: &ScHandle) -> Result<ServiceStatus, Error> {
    Err(Error::Unsupported)
}

pub(crate) fn start_service(_h: &ScHandle, _args: &[Vec<u16>]) -> Result<(), Error> {
    Err(Error::Unsupported)
}

pub(crate) fn control_service(_h: &ScHandle, _control: u32) -> Result<ServiceStatus, Error> {
    Err(Error::Unsupported)
}

pub(crate) fn delete_service(_h: &ScHandle) -> Result<(), Error> {
    Err(Error::Unsupported)
}

pub(crate) fn enum_services(_scm: &ScHandle) -> Result<Vec<RawService>, Error> {
    Err(Error::Unsupported)
}

pub(crate) fn crypt_protect(_data: &[u8], _flags: u32, _entropy: &[u8]) -> Result<Vec<u8>, Error> {
    Err(Error::Unsupported)
}

pub(crate) fn crypt_unprotect(
    _data: &[u8],
    _flags: u32,
    _entropy: &[u8],
) -> Result<Vec<u8>, Error> {
    Err(Error::Unsupported)
}

pub(crate) fn set_failure_actions(
    _h: &ScHandle,
    _reset_seconds: u32,
    _actions: &[(u32, u32)],
) -> Result<(), Error> {
    Err(Error::Unsupported)
}

pub(crate) fn query_failure_actions(_h: &ScHandle) -> Result<(u32, Vec<(u32, u32)>), Error> {
    Err(Error::Unsupported)
}

pub(crate) fn set_bool_config(_h: &ScHandle, _level: u32, _value: bool) -> Result<(), Error> {
    Err(Error::Unsupported)
}

pub(crate) fn query_bool_config(_h: &ScHandle, _level: u32) -> Result<bool, Error> {
    Err(Error::Unsupported)
}

pub(crate) fn read_security(_path: &[u16]) -> Result<crate::acl::Descriptor, Error> {
    Err(Error::Unsupported)
}

pub(crate) fn restrict_path(_path: &[u16], _is_dir: bool) -> Result<(), Error> {
    Err(Error::Unsupported)
}

pub(crate) fn parent_is_services() -> Result<bool, Error> {
    Ok(false)
}

pub(crate) fn processes() -> Result<Vec<Process>, Error> {
    Err(Error::Unsupported)
}

pub(crate) fn start_dispatcher(_name: &[u16], _main: ServiceMainFn) -> Result<(), Error> {
    Err(Error::Unsupported)
}

impl StatusHandle {
    pub(crate) fn set(&self, _status: &ServiceStatus) -> Result<(), Error> {
        Err(Error::Unsupported)
    }
}

pub(crate) fn register_handler(_name: &[u16], _handler: HandlerFn) -> Result<StatusHandle, Error> {
    Err(Error::Unsupported)
}
