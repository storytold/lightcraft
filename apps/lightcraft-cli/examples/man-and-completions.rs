//! Writes lightcraft-cli's shell completions ([`clap_complete`]: bash, zsh, fish, elvish, PowerShell)
//! and man pages ([`clap_mangen`]: `lightcraft-cli.1` and one per subcommand, such as
//! `lightcraft-cli-render.1`) into a directory. The Linux and FreeBSD packages ship the bash, zsh and
//! fish completions and the man pages (`packaging/linux/package.sh`, `packaging/freebsd/package.sh`):
//!
//! ```sh
//! cargo run -p lightcraft-cli --example man-and-completions -- <out-dir> [<command-name>]
//! ```
//!
//! The command name is `lightcraft-cli` unless the package installs it as another (the Nightly
//! packages: `lightcraft-nightly-cli`).

use std::path::Path;
use std::process::ExitCode;

use clap::ValueEnum;
use clap_complete::Shell;

fn main() -> ExitCode {
    let mut args = std::env::args_os().skip(1);
    let (Some(dir), name, None) = (args.next(), args.next(), args.next()) else {
        eprintln!("usage: man-and-completions <out-dir> [<command-name>]");
        return ExitCode::from(2);
    };
    let dir = Path::new(&dir);
    let name = name.and_then(|n| n.into_string().ok()).unwrap_or_else(|| "lightcraft-cli".to_string());
    match generate(dir, &name) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {}: {e}", dir.display());
            ExitCode::FAILURE
        }
    }
}

/// Also run by the tests.
pub(crate) fn generate(dir: &Path, name: &str) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    // clap keeps a command's name as `&'static str`; this process makes the pages and ends.
    let static_name: &'static str = Box::leak(name.to_string().into_boxed_str());
    let command = || lightcraft_cli::command().name(static_name).bin_name(static_name);
    let mut cmd = command();
    for &shell in Shell::value_variants() {
        clap_complete::generate_to(shell, &mut cmd, name, dir)?;
    }
    // A fresh command: `clap_complete::generate_to` built `cmd`, adding the `help` subcommand,
    // which `clap_mangen` would otherwise give pages of its own (`lightcraft-cli-help-render.1`).
    clap_mangen::generate_to(command(), dir)
}
