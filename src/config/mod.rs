//! Configuration loading and validation.
//!
//! Precedence: built-in defaults, then `config.toml`, `keymap.toml` and
//! `theme.toml`, then `init.lua`. Parse errors warn and keep the last good
//! value so a broken config never makes rnn unusable.

pub mod keymap;
pub mod lua;
pub mod theme;

use std::{
    env, fs,
    path::{Path, PathBuf},
};

use serde::Deserialize;

use crate::{
    action::{parse_linemode, parse_sort},
    app::{Linemode, Sort},
};

use self::{keymap::Keymap, theme::Theme};

#[derive(Clone, Copy, Debug)]
pub struct MgrConfig {
    pub ratio: (u16, u16),
    pub show_hidden: bool,
    pub show_preview: bool,
    pub sort: Sort,
    pub sort_reverse: bool,
    pub linemode: Linemode,
    /// Keep this many rows visible above and below the cursor when scrolling.
    pub scrolloff: usize,
}

impl Default for MgrConfig {
    fn default() -> Self {
        MgrConfig {
            ratio: (40, 60),
            show_hidden: false,
            show_preview: true,
            sort: Sort::Name,
            sort_reverse: false,
            linemode: Linemode::Size,
            scrolloff: 5,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct PreviewConfig {
    pub cache_size: usize,
}

impl Default for PreviewConfig {
    fn default() -> Self {
        PreviewConfig { cache_size: 16 }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct TasksConfig {
    pub show_progress: bool,
}

impl Default for TasksConfig {
    fn default() -> Self {
        TasksConfig {
            show_progress: true,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct ConfirmConfig {
    pub quit_with_tasks: bool,
}

impl Default for ConfirmConfig {
    fn default() -> Self {
        ConfirmConfig {
            quit_with_tasks: true,
        }
    }
}

#[derive(Default)]
pub struct Config {
    pub mgr: MgrConfig,
    pub preview: PreviewConfig,
    pub tasks: TasksConfig,
    pub confirm: ConfirmConfig,
    /// `[opener]` / `[open]` from `config.toml`; `None` means "use Yazi's".
    pub open: Option<toml::Value>,
    pub keymap: Keymap,
    pub theme: Theme,
    /// Kept alive so `init.lua` actions can run later.
    pub lua: Option<mlua::Lua>,
}

/// Result of loading config: the merged config plus non-fatal problems.
pub struct Loaded {
    pub config: Config,
    pub warnings: Vec<String>,
}

/// Resolve the configuration directory.
pub fn config_dir() -> Option<PathBuf> {
    if let Some(dir) = env::var_os("RNN_CONFIG_HOME") {
        return Some(PathBuf::from(dir));
    }
    if let Some(dir) = env::var_os("XDG_CONFIG_HOME") {
        return Some(PathBuf::from(dir).join("rnn"));
    }
    env::var_os("HOME").map(|home| PathBuf::from(home).join(".config/rnn"))
}

pub fn load() -> Loaded {
    match config_dir() {
        Some(dir) => load_from(&dir),
        None => Loaded {
            config: Config::default(),
            warnings: vec!["cannot locate a config directory (HOME is unset)".into()],
        },
    }
}

pub fn load_from(dir: &Path) -> Loaded {
    let mut warnings = Vec::new();
    let mut config = Config::default();

    let path = dir.join("config.toml");
    if let Some(value) = read_toml(&path, &mut warnings) {
        warn_unknown_config(&value, &mut warnings);
        match value.try_into::<PartialConfig>() {
            Ok(partial) => warnings.extend(config.apply(partial)),
            Err(e) => warnings.push(format!("{}: {e}", path.display())),
        }
    }

    let path = dir.join("keymap.toml");
    if let Some(value) = read_toml(&path, &mut warnings) {
        config.keymap.apply(&value, &mut warnings);
    }

    let path = dir.join("theme.toml");
    if let Some(value) = read_toml(&path, &mut warnings) {
        config.theme.apply(&value, &mut warnings);
    }

    lua::apply(&mut config, dir, &mut warnings);

    let registered = config
        .lua
        .as_ref()
        .map(lua::registered_actions)
        .unwrap_or_default();
    for name in config.keymap.custom_actions() {
        if !registered.iter().any(|r| r == &name) {
            warnings.push(format!("keymap: unknown action '{name}'"));
        }
    }

    Loaded { config, warnings }
}

fn read_toml(path: &Path, warnings: &mut Vec<String>) -> Option<toml::Value> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
        Err(e) => {
            warnings.push(format!("{}: {e}", path.display()));
            return None;
        }
    };
    match toml::from_str(&text) {
        Ok(value) => Some(value),
        Err(e) => {
            warnings.push(format!("{}: {e}", path.display()));
            None
        }
    }
}

impl Config {
    /// Apply a partial config document (used by `init.lua`).
    pub(crate) fn apply_value(&mut self, value: toml::Value, warnings: &mut Vec<String>) {
        warn_unknown_config(&value, warnings);
        match value.try_into::<PartialConfig>() {
            Ok(partial) => warnings.extend(self.apply(partial)),
            Err(e) => warnings.push(format!("config value: {e}")),
        }
    }

    fn apply(&mut self, p: PartialConfig) -> Vec<String> {
        let mut warnings = Vec::new();

        if let Some(ratio) = p.mgr.ratio {
            let pair = match ratio.as_slice() {
                [a, b] => Some((*a, *b)),
                // Yazi uses [parent, current, preview]; rnn only has the last two.
                [_, a, b] => Some((*a, *b)),
                _ => None,
            };
            match pair {
                Some((a, b)) if a > 0 && b > 0 => {
                    self.mgr.ratio = (a, b);
                }
                _ => warnings.push("config.toml: mgr.ratio must be two positive numbers".into()),
            }
        }
        if let Some(v) = p.mgr.show_hidden {
            self.mgr.show_hidden = v;
        }
        if let Some(v) = p.mgr.show_preview {
            self.mgr.show_preview = v;
        }
        if let Some(v) = p.mgr.sort_by {
            match parse_sort(&v) {
                Ok(sort) => self.mgr.sort = sort,
                Err(e) => warnings.push(format!("config.toml: mgr.sort_by: {e}")),
            }
        }
        if let Some(v) = p.mgr.sort_reverse {
            self.mgr.sort_reverse = v;
        }
        if let Some(v) = p.mgr.linemode {
            match parse_linemode(&v) {
                Ok(linemode) => self.mgr.linemode = linemode,
                Err(e) => warnings.push(format!("config.toml: mgr.linemode: {e}")),
            }
        }
        if let Some(v) = p.mgr.scrolloff {
            self.mgr.scrolloff = v;
        }

        if let Some(v) = p.preview.cache_size {
            if v == 0 {
                warnings.push("config.toml: preview.cache_size must be at least 1".into());
            } else {
                self.preview.cache_size = v;
            }
        }

        if let Some(v) = p.tasks.show_progress {
            self.tasks.show_progress = v;
        }

        if let Some(v) = p.confirm.quit_with_tasks {
            self.confirm.quit_with_tasks = v;
        }

        let mut open = toml::value::Table::new();
        if let Some(v) = p.opener {
            open.insert("opener".into(), v);
        }
        if let Some(v) = p.open {
            open.insert("open".into(), v);
        }
        if !open.is_empty() {
            self.open = Some(toml::Value::Table(open));
        }

        warnings
    }
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct PartialConfig {
    mgr: PartialMgr,
    preview: PartialPreview,
    tasks: PartialTasks,
    confirm: PartialConfirm,
    opener: Option<toml::Value>,
    open: Option<toml::Value>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct PartialMgr {
    ratio: Option<Vec<u16>>,
    show_hidden: Option<bool>,
    show_preview: Option<bool>,
    sort_by: Option<String>,
    sort_reverse: Option<bool>,
    linemode: Option<String>,
    scrolloff: Option<usize>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct PartialPreview {
    cache_size: Option<usize>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct PartialTasks {
    show_progress: Option<bool>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct PartialConfirm {
    quit_with_tasks: Option<bool>,
}

fn warn_unknown_config(value: &toml::Value, warnings: &mut Vec<String>) {
    let Some(table) = value.as_table() else {
        warnings.push("config.toml: expected a table".into());
        return;
    };
    for (key, value) in table {
        match key.as_str() {
            "mgr" => warn_keys(
                value,
                "mgr",
                &[
                    "ratio",
                    "show_hidden",
                    "show_preview",
                    "sort_by",
                    "sort_reverse",
                    "linemode",
                    "scrolloff",
                ],
                warnings,
            ),
            "preview" => warn_keys(value, "preview", &["cache_size"], warnings),
            "tasks" => warn_keys(value, "tasks", &["show_progress"], warnings),
            "confirm" => warn_keys(value, "confirm", &["quit_with_tasks"], warnings),
            // openers are validated by openwith.rs.
            "opener" | "open" => {}
            other => warnings.push(format!("config.toml: unknown section '{other}'")),
        }
    }
}

fn warn_keys(value: &toml::Value, section: &str, known: &[&str], warnings: &mut Vec<String>) {
    let Some(table) = value.as_table() else {
        warnings.push(format!("config.toml: [{section}] must be a table"));
        return;
    };
    for key in table.keys() {
        if !known.contains(&key.as_str()) {
            warnings.push(format!("config.toml: [{section}] unknown key '{key}'"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = env::temp_dir().join(format!(
            "rnn-config-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn defaults_load_without_files() {
        let dir = temp_dir("defaults");
        let loaded = load_from(&dir);
        assert!(loaded.warnings.is_empty(), "{:?}", loaded.warnings);
        assert_eq!(loaded.config.mgr.ratio, (40, 60));
        assert!(!loaded.config.mgr.show_hidden);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn config_toml_overrides_and_warns() {
        let dir = temp_dir("settings");
        fs::write(
            dir.join("config.toml"),
            r#"
            [mgr]
            ratio = [30, 70]
            show_hidden = true
            sort_by = "size"
            sort_reverse = true
            linemode = "mtime"
            typo = 1

            [preview]
            cache_size = 4
            "#,
        )
        .unwrap();
        let loaded = load_from(&dir);
        assert_eq!(loaded.config.mgr.ratio, (30, 70));
        assert!(loaded.config.mgr.show_hidden);
        assert_eq!(loaded.config.mgr.sort, Sort::Size);
        assert!(loaded.config.mgr.sort_reverse);
        assert_eq!(loaded.config.mgr.linemode, Linemode::Mtime);
        assert_eq!(loaded.config.preview.cache_size, 4);
        assert!(loaded
            .warnings
            .iter()
            .any(|w| w.contains("unknown key 'typo'")));
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn bad_value_keeps_default() {
        let dir = temp_dir("bad");
        fs::write(
            dir.join("config.toml"),
            r#"
            [mgr]
            ratio = [0, 200]
            sort_by = "nope"
            "#,
        )
        .unwrap();
        let loaded = load_from(&dir);
        assert_eq!(loaded.config.mgr.ratio, (40, 60));
        assert_eq!(loaded.config.mgr.sort, Sort::Name);
        assert_eq!(loaded.warnings.len(), 2, "{:?}", loaded.warnings);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn yazi_style_ratio_uses_last_two() {
        let dir = temp_dir("ratio");
        fs::write(dir.join("config.toml"), "[mgr]\nratio = [1, 4, 3]\n").unwrap();
        let loaded = load_from(&dir);
        assert!(loaded.warnings.is_empty(), "{:?}", loaded.warnings);
        assert_eq!(loaded.config.mgr.ratio, (4, 3));
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn broken_toml_warns_and_falls_back() {
        let dir = temp_dir("broken");
        fs::write(dir.join("config.toml"), "not = [valid").unwrap();
        let loaded = load_from(&dir);
        assert_eq!(loaded.config.mgr.ratio, (40, 60));
        assert_eq!(loaded.warnings.len(), 1);
        let _ = fs::remove_dir_all(dir);
    }
}
