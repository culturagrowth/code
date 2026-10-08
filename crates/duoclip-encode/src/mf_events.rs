//! Event pump for async MFTs (Windows only): `BeginGetEvent` with a self-re-arming callback that
//! runs on a Media Foundation work-queue thread and hands `(event type, status)` pairs to the
//! encoder thread through a mutex + condvar. The encoder thread can then wait for events with a
//! timeout without spinning or sleeping (a 250 µs `sleep` loop measured ~5 ms per frame on
//! Windows because of timer granularity, against ~2 ms for an immediate wake-up).

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use windows::core::{implement, IUnknownImpl, Ref};
use windows::Win32::Foundation::E_NOTIMPL;
use windows::Win32::Media::MediaFoundation::{
    IMFAsyncCallback, IMFAsyncCallback_Impl, IMFAsyncResult, IMFMediaEventGenerator,
};

use crate::EncodeError;

/// One MF event, reduced to what the encoder needs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PumpEvent {
    /// A media event: `MediaEventType` and its status HRESULT.
    Event { kind: u32, status: i32 },
    /// `EndGetEvent` / `BeginGetEvent` failed (e.g. MF_E_SHUTDOWN): no more events will come.
    Failed(i32),
}

#[derive(Default)]
struct Shared {
    queue: Mutex<VecDeque<PumpEvent>>,
    ready: Condvar,
    stop: AtomicBool,
}

impl Shared {
    /// Locks the queue; a poisoned lock is still usable (never panic across COM).
    fn lock(&self) -> MutexGuard<'_, VecDeque<PumpEvent>> {
        self.queue.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn push(&self, event: PumpEvent) {
        self.lock().push_back(event);
        self.ready.notify_all();
    }
}

#[implement(IMFAsyncCallback)]
struct EventCallback {
    generator: IMFMediaEventGenerator,
    shared: Arc<Shared>,
}

impl IMFAsyncCallback_Impl for EventCallback_Impl {
    fn GetParameters(&self, _flags: *mut u32, _queue: *mut u32) -> windows::core::Result<()> {
        // Default work queue and flags.
        Err(E_NOTIMPL.into())
    }

    fn Invoke(&self, result: Ref<IMFAsyncResult>) -> windows::core::Result<()> {
        let result = result.ok()?;
        // SAFETY: completes the BeginGetEvent request this callback was registered for.
        match unsafe { self.generator.EndGetEvent(result) } {
            Ok(event) => {
                // SAFETY: plain event queries.
                let (kind, status) = unsafe {
                    (
                        event.GetType().unwrap_or(0),
                        event.GetStatus().map(|h| h.0).unwrap_or(-1),
                    )
                };
                self.shared.push(PumpEvent::Event { kind, status });
                if !self.shared.stop.load(Ordering::Acquire) {
                    let me: IMFAsyncCallback = self.to_object().to_interface();
                    // SAFETY: re-arms the request with this same callback (kept alive by MF).
                    if let Err(e) = unsafe { self.generator.BeginGetEvent(&me, None) } {
                        self.shared.push(PumpEvent::Failed(e.code().0));
                    }
                }
            }
            Err(e) => self.shared.push(PumpEvent::Failed(e.code().0)),
        }
        Ok(())
    }
}

/// Receives the events of one async MFT.
pub(crate) struct EventPump {
    shared: Arc<Shared>,
}

impl EventPump {
    /// Starts listening to `generator` (the MFT's IMFMediaEventGenerator).
    pub(crate) fn start(generator: IMFMediaEventGenerator) -> Result<Self, EncodeError> {
        let shared = Arc::new(Shared::default());
        let callback: IMFAsyncCallback = EventCallback {
            generator: generator.clone(),
            shared: shared.clone(),
        }
        .into();
        // SAFETY: registers the callback; MF keeps a reference until it is invoked.
        unsafe { generator.BeginGetEvent(&callback, None) }.map_err(|e| EncodeError::Os {
            context: "IMFMediaEventGenerator::BeginGetEvent".into(),
            hresult: e.code().0,
        })?;
        Ok(Self { shared })
    }

    /// Pops one event; waits up to `timeout` when the queue is empty (`Duration::ZERO` = no wait).
    pub(crate) fn next(&self, timeout: Duration) -> Option<PumpEvent> {
        let deadline = Instant::now() + timeout;
        let mut queue = self.shared.lock();
        loop {
            if let Some(ev) = queue.pop_front() {
                return Some(ev);
            }
            let now = Instant::now();
            if now >= deadline {
                return None;
            }
            queue = self
                .shared
                .ready
                .wait_timeout(queue, deadline - now)
                .unwrap_or_else(|p| p.into_inner())
                .0;
        }
    }

    /// Stops re-arming (the pending request completes when the MFT shuts down).
    pub(crate) fn stop(&self) {
        self.shared.stop.store(true, Ordering::Release);
    }
}

impl Drop for EventPump {
    fn drop(&mut self) {
        self.stop();
    }
}
