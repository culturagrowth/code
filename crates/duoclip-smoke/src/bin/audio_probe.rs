//! `audio_probe`: lists the Discord root processes and, with `--capture`, records the Discord
//! process tree by process loopback (plus the game with `--game-exe <name.exe>` and the
//! microphone with `--mic`), several runs in a row (`--runs`, default 3) to expose intermittent
//! activation failures. Each source of each run is saved to `test-output/audio/` as a float32 WAV
//! plus a CSV of per-packet QPC timestamps, and summarized (packets, silence, discontinuities,
//! extrapolated timestamps, drift against QPC in ppm).
//!
//! `--analyze <dir>` recomputes the statistics from the CSVs of an earlier capture.
//! `--skip-exe <name.exe>` leaves out one Discord flavor. Without `--capture` it only lists
//! processes (nothing is recorded).

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
    use std::io::Write;
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    use duoclip_audio::wasapi::{self, CaptureHandle};
    use duoclip_audio::{
        descendants, discord_roots, AudioChunk, AudioError, AudioSink, ProcInfo, SourceKind,
        CHANNELS, SAMPLE_RATE,
    };
    use duoclip_smoke::{analyze_packets, output_dir, write_wav_f32, Args, PacketRecord};

    enum Event {
        Chunk(AudioChunk),
        Error(AudioError),
    }

    struct ChannelSink(mpsc::Sender<Event>);

    impl AudioSink for ChannelSink {
        fn on_chunk(&mut self, chunk: AudioChunk) {
            let _ = self.0.send(Event::Chunk(chunk));
        }
        fn on_error(&mut self, _source: SourceKind, err: AudioError) {
            let _ = self.0.send(Event::Error(err));
        }
    }

    #[derive(Clone)]
    enum Target {
        Process { pid: u32, kind: SourceKind },
        Mic,
    }

    struct Source {
        label: String,
        target: Target,
    }

    pub fn run() -> Result<(), Box<dyn std::error::Error>> {
        let args = Args::from_env();
        if let Some(dir) = args.value("--analyze") {
            return reanalyze(std::path::Path::new(&dir));
        }
        let seconds = args.number("--seconds", 10);
        let runs = args.number("--runs", 3);

        println!("Build do Windows: {}", wasapi::windows_build());
        let snapshot = wasapi::process_snapshot()?;
        // `--skip-exe DiscordCanary.exe` leaves out a Discord flavor (e.g. a work account).
        let skip = args.value("--skip-exe");
        let roots: Vec<u32> = discord_roots(&snapshot)
            .into_iter()
            .filter(|&pid| {
                skip.as_deref()
                    .is_none_or(|s| !exe_of(&snapshot, pid).eq_ignore_ascii_case(s))
            })
            .collect();
        println!("\n== Raízes do Discord ({})", roots.len());
        for &pid in &roots {
            println!(
                "  pid {pid} {} — {} processos na árvore",
                exe_of(&snapshot, pid),
                descendants(&snapshot, pid).len()
            );
        }

        let mut sources: Vec<Source> = roots
            .iter()
            .map(|&pid| Source {
                label: format!("discord-{pid}"),
                target: Target::Process {
                    pid,
                    kind: SourceKind::Discord,
                },
            })
            .collect();
        if let Some(exe) = args.value("--game-exe") {
            let pids: Vec<u32> = snapshot
                .iter()
                .filter(|p| p.exe.eq_ignore_ascii_case(&exe))
                .map(|p| p.pid)
                .collect();
            // The root of the game tree: a matching process whose parent is not also a match.
            let root = snapshot
                .iter()
                .filter(|p| pids.contains(&p.pid) && !pids.contains(&p.parent_pid))
                .map(|p| p.pid)
                .next();
            match root {
                Some(pid) => {
                    println!(
                        "\n== Jogo: {exe} → pid raiz {pid} ({} processos com esse nome)",
                        pids.len()
                    );
                    sources.push(Source {
                        label: format!("game-{pid}"),
                        target: Target::Process {
                            pid,
                            kind: SourceKind::Game,
                        },
                    });
                }
                None => println!("\n== Jogo: {exe} NÃO encontrado entre os processos"),
            }
        }
        if args.flag("--mic") {
            sources.push(Source {
                label: "mic".into(),
                target: Target::Mic,
            });
        }

        if !args.flag("--capture") {
            println!("\n(sem --capture: nada foi gravado)");
            return Ok(());
        }
        if sources.is_empty() {
            println!("\nNenhuma fonte para capturar.");
            return Ok(());
        }

        let dir = output_dir("audio")?;
        let mut summary = std::fs::File::create(dir.join("resumo.csv"))?;
        writeln!(
            summary,
            "run,source,activation,activation_ms,packets,frames,silent_flag_pct,silent_content_pct,discontinuities,extrapolated,gaps,jumps,max_jump_ms,qpc_span_s,sample_span_s,drift_ppm,drift_segment_s,peak_dbfs"
        )?;

        for run in 1..=runs {
            println!("\n== Rodada {run}/{runs}: capturando {seconds} s");
            let mut live: Vec<(String, CaptureHandle, mpsc::Receiver<Event>, f64)> = Vec::new();
            for src in &sources {
                let (tx, rx) = mpsc::channel();
                let sink = Box::new(ChannelSink(tx));
                let started = Instant::now();
                // One attempt per run on purpose: we want to see raw intermittent failures.
                let result = match src.target {
                    Target::Process { pid, kind } => {
                        wasapi::start_process_loopback(pid, kind, sink)
                    }
                    Target::Mic => wasapi::start_microphone(sink),
                };
                let ms = started.elapsed().as_secs_f64() * 1e3;
                match result {
                    Ok(handle) => {
                        println!("  {}: ativação OK em {ms:.1} ms", src.label);
                        live.push((src.label.clone(), handle, rx, ms));
                    }
                    Err(e) => {
                        println!("  {}: ATIVAÇÃO FALHOU em {ms:.1} ms: {e}", src.label);
                        writeln!(
                            summary,
                            "{run},{},\"{}\",{ms:.1},,,,,,,,,,,,",
                            src.label,
                            e.to_string().replace('"', "'")
                        )?;
                    }
                }
            }
            std::thread::sleep(Duration::from_secs(seconds));
            for (label, handle, rx, act_ms) in live {
                if let Err(e) = handle.stop() {
                    println!("  {label}: erro ao parar: {e}");
                }
                let mut packets = Vec::new();
                let mut samples = Vec::new();
                let mut errors = Vec::new();
                for ev in rx.try_iter() {
                    match ev {
                        Event::Chunk(c) => {
                            let peak = c.samples.iter().fold(0f32, |m, s| m.max(s.abs()));
                            packets.push(PacketRecord {
                                qpc_100ns: c.qpc_100ns,
                                frames: c.frames,
                                silent: c.silent,
                                discontinuity: c.discontinuity,
                                extrapolated: c.timestamp_extrapolated,
                                peak,
                            });
                            samples.extend_from_slice(&c.samples);
                        }
                        Event::Error(e) => errors.push(e.to_string()),
                    }
                }
                let base = format!("run{run}-{label}");
                write_wav_f32(
                    &dir.join(format!("{base}.wav")),
                    &samples,
                    SAMPLE_RATE,
                    CHANNELS,
                )?;
                let mut csv = std::io::BufWriter::new(std::fs::File::create(
                    dir.join(format!("{base}.csv")),
                )?);
                writeln!(
                    csv,
                    "index,qpc_100ns,frames,silent,discontinuity,extrapolated,peak"
                )?;
                for (i, p) in packets.iter().enumerate() {
                    writeln!(
                        csv,
                        "{i},{},{},{},{},{},{:.6}",
                        p.qpc_100ns,
                        p.frames,
                        p.silent as u8,
                        p.discontinuity as u8,
                        p.extrapolated as u8,
                        p.peak
                    )?;
                }
                let s = analyze_packets(&packets, SAMPLE_RATE);
                let drift = s.drift_ppm.map_or("n/d".into(), |d| format!("{d:+.1}"));
                let peak = s
                    .peak_dbfs
                    .map_or("silêncio digital".into(), |p| format!("{p:.1} dBFS"));
                println!(
                    "  {label}: {} pacotes, {:.2} s de áudio em {:.2} s de QPC | silêncio: {:.1}% (flag) {:.1}% (conteúdo) | descontinuidades {} | extrapolados {} | lacunas {} | saltos >2 ms {} (máx {:.2} ms) | drift {drift} ppm (trecho de {:.1} s) | pico {peak}",
                    s.packets, s.sample_span_s, s.qpc_span_s, s.silent_flag_pct, s.silent_content_pct,
                    s.discontinuities, s.extrapolated, s.gaps, s.jumps, s.max_jump_ms, s.drift_segment_s
                );
                for e in &errors {
                    println!("  {label}: ERRO no stream: {e}");
                }
                writeln!(
                    summary,
                    "{run},{label},ok,{act_ms:.1},{},{},{:.2},{:.2},{},{},{},{},{:.3},{:.3},{:.3},{},{:.2},{}",
                    s.packets, s.frames, s.silent_flag_pct, s.silent_content_pct, s.discontinuities,
                    s.extrapolated, s.gaps, s.jumps, s.max_jump_ms, s.qpc_span_s, s.sample_span_s,
                    s.drift_ppm.map_or(String::new(), |d| format!("{d:.2}")),
                    s.drift_segment_s,
                    s.peak_dbfs.map_or(String::new(), |p| format!("{p:.1}")),
                )?;
            }
        }
        println!("\nArquivos em {}", dir.display());
        Ok(())
    }

    /// Recomputes the statistics from the per-packet CSVs of an earlier capture.
    fn reanalyze(dir: &std::path::Path) -> Result<(), Box<dyn std::error::Error>> {
        let mut files: Vec<_> = std::fs::read_dir(dir)?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| {
                p.extension().is_some_and(|x| x == "csv")
                    && p.file_name().is_some_and(|n| n != "resumo.csv")
            })
            .collect();
        files.sort();
        for path in files {
            let text = std::fs::read_to_string(&path)?;
            let packets: Vec<PacketRecord> = text
                .lines()
                .skip(1)
                .filter_map(|line| {
                    let f: Vec<&str> = line.split(',').collect();
                    Some(PacketRecord {
                        qpc_100ns: f.get(1)?.parse().ok()?,
                        frames: f.get(2)?.parse().ok()?,
                        silent: *f.get(3)? == "1",
                        discontinuity: *f.get(4)? == "1",
                        extrapolated: *f.get(5)? == "1",
                        peak: f.get(6)?.parse().ok()?,
                    })
                })
                .collect();
            let s = analyze_packets(&packets, SAMPLE_RATE);
            println!(
                "{}: {} pacotes | descontinuidades {} | lacunas >20 ms {} | saltos >2 ms {} (máx {:.2} ms) | drift {} ppm (trecho de {:.2} s)",
                path.file_stem().unwrap_or_default().to_string_lossy(),
                s.packets, s.discontinuities, s.gaps, s.jumps, s.max_jump_ms,
                s.drift_ppm.map_or("n/d".into(), |d| format!("{d:+.1}")),
                s.drift_segment_s
            );
        }
        Ok(())
    }

    fn exe_of(snapshot: &[ProcInfo], pid: u32) -> &str {
        snapshot
            .iter()
            .find(|p| p.pid == pid)
            .map_or("?", |p| p.exe.as_str())
    }
}
