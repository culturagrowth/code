export const meta = {
  name: 'duoclip-borderless-win10-capture',
  description: 'Pesquisa: captura sem borda amarela no Windows 10 com eficiência nível Medal (DDA recortado, DWM shared surface, hook por jogo) + verificação',
  phases: [
    { title: 'Pesquisa', detail: '4 pesquisadores: Medal/borda, Desktop Duplication, hook + anti-cheat por jogo, DWM shared surface + comparativos' },
    { title: 'Verificação', detail: '2 verificadores adversariais' },
    { title: 'Lacunas', detail: 'crítico de completude' },
  ],
}

const SCRATCH = args.scratch
const CONTEXT = `PROJECT CONTEXT: "DuoClip" (working name) is a Windows desktop app for a group of FRIENDS (private use, not a public product) who play the same game together. Each PC continuously records the game (video + game audio + Discord audio) into a RAM replay buffer with hardware encoding (NVENC/AMF/QSV). When anyone presses the clip hotkey, every PC saves its clip (N s before, M s after) on a shared global clock; clips are exchanged via a cloud bucket (Cloudflare R2). Current plan uses Windows.Graphics.Capture (WGC) window capture as the default.

NEW USER FEEDBACK (the reason for this research): The user requires FULL Windows 10 (22H2, build 19045) support. On Windows 10, WGC always draws a yellow border around the captured window (IsBorderRequired / borderless consent need build 20348+), which the user does not accept. The user says Medal shows NO yellow border on Windows 10 and is "super efficient, doesn't lag at all" — they want that level of efficiency and no border. Earlier research found: Medal's DEFAULT capture is an injected OBS-derived game hook (medal-hook64.dll, shared texture; Medal support says "Medal injects into your games"), Medal's 'Advanced Window Capture' = WGC (yellow border, opt-in), and Medal's non-injected 'standard window capture' logged "Capture mode: DXGI" with a capture-area rect and an inactiveGame.png placeholder on focus loss (likely Desktop Duplication cropped to the game window). The app must stay SAFE: no bans, anti-cheat friendly, no admin, no kernel driver. Users are in Brazil (popular games: CS2, Valorant, LoL, Fortnite, GTA V/FiveM, Minecraft, Roblox, Rainbow Six, Apex, Rocket League, Dota 2, Rust, Warzone, PUBG, Marvel Rivals, EA FC). Today is 2026-10-08.

RESEARCH RULES: Load WebSearch and WebFetch via ToolSearch ("select:WebSearch,WebFetch") before using them. Some hosts may be unreachable via WebFetch (obsproject.com, medal.tv, learn.microsoft.com were blocked before) — then use search snippets, GitHub repos (MicrosoftDocs/* source repos mirror Microsoft Learn), raw.githubusercontent.com, or git clone through the configured proxy. Downloads go in a NEW empty directory under ${SCRATCH}/dl-<name>; only read/grep them; NEVER build, install, execute or disassemble downloaded content. Every claim needs a source URL (or repo path + line). Label confidence honestly: "primary" (official docs/source code/vendor statement), "secondary" (third-party reports, forums, blogs), "inferred" (your reasoning). Say "not found" rather than guessing. Return data only.`

const FINDINGS = {
  type: 'object',
  properties: {
    topic: { type: 'string' },
    summary: { type: 'string', description: 'Dense summary of the decision-relevant conclusions (10-25 sentences).' },
    findings: {
      type: 'array',
      items: {
        type: 'object',
        properties: {
          claim: { type: 'string' },
          evidence: { type: 'string' },
          sources: { type: 'array', items: { type: 'string' } },
          confidence: { type: 'string', enum: ['primary', 'secondary', 'inferred'] },
        },
        required: ['claim', 'evidence', 'sources', 'confidence'],
      },
    },
    design_recommendations: { type: 'array', items: { type: 'string' } },
    open_questions: { type: 'array', items: { type: 'string' } },
  },
  required: ['topic', 'summary', 'findings', 'design_recommendations', 'open_questions'],
}

