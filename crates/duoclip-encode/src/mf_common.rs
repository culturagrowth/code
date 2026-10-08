//! Media Foundation helpers shared by the video and audio encoders (Windows only).

use std::marker::PhantomData;
use std::sync::OnceLock;

use windows::core::{GUID, PWSTR};
use windows::Win32::Foundation::RPC_E_CHANGED_MODE;
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::{
    CoInitializeEx, CoTaskMemFree, CoUninitialize, COINIT_MULTITHREADED,
};
use windows::Win32::System::Variant::VARIANT;

use crate::error::OsContext;
use crate::EncodeError;

/// One COM initialization of the current thread, balanced on drop (B1-E2).
///
/// `CoInitializeEx` must be balanced by `CoUninitialize` for every success, S_FALSE (already
/// initialized) included. The guard does that on drop, on the same thread: it is `!Send` and
/// `!Sync`, and every owner keeps it as its *last* field / first local so it is dropped after
/// the COM interfaces it protects. `RPC_E_CHANGED_MODE` (the thread is already an STA, which
/// Media Foundation accepts) is not a success: nothing is balanced then. The caller's own
/// apartment is never changed once all guards are gone: after a `list_encoders()` or an
/// encoder drop the thread is back to the state it had before.
pub(crate) struct ComGuard {
    /// `true` when this guard's `CoInitializeEx` succeeded (S_OK or S_FALSE).
    balance: bool,
    /// `!Send + !Sync`: CoUninitialize must run on the initializing thread.
    _thread_bound: PhantomData<*const ()>,
}

impl ComGuard {
    /// Initializes COM (MTA) on the calling thread; an existing STA is accepted.
    pub(crate) fn init_mta() -> Result<Self, EncodeError> {
        // SAFETY: plain COM initialization of the calling thread, balanced in Drop on success.
        let hr = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        if hr == RPC_E_CHANGED_MODE {
            return Ok(Self {
                balance: false,
                _thread_bound: PhantomData,
            });
        }
        if hr.is_err() {
            return Err(EncodeError::Os {
                context: "CoInitializeEx".into(),
                hresult: hr.0,
            });
        }
        Ok(Self {
            balance: true,
            _thread_bound: PhantomData,
        })
    }
}

impl Drop for ComGuard {
    fn drop(&mut self) {
        if self.balance {
            // SAFETY: balances the successful CoInitializeEx of `init_mta` on the same thread
            // (the guard is !Send); the owner dropped its COM interfaces before this guard.
            unsafe { CoUninitialize() };
        }
    }
}

/// Initializes COM on the calling thread (MTA; an existing STA is accepted) for as long as the
/// returned guard lives, and Media Foundation once per process. MF is never shut down: the
/// encoders may live until the process exits.
pub(crate) fn mf_startup() -> Result<ComGuard, EncodeError> {
    let com = ComGuard::init_mta()?;
    static STARTED: OnceLock<i32> = OnceLock::new();
    let hr = *STARTED.get_or_init(|| {
        // SAFETY: process-wide Media Foundation start-up, done exactly once.
        match unsafe { MFStartup(MF_VERSION, MFSTARTUP_FULL) } {
            Ok(()) => 0,
            Err(e) => e.code().0,
        }
    });
    if hr != 0 {
        return Err(EncodeError::Os {
            context: "MFStartup".into(),
            hresult: hr,
        });
    }
    Ok(com)
}

/// `(hi << 32) | lo`, the packing of MF_MT_FRAME_SIZE / MF_MT_FRAME_RATE / ratios.
pub(crate) fn pack(hi: u32, lo: u32) -> u64 {
    (u64::from(hi) << 32) | u64::from(lo)
}

