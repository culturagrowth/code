# duoclip-recorder — SPEC (local recorder: the first program the user runs)

Task 20 in `docs/TAREFAS.md`. Priority: decision 19 in `docs/MEMORIA-DO-PROJETO.md` — a **usable** version for the user's PC and games,
kept simple. Network/friends come later; this program records and saves clips **locally**.

## What it does

`duoclip-recorder.exe` (console app, Windows; on other platforms it prints "somente Windows"):
1. Loads the per-person config file (creates it with defaults and Portuguese comments on first run) and prints a short summary.
2. Waits for a game: polls the foreground window (~1 s); when its process exe is in the games database (`duoclip-gamesdb` lookup) or matches
   `jogo.exe` from the config, starts recording that window. Without a known game it does nothing (no capture, no CPU).
3. Recording pipeline (all already-implemented crates, wired together):
   - video: `duoclip-capture` DdaCropBackend (backend chosen with `duoclip-gamesdb::choose_backend`; only DdaCrop is implemented — if the
     choice is something else, use DdaCrop and log it) → `duoclip-encode` FramePacer + GpuConverter (fixed output size) + MfH264Encoder
     → `duoclip-buffer` ring (video track);
   - audio: `duoclip-audio` process loopback of the game process tree, of the Discord root (if `audio.discord`), and the microphone
     (if `audio.microfone`) → **mixed into ONE stereo track** (players, WhatsApp and Discord play only the first audio track) → AacFramer +
     MfAacEncoder → ring (audio track). Mixing aligns sources by their QPC timestamps (missing data = silence; per-source volume from the config).
   - the D3D11 device is created on `adapter_luid_for_window(game)` (duoclip-capture) via `duoclip_encode::d3d::create_device`.
4. Hotkey (`RegisterHotKey`, never a global keyboard hook): on press, plays the "clip" sound (if enabled) and requests a clip with
   `segundos_antes` before and `segundos_depois` after the press (plus the hidden 2 s margins of the project defaults), using the buffer's
   pin-and-collect (`ClipManager::request`). Pressing again while the clip is collecting **extends** it (`ClipManager::extend`).
5. When the clip finishes: `duoclip-mux::write_progressive` (faststart, trimmed to the window without the hidden margins) to
   `pasta_clipes\DuoClip_AAAA-MM-DD_HH-MM-SS_<jogo>.mp4` (local wall time only for the file name), plays the "saved" sound, prints the path.
6. When the game window closes or loses its process, stops the capture cleanly (pending clips finish with what they have) and goes back to waiting.
7. Ctrl+C: stops cleanly, finishing/saving clips that are already collecting.

Out of scope now (register as later): tray icon/UI, editor, separate audio tracks per source, network/friends, WGC/hook backends.

## Per-person configuration

File: `%APPDATA%\DuoClip\config.toml` (path printed at startup; `--config <path>` overrides). TOML with **Portuguese keys and comments**
(the user edits it by hand). Every field has a default; unknown keys are reported as a warning; invalid values are reported in Portuguese with the
key name and the accepted range, and the program refuses to start instead of guessing.

```toml
# Qualidade do vídeo: "baixa" (720p 30 fps, 6 Mbps), "media" (1080p 60 fps, 15 Mbps), "alta" (1080p 60 fps, 30 Mbps),
# "muito_alta" (1440p 60 fps, 45 Mbps) ou "personalizada" (usa resolucao, fps e bitrate_mbps abaixo).
qualidade = "alta"
# resolucao = "1920x1080"      # só com qualidade = "personalizada"; lados pares, de 640x360 a 3840x2160
# fps = 60                     # 30, 60, 120 ou 144
# bitrate_mbps = 30            # 2 a 100

# Encoder: "auto" (placa de vídeo; se não houver, software), "placa_de_video" (só hardware) ou "software".
encoder = "auto"

# Quantos segundos guardar antes e depois do aperto do atalho.
segundos_antes = 30            # 5 a 120
segundos_depois = 10           # 0 a 60

# Atalho para clipar. Modificadores: Ctrl, Alt, Shift, Win. Tecla: F1–F24, A–Z, 0–9, Insert, Home, PageUp, Pause, etc.
atalho = "Alt+F10"

[aviso_sonoro]
ativado = true
# "padrao" usa os sons do Windows; ou o caminho de um arquivo .wav.
som_ao_clipar = "padrao"
som_ao_salvar = "padrao"
volume = 80                    # 0 a 100 (só para arquivos .wav)

[audio]
jogo = true
discord = true
microfone = false
volume_jogo = 100              # 0 a 200 (%)
volume_discord = 100
volume_microfone = 100
# Discord a ignorar (ex.: conta do trabalho): nomes de exe separados por vírgula.
ignorar_discord = "DiscordCanary.exe"

[jogo]
# Vazio = detectar pelo banco de jogos. Ou o nome do exe, ex.: "javaw.exe" para Minecraft Java.
exe = ""

pasta_clipes = ""              # vazio = pasta Vídeos\DuoClip do usuário
```

