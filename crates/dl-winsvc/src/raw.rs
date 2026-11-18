//! Plain data shared by the Windows bindings and the stubs.

use core::ffi::c_void;

/// `SERVICE_STATUS`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ServiceStatus {
    pub service_type: u32,
    pub current_state: u32,
    pub controls_accepted: u32,
    pub win32_exit_code: u32,
    pub service_specific_exit_code: u32,
    pub check_point: u32,
    pub wait_hint: u32,
}

/// The fields of `QUERY_SERVICE_CONFIGW` the crate exposes.
#[derive(Clone, Debug, Default)]
pub(crate) struct RawConfig {
    pub start_type: u32,
    pub binary_path_name: String,
    pub display_name: String,
    pub start_name: String,
}

/// One entry of `EnumServicesStatusExW`.
#[derive(Clone, Debug)]
pub(crate) struct RawService {
    pub name: String,
    pub display_name: String,
    pub status: ServiceStatus,
}

/// `LPSERVICE_MAIN_FUNCTIONW`.
pub(crate) type ServiceMainFn = extern "system" fn(u32, *mut *mut u16);

/// `LPHANDLER_FUNCTION_EX`.
pub(crate) type HandlerFn = extern "system" fn(u32, u32, *mut c_void, *mut c_void) -> u32;

pub(crate) const SERVICE_WIN32_OWN_PROCESS: u32 = 0x10;
pub(crate) const SERVICE_ERROR_NORMAL: u32 = 1;

pub(crate) const SERVICE_STOPPED: u32 = 1;
pub(crate) const SERVICE_START_PENDING: u32 = 2;
pub(crate) const SERVICE_STOP_PENDING: u32 = 3;
pub(crate) const SERVICE_RUNNING: u32 = 4;
pub(crate) const SERVICE_CONTINUE_PENDING: u32 = 5;
pub(crate) const SERVICE_PAUSE_PENDING: u32 = 6;
pub(crate) const SERVICE_PAUSED: u32 = 7;

pub(crate) const SERVICE_ACCEPT_STOP: u32 = 0x1;
pub(crate) const SERVICE_ACCEPT_SHUTDOWN: u32 = 0x4;

pub(crate) const SERVICE_CONTROL_STOP: u32 = 1;
pub(crate) const SERVICE_CONTROL_INTERROGATE: u32 = 4;
pub(crate) const SERVICE_CONTROL_SHUTDOWN: u32 = 5;

pub(crate) const SC_MANAGER_CONNECT: u32 = 0x1;
pub(crate) const SC_MANAGER_ENUMERATE_SERVICE: u32 = 0x4;
pub(crate) const SC_MANAGER_ALL_ACCESS: u32 = 0xF003F;
pub(crate) const SERVICE_QUERY_CONFIG: u32 = 0x1;
pub(crate) const SERVICE_QUERY_STATUS: u32 = 0x4;
pub(crate) const SERVICE_ALL_ACCESS: u32 = 0xF01FF;

pub(crate) const CRYPTPROTECT_UI_FORBIDDEN: u32 = 0x1;
pub(crate) const CRYPTPROTECT_LOCAL_MACHINE: u32 = 0x4;
