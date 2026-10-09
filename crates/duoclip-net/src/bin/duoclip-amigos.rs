//! `duoclip-amigos`: console tool to register this PC and set up friend groups.
//! All the logic lives in `duoclip_net::cli`.

fn main() {
    let args: Vec<String> = std::env::args_os()
        .skip(1)
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    let code = duoclip_net::cli::run(&args, &mut std::io::stdout(), &mut std::io::stderr());
    std::process::exit(code);
}
