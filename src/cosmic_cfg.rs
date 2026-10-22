//! Reversible edits to COSMIC's own config files.
//!
//! COSMIC stores one RON value per file at `$XDG_CONFIG_HOME/cosmic/<id>/v<version>/<key>`.
//! Every file we touch is backed up first (or recorded as absent), written atomically,
//! and re-read to make sure it parses. `restore` only reverts values that still hold
//! what we wrote, so later changes made by the user or by cosmic-settings are kept.

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Deserializer};
use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    path::{Path, PathBuf},
};
use tracing::{info, warn};

pub struct Key {
    pub label: &'static str,
    pub id: &'static str,
    pub key: &'static str,
    pub value: &'static str,
}

/// Dock shows only windows on its own monitor's active workspace.
/// Verified in cosmic-applets 8981b0b: cosmic-app-list-config/src/lib.rs:12-21.
pub const DOCK: Key = Key {
    label: "dock",
    id: "com.system76.CosmicAppList",
    key: "filter_top_levels",
    value: "Some(ConfiguredOutput)",
};

/// Apps raising a window on another workspace mark it urgent instead of switching to it.
/// Verified in cosmic-comp 2f7c34f: cosmic-comp-config/src/lib.rs:104, 232-238.
pub const ACTIVATION: Key = Key {
    label: "activation",
    id: "com.system76.CosmicComp",
    key: "activation_policy",
    value: "FocusIfActiveWorkspace",
};

const SHORTCUTS_ID: &str = "com.system76.CosmicSettings.Shortcuts";
const SYSTEM_ACTIONS: &str = "system_actions";
pub const SWITCHER_NEXT: &str = "WindowSwitcher";
pub const SWITCHER_PREV: &str = "WindowSwitcherPrevious";

pub struct Paths {
    pub config_home: PathBuf,
    pub backup_dir: PathBuf,
    /// pop-launcher's user plugin directory (it hard-codes ~/.local/share/pop-launcher/plugins).
    pub launcher_plugins: PathBuf,
}

impl Paths {
    pub fn from_env() -> Result<Self> {
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let xdg = |var: &str, fallback: &str| -> Result<PathBuf> {
            match std::env::var_os(var).filter(|v| !v.is_empty()) {
                Some(dir) => Ok(PathBuf::from(dir)),
                None => Ok(home.clone().context("HOME is not set")?.join(fallback)),
            }
        };
        Ok(Self {
            config_home: xdg("XDG_CONFIG_HOME", ".config")?,
            backup_dir: xdg("XDG_STATE_HOME", ".local/state")?.join("cosmic-ws-isolate/backup"),
            launcher_plugins: home
                .clone()
                .context("HOME is not set")?
                .join(".local/share/pop-launcher/plugins"),
        })
    }

    fn cosmic_file(&self, id: &str, key: &str) -> PathBuf {
        self.config_home.join("cosmic").join(id).join("v1").join(key)
    }

    fn backup(&self, id: &str, key: &str, ext: &str) -> PathBuf {
        self.backup_dir.join(format!("{id}.{key}.{ext}"))
    }

    fn plugin_dir(&self) -> PathBuf {
        self.launcher_plugins.join("cosmic-ws-isolate")
    }
}

