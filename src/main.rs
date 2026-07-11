//! fbd CLI entry point.
//!
//! Thin by design: parse args, resolve real XDG [`Paths`], load the roster, and
//! dispatch to a [`cli`] runner. All command logic and its tests live in
//! `cli.rs` behind injected `BdClient`/`Paths`/writers; this file is the only
//! place that touches real paths, spawns `bd`, and maps results to an exit code.

use std::io::{self, Write};
use std::process::ExitCode;

use clap::{Parser, Subcommand};

use fbd::bd::BdCli;
use fbd::cli::{self, CliError};
use fbd::config::Paths;

#[derive(Parser)]
#[command(
    name = "fbd",
    version,
    about = "Federated Beads: a read-only view across your beads repos"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Print the merged cross-repo ready list (the headless tracer bullet).
    Snapshot {
        /// Emit the serialized snapshot as JSON instead of human-readable lines.
        #[arg(long)]
        json: bool,
    },
    /// Delete the hub database; it is rebuilt on the next snapshot.
    Reset,
    /// Report bd version, config/hub paths, and per-repo roster health.
    Doctor,
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            // stderr is already the warning channel; a fatal error joins it.
            let _ = writeln!(io::stderr(), "error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), CliError> {
    let cli = Cli::parse();

    let paths = Paths::resolve().map_err(|e| CliError::Io(io::Error::other(e)))?;
    let bd = BdCli::new();

    let mut stdout = io::stdout().lock();
    let mut stderr = io::stderr().lock();

    // Load the roster per command, not up front: `reset` needs only `Paths`, and
    // `doctor` loads it itself so it can report a bad config instead of aborting.
    // Only `snapshot` treats a malformed config as fatal.
    match cli.command {
        Some(Command::Snapshot { json }) => {
            let roster = cli::load_roster(&paths)?;
            cli::run_snapshot(&roster, &bd, &paths, json, &mut stdout, &mut stderr)
        }
        Some(Command::Reset) => cli::run_reset(&paths, &mut stdout),
        Some(Command::Doctor) => cli::run_doctor(&bd, &paths, &mut stdout),
        // Bare `fbd` is reserved for launching the TUI (Slice 9). Until then,
        // orient the user toward the working subcommands rather than erroring.
        None => {
            writeln!(
                stdout,
                "fbd {} — the interactive TUI arrives in a later slice.",
                env!("CARGO_PKG_VERSION"),
            )?;
            writeln!(
                stdout,
                "For now: `fbd snapshot` (merged ready list), `fbd doctor` (diagnostics), `fbd reset`.",
            )?;
            Ok(())
        }
    }
}
