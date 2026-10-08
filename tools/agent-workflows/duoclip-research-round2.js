export const meta = {
  name: 'duoclip-research-round2',
  description: 'Pesquisa: métodos de captura do OBS/Medal/outros, relógio global, bucket temporário e pós-roll, com verificação adversarial',
  phases: [
    { title: 'Pesquisa', detail: '6 pesquisadores em paralelo (OBS, Medal, panorama, relógio, bucket, pós-roll)' },
    { title: 'Verificação', detail: '2 verificadores adversariais (captura; relógio/bucket/pós-roll)' },
    { title: 'Lacunas', detail: 'crítico de completude' },
  ],
}

const SCRATCH = args.scratch
const CONTEXT = `PROJECT CONTEXT: We are designing a Windows desktop app ("DuoClip", working name) for friends playing the same game together. Each friend runs the app, which continuously records the game window + game audio + Discord voice audio into a replay buffer with hardware encoding (minimal FPS impact, like Medal/OBS/ShadowPlay). When one friend presses a clip hotkey, BOTH PCs save their clip (N seconds before and M seconds after the press), synchronized in time, and the clipper gets an editor preview of both POVs to set start/end, then exports. Current plan: Windows.Graphics.Capture (WGC) window capture; zero-copy D3D11 -> NVENC/AMF/QSV; RAM ring buffer of encoded packets with 1 s keyframe interval; WASAPI process loopback for the game and Discord processes; QPC timestamps + NTP-style peer-to-peer clock offset estimation over a WebRTC data channel; P2P transfer of a 720p proxy and then the trimmed full-quality range. Requirements: secure (no code injection, anti-cheat friendly, no admin), capture only the game + Discord audio. Users are in Brazil. Today is 2026-10-07.

RESEARCH RULES: Load WebSearch and WebFetch via ToolSearch ("select:WebSearch,WebFetch") before using them. Some hosts may fail via WebFetch (e.g. obsproject.com gave ENOTFOUND earlier) — then rely on search results, GitHub, or git/curl through the configured proxy. If you download anything (git clone, archives), put it in a NEW empty directory under ${SCRATCH}/dl-<something>, only read/grep/list it, and NEVER build, install, execute or disassemble downloaded content. Every claim needs a source URL (or repo path + line). Label confidence honestly: "primary" (official docs/source code/vendor statement), "secondary" (third-party reports, forums, blogs), "inferred" (your reasoning). Do not invent facts; say "not found" when you couldn't find something. Return data only (your final output is consumed by another program).`

