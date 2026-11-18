//! `OposAdapter.cs` (OPOS printer, drawer, line display; POS-HW-03) and
//! `BluetoothRefusingAdapter`.
//!
//! C# refuses every OPOS verb (the SMILE_OPOS_* vendor wrappers are not in the build). The
//! UnifiedPOS OPOS Common Control Objects are a public standard, so where the verb maps
//! certainly onto a CCO method this adapter drives the CCO through `IDispatch` when it is
//! registered, with the target's endpoint as the logical device name:
//!
//! | verb | CCO (`OPOS.*`) | calls |
//! |------|----------------|-------|
//! | pos.kick | CashDrawer | Open, ClaimDevice, DeviceEnabled, OpenDrawer |
//! | pos.display | LineDisplay | Open, ClaimDevice, DeviceEnabled, ClearText, DisplayTextAt |
//!
//! then DeviceEnabled = false, ReleaseDevice and Close. When the CCO is not registered
//! (`REGDB_E_CLASSNOTREG`, `CO_E_CLASSSTRING`; also a 32-bit-only CCO in a 64-bit agent), or
//! on a host without COM, the exact C# refusal stays. pos.print (raw ESC/POS bytes, which
//! `PrintNormal` does not define), pos.read (a printer status shape the POSPrinter
//! properties do not give, and MSR data that only arrives through `DataEvent`, which
//! dl-winos cannot sink) and pos.speak (a ToneIndicator cannot speak) keep the C# refusal.
//!
//! OPOS `ResultCode` -> device code: OPOS_E_NOSERVICE (104) -> DEVICE_VENDOR_SDK_MISSING;
//! OPOS_E_NOEXIST (109) -> DEVICE_NOT_CONFIGURED; OPOS_E_CLOSED (101), OPOS_E_CLAIMED (102),
//! OPOS_E_NOHARDWARE (107), OPOS_E_OFFLINE (108), OPOS_E_TIMEOUT (112), OPOS_E_BUSY (113) ->
//! DEVICE_UNAVAILABLE; every other non-zero code (OPOS_E_ILLEGAL, OPOS_E_FAILURE,
//! OPOS_E_EXTENDED with its ResultCodeExtended, ...) -> DEVICE_ERROR. A COM failure of a
//! call is DEVICE_ERROR with its HRESULT.

use std::sync::Arc;

use dl_json::{Value, json};

use super::escpos::EscPos;
use super::router::shown_lines;
use super::{failures, target_of, text};
use crate::adapters::{Cx, PosDevice};
use crate::contracts::{
    AdapterError, PosDisplayRequest, PosKickRequest, PosPrintRequest, PosReadRequest,
    PosSpeakRequest, PosTarget, codes, read_devices, transports,
};

/// The OPOS `ResultCode` values (UnifiedPOS, Opos.h).
pub mod result_codes {
    /// OPOS_SUCCESS.
    pub const SUCCESS: i32 = 0;
    /// OPOS_E_CLOSED.
    pub const CLOSED: i32 = 101;
    /// OPOS_E_CLAIMED.
    pub const CLAIMED: i32 = 102;
    /// OPOS_E_NOTCLAIMED.
    pub const NOT_CLAIMED: i32 = 103;
    /// OPOS_E_NOSERVICE.
    pub const NO_SERVICE: i32 = 104;
    /// OPOS_E_DISABLED.
    pub const DISABLED: i32 = 105;
    /// OPOS_E_ILLEGAL.
    pub const ILLEGAL: i32 = 106;
    /// OPOS_E_NOHARDWARE.
    pub const NO_HARDWARE: i32 = 107;
    /// OPOS_E_OFFLINE.
    pub const OFFLINE: i32 = 108;
    /// OPOS_E_NOEXIST.
    pub const NO_EXIST: i32 = 109;
    /// OPOS_E_EXISTS.
    pub const EXISTS: i32 = 110;
    /// OPOS_E_FAILURE.
    pub const FAILURE: i32 = 111;
    /// OPOS_E_TIMEOUT.
    pub const TIMEOUT: i32 = 112;
    /// OPOS_E_BUSY.
    pub const BUSY: i32 = 113;
    /// OPOS_E_EXTENDED.
    pub const EXTENDED: i32 = 114;
    /// OPOS_E_DEPRECATED.
    pub const DEPRECATED: i32 = 115;
}

