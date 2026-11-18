//! `Adapters/Lock/MfLockAdapter.cs`.

use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use dl_json::{Value, json};

use super::failures::{
    eq_opt_ignore_case, format_long, is_blank, is_null_or_blank, lower_invariant,
    no_setup_for_room, not_configured, opt, require_address, required, sdk_not_installed, text,
    trim, wall,
};
use super::tp::{TpLockNative, tp_codes};
use crate::adapters::{Cx, DeviceAdapter};
use crate::config::{LockOptions, eq_ignore_case};
use crate::contracts::{
    AdapterError, CancelAllRequest, EraseRequest, KeyCardRequest, LockVendor, ReadRequest, codes,
};

/// MF lock names known to SMILE_FO whose natives or vendor services are not on site.
const PENDING: [&str; 5] = ["adel", "hikvision", "v9rf", "dormakaba", "xeeder"];

/// I-03b: the SMILE_FO 6.091 MF .NET lock bridge (mdlLockDotNET → `SMILE Dll Call.DLLCall`
/// LockWrite / LockRead), for LockKeyType MFCARD. It switches on the lower-case MF lock
/// name like `DLLCall.CallFunction`: hls / goodum / ezibee run the LockSDK TP_* family;
/// adel, hikvision, v9rf, dormakaba and xeeder are known but their natives or vendor
/// services are not on site, so they refuse with DEVICE_SDK_NOT_INSTALLED; any other name
/// is "Lock X to Write is not defined!".
pub struct MfLockAdapter {
    options: LockOptions,
    tp: Option<Arc<dyn TpLockNative>>,
    next_card_wait: Duration,
    next_card_poll: Duration,
    last_write: Mutex<Option<LastWrite>>,
}

#[derive(Debug, Clone)]
struct LastWrite {
    serial: String,
    room: Option<String>,
    at: Instant,
}

impl MfLockAdapter {
    /// `MfLockAdapter.Name`.
    pub const NAME: &'static str = "mf-lock";

    /// hls: IDD102 calls `TP_Configuration(5)` whatever the door type.
    pub const HLS_LOCK_TYPE: i32 = 5;

    /// How long after a write the next duplicate counts as part of the same card chain.
    pub const CHAIN_WINDOW: Duration = Duration::from_secs(5 * 60);

    /// An adapter over the TP native; `None` when this build has no LockSDK binding.
    pub fn new(options: LockOptions, tp: Option<Arc<dyn TpLockNative>>) -> MfLockAdapter {
        MfLockAdapter {
            options,
            tp,
            next_card_wait: Duration::from_secs(30),
            next_card_poll: Duration::from_millis(500),
            last_write: Mutex::new(None),
        }
    }

    /// `NextCardWait` (default 30 s): how long a chained duplicate waits for the clerk to
    /// swap the card (VB6 "Please Insert Card").
    pub fn with_next_card_wait(mut self, wait: Duration) -> MfLockAdapter {
        self.next_card_wait = wait;
        self
    }

    /// `NextCardPoll` (default 500 ms): the encoder poll interval while waiting.
    pub fn with_next_card_poll(mut self, poll: Duration) -> MfLockAdapter {
        self.next_card_poll = poll;
        self
    }

    fn write(&self, r: &KeyCardRequest) -> Result<Value, AdapterError> {
        let name = self.name_of(r.vendor.as_ref())?;
        let (_, address) =
            require_address(r.lock_address.as_ref(), &no_setup_for_room(text(&r.room)))?;
        // One TP encoder holds one card: VB6 calls LockWrite once per card.
        if r.cards != 1 {
            return Err(AdapterError::new(
                codes::BAD_PAYLOAD,
                format!(
                    "The {name} encoder writes one card per command, not {}",
                    r.cards
                ),
            ));
        }
        let sdk = self.open(r.vendor.as_ref())?;
        let (rc, snr) =
            sdk.make_guest_card(address, &format_long(r.from), &format_long(r.to), 0)?;
        tp_codes::check(rc, "TP_MakeGuestCardEx")?;
        if is_blank(&snr) {
            return Err(AdapterError::new(
                codes::ERROR,
                "TP_MakeGuestCardEx returned no card serial",
            ));
        }
        let serial = trim(&snr).to_owned();
        *self.last() = Some(LastWrite {
            serial: serial.clone(),
            room: r.room.clone(),
            at: Instant::now(),
        });
        let card = json!({
            "cardNumber": serial,
            "room": opt(&r.room),
            "from": wall(r.from),
            "to": wall(r.to),
        });
        Ok(json!({"cards": Value::Array(vec![card])}))
    }

