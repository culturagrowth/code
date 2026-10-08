//! Process-loopback activation: `ActivateAudioInterfaceAsync` on the virtual
//! `VAD\Process_Loopback` device with a `VT_BLOB` PROPVARIANT of `AUDIOCLIENT_ACTIVATION_PARAMS`,
//! completed through an `#[implement]`ed `IActivateAudioInterfaceCompletionHandler`.

use std::mem::ManuallyDrop;
use std::sync::{mpsc, Mutex};
use std::time::Duration;

use windows::core::{implement, IUnknown, Interface, Ref, HRESULT};
use windows::Win32::Media::Audio::{
    ActivateAudioInterfaceAsync, IActivateAudioInterfaceAsyncOperation,
    IActivateAudioInterfaceCompletionHandler, IActivateAudioInterfaceCompletionHandler_Impl,
    IAudioClient, AUDIOCLIENT_ACTIVATION_PARAMS, AUDIOCLIENT_ACTIVATION_PARAMS_0,
    AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK, AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS,
    PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE, VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK,
};
use windows::Win32::System::Com::StructuredStorage::{
    PROPVARIANT, PROPVARIANT_0, PROPVARIANT_0_0, PROPVARIANT_0_0_0,
};
use windows::Win32::System::Com::BLOB;
use windows::Win32::System::Variant::VT_BLOB;

use crate::AudioError;

/// Heap-allocated activation parameters and the `VT_BLOB` PROPVARIANT pointing at them.
///
/// They are owned by the completion handler: the async operation holds a reference to the
/// handler until it completes, so the parameters outlive the operation even if we stop waiting
/// (timeout) before it finishes. Raw pointers (not `Box`) so that moving the owner never
/// invalidates the pointer stored inside the PROPVARIANT.
struct ActivationParams {
    params: *mut AUDIOCLIENT_ACTIVATION_PARAMS,
    prop: *mut PROPVARIANT,
}

// SAFETY: the pointed-to data is plain old data, written once in `new` and only read afterwards
// (by the audio service); it is freed exactly once in `Drop`, from whichever thread releases the
// last reference to the handler, which is fine for heap memory.
unsafe impl Send for ActivationParams {}
// SAFETY: see `Send`: no interior mutability, nothing is written after construction.
unsafe impl Sync for ActivationParams {}

impl ActivationParams {
    fn new(pid: u32) -> Self {
        let params = Box::into_raw(Box::new(AUDIOCLIENT_ACTIVATION_PARAMS {
            ActivationType: AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK,
            Anonymous: AUDIOCLIENT_ACTIVATION_PARAMS_0 {
                ProcessLoopbackParams: AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS {
                    TargetProcessId: pid,
                    ProcessLoopbackMode: PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE,
                },
            },
        }));
        let prop = PROPVARIANT {
            Anonymous: PROPVARIANT_0 {
                Anonymous: ManuallyDrop::new(PROPVARIANT_0_0 {
                    vt: VT_BLOB,
                    wReserved1: 0,
                    wReserved2: 0,
                    wReserved3: 0,
                    Anonymous: PROPVARIANT_0_0_0 {
                        blob: BLOB {
                            cbSize: size_of::<AUDIOCLIENT_ACTIVATION_PARAMS>() as u32,
                            pBlobData: params.cast::<u8>(),
                        },
                    },
                }),
            },
        };
        Self {
            params,
            prop: Box::into_raw(Box::new(prop)),
        }
    }

    fn prop(&self) -> *const PROPVARIANT {
        self.prop
    }
}

impl Drop for ActivationParams {
    fn drop(&mut self) {
        // SAFETY: both pointers come from `Box::into_raw` in `new` and are freed exactly once,
        // here. The PROPVARIANT must NOT go through PropVariantClear: its blob is our Box, not
        // CoTaskMemAlloc memory (and PROPVARIANT has no Drop impl, so this only frees the box).
        unsafe {
            drop(Box::from_raw(self.prop));
            drop(Box::from_raw(self.params));
        }
    }
}

/// Signals the waiting capture thread when activation completes. Agile (free-threaded marshaler,
/// the `implement` default), as `ActivateAudioInterfaceAsync` requires: the system calls it
/// from one of its own MTA threads.
#[implement(IActivateAudioInterfaceCompletionHandler)]
struct CompletionHandler {
    done: Mutex<Option<mpsc::Sender<()>>>,
    _params: ActivationParams,
}

impl IActivateAudioInterfaceCompletionHandler_Impl for CompletionHandler_Impl {
    fn ActivateCompleted(
        &self,
        _operation: Ref<IActivateAudioInterfaceAsyncOperation>,
    ) -> windows::core::Result<()> {
        // Never panic across the COM boundary: a poisoned lock is still usable here.
        let mut done = self
            .done
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(tx) = done.take() {
            // The receiver may be gone (we timed out); nothing else to do then.
            let _ = tx.send(());
        }
        Ok(())
    }
}

/// Activates an `IAudioClient` capturing `pid` and its process tree. COM must be initialized
/// (MTA) on the calling thread. Waits at most `timeout` for the completion callback.
pub(crate) fn activate_process_loopback(
    pid: u32,
    timeout: Duration,
) -> Result<IAudioClient, AudioError> {
    let (tx, rx) = mpsc::channel();
    let params = ActivationParams::new(pid);
    let prop = params.prop();
    let handler: IActivateAudioInterfaceCompletionHandler = CompletionHandler {
        done: Mutex::new(Some(tx)),
        _params: params,
    }
    .into();

    // SAFETY: COM is initialized on this thread (caller contract). The device path is a static
    // wide string, the IID pointer is a static GUID, and `prop` points to a valid VT_BLOB
    // PROPVARIANT owned by `handler`, which stays alive for this whole call (our reference) and
    // for the whole operation (the operation's reference).
    let operation = unsafe {
        ActivateAudioInterfaceAsync(
            VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK,
            &IAudioClient::IID,
            Some(prop),
            &handler,
        )
    }
    .map_err(|e| activation_error("ActivateAudioInterfaceAsync", e.code()))?;

    match rx.recv_timeout(timeout) {
        Ok(()) => {}
        Err(mpsc::RecvTimeoutError::Timeout) => {
            return Err(AudioError::Activation(format!(
                "process loopback activation for pid {pid} did not complete within {} ms",
                timeout.as_millis()
            )))
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            return Err(AudioError::Activation(format!(
                "process loopback activation for pid {pid} was abandoned without completing"
            )))
        }
    }

    let mut activate_hr = HRESULT(0);
    let mut activated: Option<IUnknown> = None;
    // SAFETY: the operation has completed (callback received), so GetActivateResult is legal;
    // both out pointers are valid locals, and `activated` takes ownership of the returned
    // reference (released on drop).
    unsafe { operation.GetActivateResult(&mut activate_hr, &mut activated) }
        .map_err(|e| activation_error("GetActivateResult", e.code()))?;
    if activate_hr.is_err() {
        return Err(activation_error(
            &format!("process loopback activation for pid {pid}"),
            activate_hr,
        ));
    }
    let activated = activated.ok_or_else(|| {
        AudioError::Activation("process loopback activation returned no interface".into())
    })?;
    activated
        .cast::<IAudioClient>()
        .map_err(|e| activation_error("QueryInterface(IAudioClient)", e.code()))
}

fn activation_error(context: &str, hr: HRESULT) -> AudioError {
    let message = hr.message();
    AudioError::Activation(format!(
        "{context}: HRESULT 0x{:08X} {}",
        hr.0,
        message.trim()
    ))
}
