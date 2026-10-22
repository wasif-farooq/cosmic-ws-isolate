mod cosmic_cfg;
mod ipc;
mod model;
mod plugin;
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
    /// Focus the next/previous window on the current workspace immediately, without the
    /// switcher popup. For binding to a custom shortcut if you prefer instant switching.
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
    /// Alt+Tab / Super+Tab switcher lists only the current workspace (needs the daemon).
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
    // pop-launcher runs plugins without arguments, so the plugin is selected by argv[0].
    let as_plugin = std::env::args_os()
        .next()
        .and_then(|a| {
            std::path::Path::new(&a)
                .file_name()
                .map(|n| n == plugin::ARGV0)
        })
        .unwrap_or(false);
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();
    if as_plugin {
        if let Err(err) = plugin::run() {
            error!("{err:#}");
            std::process::exit(1);
        }
        return;
    }
    if let Err(err) = run(Cli::parse()) {
        error!("{err:#}");
        std::process::exit(1);
    }
}

fn run(cli: Cli) -> Result<()> {
    match cli.command {
        Cmd::Daemon { burst_ms } => wayland::run(wayland::Options {
            mode: wayland::Mode::Daemon,
            burst: Duration::from_millis(burst_ms),
        }),
        Cmd::Watch => wayland::run(wayland::Options {
            mode: wayland::Mode::Watch,
            burst: Duration::ZERO,
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
                let exe = exe
                    .to_str()
                    .context("executable path is not UTF-8")?
                    .to_owned();
                if exe.contains("/target/") {
                    tracing::warn!("binding Alt+Tab to a build-directory binary: {exe}");
                }
                if ipc::request("status").is_err() {
                    tracing::warn!(
                        "daemon is not running; the switcher lists all windows until it starts"
                    );
                }
                cosmic_cfg::apply_switcher(&paths, &exe)?;
                restart_pop_launcher();
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
                restart_pop_launcher();
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

/// pop-launcher reads plugins once at start. cosmic-launcher starts a fresh one on the next
/// search when it exits (cosmic-launcher 0adec45, `Event::ServiceIsClosed`), so stopping it
/// is enough; the launcher itself and the session keep running.
fn restart_pop_launcher() {
    let mut cmd = std::process::Command::new("pkill");
    cmd.arg("-x");
    if let Ok(user) = std::env::var("USER") {
        cmd.args(["-u", &user]);
    }
    match cmd.arg("pop-launcher").status() {
        Ok(status) if status.success() => {
            tracing::info!("restarted pop-launcher; the next Alt+Tab loads the change")
        }
        Ok(_) => tracing::info!("pop-launcher was not running; it loads the change when it starts"),
        Err(err) => tracing::warn!("could not restart pop-launcher ({err}); log out to apply"),
    }
}