fn read_optional(path: &Path) -> Result<Option<String>> {
    match fs::read_to_string(path) {
        Ok(s) => Ok(Some(s)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

/// Write via a temp file in the same directory and rename, so readers never see a partial file.
fn write_atomic(path: &Path, contents: &str) -> Result<()> {
    let dir = path.parent().context("config path has no parent")?;
    fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let tmp = dir.join(format!(
        ".{}.cosmic-ws-isolate.tmp",
        path.file_name().unwrap().to_string_lossy()
    ));
    let mut file = fs::File::create(&tmp).with_context(|| format!("creating {}", tmp.display()))?;
    file.write_all(contents.as_bytes())?;
    file.sync_all()?;
    drop(file);
    fs::rename(&tmp, path).with_context(|| format!("replacing {}", path.display()))
}

/// Records the original state of a file once. Re-applying keeps the first backup.
fn backup_once(paths: &Paths, id: &str, key: &str) -> Result<()> {
    let orig = paths.backup(id, key, "orig");
    let absent = paths.backup(id, key, "absent");
    if orig.exists() || absent.exists() {
        return Ok(());
    }
    fs::create_dir_all(&paths.backup_dir)?;
    match read_optional(&paths.cosmic_file(id, key))? {
        Some(contents) => write_atomic(&orig, &contents),
        None => write_atomic(&absent, ""),
    }
}

enum Backup {
    None,
    Absent,
    Original(String),
}

fn read_backup(paths: &Paths, id: &str, key: &str) -> Result<Backup> {
    if let Some(contents) = read_optional(&paths.backup(id, key, "orig"))? {
        return Ok(Backup::Original(contents));
    }
    if paths.backup(id, key, "absent").exists() {
        return Ok(Backup::Absent);
    }
    Ok(Backup::None)
}

fn clear_backup(paths: &Paths, id: &str, key: &str) -> Result<()> {
    for ext in ["orig", "absent"] {
        let path = paths.backup(id, key, ext);
        if path.exists() {
            fs::remove_file(&path)?;
        }
    }
    Ok(())
}

pub fn apply_key(paths: &Paths, k: &Key) -> Result<()> {
    // Validate our own constant the same way COSMIC will read it.
    ron::from_str::<ron::Value>(k.value).context("internal value is not valid RON")?;
    backup_once(paths, k.id, k.key)?;
    let path = paths.cosmic_file(k.id, k.key);
    write_atomic(&path, k.value)?;
    let back = fs::read_to_string(&path)?;
    if back != k.value {
        bail!("verification failed for {}", path.display());
    }
    info!("{}: set {} = {}", k.label, path.display(), k.value);
    Ok(())
}

pub fn restore_key(paths: &Paths, k: &Key) -> Result<()> {
    let path = paths.cosmic_file(k.id, k.key);
    let backup = read_backup(paths, k.id, k.key)?;
    if matches!(backup, Backup::None) {
        info!("{}: nothing to restore", k.label);
        return Ok(());
    }
    let current = read_optional(&path)?;
    if current.as_deref().map(str::trim) != Some(k.value) {
        warn!(
            "{}: {} was changed after apply ({:?}); leaving it as is",
            k.label,
            path.display(),
            current
        );
    } else {
        match backup {
            Backup::Original(contents) => write_atomic(&path, &contents)?,
            Backup::Absent => fs::remove_file(&path)?,
            Backup::None => unreachable!(),
        }
        info!("{}: restored {}", k.label, path.display());
    }
    clear_backup(paths, k.id, k.key)
}

/// A `system_actions` map key: a bare RON identifier such as `WindowSwitcher`.
/// Kept as a string so entries this program doesn't know about survive a rewrite.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Ident(String);

impl<'de> Deserialize<'de> for Ident {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl serde::de::Visitor<'_> for V {
            type Value = Ident;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("an identifier")
            }
            fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<Ident, E> {
                Ok(Ident(v.to_owned()))
            }
        }
        d.deserialize_identifier(V)
    }
}

type ActionMap = BTreeMap<Ident, String>;

fn parse_actions(text: &str) -> Result<ActionMap> {
    ron::from_str(text).context("parsing system_actions")
}

fn format_actions(map: &ActionMap) -> Result<String> {
    let mut out = String::from("{\n");
    for (Ident(key), cmd) in map {
        if !key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            bail!("unexpected system action name {key:?}");
        }
        out.push_str(&format!("    {key}: {},\n", ron::to_string(cmd)?));
    }
    out.push('}');
    Ok(out)
}

/// Paths end up inside RON strings and shell commands; only plain ones are accepted so
/// neither needs quoting or escapes.
fn check_plain_path(path: &str) -> Result<()> {
    let plain = path.starts_with('/')
        && path
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "/._-+".contains(c));
    if !plain {
        bail!("path {path:?} must be absolute and contain only [A-Za-z0-9/._-+]");
    }
    Ok(())
}

