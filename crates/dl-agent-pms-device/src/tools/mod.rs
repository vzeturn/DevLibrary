//! Sub-commands the module adds to `dl-agent`: `pms-device-enroll` (writes the PMS API URL,
//! agent id and token file the module reads from the DeviceAgent appsettings).

pub mod enroll;

pub use enroll::pms_device_enroll_tool;