/// The Opos.h name of a `ResultCode`.
pub fn opos_code_name(rc: i32) -> Option<&'static str> {
    use result_codes as r;
    Some(match rc {
        r::SUCCESS => "OPOS_SUCCESS",
        r::CLOSED => "OPOS_E_CLOSED",
        r::CLAIMED => "OPOS_E_CLAIMED",
        r::NOT_CLAIMED => "OPOS_E_NOTCLAIMED",
        r::NO_SERVICE => "OPOS_E_NOSERVICE",
        r::DISABLED => "OPOS_E_DISABLED",
        r::ILLEGAL => "OPOS_E_ILLEGAL",
        r::NO_HARDWARE => "OPOS_E_NOHARDWARE",
        r::OFFLINE => "OPOS_E_OFFLINE",
        r::NO_EXIST => "OPOS_E_NOEXIST",
        r::EXISTS => "OPOS_E_EXISTS",
        r::FAILURE => "OPOS_E_FAILURE",
        r::TIMEOUT => "OPOS_E_TIMEOUT",
        r::BUSY => "OPOS_E_BUSY",
        r::EXTENDED => "OPOS_E_EXTENDED",
        r::DEPRECATED => "OPOS_E_DEPRECATED",
        _ => return None,
    })
}

/// An argument of an OPOS method.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OposArg {
    /// A `Long`.
    I32(i32),
    /// A `String` (BSTR).
    Str(String),
}

/// A failed COM call (`HRESULT` and dl-winos detail).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComFailure {
    /// The `HRESULT` (0 when the failure was not a COM error).
    pub hresult: u32,
    /// What failed.
    pub detail: String,
}

/// One late-bound OPOS control object. Faked in tests.
pub trait OposControl {
    /// Calls a method that returns the OPOS `ResultCode` (Open, ClaimDevice, OpenDrawer, ...).
    fn call(&mut self, method: &str, args: &[OposArg]) -> Result<i32, ComFailure>;
    /// Sets a boolean property (DeviceEnabled).
    fn set_bool(&mut self, property: &str, value: bool) -> Result<(), ComFailure>;
    /// Reads a `Long` property (ResultCode, ResultCodeExtended).
    fn get_i32(&mut self, property: &str) -> Result<i32, ComFailure>;
}

/// The work done with one control object, on the host's COM thread.
pub type OposSession = Box<dyn FnOnce(&mut dyn OposControl) -> Result<Value, AdapterError> + Send>;

/// Why a control object could not be created.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OposOpen {
    /// The CCO is not registered (`REGDB_E_CLASSNOTREG`, `CO_E_CLASSSTRING`).
    NotRegistered,
    /// Any other creation failure.
    Failed(ComFailure),
}

/// Creates OPOS control objects by ProgID and runs a session with one. Faked in tests.
pub trait OposHost: Send + Sync {
    /// Creates `prog_id` and runs `session` with it (then releases it).
    fn with_control(
        &self,
        prog_id: &str,
        session: OposSession,
    ) -> Result<Result<Value, AdapterError>, OposOpen>;
}

/// The POSWin OPOS profiles: the generic 1.8 wrapper and the Wincor Nixdorf one, in the C#
/// dictionary order (the refusal lists them in this order).
pub const PROFILES: [(&str, &str); 2] =
    [("1.8", "SMILE_OPOS_1_8"), ("wincor", "SMILE_OPOS_WINCOR")];

/// `ClaimDevice` timeout, milliseconds.
const CLAIM_TIMEOUT_MS: i32 = 1000;

/// OPOS printer, drawer, line display, MSR and tone indicator.
pub struct OposAdapter {
    host: Option<Arc<dyn OposHost>>,
}

impl OposAdapter {
    /// The C# adapter: every verb refuses (vendor-sdk-missing).
    pub fn refusing() -> OposAdapter {
        OposAdapter { host: None }
    }

    /// An adapter driving registered CCOs through `host`.
    pub fn new(host: Arc<dyn OposHost>) -> OposAdapter {
        OposAdapter { host: Some(host) }
    }

    /// COM on Windows; the refusing adapter elsewhere.
    pub fn create() -> OposAdapter {
        #[cfg(windows)]
        {
            OposAdapter::new(Arc::new(com_host::ComOposHost::new()))
        }
        #[cfg(not(windows))]
        {
            OposAdapter::refusing()
        }
    }

