export const meta = {
  name: 'duoclip-phase-b1-implement',
  description: 'Fase B1: gamesdb (sonnet), mux MP4 fragmentado, áudio WASAPI e encoder MF/GPU (opus), cada um com revisão adversarial e correção',
  phases: [
    { title: 'Implementar', detail: 'sonnet: gamesdb · opus: mux, audio, encode' },
    { title: 'Revisar', detail: 'revisão adversarial + correção' },
  ],
}

const ROOT = '/home/user/code'
const COMMON = `You are implementing part of DuoClip, a Windows app for friends that records each player's game (no code injection by default) and, on a hotkey, saves every player's POV synchronized by a global clock (N s before and M s after the press), exchanging encrypted clips through a Cloudflare R2 bucket. Repository root: ${ROOT}. Read ${ROOT}/CLAUDE.md, the architecture doc (Portuguese) ${ROOT}/docs/pesquisa-app-clipes-sincronizados.md (sections referenced by your SPEC), and your crate's SPEC.md. Already-implemented sibling crates you may depend on or read: crates/duoclip-buffer (Packet/SharedPacket/Fragment types), crates/duoclip-proto, crates/duoclip-clock, crates/duoclip-crypto.

RULES:
- Work ONLY inside the directory you are assigned. Do NOT edit other crates, the root Cargo.toml/Cargo.lock (unless adding a dependency is truly necessary — then only your crate's Cargo.toml), README.md, CLAUDE.md or docs/. Do NOT run git commands.
- Windows API: the \`windows\` crate is version 0.62.2 (source at ~/.cargo/registry/src/*/windows-0.62.2, windows-core-0.62.2, windows-implement-0.60.2). APIs moved between versions (e.g. BOOL, implement macro path \`windows::core::implement\`, Interface/ComObject), so grep the actual source when unsure instead of guessing. You may add windows features you need to YOUR crate's Cargo.toml feature list.
- Windows-only code under #[cfg(windows)]; the crate must still build and its portable tests must pass on Linux. Windows code cannot be executed here: verify it compiles with the windows-gnu target, write it carefully (check every HRESULT, RAII for handles/COM, minimal unsafe blocks each with a // SAFETY: comment, no UB), and list in known_limitations what needs testing on real Windows.
- Required checks before finishing: \`cargo fmt -p <crate>\`, \`cargo clippy -p <crate> --all-targets -- -D warnings\`, \`cargo test -p <crate>\`, \`cargo clippy -p <crate> --all-targets --target x86_64-pc-windows-gnu -- -D warnings\`. Other agents compile concurrently; if cargo waits on a lock, just wait.
- Tests fast (< 15 s per crate) and deterministic.
- Your final output is consumed by a program: return the structured result only.`

const IMPL_SCHEMA = {
  type: 'object',
  properties: {
    component: { type: 'string' },
    status: { type: 'string', enum: ['ok', 'partial', 'failed'] },
    summary: { type: 'string' },
    tests_passed: { type: 'integer' },
    checks: { type: 'string' },
    deviations_from_spec: { type: 'array', items: { type: 'string' } },
    known_limitations: { type: 'array', items: { type: 'string' } },
    needs_windows_testing: { type: 'array', items: { type: 'string' } },
    files: { type: 'array', items: { type: 'string' } },
  },
  required: ['component', 'status', 'summary', 'tests_passed', 'checks', 'deviations_from_spec', 'known_limitations', 'needs_windows_testing', 'files'],
}
const REVIEW_SCHEMA = {
  type: 'object',
  properties: {
    component: { type: 'string' },
    issues: { type: 'array', items: { type: 'object', properties: {
      severity: { type: 'string', enum: ['critical', 'major', 'minor'] }, description: { type: 'string' }, fixed: { type: 'boolean' }, note: { type: 'string' } },
      required: ['severity', 'description', 'fixed', 'note'] } },
    tests_passed: { type: 'integer' },
    checks: { type: 'string' },
    remaining_risks: { type: 'array', items: { type: 'string' } },
    summary: { type: 'string' },
  },
  required: ['component', 'issues', 'tests_passed', 'checks', 'remaining_risks', 'summary'],
}