    /// The API sends N MF cards as a chain of one-card commands (VB6 frmKeyCard "Auto"). A
    /// duplicate right after a write for the same room waits until a different card is on
    /// the encoder, so card 2 never overwrites card 1. A duplicate for another room is a new
    /// request, not the next card of the chain.
    fn wait_for_next_card(
        &self,
        room: &Option<String>,
        vendor: Option<&LockVendor>,
        cx: &Cx,
    ) -> Result<(), AdapterError> {
        let Some(last) = self.last().clone() else {
            return Ok(());
        };
        if last.at.elapsed() > Self::CHAIN_WINDOW
            || !eq_opt_ignore_case(last.room.as_deref(), room.as_deref())
        {
            return Ok(());
        }
        let sdk = self.open(vendor)?;
        let deadline = Instant::now() + self.next_card_wait;
        loop {
            let (rc, snr) = sdk.get_card_snr()?;
            if rc == tp_codes::OK && !is_blank(&snr) && !eq_ignore_case(trim(&snr), &last.serial) {
                return Ok(());
            }
            // -1 is "no card on the encoder yet": keep waiting; any other failure is
            // reported as is.
            if rc != tp_codes::OK && rc != tp_codes::NO_CARD {
                tp_codes::check(rc, "TP_GetCardSnr")?;
            }
            if Instant::now() >= deadline {
                return Err(AdapterError::new(codes::NO_CARD, "Please Insert Card"));
            }
            cx.sleep(self.next_card_poll)?;
        }
    }

    fn name_of(&self, vendor: Option<&LockVendor>) -> Result<String, AdapterError> {
        let from_payload = vendor.and_then(|v| v.mf_lock_name.as_deref());
        let raw = trim(if is_null_or_blank(from_payload) {
            &self.options.mf_lock_name
        } else {
            from_payload.unwrap_or_default()
        });
        if raw.is_empty() {
            return Err(not_configured(
                "MF lock name is not set (WSParameter MFLockName)",
            ));
        }
        let name = lower_invariant(raw);
        match name.as_str() {
            "hls" | "goodum" | "ezibee" => Ok(name),
            n if PENDING.contains(&n) => Err(sdk_not_installed(n)),
            _ => Err(not_configured(format!(
                "Lock {raw} to Write is not defined!"
            ))),
        }
    }

    fn open(&self, vendor: Option<&LockVendor>) -> Result<&dyn TpLockNative, AdapterError> {
        let name = self.name_of(vendor)?;
        let sdk = self.tp.as_deref().ok_or_else(|| sdk_not_installed(&name))?;
        let lock_type = if name == "hls" {
            Self::HLS_LOCK_TYPE
        } else {
            self.options.mf_lock_door_type
        };
        tp_codes::check(sdk.configuration(lock_type)?, "TP_Configuration")?;
        Ok(sdk)
    }

    fn last(&self) -> std::sync::MutexGuard<'_, Option<LastWrite>> {
        self.last_write
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }
}

impl DeviceAdapter for MfLockAdapter {
    fn family(&self) -> &str {
        Self::NAME
    }

    fn issue(&self, request: &KeyCardRequest, _: &Cx) -> Result<Value, AdapterError> {
        self.write(request)
    }

    fn duplicate(&self, request: &KeyCardRequest, cx: &Cx) -> Result<Value, AdapterError> {
        self.wait_for_next_card(&request.room, request.vendor.as_ref(), cx)?;
        self.write(request)
    }

    /// The TP family has no time-change call: VB6 re-writes the card with the new end time.
    fn extend(&self, request: &KeyCardRequest, _: &Cx) -> Result<Value, AdapterError> {
        self.write(request)
    }

    fn read(&self, request: &ReadRequest, _: &Cx) -> Result<Value, AdapterError> {
        let sdk = self.open(request.vendor.as_ref())?;
        let (rc, card) = sdk.read_guest_card()?;
        tp_codes::check(rc, "TP_ReadGuestCardEx")?;
        // room_no is the lock address (RAddress), not the PMS room code, so it is not
        // reported as room.
        Ok(json!({
            "cardNumber": trim(&card.card_snr),
            "address": trim(&card.room_no),
            "from": trim(&card.checkin_time),
            "to": trim(&card.checkout_time),
        }))
    }

    /// Checks the serial first, so a wrong card on the encoder is never wiped.
    fn erase(&self, request: &EraseRequest, _: &Cx) -> Result<Value, AdapterError> {
        let sdk = self.open(request.vendor.as_ref())?;
        let (rc, snr) = sdk.get_card_snr()?;
        tp_codes::check(rc, "TP_GetCardSnr")?;
        let wanted = required(&request.card_number)?;
        if !eq_ignore_case(trim(&snr), trim(wanted)) {
            return Err(AdapterError::new(
                codes::ERROR,
                format!("The card on the encoder is {}, not {wanted}", trim(&snr)),
            ));
        }
        let (rc, _) = sdk.cancel_card()?;
        tp_codes::check(rc, "TP_CancelCard")?;
        Ok(json!({"cardNumber": wanted, "erased": true}))
    }

    /// The TP API has no room-wide cancel: the API's rows are retired and the lock is told
    /// nothing.
    fn cancel_all(&self, request: &CancelAllRequest, _: &Cx) -> Result<Value, AdapterError> {
        self.open(request.vendor.as_ref())?;
        Ok(json!({
            "room": opt(&request.room),
            "cancelled": false,
            "reason": "LockSDK has no room-wide cancel; erase each card",
        }))
    }
}
