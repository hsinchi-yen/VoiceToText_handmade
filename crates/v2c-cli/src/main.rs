//! v2c developer CLI. Each subcommand lives in its own file under `cmd/`, owned by one task card.

mod cmd;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "v2c-cli", about = "v2c developer tool")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Transcribe one WAV file (T0.1).
    Transcribe(cmd::transcribe::Args),
    /// Run the fixture set and report text, CER, and timings (T0.1).
    Bench(cmd::bench::Args),
    /// Print trigger commands for live key presses (T0.2).
    HotkeyTest(cmd::hotkey_test::Args),
    /// Inject text into the foreground window after a countdown (T0.3).
    InjectTest(cmd::inject_test::Args),
}

fn main() -> anyhow::Result<()> {
    match Cli::parse().command {
        Command::Transcribe(a) => cmd::transcribe::run(a),
        Command::Bench(a) => cmd::bench::run(a),
        Command::HotkeyTest(a) => cmd::hotkey_test::run(a),
        Command::InjectTest(a) => cmd::inject_test::run(a),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn cli_definition_is_valid() {
        Cli::command().debug_assert();
    }
}