const FINDINGS = {
  type: 'object',
  properties: {
    topic: { type: 'string' },
    summary: { type: 'string', description: 'Dense summary of the most decision-relevant conclusions (10-25 sentences).' },
    findings: {
      type: 'array',
      items: {
        type: 'object',
        properties: {
          claim: { type: 'string' },
          evidence: { type: 'string', description: 'What exactly the source says/shows (quote or code reference).' },
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
          note: { type: 'string', description: 'What you found; for refuted/partial give the correct statement.' },
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
    overall_assessment: { type: 'string' },
  },
  required: ['gaps', 'overall_assessment'],
}

const TOPICS = {
  obs: `TOPIC: OBS Studio capture methods on Windows — verified against the OBS SOURCE CODE (github.com/obsproject/obs-studio).
Cover precisely:
(1) Game Capture: how it works (graphics-hook DLL injected via inject-helper / get-graphics-offsets; hooked APIs D3D8/9/10/11/12, DXGI, OpenGL, Vulkan layer), shared-texture (GPU) vs shared-memory (CPU) capture paths, anti-cheat related options ("anti-cheat compatibility hook", "SLI/Crossfire capture mode", the signed hook / "capture hook certificate update" KB article), and what that means for a new unknown app doing the same.
(2) Window Capture methods: BitBlt vs "Windows 10 (1903 and up)" = Windows.Graphics.Capture (libobs-winrt/winrt-capture.cpp, plugins/win-capture/window-capture.c), and the AUTOMATIC method selection logic (which windows default to WGC, e.g. browsers/UWP/Electron) — quote the code.
(3) Display Capture: DXGI Desktop Duplication (duplicator-monitor-capture.c) vs WGC, and auto-selection logic (e.g. multi-GPU laptops).
(4) Game capture's fallback/"capture any fullscreen application"; whether OBS's Game Capture can use WGC as a fallback in recent versions (check for something like "window capture fallback" or WGC inside game-capture).
(5) Application Audio Capture (plugins/win-wasapi): confirm it uses AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK, include-tree mode, minimum Windows build checked in code.
(6) Replay buffer: where packets are stored (plugins/obs-ffmpeg/obs-ffmpeg-mux.c replay buffer: in memory?), how max seconds / max MB are enforced, how "save" cuts at keyframes, and whether OBS natively supports any POST-ROLL (including seconds after the save hotkey) — and known scripts/plugins that add it.
(7) Any OBS developer/KB statements about which method is cheapest/most efficient (e.g., game capture shared texture ~zero-copy; WGC overhead; DXGI duplication cost; HAGS issues).
How: try "git clone --depth 1 --filter=blob:none --sparse https://github.com/obsproject/obs-studio ${SCRATCH}/dl-obs" then "git -C ${SCRATCH}/dl-obs sparse-checkout set plugins/win-capture libobs-winrt plugins/win-wasapi plugins/obs-ffmpeg" and grep/read. If cloning fails, use raw.githubusercontent.com or github.com blob pages via WebFetch, and web search. Cite file paths with approximate line numbers and GitHub URLs.`,

  medal: `TOPIC: Which capture methods MEDAL (medal.tv Windows desktop client) uses, and how its replay buffer/clipping works. Medal's public pages do not name the APIs, so DIG DEEP.
(1) Medal support articles: "How To Choose Your Recording Method", "What is Advanced Window Capture?", "Experimental Capture", "Black Clips / Stuck in 1 frame", anti-cheat articles (Vanguard/Valorant, FACEIT, EAC, BattlEye, Ricochet), any "Game Capture"/"Window Capture"/"Desktop Capture"/"Force Window Capture" settings. List EVERY recording method name Medal exposes and map each to the likely underlying technique (game hook injection vs Windows.Graphics.Capture vs DXGI Desktop Duplication vs BitBlt), with reasoning (e.g., yellow border => WGC; "doesn't work in exclusive fullscreen"; "Game out of Focus" error => what?).
(2) Technical evidence of the engine: Medal third-party licenses / open-source notices (search "medal.tv licenses", "Medal open source notices", "Medal libobs", "Medal OBS based", "medal graphics-hook", "MedalEncoder", "medal-recorder"), job postings (C++ / Windows capture / video engineer at Medal), engineering blog posts, founder interviews (Pim de Witte) about building the recorder, Medal GitHub org repos, Reddit/Discord/forum posts by Medal staff about hooks/injection/OBS; anti-cheat partnerships (e.g. Riot, FACEIT) and whether Medal's game capture injects into games; whether Medal is known to be whitelisted by anti-cheats.
(3) Medal's encoders (NVENC/AMF/QSV/software), replay buffer storage (RAM vs disk), clip length options, and whether Medal supports capturing seconds AFTER the hotkey (post-roll/"clip after") or only before.
(4) How Medal stores/uploads clips (cloud storage/CDN provider if known) — we are considering a cloud bucket for temporary clips.
Optionally, if a public Medal installer is downloadable through the proxy, you may ONLY list archive entries (e.g. with 7z l or unzip -l) and read bundled license/notice TEXT files in a fresh directory ${SCRATCH}/dl-medal — never execute/install/disassemble. Skip this if download is blocked.
Clearly separate: confirmed by Medal/primary; reported by third parties; inferred.`,

  landscape: `TOPIC: Capture methods used by the main game clipping/recording tools on Windows, to judge whether WGC window capture + hardware encoding is the MOST EFFECTIVE approach for a NEW third-party app that must be anti-cheat-safe.
For each tool give method + evidence + performance claims + anti-cheat stance:
- NVIDIA ShadowPlay / NVIDIA App Instant Replay (NvFBC? driver-level capture?) and whether NvFBC is available to third-party apps on GeForce cards (NVIDIA deprecated/restricted NvFBC on Windows? check Capture SDK docs).
- AMD Adrenalin ReLive / Instant Replay (driver-level).
- Xbox Game Bar / Game DVR (Windows internal Broadcast DVR; available to third parties? Windows.Media.AppRecording?).
- Steam Game Recording (2024): how it captures (overlay hook / gameoverlayrenderer?), background recording to disk, timeline with markers, choosing any clip range after the fact.
- Discord Clips (2024) — what capture it uses.
- Overwolf / Outplayed (built on OBS/libobs? "ow-obs"), Insights Capture, SteelSeries Moments, Eklipse.
- Allstar (renders clips server-side from game demo files instead of screen recording; which games) — relevant because demo-based rendering yields perfectly synced multi-POV for games with demos (CS2, Dota 2, LoL replays).
Then synthesize: (a) ranking of capture methods by efficiency (GPU copy cost / FPS impact) with real measurements if any exist (benchmarks, PresentMon studies, dev statements) — be explicit when no hard data exists; (b) which methods are actually available to a third-party app (NvFBC restrictions, driver capture unavailable, hooks require anti-cheat whitelisting); (c) a verdict on whether WGC is the best default, and the recommended fallback chain / hybrid (e.g., WGC window -> WGC monitor -> DXGI duplication; demo-based rendering as an optional "perfect sync" mode for supported games).`,

  clock: `TOPIC: Design input for an in-app GLOBAL CLOCK that timestamps every clip of every participant (instead of each PC's Windows system clock, which can be off by a lot and can jump). The user explicitly wants "a global clock inside the app instead of the computer clock, because that can cause delay".
Research:
(1) Accuracy of the Windows system clock / W32Time by default (domain vs standalone, time.windows.com, poll interval) and Microsoft's documented "high accuracy" support boundaries (1 s / 50 ms / 1 ms) and their requirements; how Windows steps vs slews; GetSystemTimePreciseAsFileTime vs QueryPerformanceCounter; why an app should not rely on the OS clock for sub-frame sync.
(2) Public time sources an app can query with its OWN SNTP/NTP client: NTP.br (NIC.br Brazil: a.st1.ntp.br etc. — do they support NTS?), time.cloudflare.com (NTS; anycast PoPs in Brazil?), Google Public NTP (leap smear — consequences of mixing smeared and non-smeared sources), time.windows.com, pool.ntp.org br zone; NTS (RFC 8915) for authenticated time (prevents spoofing/MITM); Roughtime. Typical accuracy achievable from Brazilian residential/fiber/4G connections (ms), asymmetry issues.
(3) Architecture options for 2–5 friends possibly in different cities: (i) everyone syncs to the same public/own NTP server => global time (error between two clients ~ sum of individual errors); (ii) peer-to-peer NTP-style offsets (error bounded by RTT/2 of the peer path); (iii) hybrid: global time base for consistency across all clips + peer-to-peer refinement within a session; (iv) own time endpoint on our server (UDP NTP needs a VM; Cloudflare Workers can't do UDP NTP; WebSocket/HTTP-based time sync accuracy e.g. like "ntp over websocket"/"timesync" libraries). Which is most accurate and robust; quantify.
(4) Implementation: a monotonic "app clock" = QPC mapped to global time via continuously estimated offset + drift (min-RTT filtering, linear regression / PLL / Kalman as in chrony/ntpd), slewing (never stepping backwards), behavior when offline, persistence across restarts, leap seconds; Rust crates for SNTP/NTS clients (rsntp, sntpc, ntp-proto from ntpd-rs, others) with licenses and NTS support.
(5) How to measure and display uncertainty, and how clips should store timestamps (global ns since epoch + uncertainty).`,

  bucket: `TOPIC: Using a cloud object-storage BUCKET to hold TEMPORARY clips exchanged between friends (friend's app uploads its clip window / 720p proxy / trimmed full-quality range; clipper downloads; auto-delete after e.g. 24–72 h), as an alternative or complement to direct P2P (WebRTC) transfer. Users are in Brazil.
Compare Cloudflare R2, Supabase Storage, AWS S3 (sa-east-1 São Paulo), Backblaze B2, Wasabi, Google Cloud Storage (southamerica-east1):
- CURRENT pricing from official pricing pages: storage per GB-month, egress, operations (class A/B), free tiers, minimum storage duration charges (Wasabi 90 days? others), minimum object size billing.
- Automatic expiration: R2 object lifecycle rules, S3 lifecycle (day granularity), GCS lifecycle / Object Retention, B2 lifecycle, Supabase Storage (no native TTL? needs pg_cron/Edge Function) — granularity (can we delete after hours, or minimum 1 day?).
- Regions near Brazil (R2 location hints / jurisdictions — is there a South America hint?; Supabase sa-east-1), expected upload speeds/latency from Brazil.
- Upload mechanisms: presigned URLs (PUT/POST), S3 multipart, resumable uploads (tus in Supabase), direct-from-client upload without shipping long-lived credentials; download via signed URLs.
- Security: client-side end-to-end encryption (AES-256-GCM, per-clip key shared only between the paired friends over the authenticated channel), short-lived signed URLs, per-user access policies (Supabase RLS on storage.objects), abuse/rate limiting, LGPD notes (data stored abroad, deletion guarantees).
- Estimate monthly cost for 1,000 active users each exchanging 20 clips/month of ~60 MB with 72 h retention (show the math), for at least R2, S3 sa-east-1 and Supabase.
- Recommend architecture: P2P-first with bucket fallback vs bucket-first (works when friend is offline/NAT fails/async, lets the uploading PC finish quickly). Also note how "bucket" could alternatively mean a LOCAL segmented disk buffer (time buckets of 1–2 s segments), and when that is useful.`,

  postroll: `TOPIC: "Recording a bit beyond the press time" — POST-ROLL and safety margins for a replay-buffer clipper (the user wants the clip to include some seconds AFTER the button press, and margins beyond the exact requested window).
(1) How existing tools handle seconds AFTER the hotkey: OBS replay buffer (native behavior = only past? scripts/plugins that delay the save to include post-roll, e.g. Lua/Python scripts, Advanced Scene Switcher macros, "Replay Buffer Pro"-like plugins), Medal (clip settings: any after-press option?), NVIDIA Instant Replay, AMD Instant Replay, Steam Game Recording (background recording + choose any range later), Outplayed (event-based pre/post seconds), Xbox Game Bar "record what happened", SteelSeries Moments. Cite sources.
(2) Implementation details: pin/hold the buffer range so it is not evicted while waiting the post-roll; extend/merge the window if the hotkey is pressed again during post-roll; end on a GOP boundary or force an IDR at the end (NVENC/AMF/QSV force keyframe APIs); encoder pipeline latency (packets for time T arrive later — wait until the encoder has emitted packets with PTS >= end + margin); audio/video interleaving; game closing or app crash during post-roll (write incremental segments to disk); padding margins (e.g. +1–2 s on each side) for keyframe alignment and clock-sync uncertainty so the editor has slack; RAM ring buffer vs DISK-BACKED SEGMENTED buffer (fragmented MP4 / 1–2 s segment files, i.e. local "buckets") — trade-offs for long post-roll, memory, SSD wear, privacy (temp files), crash safety.
(3) Two-PC case: the remote PC must compute the same window in global time, may receive the request late (network delay/loss), and must wait until global T+M (+margin) before finalizing; what if the remote buffer doesn't cover the start; retries/idempotency.
Give concrete recommended defaults (pre, post, margins, segment length, keyframe interval) and an edge-case table.`,
}

const groups = [
  { key: 'captura', topics: ['obs', 'medal', 'landscape'] },
  { key: 'tempo-armazenamento', topics: ['clock', 'bucket', 'postroll'] },
]

const groupResults = await parallel(groups.map(g => async () => {
  const research = await parallel(g.topics.map(t => () =>
    agent(`${CONTEXT}\n\n${TOPICS[t]}`, { label: `pesquisa:${t}`, phase: 'Pesquisa', schema: FINDINGS })
  ))
  const ok = research.filter(Boolean)
  log(`${g.key}: ${ok.length}/${g.topics.length} pesquisas concluídas; iniciando verificação`)
  const verify = await agent(
    `${CONTEXT}\n\nYou are an ADVERSARIAL FACT-CHECKER. Below are JSON findings from ${ok.length} researchers (group: ${g.key}). Select the 12–18 most decision-relevant claims (prioritize: anything the app design depends on, any surprising claim, any claim labeled primary that you can re-check, prices/limits/versions, and what OBS/Medal/other tools actually use). For each, try to REFUTE it by independently checking primary sources (source code, official docs, official pricing pages, vendor statements). If you cannot confirm it, mark "unverifiable" — do not give the benefit of the doubt. For refuted/partially-true claims, state the correct fact with source. Also list contradictions between the researchers.\n\nFINDINGS JSON:\n${JSON.stringify(ok)}`,
    { label: `verificar:${g.key}`, phase: 'Verificação', schema: VERDICTS }
  )
  return { group: g.key, research: ok, verification: verify }
}))

phase('Lacunas')
const all = groupResults.filter(Boolean)
const critic = await agent(
  `${CONTEXT}\n\nThe user asked for these changes to the plan: (1) use a GLOBAL synchronized clock inside the app for all clips instead of the computer's clock; (2) verify recording a bit beyond the specific press time (post-roll + margins); (3) use a BUCKET system to store temporary clips; (4) verify that screen recording (WGC) is the most effective method, and research whether Medal or OBS use it — if nothing specific is found, research deeper to discover which methods they use.\n\nBelow are the researched and adversarially-verified results. Act as a COMPLETENESS CRITIC: what is still missing, unverified, contradictory or under-specified that would change the design or the answer to the user? Especially check whether request (4) is answered definitively for BOTH OBS and Medal (and if Medal remains unknown, what further evidence could settle it). Do NOT repeat what is already covered. Return concrete gaps with how to resolve each (a specific search/source/decision).\n\nRESULTS JSON:\n${JSON.stringify(all)}`,
  { label: 'critico-completude', phase: 'Lacunas', schema: GAPS }
)

return { groups: all, critic }
