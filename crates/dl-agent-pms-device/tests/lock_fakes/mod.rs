//! Port of Fakes/FakeLockNatives.cs plus the request builders the lock tests share.

#![allow(dead_code)]

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, MutexGuard};

use dl_agent_pms_device::adapters::lock::{
    HuneCard, HuneCardInfo, HuneNative, OrbitaCard, OrbitaCardInfo, OrbitaNative, TpGuestCard,
    TpLockNative, tp_codes,
};
use dl_agent_pms_device::adapters::{AdapterError, Cx, DeviceAdapter};
use dl_agent_pms_device::contracts::{KeyCardRequest, LockAddress, LockVendor, parse_wall_clock};
use dl_json::Value;
use dl_time::DateTime;

pub fn at(text: &str) -> DateTime {
    parse_wall_clock(text).expect("wall clock")
}

pub fn some(s: &str) -> Option<String> {
    Some(s.to_owned())
}

/// `LockAddress(building, floor, room, door, address, rmm, fmm, lockType)`.
#[allow(clippy::too_many_arguments)]
pub fn address(
    building: &str,
    floor: &str,
    room: &str,
    door: &str,
    raddress: &str,
    rmm: &str,
    fmm: &str,
    lock_type: &str,
) -> LockAddress {
    LockAddress {
        building: some(building),
        floor: some(floor),
        room: some(room),
        door: some(door),
        address: some(raddress),
        rmm: some(rmm),
        fmm: some(fmm),
        lock_type: some(lock_type),
    }
}

/// `MfLockAdapterTests.Address101`.
pub fn address_101() -> LockAddress {
    address("01", "01", "101", "01", "0101", "", "", "MFCARD")
}

/// `new LockVendor(keyType, mfLockName)`.
pub fn vendor(key_type: &str, mf_lock_name: &str) -> LockVendor {
    LockVendor {
        key_type: some(key_type),
        mf_lock_name: some(mf_lock_name),
    }
}

/// `new KeyCardRequest(room, from, to, cards, folioNum, address, vendor)`.
pub fn key_card(
    room: &str,
    from: &str,
    to: &str,
    cards: i32,
    folio: i64,
    address: Option<LockAddress>,
    vendor: Option<LockVendor>,
) -> KeyCardRequest {
    KeyCardRequest {
        room: some(room),
        from: at(from),
        to: at(to),
        cards,
        folio_num: Some(folio),
        lock_address: address,
        vendor,
        add_guest: None,
    }
}

pub fn cx() -> Cx {
    Cx::unbounded()
}

/// The refusal of a verb that must fail.
pub fn refusal(r: Result<Value, AdapterError>) -> AdapterError {
    match r {
        Ok(v) => panic!("expected a refusal, got {}", dl_json::to_string(&v)),
        Err(e) => e,
    }
}

/// The family of the adapter a router picked (`Assert.IsType`).
pub fn family(a: &dyn DeviceAdapter) -> &str {
    a.family()
}

/// `FakeTpLockNative`: records every TP_* call; return codes and serials are set per test.
pub struct FakeTp {
    state: Mutex<TpState>,
}

pub struct TpState {
    pub calls: Vec<String>,
    pub configured: Vec<i32>,
    pub last_write: Option<(String, String, String, i32)>,
    pub configuration_rc: i32,
    pub make_rc: i32,
    pub read_rc: i32,
    pub card_snr: String,
    /// Serials TP_GetCardSnr reports first, one per call (a clerk swapping cards), before
    /// `card_snr`.
    pub inserted: VecDeque<String>,
}

impl FakeTp {
    pub fn new() -> Arc<FakeTp> {
        Arc::new(FakeTp {
            state: Mutex::new(TpState {
                calls: Vec::new(),
                configured: Vec::new(),
                last_write: None,
                configuration_rc: tp_codes::OK,
                make_rc: tp_codes::OK,
                read_rc: tp_codes::OK,
                card_snr: "A1B2C3D4".to_owned(),
                inserted: VecDeque::new(),
            }),
        })
    }

    pub fn state(&self) -> MutexGuard<'_, TpState> {
        self.state.lock().unwrap()
    }

    pub fn calls(&self) -> Vec<String> {
        self.state().calls.clone()
    }

    pub fn count(&self, call: &str) -> usize {
        self.state().calls.iter().filter(|c| *c == call).count()
    }

    pub fn native(self: &Arc<Self>) -> Option<Arc<dyn TpLockNative>> {
        Some(self.clone())
    }
}

impl TpLockNative for FakeTp {
    fn configuration(&self, lock_type: i32) -> Result<i32, AdapterError> {
        let mut s = self.state();
        s.calls.push("TP_Configuration".into());
        s.configured.push(lock_type);
        Ok(s.configuration_rc)
    }

    fn make_guest_card(
        &self,
        room_no: &str,
        checkin_time: &str,
        checkout_time: &str,
        flags: i32,
    ) -> Result<(i32, String), AdapterError> {
        let mut s = self.state();
        s.calls.push("TP_MakeGuestCardEx".into());
        s.last_write = Some((
            room_no.to_owned(),
            checkin_time.to_owned(),
            checkout_time.to_owned(),
            flags,
        ));
        let snr = if s.make_rc == tp_codes::OK {
            s.card_snr.clone()
        } else {
            String::new()
        };
        Ok((s.make_rc, snr))
    }

