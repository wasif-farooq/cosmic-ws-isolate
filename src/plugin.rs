//! pop-launcher plugin that replaces the stock "COSMIC Windows" (cosmic_toplevel) plugin.
//!
//! cosmic-launcher's Alt+Tab view lists whatever this plugin returns for an empty search,
//! so returning only the current workspace's windows gives a per-workspace switcher with the
//! native UI (hold Alt, Tab to move, release to pick). Typed searches in the launcher still
//! find windows on every workspace, like the stock plugin.
//!
//! The window list comes from the daemon. If the daemon can't be reached, this process
//! replaces itself with the stock plugin so the switcher keeps working unfiltered.

use crate::{ipc, wayland::ListedWindow};
use anyhow::{Context, Result};
use freedesktop_desktop_entry::{self as fde, DesktopEntry, PathSource};
use pop_launcher::{IconSource, PluginResponse, PluginSearchResult, Request};
use std::{
    borrow::Cow,
    io::{self, BufRead, Write},
    os::unix::process::CommandExt,
    process::Command,
};
use tracing::{debug, warn};

/// Name of the symlink pop-launcher runs; `main` dispatches on it because pop-launcher
/// (5b86851) ignores the `args` field of plugin.ron.
pub const ARGV0: &str = "cosmic-ws-isolate-plugin";

/// The stock plugin. pop-launcher picks the plugin from argv[0]'s file name.
const STOCK_PLUGIN: &str = "/usr/lib/pop-launcher/plugins/cosmic_toplevel/cosmic-toplevel";

/// Same name as the stock plugin: pop-launcher loads ~/.local/share/pop-launcher/plugins
/// first and skips later plugins with a name it already has.
pub fn plugin_ron(bin: &str) -> String {
    format!(
        r#"(
    name: "COSMIC Windows",
    description: "Windows on the current workspace (cosmic-ws-isolate)",
    query: (persistent: true, priority: High),
    bin: (path: "{bin}"),
    icon: Name("focus-windows-symbolic"),
    long_lived: true,
)
"#
    )
}

fn send(out: &mut impl Write, response: &PluginResponse) -> Result<()> {
    let mut line = serde_json::to_string(response)?;
    line.push('\n');
    out.write_all(line.as_bytes())?;
    out.flush()?;
    Ok(())
}

fn exec_stock() -> anyhow::Error {
    warn!("daemon unavailable; handing over to the stock window plugin");
    let err = Command::new(STOCK_PLUGIN).exec();
    anyhow::Error::new(err).context("running the stock cosmic-toplevel plugin")
}

fn fetch(query: &str) -> Result<Vec<ListedWindow>> {
    let query = query.replace(['\n', '\r'], " ");
    let answer = ipc::request(&format!("list {query}"))?;
    serde_json::from_str(&answer).with_context(|| format!("daemon replied {answer:?}"))
}

pub fn run() -> Result<()> {
    let mut out = io::stdout().lock();

    if !std::env::var("XDG_CURRENT_DESKTOP").is_ok_and(|v| v.contains("COSMIC")) {
        return send(&mut out, &PluginResponse::Deactivate);
    }
    if ipc::request("status").is_err() {
        return Err(exec_stock());
    }

    let locales = fde::get_languages_from_env();
    let entries: Vec<DesktopEntry> = fde::Iter::new(fde::default_paths())
        .filter_map(|path| DesktopEntry::from_path(path, Some(&locales)).ok())
        .collect();

    for line in io::stdin().lock().lines() {
        let line = line?;
        let request: Request = match serde_json::from_str(&line) {
            Ok(request) => request,
            Err(err) => {
                warn!("malformed request {line:?}: {err}");
                continue;
            }
        };
        match request {
            Request::Search(query) => match fetch(&query) {
                Ok(windows) => {
                    for w in windows {
                        send(&mut out, &PluginResponse::Append(search_result(&w, &entries, &locales)))?;
                    }
                    send(&mut out, &PluginResponse::Finished)?;
                }
                Err(err) => {
                    warn!("listing windows failed: {err:#}");
                    // Answer this search, then let the stock plugin serve the next ones.
                    send(&mut out, &PluginResponse::Finished)?;
                    drop(out);
                    return Err(exec_stock());
                }
            },
            Request::Activate(id) => forward(&format!("activate {id}")),
            Request::Quit(id) => forward(&format!("close {id}")),
            Request::Exit => break,
            _ => {}
        }
    }
    Ok(())
}

fn forward(command: &str) {
    match ipc::request(command) {
        Ok(answer) => debug!("{command} -> {answer}"),
        Err(err) => warn!("{command} failed: {err:#}"),
    }
}

/// Built like the stock plugin's results, so the switcher looks the same: cosmic-launcher
/// shows `description` (the window title) as the main line for window results.
fn search_result(w: &ListedWindow, entries: &[DesktopEntry], locales: &[String]) -> PluginSearchResult {
    let appid = fde::unicase::Ascii::new(w.app_id.as_str());
    let entry = fde::find_app_by_id(entries, appid)
        .cloned()
        .unwrap_or_else(|| DesktopEntry::from_appid(w.app_id.clone()));
    let icon = entry
        .icon()
        .map(|icon| Cow::Owned(icon.to_owned()))
        .unwrap_or(Cow::Borrowed("application-x-executable"));
    PluginSearchResult {
        id: w.id,
        window: Some((0, w.id)),
        description: w.title.clone(),
        name: description(&entry, locales),
        icon: Some(IconSource::Name(icon)),
        ..Default::default()
    }
}

/// Mirrors pop-launcher's `desktop_entries::utils::get_description` (5b86851).
fn description(entry: &DesktopEntry, locales: &[String]) -> String {
    let source = match PathSource::guess_from(&entry.path) {
        PathSource::Local | PathSource::LocalDesktop => "Local".to_owned(),
        PathSource::LocalFlatpak => "Flatpak".to_owned(),
        PathSource::LocalNix => "Nix".to_owned(),
        PathSource::Nix => "Nix (System)".to_owned(),
        PathSource::System => "System".to_owned(),
        PathSource::SystemLocal => "Local (System)".to_owned(),
        PathSource::SystemFlatpak => "Flatpak (System)".to_owned(),
        PathSource::SystemSnap => "Snap (System)".to_owned(),
        PathSource::Other(other) => other,
    };
    match entry.comment(locales) {
        Some(comment) if !comment.is_empty() => format!("{source} - {comment}"),
        _ => source,
    }
}
