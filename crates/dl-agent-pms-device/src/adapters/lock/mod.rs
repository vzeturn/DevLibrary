//! Door-lock vendor adapters (`Adapters/Lock/`): [`SmileLockRouter`] picks the vendor
//! adapter of each command ([`MfLockAdapter`] over the LockSDK TP_* family,
//! [`HuneHSmartV8Adapter`], [`OrbitaCLockAdapter`], [`TtLockAdapter`]). The vendor DLLs sit
//! behind native traits ([`TpLockNative`], [`HuneNative`], [`OrbitaNative`]) so tests run on
//! fakes; [`LockNatives::installed`] has the real LockSDK binding in the 32-bit Windows
//! build only. The lock settings are [`crate::config::LockOptions`].

pub mod failures;
mod hune;
mod mf;
mod orbita;
mod router;
mod tp;
mod ttlock;

pub use hune::{HuneCard, HuneCardInfo, HuneHSmartV8Adapter, HuneNative};
pub use mf::MfLockAdapter;
pub use orbita::{OrbitaCLockAdapter, OrbitaCard, OrbitaCardInfo, OrbitaNative};
pub use router::{LockNatives, SmileLockRouter};
pub use tp::{TpGuestCard, TpLockNative, tp_codes};
pub use ttlock::{TtLockAdapter, tt_lock_args};

use super::DeviceAdapter;
use crate::config::LockOptions;

/// `DeviceAdapters.Create("smile-lock", options)`: the SMILE_FO lock router over the
/// natives of this build.
pub fn smile_lock(options: &LockOptions) -> Box<dyn DeviceAdapter> {
    Box::new(SmileLockRouter::create(options, LockNatives::installed()))
}
