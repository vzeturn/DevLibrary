//! Port of Lock/SmileLockRouterTests.cs. DoorLockPort contract: every lock adapter either
//! answers or refuses with a typed error (never a silent success), and the router picks
//! the family from the payload.

mod lock_fakes;

use dl_agent_pms_device::adapters::lock::{
    HuneHSmartV8Adapter, LockNatives, MfLockAdapter, OrbitaCLockAdapter, SmileLockRouter,
    TtLockAdapter,
};
use dl_agent_pms_device::adapters::{AdapterError, DeviceAdapter, SMILE_LOCK, create};
use dl_agent_pms_device::config::LockOptions;
use dl_agent_pms_device::contracts::{
    CancelAllRequest, EraseRequest, KeyCardRequest, LockAddress, LockVendor, ReadRequest, codes,
};
use dl_json::Value;
use lock_fakes::*;

type Verb = Box<dyn Fn(&dyn DeviceAdapter) -> Result<Value, AdapterError>>;

/// The six verbs with one payload each, like `Verbs(vendor, ct)`.
fn verbs(v: LockVendor) -> Vec<(&'static str, Verb)> {
    let r = key_card(
        "101",
        "2026-10-14T14:00:00",
        "2026-10-16T12:00:00",
        1,
        1,
        Some(address_101()),
        Some(v.clone()),
    );
    let dup = KeyCardRequest {
        add_guest: Some(true),
        ..r.clone()
    };
    let r2 = r.clone();
    let read = ReadRequest {
        vendor: Some(v.clone()),
    };
    let erase = EraseRequest {
        room: some("101"),
        card_number: some("A1B2C3D4"),
        lock_address: Some(address_101()),
        vendor: Some(v.clone()),
    };
    let cancel = CancelAllRequest {
        room: some("101"),
        lock_address: Some(address_101()),
        vendor: Some(v),
    };
    vec![
        ("issue", boxed(move |a| a.issue(&r, &cx()))),
        ("duplicate", boxed(move |a| a.duplicate(&dup, &cx()))),
        ("extend", boxed(move |a| a.extend(&r2, &cx()))),
        ("read", boxed(move |a| a.read(&read, &cx()))),
        ("erase", boxed(move |a| a.erase(&erase, &cx()))),
        ("cancelAll", boxed(move |a| a.cancel_all(&cancel, &cx()))),
    ]
}

fn boxed(f: impl Fn(&dyn DeviceAdapter) -> Result<Value, AdapterError> + 'static) -> Verb {
    Box::new(f)
}

#[test]
fn lock_port_without_natives_every_vendor_refuses_every_verb_with_a_typed_code() {
    let cases: [(&str, &str, &str, Option<&str>); 8] = [
        (
            "MFCARD",
            "hls",
            codes::SDK_NOT_INSTALLED,
            Some("lock SDK not installed for hls"),
        ),
        (
            "MFCARD",
            "adel",
            codes::SDK_NOT_INSTALLED,
            Some("lock SDK not installed for adel"),
        ),
        (
            "HUNE2013",
            "",
            codes::SDK_NOT_INSTALLED,
            Some("lock SDK not installed for hune"),
        ),
        (
            "ORBITA",
            "",
            codes::SDK_NOT_INSTALLED,
            Some("lock SDK not installed for orbita"),
        ),
        (
            "HUNERF",
            "",
            codes::SDK_NOT_INSTALLED,
            Some("lock SDK not installed for HUNERF"),
        ),
        ("OTHER", "", codes::NOT_CONFIGURED, None),
        (
            "",
            "",
            codes::NOT_CONFIGURED,
            Some("Lock type is not set (WSParameter LockKeyType)"),
        ),
        (
            "SALTO",
            "",
            codes::NOT_CONFIGURED,
            Some("Lock type SALTO is not defined!"),
        ),
    ];
    for (key_type, mf_lock_name, code, message) in cases {
        let router = SmileLockRouter::create(&LockOptions::default(), LockNatives::default());
        for (name, verb) in verbs(vendor(key_type, mf_lock_name)) {
            let e = refusal(verb(&router));
            // Hune has no erase in SMILE_FO: it is a typed unsupported verb, not a missing SDK.
            if e.code == codes::UNSUPPORTED_KIND && key_type == "HUNE2013" {
                continue;
            }
            assert_eq!(e.code, code, "{key_type} {name}");
            if let Some(m) = message {
                assert_eq!(e.message, m, "{key_type} {name}");
            }
        }
    }
}

