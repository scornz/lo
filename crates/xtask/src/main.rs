//! rozed's repo tasks, run as `cargo xtask <task>` (an alias in `.cargo/config.toml`).

mod sync_zed;

use std::process::ExitCode;

const USAGE: &str = "\
usage: cargo xtask <task>

Tasks:
  sync-zed   copy the Cargo settings rozed needs from third_party/zed (--help for more)";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("sync-zed") => sync_zed::run(&args[1..]),
        Some("--help" | "-h") => {
            println!("{USAGE}");
            ExitCode::SUCCESS
        }
        _ => {
            eprintln!("{USAGE}");
            ExitCode::from(2)
        }
    }
}
