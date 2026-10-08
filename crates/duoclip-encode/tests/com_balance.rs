//! B1-E2: the crate's COM initialization is balanced per call / per encoder, on the same thread.
//! A caller's thread must be left exactly as it was: after the caller undoes its own
//! `CoInitializeEx`, the thread can enter any apartment again.
//!
//! No GPU needed: MFT enumeration and the (software) Microsoft AAC encoder only.

#![cfg(windows)]

use duoclip_encode::mf_audio::MfAacEncoder;
use duoclip_encode::mf_video::list_encoders;
use windows::core::HRESULT;
use windows::Win32::Foundation::{RPC_E_CHANGED_MODE, S_OK};
use windows::Win32::System::Com::{
    CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED, COINIT_MULTITHREADED,
};

fn co_init(mta: bool) -> HRESULT {
    let mode = if mta {
        COINIT_MULTITHREADED
    } else {
        COINIT_APARTMENTTHREADED
    };
    // SAFETY: plain COM initialization of the test thread; balanced by `co_uninit` on success.
    unsafe { CoInitializeEx(None, mode) }
}

fn co_uninit() {
    // SAFETY: balances a successful `co_init` on the same thread.
    unsafe { CoUninitialize() };
}

/// Runs `f` on a fresh thread (fresh COM state) and propagates its panics.
fn on_new_thread(f: impl FnOnce() + Send + 'static) {
    std::thread::spawn(f).join().expect("test thread panicked");
}

/// The reviewer's reproduction: MTA by the caller, `list_encoders()`, the caller uninitializes,
/// then STA must work (it failed with RPC_E_CHANGED_MODE when the crate leaked its init).
#[test]
fn list_encoders_leaves_a_caller_mta_balanced() {
    on_new_thread(|| {
        assert_eq!(co_init(true), S_OK);
        let list = list_encoders().expect("list_encoders");
        assert!(
            !list.is_empty(),
            "no H.264 encoder MFT at all (not even software)"
        );
        co_uninit();
        let hr = co_init(false);
        assert_ne!(hr, RPC_E_CHANGED_MODE, "COM init leaked by list_encoders");
        assert_eq!(hr, S_OK);
        co_uninit();
    });
}

/// Without any caller init, nothing remains initialized afterwards: the next init is the
/// first one (S_OK, not S_FALSE), in either apartment.
#[test]
fn list_encoders_on_an_uninitialized_thread_leaves_it_uninitialized() {
    for mta_after in [false, true] {
        on_new_thread(move || {
            list_encoders().expect("list_encoders");
            list_encoders().expect("list_encoders twice");
            assert_eq!(co_init(mta_after), S_OK, "mta_after = {mta_after}");
            co_uninit();
        });
    }
}

/// A caller in an STA is accepted (RPC_E_CHANGED_MODE is not counted as an init to balance)
/// and its STA is not torn down by the crate.
#[test]
fn list_encoders_accepts_a_caller_sta_and_keeps_it() {
    on_new_thread(|| {
        assert_eq!(co_init(false), S_OK);
        list_encoders().expect("list_encoders in an STA");
        // Still an STA owned by the caller: asking for MTA is refused, STA again is S_FALSE.
        assert_eq!(co_init(true), RPC_E_CHANGED_MODE);
        let again = co_init(false);
        assert!(again.is_ok() && again != S_OK, "{again:?}");
        co_uninit();
        co_uninit();
        // Fully released: MTA works now.
        assert_eq!(co_init(true), S_OK);
        co_uninit();
    });
}

/// An encoder holds its COM init for its whole life and balances it on drop (same thread).
#[test]
fn aac_encoder_balances_com_on_drop() {
    on_new_thread(|| {
        assert_eq!(co_init(true), S_OK);
        let mut enc = MfAacEncoder::new(48_000, 2, 160).expect("AAC encoder");
        co_uninit();
        // The caller's init is gone but the encoder's own keeps the MTA alive while it works.
        let mut frames = Vec::new();
        for k in 0..10 {
            frames.extend(enc.encode(k * 213_333, &[0i16; 2048]).expect("encode"));
        }
        frames.extend(enc.drain().expect("drain"));
        assert!(frames.len() >= 9, "{}", frames.len());
        assert_eq!(co_init(false), RPC_E_CHANGED_MODE, "still MTA while alive");
        drop(enc);
        assert_eq!(co_init(false), S_OK, "COM init leaked by the AAC encoder");
        co_uninit();
    });
}