const TASKS = [
  {
    key: 'gamesdb', model: 'sonnet',
    impl: `${COMMON}\n\nYOUR TASK: implement ${ROOT}/crates/duoclip-gamesdb exactly per its SPEC.md (games database JSON embedded with include_str!, validation, lookup, merge, and the choose_backend policy). Only edit files under crates/duoclip-gamesdb (create games.json there). Anti-cheat data: use your best knowledge, verified=false everywhere; only games with NO anti-cheat may allow 'hook'.`,
    review: `${COMMON}\n\nYOUR TASK: ADVERSARIAL REVIEW + FIX of ${ROOT}/crates/duoclip-gamesdb against its SPEC.md. Critical property: the Hook backend must be impossible to select unless ALL conditions hold (no anti-cheat, allowed, user-enabled for that exact game id, installed) and must never appear as a fallback — try to break this with crafted DB JSON (e.g. anticheat [none, eac], duplicate ids, case tricks in exe names, merged updates that flip anti-cheat to none with a lower version, unknown enum values, huge input). Also double-check the embedded anti-cheat data for obvious errors (e.g. Valorant/LoL = Vanguard kernel; Fortnite = EAC+BattlEye; CS2 = VAC; Minecraft = none). Fix every real issue with regression tests.`,
  },
  {
    key: 'mux', model: 'opus',
    impl: `${COMMON}\n\nYOUR TASK: implement ${ROOT}/crates/duoclip-mux exactly per its SPEC.md: Annex B helpers, avcC, crash-safe fragmented MP4 writer (ftyp/moov/mvex + moof/mdat per GOP) and progressive faststart MP4 with optional edit-list trim, for H.264 + AAC tracks, consuming duoclip_buffer::SharedPacket. Validate with the REAL system ffmpeg/ffprobe (installed at /usr/bin; libx264 + aac available) exactly as the SPEC's tests describe (generate fixtures in a temp dir, mux, ffprobe stream/frames/duration checks, decode with -f null without errors, truncated-file test, trim test). Only edit files under crates/duoclip-mux.`,
    review: `${COMMON}\n\nYOUR TASK: ADVERSARIAL REVIEW + FIX of ${ROOT}/crates/duoclip-mux. Verify box layouts against ISO/IEC 14496-12/15 semantics: size fields, full-box version/flags, tfhd default-base-is-moof + trun data_offset correctness, tfdt continuity across fragments, sample flags (sync vs non-sync, depends_on), mdhd/mvhd durations, elst media_time and segment_duration units (movie timescale vs media timescale!), stco vs co64, avcC profile/level bytes from SPS, esds descriptor lengths, AAC priming not mishandled. Hunt timestamp drift/rounding bugs and panics on malformed input. Strengthen the ffprobe/ffmpeg-based tests (e.g. check per-stream frame counts, pts monotonicity via ffprobe -show_packets, A/V start alignment, audio/video duration within 1 frame; run ffmpeg with -xerror). Fix every real issue with regression tests.`,
  },
  {
    key: 'audio', model: 'opus',
    impl: `${COMMON}\n\nYOUR TASK: implement ${ROOT}/crates/duoclip-audio exactly per its SPEC.md: portable types + discord_roots/descendants + TimestampTracker (unit-tested), and the #[cfg(windows)] WASAPI module: process loopback via ActivateAudioInterfaceAsync with an #[implement]ed IActivateAudioInterfaceCompletionHandler and PROPVARIANT VT_BLOB activation params, event-driven capture thread with MMCSS, GetBuffer QPC timestamps, retry/backoff, endpoint loopback fallback, microphone capture, process snapshot via Toolhelp32. Only edit files under crates/duoclip-audio.`,
    review: `${COMMON}\n\nYOUR TASK: ADVERSARIAL REVIEW + FIX of ${ROOT}/crates/duoclip-audio, focusing on the Windows code you cannot run: COM initialization/apartment correctness on each thread, the completion-handler lifetime and threading (agile/FTM requirements for ActivateAudioInterfaceAsync callbacks), PROPVARIANT blob memory lifetime (must outlive the async call), WAVEFORMATEXTENSIBLE construction, buffer/packet loop correctness (ReleaseBuffer always called, flags handling), event + stop flag shutdown without deadlock, handle leaks, MMCSS revert, HRESULT checks, unsafe soundness (aliasing, lifetimes of raw pointers from GetBuffer), and that silent packets / missing QPC are handled. Also re-check the portable logic edge cases. Fix every real issue; add tests where portable.`,
  },
  {
    key: 'encode', model: 'opus',
    impl: `${COMMON}\n\nYOUR TASK: implement ${ROOT}/crates/duoclip-encode exactly per its SPEC.md: portable config/fit_rect/vendor/FramePacer/AacFramer (unit-tested), and the #[cfg(windows)] modules: D3D11 device creation on an adapter LUID, D3D11 video-processor GpuConverter (BGRA8/RGBA16F → NV12 fixed size with letterbox), Media Foundation hardware H.264 MFT encoder (async MFT unlock, DXGI device manager, ICodecAPI settings: GOP, zero B-frames, rate control, low latency, force IDR; event loop; Annex B output) with software MFT fallback, and the MF AAC encoder exposing the AudioSpecificConfig. Do NOT implement an FFmpeg backend (future feature). Only edit files under crates/duoclip-encode.`,
    review: `${COMMON}\n\nYOUR TASK: ADVERSARIAL REVIEW + FIX of ${ROOT}/crates/duoclip-encode, focusing on the Windows code you cannot run: async MFT protocol (MF_TRANSFORM_ASYNC_UNLOCK, event generator usage, never calling ProcessInput without METransformNeedInput, drain/flush handling, stream IDs), IMFDXGIDeviceManager + multithread protection, media type order (output before input for encoders), ICodecAPI property types (VT_UI4/VT_BOOL), MFCreateDXGISurfaceBuffer + sample time/duration units (100 ns), CleanPoint keyframe detection, NAL output format (Annex B vs length-prefixed — normalize to Annex B), AAC encoder media types and ASC extraction offset, D3D11 video processor stream/output views and color space hints, MFStartup/MFShutdown refcount, COM lifetimes, unsafe soundness. Re-check FramePacer for drift over hours and AacFramer timestamp math. Fix every real issue; add portable tests where possible.`,
  },
]

const results = await pipeline(
  TASKS,
  t => agent(t.impl, { label: `implementar:${t.key}`, phase: 'Implementar', model: t.model, schema: IMPL_SCHEMA }),
  (implResult, t) => agent(`${t.review}\n\nThe implementer reported:\n${JSON.stringify(implResult)}`,
    { label: `revisar:${t.key}`, phase: 'Revisar', model: t.model, schema: REVIEW_SCHEMA })
    .then(review => ({ task: t.key, model: t.model, impl: implResult, review })),
)
return results.filter(Boolean)
