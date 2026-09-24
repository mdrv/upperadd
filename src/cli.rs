use std::io::{Read, Write};
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
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
            crate::daemon::run(cfg)
        }
        Command::Toggle => send_verb("toggle"),
        Command::Show => send_verb("show"),
        Command::Stop => send_verb("stop"),
        Command::Status => send_verb("status"),
        Command::Reindex => send_verb("reindex"),
    }
}

pub fn socket_path() -> PathBuf {
    PathBuf::from(std::env::var_os("XDG_RUNTIME_DIR").unwrap_or_default()).join("upperadd.sock")
}

/// Send a verb to the daemon over `$XDG_RUNTIME_DIR/upperadd.sock`.
/// Protocol: one verb line in, one `ok <payload>` / `err <message>` line out.
/// No auto-spawn: systemd owns the daemon lifecycle, so a missing socket
/// just means "not running" (owner decision, grilling round 1 Q8).
fn send_verb(verb: &str) -> anyhow::Result<()> {
    let mut stream = UnixStream::connect(socket_path()).map_err(|_| {
        anyhow::anyhow!(
            "upperadd not running (socket {}); start it with: systemctl --user start upperadd",
            socket_path().display()
        )
    })?;
    stream
        .write_all(verb.as_bytes())
        .and_then(|_| stream.shutdown(Shutdown::Write))?;
    let mut resp = String::new();
    stream.read_to_string(&mut resp)?;
    let resp = resp.trim();
    match resp.strip_prefix("ok") {
        Some(payload) => {
            let payload = payload.trim();
            if !payload.is_empty() {
                println!("{payload}");
            }
            Ok(())
        }
        None => Err(anyhow::anyhow!(
            "{}",
            resp.strip_prefix("err").unwrap_or(resp).trim()
        )),
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