#[test]
fn h10_ttlock_refuses_credentials_first_then_lock_sdk_not_installed() {
    for (name, verb) in verbs(vendor("TTLOCK", "")) {
        let no_creds = refusal(verb(&SmileLockRouter::create(
            &LockOptions::default(),
            LockNatives::default(),
        )));
        assert_eq!(
            (no_creds.code.as_str(), no_creds.message.as_str()),
            (
                codes::NOT_CONFIGURED,
                "Invalid Client ID/Secret configuration!"
            ),
            "{name}"
        );

        let options = LockOptions {
            tt_lock_client_id: "id".into(),
            tt_lock_client_secret: "s3cr3t".into(),
            ..LockOptions::default()
        };
        let e = refusal(verb(&SmileLockRouter::create(
            &options,
            LockNatives::default(),
        )));
        assert_eq!(
            (e.code.as_str(), e.message.as_str()),
            (
                codes::SDK_NOT_INSTALLED,
                "lock SDK not installed for ttlock"
            ),
            "{name}"
        );
        assert!(!e.message.contains("s3cr3t"));
    }
}

#[test]
fn lock_port_the_fake_tp_native_passes_every_verb_through_the_router() {
    let tp = FakeTp::new();
    // The duplicate after the issue waits for a fresh card.
    tp.state().inserted.push_back("E5F6A7B8".into());
    let natives = LockNatives {
        tp: tp.native(),
        ..LockNatives::default()
    };
    let router = SmileLockRouter::create(&LockOptions::default(), natives);
    for (name, verb) in verbs(vendor("MFCARD", "hls")) {
        let v = verb(&router).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert!(v.is_object(), "{name}");
    }
}

#[test]
fn lock_port_the_key_type_picks_the_vendor_adapter_multi_uses_room_lock_def_lock_type() {
    let router = SmileLockRouter::create(&LockOptions::default(), LockNatives::default());
    for (key_type, lock_type, expected) in [
        ("MFCARD", "", MfLockAdapter::NAME),
        ("hune2013", "", HuneHSmartV8Adapter::NAME),
        ("ORBITA", "", OrbitaCLockAdapter::NAME),
        ("TTLOCK", "", TtLockAdapter::NAME),
        ("MULTI", "ORBITA", OrbitaCLockAdapter::NAME),
        ("MULTI", "MFCARD", MfLockAdapter::NAME),
        (" multi ", " mfcard ", MfLockAdapter::NAME),
    ] {
        let a = LockAddress {
            lock_type: some(lock_type),
            ..address_101()
        };
        let picked = router
            .pick(Some(&vendor(key_type, "")), Some(&a))
            .unwrap_or_else(|e| panic!("{key_type}: {e}"));
        assert_eq!(family(picked), expected, "{key_type}/{lock_type}");
    }
}

#[test]
fn lock_port_multi_without_a_room_lock_def_lock_type_and_the_agent_key_type_fallback() {
    let options = LockOptions {
        lock_key_type: "ORBITA".into(),
        ..LockOptions::default()
    };
    let router = SmileLockRouter::create(&options, LockNatives::default());
    assert_eq!(
        family(router.pick(None, None).unwrap()),
        OrbitaCLockAdapter::NAME
    );
    let blank = LockVendor {
        key_type: some("  "),
        mf_lock_name: None,
    };
    assert_eq!(
        family(router.pick(Some(&blank), None).unwrap()),
        OrbitaCLockAdapter::NAME
    );
    let e = match router.pick(Some(&vendor("MULTI", "")), None) {
        Err(e) => e,
        Ok(a) => panic!("picked {}", a.family()),
    };
    assert_eq!(
        (e.code.as_str(), e.message.as_str()),
        (
            codes::NOT_CONFIGURED,
            "Multi Lock Types needs the room's RoomLockDef.LockType"
        )
    );
    let other = match router.pick(Some(&vendor("other", "")), None) {
        Err(e) => e,
        Ok(a) => panic!("picked {}", a.family()),
    };
    assert_eq!(
        other.message,
        "Lock type OTHER uses the external lock link (tmpLockLink); no encoder runs on this agent"
    );
}

#[test]
fn lock_port_agent_adapter_smile_lock_builds_the_router() {
    let a = create(SMILE_LOCK, &LockOptions::default()).unwrap();
    assert_eq!(a.family(), SmileLockRouter::NAME);
    assert_eq!(SmileLockRouter::NAME, "smile-lock");
}