- Quality presets map to `VideoConfig` (encode) with GOP = fps and no B-frames (project decision). The output size is fixed for the whole
  session; a game window of another size is letterboxed by GpuConverter.
- `segundos_antes` sizes the ring buffer: `max_duration = segundos_antes + 2 + 5 s` slack; memory budget = bitrate × duration with a 1.5×
  margin, printed at startup (e.g. "buffer de 37 s ≈ 140 MB de RAM").
- The hotkey string is parsed into `RegisterHotKey` modifiers + virtual key; if registration fails (already used by another program), the
  program says so in Portuguese and refuses to start.
- Sounds: `PlaySoundW` async (`SND_ASYNC`): "padrao" = `SystemAsterisk` for the clip press and `SystemExclamation` (or similar documented
  system alias) for saved; a .wav path plays that file. Our own sound is NOT captured into the clip (we only capture the game/Discord/mic processes).

## Code layout and tests

- Portable, unit-tested modules (`#![forbid(unsafe_code)]` outside `win/`): `config` (parse/defaults/validation/Portuguese messages, writing the
  commented default file), `hotkey` (string → modifiers/key, round trip, errors), `presets` (quality → VideoConfig), `mixer` (QPC-aligned mixing of
  N sources into 1024-sample stereo frames for AacFramer; gaps = silence; volume; clipping), `naming` (file name sanitizing the game name).
- Windows (`win/`): wiring of capture/audio/encode/buffer/mux, hotkey message loop, sounds, foreground polling, Ctrl+C.
- A Windows integration test marked `#[ignore = "records screen and audio: run only after the user confirms"]` that records a test-owned
  window for ~5 s with synthetic audio sources replaced where possible, presses the hotkey programmatically (posting the WM_HOTKEY message
  or calling the same handler), and checks the MP4 with ffprobe (duration ≈ antes + depois, h264 + aac present). Agents must NOT run it.

Required checks: `cargo fmt --all`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`,
`cargo check --workspace --all-targets --target x86_64-pc-windows-gnu`. Keep it simple (decision 19): no speculative hardening.

## Implementation notes (accepted deviations / additions)

Status: implemented on branch `claude/recorder`. Portable checks pass on Windows 11 (MSVC) and
`cargo check --target x86_64-pc-windows-gnu`. **The program and the ignored hardware test have NOT been run yet** (they record the
screen/audio and need the user's authorization); only `--help` and `--check-config` were run.

Layout (all portable modules have `#![forbid(unsafe_code)]` and never panic on the config file):
- `config` (parse/defaults/validation, `DEFAULT_TEXT`, `load_or_create`), `hotkey`, `presets` (quality → `VideoConfig`, `BufferPlan`),
  `mixer`, `naming`, `wav` (volume scaling of `.wav` sounds), plus three additive modules: `clip` (hotkey → `ClipManager`
  request/extend, cutting a `FinishedClip` to the visible window, `write_clip`), `detect` (foreground exe → game; Discord roots minus
  `ignorar_discord`) and `cli` (arguments, startup summary).