/// Matches Alt+Tab commands written by any earlier version of this program.
fn is_our_command(cmd: &str) -> bool {
    cmd.contains("cosmic-ws-isolate") && cmd.contains(" switch ")
}

fn read_actions(path: &Path) -> Result<ActionMap> {
    match read_optional(path)? {
        Some(text) => parse_actions(&text),
        None => Ok(ActionMap::new()),
    }
}

fn write_actions(path: &Path, map: &ActionMap) -> Result<()> {
    let text = format_actions(map)?;
    // Refuse to write anything we can't read back identically.
    if &parse_actions(&text)? != map {
        bail!("system_actions round-trip mismatch");
    }
    write_atomic(path, &text)
}

/// Removes the Alt+Tab -> `cosmic-ws-isolate switch` binding that earlier versions put in
/// `system_actions`, so Alt+Tab runs the stock cosmic-launcher switcher again.
/// Entries the user or cosmic-settings added are kept; the file is deleted only if it
/// didn't exist before and nothing else is left in it.
fn remove_legacy_binding(paths: &Paths) -> Result<()> {
    let path = paths.cosmic_file(SHORTCUTS_ID, SYSTEM_ACTIONS);
    let backup = read_backup(paths, SHORTCUTS_ID, SYSTEM_ACTIONS)?;
    let exe_file = paths.backup_dir.join("switcher-exe");
    let mut map = read_actions(&path)?;
    let original = match &backup {
        Backup::Original(text) => parse_actions(text)?,
        _ => ActionMap::new(),
    };
    let mut changed = false;
    for name in [SWITCHER_NEXT, SWITCHER_PREV] {
        let key = Ident(name.into());
        if !map.get(&key).is_some_and(|cmd| is_our_command(cmd)) {
            continue;
        }
        changed = true;
        match original.get(&key) {
            Some(orig) => map.insert(key, orig.clone()),
            None => map.remove(&key),
        };
    }
    if changed {
        if map.is_empty() && !matches!(backup, Backup::Original(_)) {
            fs::remove_file(&path)?;
        } else {
            write_actions(&path, &map)?;
        }
        info!("switcher: removed the old Alt+Tab binding from {}", path.display());
    }
    clear_backup(paths, SHORTCUTS_ID, SYSTEM_ACTIONS)?;
    if exe_file.exists() {
        fs::remove_file(exe_file)?;
    }
    Ok(())
}

/// Installs the launcher plugin that makes cosmic-launcher's Alt+Tab view per-workspace.
/// A symlink named [`plugin::ARGV0`] points at `exe`; `main` dispatches on that name.
pub fn apply_switcher(paths: &Paths, exe: &str) -> Result<()> {
    check_plain_path(exe)?;
    let dir = paths.plugin_dir();
    let link = dir.join(crate::plugin::ARGV0);
    let link_str = link.to_str().context("plugin path is not UTF-8")?;
    check_plain_path(link_str)?;
    // Parses system_actions first: if it's unreadable we stop before changing anything.
    remove_legacy_binding(paths)?;

    fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    let tmp_link = dir.join(format!(".{}.tmp", crate::plugin::ARGV0));
    let _ = fs::remove_file(&tmp_link);
    std::os::unix::fs::symlink(exe, &tmp_link)?;
    fs::rename(&tmp_link, &link)?;
    write_atomic(&dir.join("plugin.ron"), &crate::plugin::plugin_ron(link_str))?;
    info!("switcher: installed launcher plugin in {}", dir.display());
    Ok(())
}