const VERDICTS = {
  type: 'object',
  properties: {
    checks: {
      type: 'array',
      items: {
        type: 'object',
        properties: {
          claim: { type: 'string' },
          verdict: { type: 'string', enum: ['confirmed', 'refuted', 'partially-true', 'unverifiable'] },
          note: { type: 'string' },
          sources: { type: 'array', items: { type: 'string' } },
        },
        required: ['claim', 'verdict', 'note', 'sources'],
      },
    },
    contradictions_between_researchers: { type: 'array', items: { type: 'string' } },
    corrections_summary: { type: 'string' },
  },
  required: ['checks', 'contradictions_between_researchers', 'corrections_summary'],
}

const GAPS = {
  type: 'object',
  properties: {
    gaps: {
      type: 'array',
      items: {
        type: 'object',
        properties: {
          gap: { type: 'string' },
          why_it_matters: { type: 'string' },
          suggested_resolution: { type: 'string' },
          severity: { type: 'string', enum: ['high', 'medium', 'low'] },
        },
        required: ['gap', 'why_it_matters', 'suggested_resolution', 'severity'],
      },
    },
    recommended_capture_strategy: { type: 'string', description: 'Your synthesized recommendation: which capture method per Windows version / per game type, given all verified evidence.' },
    overall_assessment: { type: 'string' },
  },
  required: ['gaps', 'recommended_capture_strategy', 'overall_assessment'],
}

