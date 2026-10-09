//! `duoclip-recorder.exe`: the local recorder (see the crate `SPEC.md` and `--help`).

use duoclip_recorder::cli::{parse_args, HELP};

fn main() {
    let opts = match parse_args(std::env::args_os().skip(1)) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("Erro: {e}");
            std::process::exit(2);
        }
    };
    if opts.help {
        println!("{HELP}");
        return;
    }
    std::process::exit(run(opts));
}

#[cfg(windows)]
fn run(opts: duoclip_recorder::cli::Options) -> i32 {
    duoclip_recorder::win::app::run(opts)
}

#[cfg(not(windows))]
fn run(_opts: duoclip_recorder::cli::Options) -> i32 {
    eprintln!("O DuoClip funciona somente no Windows.");
    1
}
