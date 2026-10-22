mod cosmic_cfg;
mod ipc;
mod model;
mod wayland;

use anyhow::{Context, Result, bail};
use clap::{Args, Parser, Subcommand, ValueEnum};
use std::time::Duration;
use tracing::error;
use tracing_subscriber::EnvFilter;

/// Per-workspace dock and Alt+Tab for the COSMIC desktop.
#[derive(Parser)]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run the background daemon that powers the per-workspace Alt+Tab.
    Daemon {
        /// Presses closer together than this walk one frozen window list.
        #[arg(long, default_value_t = 700)]
        burst_ms: u64,
    },
    /// Read-only: print workspaces and windows live. Sends nothing to the compositor.
    Watch,
    /// Focus the next/previous window on the current workspace (bound to Alt+Tab).
    Switch {
        #[arg(value_enum)]
        direction: SwitchDir,
    },
    /// Print the daemon's view of workspaces and windows as JSON.
    Status,
    /// Change COSMIC settings (originals are backed up first).
    Apply(Parts),
    /// Undo `apply` (values changed by you since then are left alone).
    Restore(Parts),
}

#[derive(Clone, Copy, ValueEnum)]
enum SwitchDir {
    Next,
    Prev,
}

#[derive(Args)]
struct Parts {
    /// Dock shows only windows on the current workspace.
    #[arg(long)]
    dock: bool,
    /// Don't jump to another workspace when an app there asks for focus.
    #[arg(long)]
    activation: bool,
    /// Alt+Tab / Super+Tab cycle only the current workspace (needs the daemon).
    #[arg(long)]
    switcher: bool,
    /// All of the above.
    #[arg(long)]
    all: bool,
}

impl Parts {
    fn selected(&self) -> Result<(bool, bool, bool)> {
        let any = self.dock || self.activation || self.switcher;
        if !any && !self.all {
            bail!("choose at least one of --dock, --activation, --switcher, --all");
        }
        Ok((
            self.all || self.dock,
            self.all || self.activation,
            self.all || self.switcher,
        ))
    }
}

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .with_writer(std::io::stderr)
        .init();
    if let Err(err) = run(Cli::parse()) {
        error!("{err:#}");
        std::process::exit(1);
    }
}

fn run(cli: Cli) -> Result<()> {
    match cli.command {
        Cmd::Daemon { burst_ms } => {
            let paths = cosmic_cfg::Paths::from_env()?;
            wayland::run(wayland::Options {
                mode: wayland::Mode::Daemon,
                burst: Duration::from_millis(burst_ms),
                on_tick: Some(Box::new(move || {
                    if let Err(err) = cosmic_cfg::ensure_switcher(&paths) {
                        error!("checking Alt+Tab binding: {err:#}");
                    }
                })),
            })
        }
        Cmd::Watch => wayland::run(wayland::Options {
            mode: wayland::Mode::Watch,
            burst: Duration::ZERO,
            on_tick: None,
        }),
        Cmd::Switch { direction } => ipc::switch(match direction {
            SwitchDir::Next => "next",
            SwitchDir::Prev => "prev",
        }),
        Cmd::Status => {
            println!("{}", ipc::request("status")?);
            Ok(())
        }
        Cmd::Apply(parts) => {
            let (dock, activation, switcher) = parts.selected()?;
            let paths = cosmic_cfg::Paths::from_env()?;
            if switcher {
                // Check everything that could fail before touching any file.
                let exe = std::env::current_exe()?.canonicalize()?;
                let exe = exe.to_str().context("executable path is not UTF-8")?.to_owned();
                if exe.contains("/target/") {
                    tracing::warn!("binding Alt+Tab to a build-directory binary: {exe}");
                }
                if ipc::request("status").is_err() {
                    tracing::warn!(
                        "daemon is not running; Alt+Tab will use the stock switcher until it starts"
                    );
                }
                cosmic_cfg::apply_switcher(&paths, &exe)?;
            }
            if dock {
                cosmic_cfg::apply_key(&paths, &cosmic_cfg::DOCK)?;
            }
            if activation {
                cosmic_cfg::apply_key(&paths, &cosmic_cfg::ACTIVATION)?;
            }
            Ok(())
        }
        Cmd::Restore(parts) => {
            let (dock, activation, switcher) = parts.selected()?;
            let paths = cosmic_cfg::Paths::from_env()?;
            if switcher {
                cosmic_cfg::restore_switcher(&paths)?;
            }
            if dock {
                cosmic_cfg::restore_key(&paths, &cosmic_cfg::DOCK)?;
            }
            if activation {
                cosmic_cfg::restore_key(&paths, &cosmic_cfg::ACTIVATION)?;
            }
            Ok(())
        }
    }
}