- `win/`: `sys` (QPC, local time, Videos folder, foreground window, process image), `ctrlc`, `sounds`, `video` (capture sink:
  FramePacer → GpuConverter → MfH264Encoder, created lazily on the capture thread as in the capture crate's end-to-end test), `audio`
  (WASAPI inputs → mixer thread → AacFramer → MfAacEncoder), `session` (one game window: capture + audio + `ClipController`),
  `app` (main loop).

Deviations and decisions:
- **`pasta_clipes` is at the top of the default file**, before `[aviso_sonoro]`. In the SPEC's sample it comes after `[jogo]`, where
  TOML makes it `jogo.pasta_clipes`. A top-level key found inside a section is reported as an unknown key with the hint "deve ficar no
  início do arquivo".
- Config: all invalid values are reported together; `resolucao`/`fps`/`bitrate_mbps` without `personalizada` give a warning (ignored);
  `jogo.exe` must be a bare `*.exe` name; `"padrão"` is accepted like `"padrao"`; a `.wav` that cannot be read refuses the start, one
  whose volume cannot be scaled (not PCM 8/16-bit) plays unscaled with a warning. Syntax errors give the line (the `toml` crate's
  message itself is English).
- Hotkey: letters, digits and Space require Ctrl, Alt or Win (otherwise the hotkey would eat normal typing). `MOD_NOREPEAT` is always
  added. Key names: F1–F24, A–Z, 0–9, Num0–Num9, Insert/Ins, Delete/Del, Home, End, PageUp/PgUp, PageDown/PgDn, Pause, ScrollLock,
  PrintScreen/PrtSc, Space, Up/Down/Left/Right. A Windows unit test compares the codes with the `windows` crate constants.
- Presets: VBR with peak = 1.5 × average (as `VideoConfig::default_for`), GOP = fps, low latency. Ring byte cap = (video + 160 kbps AAC)
  × `segundos_antes + 7` s × 1.5 (default: "buffer de 37 s ≈ 210 MB de RAM"). Clip max length (extensions included) =
  antes + depois + 4 + 120 s; pin budget = that clip at the capped rate, at least 1 GiB (the buffer's default).
- Mixer: per-source timeline starting at the mixer cursor; a chunk within 2 ms of where the previous one of the same source ended is
  appended contiguously (absorbs jitter), otherwise it is placed by its timestamp (gaps = silence, overlaps replace). The mixer thread
  emits 250 ms behind real time; data older than the cursor or > 10 s ahead is dropped; one call emits at most 10 s. Output = 1024-frame
  blocks → `AacFramer` (f32 → i16) → `MfAacEncoder` 160 kbps. Audio pts are the encoder's output sample times (the input samples carry
  absolute QPC times; that the Microsoft AAC MFT keeps them is an assumption until the hardware test runs); a frame going backwards
  would be dropped (defensive).
- Audio inputs: process loopback (with retry) of the game window's pid tree and of each Discord root (`discord_roots` minus
  `ignorar_discord`), and the default communications microphone. An input that fails is left out with a warning; **the endpoint-loopback
  fallback is never used** (it would capture everything). Discord is looked up once per session start. If no input is enabled/found,
  the clip has no audio track.
- Capture backend: `choose_backend` is called with `borderless_wgc_available = false`, `hook_installed = false`, kernel anti-cheat
  `Unknown`, so it always returns `DdaCrop` (the message for another choice exists but cannot trigger in this phase).
- Clip: the window is computed with the identity UTC→local mapping and eps = 0 (local only). A press while the clip is collecting calls
  `ClipManager::extend(press + depois + 2 s)`. The finished clip keeps packets with `pts < last press + depois` and uses
  `trim_start = press − antes` (edit list) when it lies inside the clip; `base_ns` = earliest packet. Fragments are drained and dropped
  (no crash-safe bucket yet). If no video was captured the clip is skipped with a warning.
- File: written as `<name>.mp4.part` and renamed; names are reserved in memory so two clips in the same second get `_2`. The local time
  in the name is the time of the (first) press. The "saved" sound and message come from the main thread.
- Sounds: "padrao" = `SystemAsterisk` (clip) / `SystemExclamation` (saved), `SND_ALIAS | SND_ASYNC | SND_NODEFAULT`; `.wav` files are
  read into memory, scaled and played with `SND_MEMORY | SND_ASYNC`; `Drop` stops playback before the buffer is freed.
- Main loop: 10 ms sleep; `PeekMessageW` for `WM_HOTKEY` (thread hotkey, no window); foreground polled every 1 s while waiting; while
  recording, the window is checked with `IsWindow` every 1 s and the capture's `WindowNotFound` also ends the session. A window whose
  session failed is not retried for 30 s. Ctrl+C / Ctrl+Break / closing the console: stop the session (pending clips finish with what
  they have, `source_ended`), join the writer threads, unregister the hotkey; the close event waits up to 4.5 s for that.
- Exit codes: 0 ok, 1 runtime error / not Windows, 2 configuration or argument error. `--check-config` never creates files.
- Integration test (`tests/recorder_hw.rs`, ignored): it calls `Session::press` (what the WM_HOTKEY handler calls) instead of posting
  WM_HOTKEY; audio is one synthetic 440 Hz input generated in the test process (no WASAPI); timing 2 s before / 1 s after; output in
  `test-output/recorder/recorder-e2e.mp4`; checks h264 + aac, duration 3 s ± 0.3 s and a clean `ffmpeg -xerror` decode.

Known limits (for later):
- Desktop Duplication delivers no frame while the game window's pixels do not change; then no video packets are produced (the pacer
  repeats the last frame only when the next one arrives, at most 2 s). A clip whose end falls in a long static stretch finalizes by the
  3 s timeout and can be shorter at the end. Games normally redraw every frame.
- While the game is not in the foreground (alt-tab) the clip shows the capture crate's dark placeholder (privacy rule), as designed.
- Discord started after the game is not picked up until the next session; the game's audio follows its process tree only.
- HDR: FP16 frames are converted with the SDR white level (values above it are clipped; no tone mapping), per duoclip-encode.
- Out of scope (as in the SPEC): tray/UI, editor, separate audio tracks, network/friends, WGC/hook backends.
