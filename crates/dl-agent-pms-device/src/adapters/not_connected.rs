//! `Adapters/NotConnectedAdapter.cs`.

use dl_json::Value;

use super::{Cx, DeviceAdapter, NOT_CONNECTED};
use crate::contracts::{
    AdapterError, CancelAllRequest, EraseRequest, KeyCardRequest, ReadRequest, codes,
};

/// The production default (`Agent:Adapter = none`): no encoder is configured on this
/// workstation, so every verb fails with DEVICE_NOT_CONNECTED and the clerk sees the reason.
#[derive(Debug, Clone, Copy, Default)]
pub struct NotConnectedAdapter;

impl NotConnectedAdapter {
    /// `NotConnectedAdapter.Message`.
    pub const MESSAGE: &'static str = "Chưa kết nối thiết bị khoá";

    fn refuse() -> Result<Value, AdapterError> {
        Err(AdapterError::new(codes::NOT_CONNECTED, Self::MESSAGE))
    }
}

impl DeviceAdapter for NotConnectedAdapter {
    fn family(&self) -> &str {
        NOT_CONNECTED
    }

    fn issue(&self, _: &KeyCardRequest, _: &Cx) -> Result<Value, AdapterError> {
        Self::refuse()
    }

    fn duplicate(&self, _: &KeyCardRequest, _: &Cx) -> Result<Value, AdapterError> {
        Self::refuse()
    }

    fn read(&self, _: &ReadRequest, _: &Cx) -> Result<Value, AdapterError> {
        Self::refuse()
    }

    fn erase(&self, _: &EraseRequest, _: &Cx) -> Result<Value, AdapterError> {
        Self::refuse()
    }

    fn extend(&self, _: &KeyCardRequest, _: &Cx) -> Result<Value, AdapterError> {
        Self::refuse()
    }

    fn cancel_all(&self, _: &CancelAllRequest, _: &Cx) -> Result<Value, AdapterError> {
        Self::refuse()
    }
}