/// Enumerates MFTs; every returned activate is owned (released on drop).
pub(crate) fn enum_mfts(
    category: GUID,
    flags: MFT_ENUM_FLAG,
    input: Option<(GUID, GUID)>,
    output: Option<(GUID, GUID)>,
) -> Result<Vec<IMFActivate>, EncodeError> {
    let to_info = |(major, sub): (GUID, GUID)| MFT_REGISTER_TYPE_INFO {
        guidMajorType: major,
        guidSubtype: sub,
    };
    let input = input.map(to_info);
    let output = output.map(to_info);
    let mut array: *mut Option<IMFActivate> = std::ptr::null_mut();
    let mut count = 0u32;
    // SAFETY: out-pointers are locals; `input`/`output` outlive the call.
    unsafe {
        MFTEnumEx(
            category,
            flags,
            input.as_ref().map(|i| i as *const _),
            output.as_ref().map(|o| o as *const _),
            &mut array,
            &mut count,
        )
    }
    .ctx("MFTEnumEx")?;
    if array.is_null() {
        return Ok(Vec::new());
    }
    // SAFETY: MFTEnumEx returned `count` initialized entries in a CoTaskMemAlloc'd array.
    let slots = unsafe { std::slice::from_raw_parts_mut(array, count as usize) };
    // `take()` moves each reference out of the array, so it is released exactly once (on drop).
    let list = slots.iter_mut().filter_map(Option::take).collect();
    // SAFETY: the array itself was allocated by MFTEnumEx with CoTaskMemAlloc.
    unsafe { CoTaskMemFree(Some(array as *const _)) };
    Ok(list)
}

/// Reads a string attribute (e.g. MFT_FRIENDLY_NAME_Attribute).
pub(crate) fn string_attr(attrs: &IMFAttributes, key: &GUID) -> Option<String> {
    let mut value = PWSTR::null();
    let mut len = 0u32;
    // SAFETY: out-pointers are locals; on success `value` is CoTaskMemAlloc'd and freed below.
    unsafe { attrs.GetAllocatedString(key, &mut value, &mut len) }.ok()?;
    // SAFETY: `value` is a valid NUL-terminated string returned by the call above.
    let text = unsafe { value.to_string() }.ok();
    // SAFETY: frees the string allocated by GetAllocatedString.
    unsafe { CoTaskMemFree(Some(value.0 as *const _)) };
    text
}

/// Reads a blob attribute (e.g. MF_MT_USER_DATA, MF_MT_MPEG_SEQUENCE_HEADER).
pub(crate) fn blob_attr(attrs: &IMFAttributes, key: &GUID) -> Option<Vec<u8>> {
    // SAFETY: plain attribute reads into a buffer of the reported size.
    unsafe {
        let size = attrs.GetBlobSize(key).ok()?;
        let mut buf = vec![0u8; size as usize];
        let mut written = 0u32;
        attrs.GetBlob(key, &mut buf, Some(&mut written)).ok()?;
        buf.truncate(written as usize);
        Some(buf)
    }
}

/// Copies the bytes of a sample (all buffers, made contiguous).
pub(crate) fn sample_bytes(sample: &IMFSample) -> Result<Vec<u8>, EncodeError> {
    // SAFETY: Lock/Unlock are paired; the slice is only used while the buffer is locked and
    // `cur` bytes are valid per the IMFMediaBuffer contract.
    unsafe {
        let buffer = sample
            .ConvertToContiguousBuffer()
            .ctx("ConvertToContiguousBuffer")?;
        let mut ptr: *mut u8 = std::ptr::null_mut();
        let mut cur = 0u32;
        buffer
            .Lock(&mut ptr, None, Some(&mut cur))
            .ctx("IMFMediaBuffer::Lock")?;
        let data = if ptr.is_null() || cur == 0 {
            Vec::new()
        } else {
            std::slice::from_raw_parts(ptr, cur as usize).to_vec()
        };
        buffer.Unlock().ctx("IMFMediaBuffer::Unlock")?;
        Ok(data)
    }
}

/// Creates a sample with one memory buffer holding `bytes`.
pub(crate) fn memory_sample(bytes: &[u8]) -> Result<IMFSample, EncodeError> {
    let len = u32::try_from(bytes.len())
        .map_err(|_| EncodeError::Config("sample larger than 4 GiB".into()))?;
    // SAFETY: the buffer is created with `len` bytes of capacity and locked while copying.
    unsafe {
        let buffer = MFCreateMemoryBuffer(len.max(1)).ctx("MFCreateMemoryBuffer")?;
        let mut ptr: *mut u8 = std::ptr::null_mut();
        buffer
            .Lock(&mut ptr, None, None)
            .ctx("IMFMediaBuffer::Lock")?;
        if !ptr.is_null() && len > 0 {
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr, bytes.len());
        }
        buffer.Unlock().ctx("IMFMediaBuffer::Unlock")?;
        buffer.SetCurrentLength(len).ctx("SetCurrentLength")?;
        let sample = MFCreateSample().ctx("MFCreateSample")?;
        sample.AddBuffer(&buffer).ctx("IMFSample::AddBuffer")?;
        Ok(sample)
    }
}

