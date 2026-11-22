//! `app-cli`: the command-line tool on its own ([`lcc_cli::main`]). The desktop packages ship the
//! app's executable under the CLI's name instead.
#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

fn main() -> std::process::ExitCode {
    lcc_cli::main()
}