const TOPICS = {
  border: `TOPIC: The WGC yellow border on Windows 10, and exactly why/how Medal avoids it.
(1) Confirm from primary sources (MicrosoftDocs/winrt-api repo on GitHub) the minimum builds of GraphicsCaptureSession.IsBorderRequired, GraphicsCaptureAccess.RequestAccessAsync(Borderless) and the Windows 11 Settings > Privacy > "Graphics capture"/"screenshot borders" toggle; whether ANY documented or widely-used method removes the WGC border on Windows 10 19041-19045 (search developer forums, GitHub issues of OBS/Sunshine/ShareX/Magpie/robmikh Win32CaptureSample/WindowsCaptureAPI, the Handmade Network 'wcap' thread claiming it's possible, Stack Overflow). Distinguish documented vs undocumented/hacky (e.g., patching, private interfaces, DWM tricks) and the risk of each.
(2) Medal on Windows 10: which method Medal uses by default (per game), and which of its methods show/don't show the border. Search Medal support/learn pages and community posts mentioning "yellow border" + Medal, "Advanced Window Capture" + Windows 10, Medal capture method recommendations for anti-cheat titles (CS2, Valorant, FACEIT, Fortnite), PreferGameCapture, "Force Window Capture", "Experimental Capture". Use the third-party evidence repos already known: github.com/lokritshok/FinalAssignmentVisualStudio (Medal logs 2023) and github.com/RyanTheTechMan/medal-cross-platform (2026 recorder static analysis: settings_catalog.json, recorder_metadata.json, scopesharp_metadata.json, FINDINGS.md) — read them (clone into ${SCRATCH}/dl-medal2 or fetch raw) and extract: the list of capture modes/targets in scope.dll (window/screen/texture targets), any per-game method selection (MedalGameInfo.db fields), any hint of a non-WGC non-injection window capture (DXGI duplication crop, DwmGetDxSharedSurface, BitBlt/PrintWindow), and the default for anti-cheat games.
(3) Explain, with evidence, why Medal feels "super efficient / no lag": hook shared texture, hardware encoder, GPU priority tweaks (SetGPUPriority/IncreaseFrameLatency), Game Mode, HAGS checks, buffer in RAM, etc.
Deliver a clear statement of what Medal does on Windows 10 that results in no border, and what non-injection alternatives exist.`,

  dda: `TOPIC: DXGI Desktop Duplication (DDA) cropped to the game window as the DEFAULT non-injection, borderless capture on Windows 10/11 (this appears to be what Medal's non-injected 'standard window capture' does).
Research with primary sources (MicrosoftDocs/sdk-api & win32 GitHub repos, Microsoft samples, OBS libobs-d3d11/d3d11-duplicator.cpp and duplicator-monitor-capture.c, Sunshine src/platform/windows/display_duplication.cpp / display_vram.cpp / display_base.cpp, Parsec/Moonlight/Apollo/Sunshine docs and issues):
(1) Performance/latency on games: does an active Desktop Duplication session force DWM composition or break independent flip / multiplane overlay for a fullscreen or borderless game? Any evidence of FPS loss, frame pacing or input-latency impact (Sunshine/Parsec issues, OBS forum, PresentMon studies, Microsoft statements)? Compare with WGC's known side effects (software cursor switch, possible independent-flip loss).
(2) Exclusive fullscreen: what DDA captures for legacy exclusive fullscreen vs flip-model borderless; DXGI_ERROR_ACCESS_LOST on mode switches and how Sunshine/OBS recreate; protected content (black); secure desktop/UAC.
(3) Implementation details: IDXGIOutput1::DuplicateOutput vs IDXGIOutput5::DuplicateOutput1 (HDR/FP16 formats), AcquireNextFrame timeout semantics, DXGI_OUTDUPL_FRAME_INFO.LastPresentTime (is it QPC? usable for the global clock?), AccumulatedFrames, cropping with CopySubresourceRegion directly into the encoder input texture (zero CPU copy), rotation, DPI/scaling, multi-monitor (game on secondary monitor), following the game window rect (moves/resizes), only one duplication per output per process & concurrent use by other apps (OBS/Discord/Medal simultaneously), mouse cursor (pointer shape is separate; not drawn unless we draw it).
(4) Hybrid-GPU laptops: DDA must run on the adapter that owns the output; consequences (iGPU duplication then cross-adapter copy to dGPU NVENC, or encode with QSV on iGPU); how OBS/Sunshine handle it.
(5) Privacy & focus: DDA crop records whatever is on top of the game (Discord popups, Windows toasts, volume OSD, Steam notifications). How Medal handles focus loss (inactiveGame.png placeholder). Concrete mitigations: detect foreground window != game (SetWinEventHook EVENT_SYSTEM_FOREGROUND, out-of-process, no injection), detect occluding topmost windows over the game rect, pause/placeholder, Windows Focus Assist/Do Not Disturb.
(6) Verdict: is DDA-crop a good default on Windows 10 (and on 11?) vs WGC, in efficiency, robustness and privacy? Concrete recommended settings.`,

  hook: `TOPIC: Feasibility and safety of an OPTIONAL Medal-style game hook ("maximum performance mode") for DuoClip, used ONLY in games where it is safe, plus a per-game anti-cheat table for the games popular in Brazil.
(1) For each game: CS2 (VAC + Trusted Mode; -allow_third_party_software), Dota 2, Valorant (Vanguard), League of Legends (Vanguard since 2024), Fortnite (EAC + BattlEye), Apex Legends (EAC), Rust (EAC), Rainbow Six Siege (BattlEye), PUBG (BattlEye/other), GTA V Online (BattlEye since 2024) and FiveM (FiveM's own policy), Warzone/CoD (Ricochet, kernel), Marvel Rivals, EA FC (EA Javelin), Rocket League, Roblox (Hyperion/Byfron), Minecraft Java/Bedrock (none), Free Fire (mobile/emulator), and community platforms Gamers Club AC and FACEIT AC: which anti-cheat, whether third-party DLL injection by unknown/unsigned recording software is blocked or risky, any official statements or allowlists for recording software (OBS, Medal, Overwolf, Discord overlay, NVIDIA), and evidence of bans/kicks. Output this as findings with sources; mark unknowns.
(2) What a hook implementation entails: OBS graphics-hook architecture as reference (GPL-2.0 — Medal ships its derived hook as a separate GPL component with a source offer), injection methods (SetWindowsHookEx vs CreateRemoteThread), Vulkan implicit layer, DX9/11/12/OpenGL coverage, shared texture IPC; required code signing (EV/OV) to reduce antivirus false positives (Medal has an antivirus whitelisting article), maintenance burden, crash risk (game crashes blamed on medal-hook64.dll).
(3) Explicitly assess: is it legitimate/safe for a NEW unknown app to inject into games without kernel anti-cheat (Minecraft, single-player, some indie)? Could injection into an anti-cheat game cause bans (and which vendors treat 'unknown module' as a ban vs just a block)? NOTE: Do NOT propose or research ways to evade, bypass or hide from anti-cheat, nor reusing another vendor's signed binaries/certificates to obtain anti-cheat trust; if you encounter such techniques, just state that they are out of scope.
(4) Recommendation: should DuoClip include a hook at all (MVP vs later), and if so with what allowlist policy (e.g., only games with no anti-cheat, decided by a curated per-game database), given that the non-injection alternative may already reach Medal-like efficiency.`,

  dwm: `TOPIC: Non-injection, borderless, window-only capture alternatives on Windows 10 besides WGC and DDA — especially the undocumented DwmGetDxSharedSurface ("DwmSharedSurface") used by Magpie — and comparative efficiency evidence across methods.
(1) DwmGetDxSharedSurface: what it is (user32 export, returns the DWM redirection surface shared handle of a top-level window), which Windows versions support it (Win7..11?), limitations (no flip-model/DirectFlip content? independent flip / fullscreen games? HDR? DPI? window must be composed?), whether frames update at game FPS and how to detect new frames (no frame event — polling vs DwmFlush / DCompositionWaitForCompositorClock), whether it shows any border (no), stability/risk of being undocumented (any Microsoft statement? used by which projects: Magpie, Lossless Scaling?, others), and whether it works on flip-model borderless games (Magpie wiki 'Comparison of capture methods' and Magpie source code src/Magpie.Core/*Capture*; clone github.com/Blinue/Magpie into ${SCRATCH}/dl-magpie and read). Also check Lossless Scaling's capture APIs (it offers WGC and DXGI) and any notes about which is lighter.
(2) Other options: PrintWindow(PW_RENDERFULLCONTENT) and BitBlt (GDI) — cost and whether they capture DirectX/flip-model content; Windows.Media.Capture/AppBroadcasting (not for third parties); DXGI frame statistics.
(3) Comparative efficiency evidence: Magpie wiki comparisons, OBS PR #2208 (WGC vs BitBlt), Sunshine capture benchmarks (DDA vs WGC), Lossless Scaling developer notes about WGC vs DXGI (independent flip, MPO, latency), Apollo/Parsec docs, any PresentMon measurements. Build a table: method | border on Win10 | injection | captures only the game window? | works with exclusive fullscreen | works when occluded/minimized | relative GPU/CPU cost | latency | risks.
(4) Verdict: which non-injection method best matches "no border on Windows 10 + Medal-like efficiency + safe", and a recommended per-Windows-version default + fallbacks.`,
}