/// Creates an empty sample with a memory buffer of `size` bytes (for MFTs that do not provide
/// their own output samples).
pub(crate) fn empty_sample(size: u32, alignment: u32) -> Result<IMFSample, EncodeError> {
    // SAFETY: plain object creation.
    unsafe {
        let buffer = if alignment > 1 {
            MFCreateAlignedMemoryBuffer(size.max(1), alignment - 1)
        } else {
            MFCreateMemoryBuffer(size.max(1))
        }
        .ctx("MFCreateMemoryBuffer")?;
        let sample = MFCreateSample().ctx("MFCreateSample")?;
        sample.AddBuffer(&buffer).ctx("IMFSample::AddBuffer")?;
        Ok(sample)
    }
}

/// Result of one ProcessOutput call.
pub(crate) enum Output {
    /// A sample was produced.
    Sample(IMFSample),
    /// MF_E_TRANSFORM_NEED_MORE_INPUT.
    NeedMoreInput,
    /// MF_E_TRANSFORM_STREAM_CHANGE: the output type must be renegotiated.
    StreamChange,
}

/// Calls ProcessOutput once on stream `stream_id`, allocating the output sample when the MFT
/// does not provide it, and releasing everything the MFT hands back.
pub(crate) fn process_output(
    mft: &IMFTransform,
    stream_id: u32,
    provides_samples: bool,
    info: &MFT_OUTPUT_STREAM_INFO,
) -> Result<Output, EncodeError> {
    let sample = if provides_samples {
        None
    } else {
        Some(empty_sample(info.cbSize, info.cbAlignment)?)
    };
    let mut buffers = [MFT_OUTPUT_DATA_BUFFER {
        dwStreamID: stream_id,
        pSample: std::mem::ManuallyDrop::new(sample),
        dwStatus: 0,
        pEvents: std::mem::ManuallyDrop::new(None),
    }];
    let mut status = 0u32;
    // SAFETY: `buffers` is a valid one-element array; ownership of pSample/pEvents is taken
    // back below in every case so each reference is released exactly once.
    let result = unsafe { mft.ProcessOutput(0, &mut buffers, &mut status) };
    // SAFETY: the fields are only taken once; the array is not used afterwards.
    let (sample, events) = unsafe {
        (
            std::mem::ManuallyDrop::take(&mut buffers[0].pSample),
            std::mem::ManuallyDrop::take(&mut buffers[0].pEvents),
        )
    };
    drop(events);
    match result {
        Ok(()) => sample.map(Output::Sample).ok_or(EncodeError::Os {
            context: "ProcessOutput returned no sample".into(),
            hresult: 0x8000_4005_u32 as i32,
        }),
        Err(e) if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => Ok(Output::NeedMoreInput),
        Err(e) if e.code() == MF_E_TRANSFORM_STREAM_CHANGE => Ok(Output::StreamChange),
        Err(e) => Err(EncodeError::Os {
            context: "IMFTransform::ProcessOutput".into(),
            hresult: e.code().0,
        }),
    }
}

/// Input/output stream ids of an MFT (0/0 when the MFT uses fixed ids, E_NOTIMPL).
pub(crate) fn stream_ids(mft: &IMFTransform) -> (u32, u32) {
    let mut input = [0u32; 1];
    let mut output = [0u32; 1];
    // SAFETY: one-element arrays as documented; E_NOTIMPL means ids are 0..n-1.
    match unsafe { mft.GetStreamIDs(&mut input, &mut output) } {
        Ok(()) => (input[0], output[0]),
        Err(_) => (0, 0),
    }
}

/// Sets an ICodecAPI property.
pub(crate) fn codec_set(api: &ICodecAPI, key: &GUID, value: VARIANT) -> windows::core::Result<()> {
    // SAFETY: `value` outlives the call; ICodecAPI copies the value. The VARIANT holds no
    // pointers (integers / bool), so its Drop (VariantClear) is a no-op beyond resetting vt.
    unsafe { api.SetValue(key, &value) }
}