    /// The wrapper of the target's profile, or the C# bad-payload refusal.
    fn wrapper(target: &PosTarget) -> Result<&'static str, AdapterError> {
        let profile = text(&target.opos_profile);
        PROFILES
            .iter()
            .find(|(name, _)| eq_ignore_case(name, profile))
            .map(|&(_, wrapper)| wrapper)
            .ok_or_else(|| {
                failures::bad_payload(format!(
                    "OPOS profile must be one of {}, got '{profile}'",
                    PROFILES.map(|(name, _)| name).join(", ")
                ))
            })
    }

    /// The C# `Refuse(target, deviceClass)`.
    fn refuse(target: &Option<PosTarget>, class: &str) -> Result<Value, AdapterError> {
        let target = target_of(target)?;
        Err(Self::refusal(target, class, Self::wrapper(target)?))
    }

    fn refusal(target: &PosTarget, class: &str, wrapper: &str) -> AdapterError {
        failures::vendor_sdk_missing(&format!(
            "OPOS {class} '{}' needs the {wrapper} service object, which is not installed in this agent",
            text(&target.endpoint)
        ))
    }

    /// Drives `OPOS.{class}` when it is registered; the C# refusal otherwise.
    fn drive(
        &self,
        target: &Option<PosTarget>,
        class: &'static str,
        validate: impl FnOnce() -> Result<(), AdapterError> + Send + 'static,
        body: impl FnOnce(&mut Device<'_>) -> Result<Value, AdapterError> + Send + 'static,
    ) -> Result<Value, AdapterError> {
        let target = target_of(target)?;
        let wrapper = Self::wrapper(target)?;
        let Some(host) = &self.host else {
            return Err(Self::refusal(target, class, wrapper));
        };
        let name = text(&target.endpoint).trim().to_owned();
        let session_name = name.clone();
        let session: OposSession = Box::new(move |control| {
            // Checked once the control object exists: an unregistered one keeps the C# refusal.
            if session_name.is_empty() {
                return Err(failures::bad_payload(
                    "opos endpoint (logical device name) is empty",
                ));
            }
            validate()?;
            let mut device = Device {
                control,
                class,
                name: &session_name,
            };
            device.session(body)
        });
        match host.with_control(&format!("OPOS.{class}"), session) {
            Ok(result) => result,
            Err(OposOpen::NotRegistered) => Err(Self::refusal(target, class, wrapper)),
            Err(OposOpen::Failed(f)) => Err(AdapterError::new(
                codes::ERROR,
                format!(
                    "OPOS {class} '{name}' could not be created: 0x{:08X} {}",
                    f.hresult, f.detail
                ),
            )),
        }
    }
}

/// ASCII-insensitive like .NET OrdinalIgnoreCase (simple upper-case mapping per char).
fn eq_ignore_case(a: &str, b: &str) -> bool {
    a.chars().count() == b.chars().count() && super::contains_ignore_case(a, b)
}

/// An open session with one control object and its logical device.
struct Device<'a> {
    control: &'a mut dyn OposControl,
    class: &'static str,
    name: &'a str,
}