const groups = [
  { key: 'medal-borda-e-hook', topics: ['border', 'hook'] },
  { key: 'metodos-sem-borda', topics: ['dda', 'dwm'] },
]

const groupResults = await parallel(groups.map(g => async () => {
  const research = await parallel(g.topics.map(t => () =>
    agent(`${CONTEXT}\n\n${TOPICS[t]}`, { label: `pesquisa:${t}`, phase: 'Pesquisa', schema: FINDINGS })
  ))
  const ok = research.filter(Boolean)
  log(`${g.key}: ${ok.length}/${g.topics.length} pesquisas concluídas; verificando`)
  const verify = await agent(
    `${CONTEXT}\n\nYou are an ADVERSARIAL FACT-CHECKER. Below are JSON findings from ${ok.length} researchers (group: ${g.key}). Select the 12–18 most decision-relevant claims (prioritize: claims about what removes or avoids the yellow border on Windows 10, what Medal does by default, performance/latency side effects of each capture method, anti-cheat policies per game, and anything surprising). Try to REFUTE each by independently checking primary sources. If you cannot confirm, mark "unverifiable". For refuted/partially-true claims give the correct fact with a source. List contradictions between researchers. Do not research anti-cheat evasion techniques.\n\nFINDINGS JSON:\n${JSON.stringify(ok)}`,
    { label: `verificar:${g.key}`, phase: 'Verificação', schema: VERDICTS }
  )
  return { group: g.key, research: ok, verification: verify }
}))

phase('Lacunas')
const all = groupResults.filter(Boolean)
const critic = await agent(
  `${CONTEXT}\n\nThe user's requirement now: full Windows 10 support with NO yellow border and Medal-like efficiency, while staying safe (no bans). Below are the researched and adversarially-verified results. Act as a COMPLETENESS CRITIC and SYNTHESIZER: (a) list what is still missing/unverified/contradictory that would change the capture decision, with concrete resolutions (e.g., a 1-day prototype measurement); (b) give your recommended capture strategy per Windows version and per game category (anti-cheat vs no anti-cheat), including defaults and fallbacks, grounded ONLY in the verified evidence. Do not recommend anti-cheat evasion.\n\nRESULTS JSON:\n${JSON.stringify(all)}`,
  { label: 'critico-completude', phase: 'Lacunas', schema: GAPS }
)

return { groups: all, critic }
