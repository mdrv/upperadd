use std::path::PathBuf;

use clap::{Parser, Subcommand};

use crate::config;

/// Wayland overlay to jump into your markdown notes.
#[derive(Parser, Debug)]
#[command(name = "upperadd", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Run the daemon (systemd user service)
    Daemon {
        #[command(subcommand)]
        cmd: Daemon,
    },
    /// Toggle overlay visibility (default verb)
    Toggle,
    /// Show the overlay
    Show,
    /// Stop the daemon
    Stop,
    /// Show daemon status
    Status,
    /// Rebuild the search index from the notes dir
    Reindex,
}

#[derive(Subcommand, Debug)]
enum Daemon {
    /// Run in the foreground (systemd service mode)
    Start {
        /// Kept for service-file symmetry; foreground is the only mode
        #[arg(long)]
        foreground: bool,
    },
}

pub fn run() -> anyhow::Result<()> {
    match Cli::parse().command.unwrap_or(Command::Toggle) {
        Command::Daemon {
            cmd: Daemon::Start { foreground },
        } => {
            if !foreground {
                log::debug!("--foreground not passed; foreground is the only mode");
            }
            let cfg = config::load()?;
            log::info!(
                "daemon: notes dir {}, socket {} (GUI shell lands in M1)",
                cfg.notes.dir.display(),
                socket_path().display()
            );
            Ok(())
        }
        Command::Toggle => todo_verb("toggle"),
        Command::Show => todo_verb("show"),
        Command::Stop => todo_verb("stop"),
        Command::Status => todo_verb("status"),
        Command::Reindex => todo_verb("reindex"),
    }
}

pub fn socket_path() -> PathBuf {
    PathBuf::from(std::env::var_os("XDG_RUNTIME_DIR").unwrap_or_default()).join("upperadd.sock")
}

/// M1 replaces this with real socket IPC (no auto-spawn: systemd owns the
/// daemon lifecycle, so a missing socket just means "not running").
fn todo_verb(verb: &str) -> anyhow::Result<()> {
    anyhow::bail!(
        "{verb}: daemon IPC lands in M1 (socket {})",
        socket_path().display()
    )
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