    fn read_guest_card(&self) -> Result<(i32, TpGuestCard), AdapterError> {
        let mut s = self.state();
        s.calls.push("TP_ReadGuestCardEx".into());
        let (room, cin, cout) = s
            .last_write
            .as_ref()
            .map(|w| (w.0.clone(), w.1.clone(), w.2.clone()))
            .unwrap_or_default();
        let card = TpGuestCard {
            card_snr: s.card_snr.clone(),
            room_no: room,
            checkin_time: cin,
            checkout_time: cout,
        };
        Ok((s.read_rc, card))
    }

    fn cancel_card(&self) -> Result<(i32, String), AdapterError> {
        let mut s = self.state();
        s.calls.push("TP_CancelCard".into());
        Ok((tp_codes::OK, s.card_snr.clone()))
    }

    fn get_card_snr(&self) -> Result<(i32, String), AdapterError> {
        let mut s = self.state();
        s.calls.push("TP_GetCardSnr".into());
        let snr = match s.inserted.pop_front() {
            Some(snr) => snr,
            None => s.card_snr.clone(),
        };
        Ok((s.read_rc, snr))
    }
}

/// `FakeHuneNative`.
pub struct FakeHune {
    state: Mutex<HuneState>,
}

pub struct HuneState {
    pub written: Vec<HuneCard>,
    pub rc: i32,
    pub rmm: String,
    next: i32,
}

impl FakeHune {
    pub fn new() -> Arc<FakeHune> {
        FakeHune::with_rc(0)
    }

    pub fn with_rc(rc: i32) -> Arc<FakeHune> {
        Arc::new(FakeHune {
            state: Mutex::new(HuneState {
                written: Vec::new(),
                rc,
                rmm: "RMM-7".into(),
                next: 0,
            }),
        })
    }

    pub fn state(&self) -> MutexGuard<'_, HuneState> {
        self.state.lock().unwrap()
    }

    pub fn native(self: &Arc<Self>) -> Option<Arc<dyn HuneNative>> {
        Some(self.clone())
    }
}

impl HuneNative for FakeHune {
    fn key_card(&self, card: &HuneCard) -> Result<(i32, String, String), AdapterError> {
        let mut s = self.state();
        s.written.push(card.clone());
        if s.rc == 0 {
            s.next += 1;
            Ok((0, format!("H{}", s.next), s.rmm.clone()))
        } else {
            Ok((s.rc, String::new(), String::new()))
        }
    }

    fn read_message(
        &self,
        _com_port: i32,
        _card_pass: &str,
        _sys_code: &str,
    ) -> Result<(i32, HuneCardInfo), AdapterError> {
        let info = HuneCardInfo {
            card_number: "H1".into(),
            address: "0101".into(),
            start: "26-10-14 14:00:00".into(),
            end: "26-10-16 12:00:00".into(),
        };
        Ok((self.state().rc, info))
    }
}

/// `FakeOrbitaNative`.
pub struct FakeOrbita {
    state: Mutex<OrbitaState>,
}

pub struct OrbitaState {
    pub calls: Vec<String>,
    pub written: Vec<OrbitaCard>,
    pub rc: i32,
    pub on_encoder: String,
}

impl FakeOrbita {
    pub fn new() -> Arc<FakeOrbita> {
        FakeOrbita::with_rc(0)
    }

    pub fn with_rc(rc: i32) -> Arc<FakeOrbita> {
        Arc::new(FakeOrbita {
            state: Mutex::new(OrbitaState {
                calls: Vec::new(),
                written: Vec::new(),
                rc,
                on_encoder: "OB1".into(),
            }),
        })
    }

    pub fn state(&self) -> MutexGuard<'_, OrbitaState> {
        self.state.lock().unwrap()
    }

    pub fn calls(&self) -> Vec<String> {
        self.state().calls.clone()
    }

    pub fn native(self: &Arc<Self>) -> Option<Arc<dyn OrbitaNative>> {
        Some(self.clone())
    }
}

impl OrbitaNative for FakeOrbita {
    fn connect(&self, port: i32) -> Result<i32, AdapterError> {
        self.state().calls.push(format!("dv_connect({port})"));
        Ok(0)
    }

    fn write_card(&self, card: &OrbitaCard) -> Result<(i32, String), AdapterError> {
        let mut s = self.state();
        s.calls.push("dv_write_card".into());
        s.written.push(card.clone());
        let number = if s.rc == 0 {
            s.on_encoder.clone()
        } else {
            String::new()
        };
        Ok((s.rc, number))
    }

    fn read_card(&self) -> Result<(i32, OrbitaCardInfo), AdapterError> {
        let mut s = self.state();
        s.calls.push("dv_read_card".into());
        let info = OrbitaCardInfo {
            card_number: s.on_encoder.clone(),
            building: "01".into(),
            floor: "0001".into(),
            room: "101".into(),
            start: String::new(),
            end: String::new(),
        };
        Ok((0, info))
    }

    fn delete_card(&self) -> Result<(i32, String), AdapterError> {
        let mut s = self.state();
        s.calls.push("dv_delete_card".into());
        Ok((s.rc, s.on_encoder.clone()))
    }

    fn disconnect(&self) {
        self.state().calls.push("dv_disconnect".into());
    }
}
