//! `Adapters/Lock/SmileLockRouter.cs` and `LockNatives.cs`.

use std::sync::Arc;

use dl_json::Value;

use super::failures::{is_null_or_blank, not_configured, sdk_not_installed, trim, upper_invariant};
use super::{
    HuneHSmartV8Adapter, HuneNative, MfLockAdapter, OrbitaCLockAdapter, OrbitaNative, TpLockNative,
    TtLockAdapter,
};
use crate::adapters::{Cx, DeviceAdapter, SMILE_LOCK};
use crate::config::LockOptions;
use crate::contracts::{
    AdapterError, CancelAllRequest, EraseRequest, KeyCardRequest, LockAddress, LockVendor,
    ReadRequest,
};

/// The vendor natives this build carries (`LockNatives`). Only LockSDK.dll (TP_*) has a
/// known contract; HNAccessG.DLL (Hune HSmart V8) and CLock.dll (Orbita) have no header or
/// DLL in hand, so they stay `None` and their adapters refuse with
/// DEVICE_SDK_NOT_INSTALLED until the vendor ships them.
#[derive(Clone, Default)]
pub struct LockNatives {
    /// LockSDK.dll (MFCARD hls / goodum / ezibee).
    pub tp: Option<Arc<dyn TpLockNative>>,
    /// HNAccessG.DLL (HUNE2013).
    pub hune: Option<Arc<dyn HuneNative>>,
    /// CLock.dll (ORBITA).
    pub orbita: Option<Arc<dyn OrbitaNative>>,
}

impl LockNatives {
    /// `LockNatives.Installed()`: the LockSDK binding in the 32-bit Windows build (like the
    /// C# win-x86 `LOCKSDK_NATIVE` build), nothing elsewhere.
    pub fn installed() -> LockNatives {
        LockNatives {
            tp: crate::native::tp_lock(),
            ..LockNatives::default()
        }
    }
}

/// frmLockSetup types whose vendor native has no adapter here (hunerf.dll, btLock56/57,
/// proRFL, ...).
const PENDING: [&str; 11] = [
    "HUNERF",
    "TYCARD",
    "BTCARD",
    "BTCARD57",
    "BTCARD57L",
    "PHG",
    "PRORFL",
    "HAFELE",
    "BOWNIN",
    "PROLOK",
    "KAIMI",
];

/// `Agent:Adapter = smile-lock`: the DoorLockPort for SMILE_FO lock families. Each command
/// goes to the vendor adapter named by its payload's `vendor.keyType` (WSParameter
/// LockKeyType, read live by the API) or, when absent, `Agent:Lock:LockKeyType`. MULTI
/// ("Multi Lock Types") routes by the room's RoomLockDef.LockType. Known families without an
/// adapter refuse with DEVICE_SDK_NOT_INSTALLED.
pub struct SmileLockRouter {
    options: LockOptions,
    by_key_type: Vec<(String, Box<dyn DeviceAdapter>)>,
}

impl SmileLockRouter {
    /// `SmileLockRouter.Name`.
    pub const NAME: &'static str = SMILE_LOCK;

    /// A router over `by_key_type` (upper-case key type → adapter, matched exactly).
    pub fn new(
        options: LockOptions,
        by_key_type: Vec<(String, Box<dyn DeviceAdapter>)>,
    ) -> SmileLockRouter {
        SmileLockRouter {
            options,
            by_key_type,
        }
    }

    /// `SmileLockRouter.Create`: MFCARD, HUNE2013, ORBITA and TTLOCK over `natives`.
    pub fn create(options: &LockOptions, natives: LockNatives) -> SmileLockRouter {
        let o = options.clone();
        SmileLockRouter::new(
            options.clone(),
            vec![
                (
                    "MFCARD".to_owned(),
                    Box::new(MfLockAdapter::new(o.clone(), natives.tp)),
                ),
                (
                    "HUNE2013".to_owned(),
                    Box::new(HuneHSmartV8Adapter::new(o.clone(), natives.hune)),
                ),
                (
                    "ORBITA".to_owned(),
                    Box::new(OrbitaCLockAdapter::new(o.clone(), natives.orbita)),
                ),
                ("TTLOCK".to_owned(), Box::new(TtLockAdapter::new(o))),
            ],
        )
    }

    /// `Pick`: the adapter for this command's lock family, or a typed refusal.
    pub fn pick(
        &self,
        vendor: Option<&LockVendor>,
        address: Option<&LockAddress>,
    ) -> Result<&dyn DeviceAdapter, AdapterError> {
        let from_payload = vendor.and_then(|v| v.key_type.as_deref());
        let mut key_type = normalize(if is_null_or_blank(from_payload) {
            Some(self.options.lock_key_type.as_str())
        } else {
            from_payload
        });
        if key_type == "MULTI" {
            key_type = normalize(address.and_then(|a| a.lock_type.as_deref()));
            if key_type.is_empty() {
                return Err(not_configured(
                    "Multi Lock Types needs the room's RoomLockDef.LockType",
                ));
            }
        }
        if let Some((_, adapter)) = self.by_key_type.iter().find(|(k, _)| *k == key_type) {
            return Ok(adapter.as_ref());
        }
        if PENDING.contains(&key_type.as_str()) {
            return Err(sdk_not_installed(&key_type));
        }
        Err(not_configured(match key_type.as_str() {
            "" => "Lock type is not set (WSParameter LockKeyType)".to_owned(),
            "OTHER" => "Lock type OTHER uses the external lock link (tmpLockLink); no encoder runs on this agent".to_owned(),
            other => format!("Lock type {other} is not defined!"),
        }))
    }
}

fn normalize(key_type: Option<&str>) -> String {
    upper_invariant(trim(key_type.unwrap_or("")))
}

impl DeviceAdapter for SmileLockRouter {
    fn family(&self) -> &str {
        Self::NAME
    }

    fn issue(&self, r: &KeyCardRequest, cx: &Cx) -> Result<Value, AdapterError> {
        self.pick(r.vendor.as_ref(), r.lock_address.as_ref())?
            .issue(r, cx)
    }

    fn duplicate(&self, r: &KeyCardRequest, cx: &Cx) -> Result<Value, AdapterError> {
        self.pick(r.vendor.as_ref(), r.lock_address.as_ref())?
            .duplicate(r, cx)
    }

    fn read(&self, r: &ReadRequest, cx: &Cx) -> Result<Value, AdapterError> {
        self.pick(r.vendor.as_ref(), None)?.read(r, cx)
    }

    fn erase(&self, r: &EraseRequest, cx: &Cx) -> Result<Value, AdapterError> {
        self.pick(r.vendor.as_ref(), r.lock_address.as_ref())?
            .erase(r, cx)
    }

    fn extend(&self, r: &KeyCardRequest, cx: &Cx) -> Result<Value, AdapterError> {
        self.pick(r.vendor.as_ref(), r.lock_address.as_ref())?
            .extend(r, cx)
    }

    fn cancel_all(&self, r: &CancelAllRequest, cx: &Cx) -> Result<Value, AdapterError> {
        self.pick(r.vendor.as_ref(), r.lock_address.as_ref())?
            .cancel_all(r, cx)
    }
}