impl Device<'_> {
    /// Calls a method and checks its `ResultCode`.
    fn call(&mut self, method: &str, args: &[OposArg]) -> Result<(), AdapterError> {
        let rc = self
            .control
            .call(method, args)
            .map_err(|f| self.com_failed(method, &f))?;
        self.check(method, rc)
    }

    fn check(&mut self, step: &str, rc: i32) -> Result<(), AdapterError> {
        use result_codes as r;
        if rc == r::SUCCESS {
            return Ok(());
        }
        let name = opos_code_name(rc).unwrap_or("an unknown OPOS code");
        let mut what = format!(
            "OPOS {} '{}' {step} returned {name} ({rc})",
            self.class, self.name
        );
        if rc == r::EXTENDED
            && let Ok(extended) = self.control.get_i32("ResultCodeExtended")
        {
            what.push_str(&format!(", extended {extended}"));
        }
        Err(match rc {
            r::NO_SERVICE => failures::vendor_sdk_missing(&what),
            r::NO_EXIST => AdapterError::new(codes::NOT_CONFIGURED, what),
            r::CLOSED | r::CLAIMED | r::NO_HARDWARE | r::OFFLINE | r::TIMEOUT | r::BUSY => {
                AdapterError::new(codes::UNAVAILABLE, what)
            }
            _ => AdapterError::new(codes::ERROR, what),
        })
    }

    fn com_failed(&self, step: &str, f: &ComFailure) -> AdapterError {
        AdapterError::new(
            codes::ERROR,
            format!(
                "OPOS {} '{}' {step} failed: 0x{:08X} {}",
                self.class, self.name, f.hresult, f.detail
            ),
        )
    }

    /// Open, ClaimDevice, DeviceEnabled = true, `body`, then DeviceEnabled = false,
    /// ReleaseDevice and Close (their results are ignored, like a `finally`).
    fn session(
        &mut self,
        body: impl FnOnce(&mut Device<'_>) -> Result<Value, AdapterError>,
    ) -> Result<Value, AdapterError> {
        self.call("Open", &[OposArg::Str(self.name.to_owned())])?;
        let result = self.claimed(body);
        let _ = self.control.call("Close", &[]);
        result
    }

    fn claimed(
        &mut self,
        body: impl FnOnce(&mut Device<'_>) -> Result<Value, AdapterError>,
    ) -> Result<Value, AdapterError> {
        self.call("ClaimDevice", &[OposArg::I32(CLAIM_TIMEOUT_MS)])?;
        let result = self.enabled(body);
        let _ = self.control.set_bool("DeviceEnabled", false);
        let _ = self.control.call("ReleaseDevice", &[]);
        result
    }

    fn enabled(
        &mut self,
        body: impl FnOnce(&mut Device<'_>) -> Result<Value, AdapterError>,
    ) -> Result<Value, AdapterError> {
        self.control
            .set_bool("DeviceEnabled", true)
            .map_err(|f| self.com_failed("DeviceEnabled", &f))?;
        let rc = self
            .control
            .get_i32("ResultCode")
            .map_err(|f| self.com_failed("DeviceEnabled", &f))?;
        self.check("DeviceEnabled", rc)?;
        body(self)
    }
}

impl PosDevice for OposAdapter {
    fn print(&self, request: &PosPrintRequest, _cx: &Cx) -> Result<Value, AdapterError> {
        Self::refuse(&request.target, "POSPrinter")
    }

    fn kick(&self, request: &PosKickRequest, _cx: &Cx) -> Result<Value, AdapterError> {
        let pin = request.pin;
        self.drive(
            &request.target,
            "CashDrawer",
            || Ok(()),
            move |d| {
                d.call("OpenDrawer", &[])?;
                Ok(json!({"kicked": true, "pin": pin}))
            },
        )
    }

    fn display(&self, request: &PosDisplayRequest, _cx: &Cx) -> Result<Value, AdapterError> {
        let checked = request.clone();
        let request = request.clone();
        // The same bounds and folding as the ESC/POS pole display.
        let validate = move || EscPos::display(checked.lines.as_deref(), checked.columns).map(drop);
        self.drive(&request.target.clone(), "LineDisplay", validate, move |d| {
            let shown = shown_lines(&request);
            d.call("ClearText", &[])?;
            for (row, line) in request.lines.iter().flatten().enumerate() {
                let fitted = EscPos::fit_line(line.as_deref().unwrap_or(""), request.columns);
                // At most 4 rows: the index fits an i32.
                d.call(
                    "DisplayTextAt",
                    &[
                        OposArg::I32(row as i32),
                        OposArg::I32(0),
                        OposArg::Str(fitted),
                        OposArg::I32(0),
                    ],
                )?;
            }
            Ok(json!({"lines": shown}))
        })
    }

    fn read(&self, request: &PosReadRequest, _cx: &Cx) -> Result<Value, AdapterError> {
        let class = if request.device.as_deref() == Some(read_devices::CARD) {
            "MSR"
        } else {
            "POSPrinter"
        };
        Self::refuse(&request.target, class)
    }

    fn speak(&self, request: &PosSpeakRequest, _cx: &Cx) -> Result<Value, AdapterError> {
        Self::refuse(&request.target, "ToneIndicator")
    }
}

/// Bluetooth ESC/POS (POS-PRT-03, PDAOrder PDO-11). The agent does not pair Bluetooth
/// printers: the browser prints over Web Bluetooth, and a printer paired with Windows is
/// reached on its virtual COM port with the serial transport. Every verb refuses with the
/// hint (kept: Bluetooth RFCOMM sockets are not in DevLibrary, and the serial transport
/// already reaches a paired printer).
#[derive(Debug, Default, Clone, Copy)]
pub struct BluetoothRefusingAdapter;

impl BluetoothRefusingAdapter {
    const HINT: &'static str = "print from the browser (Web Bluetooth) or use the paired printer's COM port with the serial transport";

    fn refuse(verb: &str) -> Result<Value, AdapterError> {
        Err(failures::unsupported(
            transports::BLUETOOTH,
            verb,
            Self::HINT,
        ))
    }
}

impl PosDevice for BluetoothRefusingAdapter {
    fn print(&self, _: &PosPrintRequest, _: &Cx) -> Result<Value, AdapterError> {
        Self::refuse("pos.print")
    }

    fn kick(&self, _: &PosKickRequest, _: &Cx) -> Result<Value, AdapterError> {
        Self::refuse("pos.kick")
    }

    fn display(&self, _: &PosDisplayRequest, _: &Cx) -> Result<Value, AdapterError> {
        Self::refuse("pos.display")
    }

    fn read(&self, _: &PosReadRequest, _: &Cx) -> Result<Value, AdapterError> {
        Self::refuse("pos.read")
    }

    fn speak(&self, _: &PosSpeakRequest, _: &Cx) -> Result<Value, AdapterError> {
        Self::refuse("pos.speak")
    }
}

#[cfg(windows)]
mod com_host {
    use dl_json::Value;
    use dl_winos::com::{Object, Variant, hresult};

    use super::super::sta::StaThread;
    use super::{ComFailure, OposArg, OposControl, OposHost, OposOpen, OposSession};
    use crate::contracts::AdapterError;

    /// OPOS control objects created with `Object::create` on one STA thread.
    pub(super) struct ComOposHost {
        thread: StaThread,
    }

    impl ComOposHost {
        pub(super) fn new() -> ComOposHost {
            ComOposHost {
                thread: StaThread::new("opos-cco"),
            }
        }
    }

    fn failure(e: &dl_winos::Error) -> ComFailure {
        ComFailure {
            hresult: e.hresult().unwrap_or(0),
            detail: e.to_string(),
        }
    }

    struct ComControl(Object);

    impl OposControl for ComControl {
        fn call(&mut self, method: &str, args: &[OposArg]) -> Result<i32, ComFailure> {
            let args: Vec<Variant> = args
                .iter()
                .map(|a| match a {
                    OposArg::I32(n) => Variant::I32(*n),
                    OposArg::Str(s) => Variant::from(s.as_str()),
                })
                .collect();
            self.0
                .call(method, &args)
                .and_then(i32::try_from)
                .map_err(|e| failure(&e))
        }

        fn set_bool(&mut self, property: &str, value: bool) -> Result<(), ComFailure> {
            self.0.set(property, value).map_err(|e| failure(&e))
        }

        fn get_i32(&mut self, property: &str) -> Result<i32, ComFailure> {
            self.0
                .get(property)
                .and_then(i32::try_from)
                .map_err(|e| failure(&e))
        }
    }

    impl OposHost for ComOposHost {
        fn with_control(
            &self,
            prog_id: &str,
            session: OposSession,
        ) -> Result<Result<Value, AdapterError>, OposOpen> {
            let prog_id = prog_id.to_owned();
            self.thread
                .run(move || {
                    let object = match Object::create(&prog_id) {
                        Ok(o) => o,
                        Err(e)
                            if matches!(
                                e.hresult(),
                                Some(hresult::REGDB_E_CLASSNOTREG | hresult::CO_E_CLASSSTRING)
                            ) =>
                        {
                            return Err(OposOpen::NotRegistered);
                        }
                        Err(e) => return Err(OposOpen::Failed(failure(&e))),
                    };
                    let mut control = ComControl(object);
                    Ok(session(&mut control))
                })
                .unwrap_or_else(|| {
                    Err(OposOpen::Failed(ComFailure {
                        hresult: 0,
                        detail: "the OPOS COM thread stopped".to_owned(),
                    }))
                })
        }
    }
}
