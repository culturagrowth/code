//! `encoder_probe`: lists the Media Foundation encoders (MFTEnumEx) — hardware and software
//! H.264 (plus HEVC/AV1 for information) and AAC — with each MFT's name, vendor and adapter LUID,
//! and checks that each one can be activated. Records nothing.
//!
//! The real encode test (D3D11 → `duoclip-encode` → `duoclip-mux` → ffprobe) lives in
//! `crates/duoclip-encode/tests/gpu_encode.rs` (`--ignored`); this probe points to it.

#[cfg(not(windows))]
fn main() {
    duoclip_smoke::only_windows();
}

#[cfg(windows)]
fn main() -> std::process::ExitCode {
    match win::run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("erro: {e}");
            std::process::ExitCode::FAILURE
        }
    }
}

#[cfg(windows)]
mod win {
    use windows::core::{GUID, PWSTR};
    use windows::Win32::Media::MediaFoundation::*;
    use windows::Win32::System::Com::{CoInitializeEx, CoTaskMemFree, COINIT_MULTITHREADED};

    struct MftInfo {
        name: String,
        vendor: Option<String>,
        luid: Option<u64>,
        activation: Result<(), windows::core::Error>,
    }

    pub fn run() -> windows::core::Result<()> {
        // SAFETY: COM + Media Foundation initialization for this process.
        unsafe {
            CoInitializeEx(None, COINIT_MULTITHREADED).ok()?;
            MFStartup(MF_VERSION, MFSTARTUP_FULL)?;
        }
        let hw = MFT_ENUM_FLAG_HARDWARE | MFT_ENUM_FLAG_SORTANDFILTER;
        let sw = MFT_ENUM_FLAG_SYNCMFT | MFT_ENUM_FLAG_ASYNCMFT | MFT_ENUM_FLAG_SORTANDFILTER;
        let video = [
            ("H.264", MFVideoFormat_H264),
            ("HEVC", MFVideoFormat_HEVC),
            ("AV1", MFVideoFormat_AV1),
        ];
        for (label, subtype) in video {
            for (kind, flags) in [("hardware", hw), ("software", sw)] {
                report(
                    &format!("Encoders {label} de {kind}"),
                    MFT_CATEGORY_VIDEO_ENCODER,
                    flags,
                    MFMediaType_Video,
                    subtype,
                );
            }
        }
        report(
            "Encoders AAC",
            MFT_CATEGORY_AUDIO_ENCODER,
            sw,
            MFMediaType_Audio,
            MFAudioFormat_AAC,
        );
        println!(
            "\nTeste de codificação real (GPU → NV12 → H.264 + AAC → duoclip-mux → ffprobe):\n  \
             cargo test -p duoclip-encode --test gpu_encode -- --ignored --nocapture"
        );
        // SAFETY: balanced with MFStartup above.
        unsafe { MFShutdown() }?;
        Ok(())
    }

    fn report(title: &str, category: GUID, flags: MFT_ENUM_FLAG, major: GUID, subtype: GUID) {
        println!("\n== {title}");
        match enumerate(category, flags, major, subtype) {
            Ok(list) if list.is_empty() => println!("  (nenhum)"),
            Ok(list) => {
                for (i, m) in list.iter().enumerate() {
                    println!(
                        "  [{i}] {}{}{} | ativação: {}",
                        m.name,
                        m.vendor
                            .as_deref()
                            .map(|v| format!(" | vendor {v}"))
                            .unwrap_or_default(),
                        m.luid
                            .map(|l| format!(" | LUID {:08X}:{:08X}", l >> 32, l & 0xFFFF_FFFF))
                            .unwrap_or_default(),
                        match &m.activation {
                            Ok(()) => "OK".to_string(),
                            Err(e) => format!("FALHOU {e} (HRESULT 0x{:08X})", e.code().0 as u32),
                        }
                    );
                }
            }
            Err(e) => println!(
                "  MFTEnumEx falhou: {e} (HRESULT 0x{:08X})",
                e.code().0 as u32
            ),
        }
    }

    fn enumerate(
        category: GUID,
        flags: MFT_ENUM_FLAG,
        major: GUID,
        subtype: GUID,
    ) -> windows::core::Result<Vec<MftInfo>> {
        let output = MFT_REGISTER_TYPE_INFO {
            guidMajorType: major,
            guidSubtype: subtype,
        };
        let mut array: *mut Option<IMFActivate> = std::ptr::null_mut();
        let mut count = 0u32;
        // SAFETY: out-pointers are locals; `output` outlives the call.
        unsafe { MFTEnumEx(category, flags, None, Some(&output), &mut array, &mut count) }?;
        if array.is_null() {
            return Ok(Vec::new());
        }
        // SAFETY: MFTEnumEx returned `count` initialized entries in a CoTaskMemAlloc'd array.
        let slots = unsafe { std::slice::from_raw_parts_mut(array, count as usize) };
        // `take()` moves each reference out, so it is released when `activate` drops.
        let infos = slots
            .iter_mut()
            .filter_map(Option::take)
            .map(|activate| describe(&activate))
            .collect();
        // SAFETY: the array itself was allocated by MFTEnumEx with CoTaskMemAlloc.
        unsafe { CoTaskMemFree(Some(array as *const _)) };
        Ok(infos)
    }

    fn string_attr(activate: &IMFActivate, key: &GUID) -> Option<String> {
        let mut value = PWSTR::null();
        let mut len = 0u32;
        // SAFETY: out-pointers are locals; on success `value` is CoTaskMemAlloc'd and freed below.
        unsafe { activate.GetAllocatedString(key, &mut value, &mut len) }.ok()?;
        // SAFETY: `value` is a valid NUL-terminated string returned by the call above.
        let text = unsafe { value.to_string() }.ok();
        // SAFETY: frees the string allocated by GetAllocatedString.
        unsafe { CoTaskMemFree(Some(value.0 as *const _)) };
        text
    }

    fn describe(activate: &IMFActivate) -> MftInfo {
        let name = string_attr(activate, &MFT_FRIENDLY_NAME_Attribute)
            .unwrap_or_else(|| "(sem nome)".into());
        let vendor = string_attr(activate, &MFT_ENUM_HARDWARE_VENDOR_ID_Attribute);
        // SAFETY: plain attribute read.
        let luid = unsafe { activate.GetUINT64(&MFT_ENUM_ADAPTER_LUID) }.ok();
        // SAFETY: activation creates the MFT; ShutdownObject releases its resources again.
        let activation = unsafe {
            activate
                .ActivateObject::<IMFTransform>()
                .map(|_transform| ())
                .and_then(|()| activate.ShutdownObject())
        };
        MftInfo {
            name,
            vendor,
            luid,
            activation,
        }
    }
}