pub fn restore_switcher(paths: &Paths) -> Result<()> {
    remove_legacy_binding(paths)?;
    let dir = paths.plugin_dir();
    for name in ["plugin.ron", crate::plugin::ARGV0] {
        let file = dir.join(name);
        if file.symlink_metadata().is_ok() {
            fs::remove_file(&file)?;
        }
    }
    if dir.exists() {
        // Only succeeds if empty, so nothing else is ever deleted.
        if let Err(err) = fs::remove_dir(&dir) {
            warn!("leaving {}: {err}", dir.display());
        }
    }
    info!("switcher: launcher plugin removed; Alt+Tab is the stock switcher again");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(dir: &Path) -> Paths {
        let p = Paths {
            config_home: dir.join("config"),
            backup_dir: dir.join("state/backup"),
            launcher_plugins: dir.join("share/pop-launcher/plugins"),
        };
        fs::create_dir_all(&p.launcher_plugins).unwrap();
        p
    }

    #[test]
    fn parses_system_default_file_format() {
        // Same shape as /usr/share/cosmic/com.system76.CosmicSettings.Shortcuts/v1/system_actions,
        // including doc comments and commented-out entries.
        let text = r#"{
    /// Opens the application library
    AppLibrary: "cosmic-app-library",
    /// Decreases keyboard brightness
    // KeyboardBrightnessDown,
    HomeFolder: "xdg-open ~",
    WindowSwitcher: "cosmic-launcher alt-tab",
}"#;
        let map = parse_actions(text).unwrap();
        assert_eq!(map.len(), 3);
        assert_eq!(map[&Ident("WindowSwitcher".into())], "cosmic-launcher alt-tab");
    }

    #[test]
    fn simple_key_round_trip_from_existing_value() {
        let dir = tempfile::tempdir().unwrap();
        let p = paths(dir.path());
        let file = p.cosmic_file(DOCK.id, DOCK.key);
        write_atomic(&file, "None").unwrap();

        apply_key(&p, &DOCK).unwrap();
        assert_eq!(fs::read_to_string(&file).unwrap(), "Some(ConfiguredOutput)");
        // Re-apply keeps the very first backup.
        apply_key(&p, &DOCK).unwrap();
        restore_key(&p, &DOCK).unwrap();
        assert_eq!(fs::read_to_string(&file).unwrap(), "None");
    }

    #[test]
    fn simple_key_round_trip_from_absent() {
        let dir = tempfile::tempdir().unwrap();
        let p = paths(dir.path());
        let file = p.cosmic_file(ACTIVATION.id, ACTIVATION.key);
        apply_key(&p, &ACTIVATION).unwrap();
        assert!(file.exists());
        restore_key(&p, &ACTIVATION).unwrap();
        assert!(!file.exists());
    }

    #[test]
    fn restore_leaves_user_changes_alone() {
        let dir = tempfile::tempdir().unwrap();
        let p = paths(dir.path());
        let file = p.cosmic_file(ACTIVATION.id, ACTIVATION.key);
        apply_key(&p, &ACTIVATION).unwrap();
        write_atomic(&file, "Urgent").unwrap();
        restore_key(&p, &ACTIVATION).unwrap();
        assert_eq!(fs::read_to_string(&file).unwrap(), "Urgent");
    }

    fn legacy_binding(p: &Paths, file: &Path, other: &str) {
        // What the previous version wrote: backup marker, exe record and the two entries.
        fs::create_dir_all(&p.backup_dir).unwrap();
        write_atomic(&p.backup(SHORTCUTS_ID, SYSTEM_ACTIONS, "absent"), "").unwrap();
        write_atomic(&p.backup_dir.join("switcher-exe"), "/x/cosmic-ws-isolate").unwrap();
        write_atomic(
            file,
            &format!(
                "{{ {other} WindowSwitcher: \"/x/cosmic-ws-isolate switch next\", \
                 WindowSwitcherPrevious: \"'/old/cosmic-ws-isolate' switch prev\" }}"
            ),
        )
        .unwrap();
    }

    #[test]
    fn switcher_installs_plugin_and_removes_legacy_binding() {
        let dir = tempfile::tempdir().unwrap();
        let p = paths(dir.path());
        let file = p.cosmic_file(SHORTCUTS_ID, SYSTEM_ACTIONS);
        legacy_binding(&p, &file, "");

        apply_switcher(&p, "/home/u/.local/bin/cosmic-ws-isolate").unwrap();
        assert!(!file.exists(), "file did not exist before the old apply");
        let ron = fs::read_to_string(p.plugin_dir().join("plugin.ron")).unwrap();
        assert!(ron.contains(r#"name: "COSMIC Windows""#));
        let link = p.plugin_dir().join(crate::plugin::ARGV0);
        assert!(ron.contains(link.to_str().unwrap()));
        assert_eq!(
            fs::read_link(&link).unwrap(),
            PathBuf::from("/home/u/.local/bin/cosmic-ws-isolate")
        );
        // Re-apply is fine.
        apply_switcher(&p, "/home/u/.local/bin/cosmic-ws-isolate").unwrap();

        restore_switcher(&p).unwrap();
        assert!(!p.plugin_dir().exists());
        assert!(p.launcher_plugins.exists(), "parent plugin dir is left alone");
    }

    #[test]
    fn legacy_removal_keeps_other_entries() {
        let dir = tempfile::tempdir().unwrap();
        let p = paths(dir.path());
        let file = p.cosmic_file(SHORTCUTS_ID, SYSTEM_ACTIONS);
        legacy_binding(&p, &file, "Terminal: \"alacritty\",");
        restore_switcher(&p).unwrap();
        let map = parse_actions(&fs::read_to_string(&file).unwrap()).unwrap();
        assert_eq!(map.len(), 1);
        assert_eq!(map[&Ident("Terminal".into())], "alacritty");
        assert!(!p.backup_dir.join("switcher-exe").exists());
    }

    #[test]
    fn legacy_removal_restores_original_values() {
        let dir = tempfile::tempdir().unwrap();
        let p = paths(dir.path());
        let file = p.cosmic_file(SHORTCUTS_ID, SYSTEM_ACTIONS);
        fs::create_dir_all(&p.backup_dir).unwrap();
        write_atomic(
            &p.backup(SHORTCUTS_ID, SYSTEM_ACTIONS, "orig"),
            "{ WindowSwitcher: \"my-switcher\" }",
        )
        .unwrap();
        write_atomic(&file, "{ WindowSwitcher: \"/x/cosmic-ws-isolate switch next\" }").unwrap();
        restore_switcher(&p).unwrap();
        let map = parse_actions(&fs::read_to_string(&file).unwrap()).unwrap();
        assert_eq!(map[&Ident(SWITCHER_NEXT.into())], "my-switcher");
    }

    #[test]
    fn restore_never_deletes_foreign_plugin_files() {
        let dir = tempfile::tempdir().unwrap();
        let p = paths(dir.path());
        apply_switcher(&p, "/x/cosmic-ws-isolate").unwrap();
        write_atomic(&p.plugin_dir().join("notes.txt"), "mine").unwrap();
        restore_switcher(&p).unwrap();
        assert!(p.plugin_dir().join("notes.txt").exists());
    }

    #[test]
    fn switcher_refuses_unusual_paths() {
        let dir = tempfile::tempdir().unwrap();
        let p = paths(dir.path());
        for bad in ["relative/bin", "/home/a b/bin", "/x/it's", "/x/$(rm)", "/x/\"q"] {
            assert!(apply_switcher(&p, bad).is_err(), "{bad}");
        }
        assert!(!p.plugin_dir().exists());
    }

    #[test]
    fn switcher_refuses_unparseable_system_actions() {
        let dir = tempfile::tempdir().unwrap();
        let p = paths(dir.path());
        let file = p.cosmic_file(SHORTCUTS_ID, SYSTEM_ACTIONS);
        write_atomic(&file, "{ oops").unwrap();
        assert!(apply_switcher(&p, "/x/cosmic-ws-isolate").is_err());
        assert_eq!(fs::read_to_string(&file).unwrap(), "{ oops");
        assert!(!p.plugin_dir().exists());
    }
}
