//! The natives of this build (`LockNatives.Installed()`): the LockSDK binding exists only
//! in the 32-bit Windows build. Without LockSDK.dll next to the agent the x86 build fails
//! each MFCARD command like the C# win-x86 build (`DllNotFoundException`, reported as
//! DEVICE_ADAPTER_FAILED); every other build refuses like the C# build without the native.

mod lock_fakes;

use dl_agent_pms_device::adapters::lock::{LockNatives, smile_lock};
use dl_agent_pms_device::config::LockOptions;
use dl_agent_pms_device::contracts::codes;
use lock_fakes::*;

fn hls_issue() -> dl_agent_pms_device::contracts::KeyCardRequest {
    key_card(
        "101",
        "2026-10-14T14:00:00",
        "2026-10-16T12:00:00",
        1,
        1,
        Some(address_101()),
        Some(vendor("MFCARD", "hls")),
    )
}

#[cfg(all(windows, target_arch = "x86"))]
#[test]
fn x86_build_without_locksdk_dll_fails_like_the_csharp_dll_import() {
    let natives = LockNatives::installed();
    assert!(natives.tp.is_some());
    assert!(natives.hune.is_none() && natives.orbita.is_none());
    let router = smile_lock(&LockOptions::default());
    for _ in 0..2 {
        // Retried per command, like .NET (no cached load failure).
        let e = refusal(router.issue(&hls_issue(), &cx()));
        assert_eq!(e.code, codes::ADAPTER_FAILED);
        assert!(
            e.message.starts_with(
                "DllNotFoundException: Unable to load DLL 'LockSDK.dll' or one of its dependencies: "
            ) && e.message.ends_with(" (0x8007007E)"),
            "{}",
            e.message
        );
    }
    // Refusals that come before the native call are unchanged.
    let mut salto = hls_issue();
    salto.vendor = Some(vendor("MFCARD", "salto"));
    assert_eq!(
        refusal(router.issue(&salto, &cx())).message,
        "Lock salto to Write is not defined!"
    );
}

/// ABI smoke test against a stand-in LockSDK.dll with the LockSDK.h exports, placed next to
/// this test executable. The stand-in answers 1 (OPR_OK), echoes the written room in its
/// serial (`SNR-<room>`) and reads the last write back.
#[cfg(all(windows, target_arch = "x86"))]
#[test]
#[ignore = "needs a stand-in LockSDK.dll next to the test executable"]
fn x86_build_drives_a_locksdk_dll_next_to_the_exe() {
    use dl_agent_pms_device::contracts::{EraseRequest, ReadRequest};
    let router = smile_lock(&LockOptions::default());
    let issued = router.issue(&hls_issue(), &cx()).unwrap();
    assert_eq!(issued["cards"][0]["cardNumber"], "SNR-0101");
    let read = router
        .read(
            &ReadRequest {
                vendor: Some(vendor("MFCARD", "hls")),
            },
            &cx(),
        )
        .unwrap();
    assert_eq!(
        dl_json::to_string(&read),
        r#"{"cardNumber":"SNR-0101","address":"0101","from":"2026-10-14 14:00:00","to":"2026-10-16 12:00:00"}"#
    );
    let erase = |card: &str| EraseRequest {
        room: some("101"),
        card_number: some(card),
        lock_address: Some(address_101()),
        vendor: Some(vendor("MFCARD", "hls")),
    };
    assert_eq!(
        refusal(router.erase(&erase("SNR-9999"), &cx())).message,
        "The card on the encoder is SNR-0101, not SNR-9999"
    );
    assert_eq!(
        router.erase(&erase("snr-0101"), &cx()).unwrap()["erased"],
        true
    );
    let mut fail = hls_issue();
    fail.lock_address.as_mut().unwrap().address = some("FAIL");
    assert_eq!(
        refusal(router.issue(&fail, &cx())).message,
        "TP_MakeGuestCardEx failed: ERR_NOT_REGISTERED (-29)"
    );
    // goodum with door type 7: the stand-in refuses the configuration.
    let options = LockOptions {
        mf_lock_door_type: 7,
        ..LockOptions::default()
    };
    let mut goodum = hls_issue();
    goodum.vendor = Some(vendor("MFCARD", "goodum"));
    assert_eq!(
        refusal(smile_lock(&options).issue(&goodum, &cx())).message,
        "TP_Configuration failed: INVALID_PARAMETER (-8)"
    );
}

#[cfg(not(all(windows, target_arch = "x86")))]
#[test]
fn other_builds_have_no_locksdk_and_refuse_like_csharp_without_the_native() {
    let natives = LockNatives::installed();
    assert!(natives.tp.is_none() && natives.hune.is_none() && natives.orbita.is_none());
    let e = refusal(smile_lock(&LockOptions::default()).issue(&hls_issue(), &cx()));
    assert_eq!(
        (e.code.as_str(), e.message.as_str()),
        (codes::SDK_NOT_INSTALLED, "lock SDK not installed for hls")
    );
}
