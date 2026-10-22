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
        })
    }

    fn cosmic_file(&self, id: &str, key: &str) -> PathBuf {
        self.config_home.join("cosmic").join(id).join("v1").join(key)
    }

    fn backup(&self, id: &str, key: &str, ext: &str) -> PathBuf {
        self.backup_dir.join(format!("{id}.{key}.{ext}"))
    }

    fn switcher_exe_file(&self) -> PathBuf {
        self.backup_dir.join("switcher-exe")
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

/// The command runs through `/bin/sh -c`. Only plain paths are accepted so the string
/// needs no shell quoting or RON escapes.
fn check_exe_path(exe: &str) -> Result<()> {
    let plain = exe.starts_with('/')
        && exe
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "/._-+".contains(c));
    if !plain {
        bail!("executable path {exe:?} must be absolute and contain only [A-Za-z0-9/._-+]");
    }
    Ok(())
}

/// Matches commands written by any version of this program, wherever it was installed.
fn is_our_command(cmd: &str) -> bool {
    cmd.contains("cosmic-ws-isolate") && cmd.contains(" switch ")
}

fn switcher_commands(exe: &str) -> [(Ident, String); 2] {
    [
        (Ident(SWITCHER_NEXT.into()), format!("{exe} switch next")),
        (Ident(SWITCHER_PREV.into()), format!("{exe} switch prev")),
    ]
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

/// Points Alt+Tab / Super+Tab (and the Shift variants) at `exe switch next|prev`.
/// Only our two keys are written; COSMIC merges the user file over the system map.
pub fn apply_switcher(paths: &Paths, exe: &str) -> Result<()> {
    let path = paths.cosmic_file(SHORTCUTS_ID, SYSTEM_ACTIONS);
    // Parse before taking a backup or writing: if the existing file is unreadable, touch nothing.
    check_exe_path(exe)?;
    let mut map = read_actions(&path)?;
    backup_once(paths, SHORTCUTS_ID, SYSTEM_ACTIONS)?;
    map.extend(switcher_commands(exe));
    write_actions(&path, &map)?;
    write_atomic(&paths.switcher_exe_file(), exe)?;
    info!("switcher: Alt+Tab now runs {exe} switch next|prev");
    Ok(())
}

/// Re-adds our keys if something (e.g. cosmic-settings changing the default terminal) dropped them.
/// Does nothing unless the switcher was applied. Returns true if the file was rewritten.
pub fn ensure_switcher(paths: &Paths) -> Result<bool> {
    let Some(exe) = read_optional(&paths.switcher_exe_file())? else {
        return Ok(false);
    };
    let path = paths.cosmic_file(SHORTCUTS_ID, SYSTEM_ACTIONS);
    let mut map = read_actions(&path)?;
    let wanted = switcher_commands(&exe);
    if wanted.iter().all(|(k, v)| map.get(k) == Some(v)) {
        return Ok(false);
    }
    map.extend(wanted);
    write_actions(&path, &map)?;
    warn!("switcher: system_actions lost our entries; re-applied");
    Ok(true)
}

pub fn restore_switcher(paths: &Paths) -> Result<()> {
    let path = paths.cosmic_file(SHORTCUTS_ID, SYSTEM_ACTIONS);
    let backup = read_backup(paths, SHORTCUTS_ID, SYSTEM_ACTIONS)?;
    let exe = read_optional(&paths.switcher_exe_file())?;
    if matches!(backup, Backup::None) && exe.is_none() {
        info!("switcher: nothing to restore");
        return Ok(());
    }
    let original = match &backup {
        Backup::Original(text) => parse_actions(text)?,
        _ => ActionMap::new(),
    };
    let mut map = read_actions(&path)?;
    for name in [SWITCHER_NEXT, SWITCHER_PREV] {
        let key = Ident(name.into());
        if !map.get(&key).is_some_and(|cmd| is_our_command(cmd)) {
            continue;
        }
        match original.get(&key) {
            Some(orig) => map.insert(key, orig.clone()),
            None => map.remove(&key),
        };
    }
    if map.is_empty() && matches!(backup, Backup::Absent) {
        if path.exists() {
            fs::remove_file(&path)?;
        }
    } else {
        write_actions(&path, &map)?;
    }
    clear_backup(paths, SHORTCUTS_ID, SYSTEM_ACTIONS)?;
    if exe.is_some() {
        fs::remove_file(paths.switcher_exe_file())?;
    }
    info!("switcher: restored {}", path.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(dir: &Path) -> Paths {
        Paths {
            config_home: dir.join("config"),
            backup_dir: dir.join("state/backup"),
        }
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

    #[test]
    fn switcher_round_trip_from_absent() {
        let dir = tempfile::tempdir().unwrap();
        let p = paths(dir.path());
        let file = p.cosmic_file(SHORTCUTS_ID, SYSTEM_ACTIONS);
        apply_switcher(&p, "/home/u/.local/bin/cosmic-ws-isolate").unwrap();
        let map = parse_actions(&fs::read_to_string(&file).unwrap()).unwrap();
        assert_eq!(
            map[&Ident(SWITCHER_NEXT.into())],
            "/home/u/.local/bin/cosmic-ws-isolate switch next"
        );
        assert_eq!(map.len(), 2);
        restore_switcher(&p).unwrap();
        assert!(!file.exists());
    }

    #[test]
    fn switcher_keeps_other_entries_and_reapplies() {
        let dir = tempfile::tempdir().unwrap();
        let p = paths(dir.path());
        let file = p.cosmic_file(SHORTCUTS_ID, SYSTEM_ACTIONS);
        write_atomic(&file, "{ Terminal: \"kitty\" }").unwrap();
        apply_switcher(&p, "/x/bin/cosmic-ws-isolate").unwrap();
        assert!(!ensure_switcher(&p).unwrap());

        // cosmic-settings rewrites the file with only its own entry.
        write_atomic(&file, "{\n    Terminal: \"alacritty\",\n}").unwrap();
        assert!(ensure_switcher(&p).unwrap());
        let map = parse_actions(&fs::read_to_string(&file).unwrap()).unwrap();
        assert_eq!(map.len(), 3);
        assert_eq!(map[&Ident(SWITCHER_PREV.into())], "/x/bin/cosmic-ws-isolate switch prev");

        restore_switcher(&p).unwrap();
        let map = parse_actions(&fs::read_to_string(&file).unwrap()).unwrap();
        // The later Terminal change survives; our keys are gone.
        assert_eq!(map.len(), 1);
        assert_eq!(map[&Ident("Terminal".into())], "alacritty");
        assert!(!ensure_switcher(&p).unwrap());
    }

    #[test]
    fn restore_removes_entries_from_older_versions() {
        let dir = tempfile::tempdir().unwrap();
        let p = paths(dir.path());
        let file = p.cosmic_file(SHORTCUTS_ID, SYSTEM_ACTIONS);
        apply_switcher(&p, "/new/cosmic-ws-isolate").unwrap();
        write_atomic(
            &file,
            "{ WindowSwitcher: \"'/old/cosmic-ws-isolate' switch next\", Terminal: \"kitty\" }",
        )
        .unwrap();
        restore_switcher(&p).unwrap();
        let map = parse_actions(&fs::read_to_string(&file).unwrap()).unwrap();
        assert_eq!(map.len(), 1);
        assert!(map.contains_key(&Ident("Terminal".into())));
    }

    #[test]
    fn switcher_refuses_unusual_paths() {
        let dir = tempfile::tempdir().unwrap();
        let p = paths(dir.path());
        for bad in ["relative/bin", "/home/a b/bin", "/x/it's", "/x/$(rm)"] {
            assert!(apply_switcher(&p, bad).is_err(), "{bad}");
        }
        assert!(!p.backup_dir.exists());
    }

    #[test]
    fn switcher_refuses_unparseable_file() {
        let dir = tempfile::tempdir().unwrap();
        let p = paths(dir.path());
        let file = p.cosmic_file(SHORTCUTS_ID, SYSTEM_ACTIONS);
        write_atomic(&file, "{ oops").unwrap();
        assert!(apply_switcher(&p, "/x").is_err());
        assert_eq!(fs::read_to_string(&file).unwrap(), "{ oops");
        assert!(!p.backup_dir.join("switcher-exe").exists());
    }
}
