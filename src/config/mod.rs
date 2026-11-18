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
    app::{Linemode, Sort, SortFallback},
    preview::kitty::Filter,
};

use self::{keymap::Keymap, theme::Theme};

#[derive(Clone, Copy, Debug)]
pub struct MgrConfig {
    pub ratio: (u16, u16),
    pub show_hidden: bool,
    pub show_preview: bool,
    pub sort: Sort,
    pub sort_reverse: bool,
    /// Compare names case-sensitively (Yazi's `mgr.sort_sensitive`).
    pub sort_sensitive: bool,
    /// Keep directories above files regardless of the sort key (Yazi's
    /// `mgr.sort_dir_first`).
    pub sort_dir_first: bool,
    /// Tie-break when the primary sort keys are equal (Yazi's
    /// `mgr.sort_fallback`).
    pub sort_fallback: SortFallback,
    /// Append `-> target` to symlinks in the file list (Yazi's
    /// `mgr.show_symlink`).
    pub show_symlink: bool,
    pub linemode: Linemode,
    /// Keep this many rows visible above and below the cursor when scrolling.
    pub scrolloff: usize,
    /// Bitmask of accepted mouse events (see `MOUSE_*` constants).
    pub mouse_events: u8,
}

/// Mouse event flags, mirroring Yazi's `mgr.mouse_events` values.
pub const MOUSE_CLICK: u8 = 1 << 0;
pub const MOUSE_SCROLL: u8 = 1 << 1;
pub const MOUSE_TOUCH: u8 = 1 << 2;
pub const MOUSE_MOVE: u8 = 1 << 3;
pub const MOUSE_DRAG: u8 = 1 << 4;

/// Parse `mgr.mouse_events` names into flags.
pub fn parse_mouse_events(events: &[String]) -> Result<u8, String> {
    let mut flags = 0;
    for event in events {
        flags |= match event.as_str() {
            "click" => MOUSE_CLICK,
            "scroll" => MOUSE_SCROLL,
            "touch" => MOUSE_TOUCH,
            "move" => MOUSE_MOVE,
            "drag" => MOUSE_DRAG,
            other => return Err(format!("invalid mouse event '{other}'")),
        };
    }
    Ok(flags)
}

/// The configured mouse event names, in Yazi's order.
pub fn mouse_event_names(flags: u8) -> Vec<&'static str> {
    [
        (MOUSE_CLICK, "click"),
        (MOUSE_SCROLL, "scroll"),
        (MOUSE_TOUCH, "touch"),
        (MOUSE_MOVE, "move"),
        (MOUSE_DRAG, "drag"),
    ]
    .into_iter()
    .filter(|(flag, _)| flags & flag != 0)
    .map(|(_, name)| name)
    .collect()
}

impl Default for MgrConfig {
    fn default() -> Self {
        MgrConfig {
            ratio: (40, 60),
            show_hidden: false,
            show_preview: true,
            sort: Sort::Name,
            sort_reverse: false,
            sort_sensitive: false,
            sort_dir_first: true,
            sort_fallback: SortFallback::Alphabetical,
            show_symlink: true,
            linemode: Linemode::Size,
            scrolloff: 5,
            mouse_events: MOUSE_CLICK | MOUSE_SCROLL | MOUSE_DRAG,
        }
    }
}

#[derive(Clone, Debug)]
pub struct PreviewConfig {
    pub cache_size: usize,
    /// Cap on the decoded preview image, in pixels. Larger values keep more
    /// detail when zooming in but cost more to decode, cache, and transmit.
    pub max_width: u32,
    pub max_height: u32,
    /// Resize filter for downscaling. `Box` is the fast default; the named
    /// filters match Yazi's `preview.image_filter` values.
    pub image_filter: Filter,
    /// JPEG quality used for cached previews (1–100).
    pub image_quality: u8,
    /// Debounce before decoding a newly selected image, in milliseconds.
    pub image_delay: u8,
    /// Spaces a tab expands to in text previews (Yazi's `preview.tab_size`).
    pub tab_size: u8,
    /// Wrap long lines in text previews instead of truncating them (Yazi's
    /// `preview.wrap`).
    pub wrap: bool,
    /// Directory for rnn's own preview cache (Yazi's `preview.cache_dir`).
    /// `None` uses the system temp directory; `RNN_CACHE_DIR` still wins.
    pub cache_dir: Option<PathBuf>,
}

impl Default for PreviewConfig {
    fn default() -> Self {
        PreviewConfig {
            cache_size: 16,
            max_width: 1920,
            max_height: 1080,
            image_filter: Filter::Box,
            image_quality: 90,
            image_delay: 30,
            tab_size: 2,
            wrap: false,
            cache_dir: None,
        }
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

/// Expand a leading `~` (and `~/`) using `HOME`. Returns `None` when the path
/// starts with `~` but no home directory is available.
fn expand_tilde(path: &str) -> Option<PathBuf> {
    if let Some(rest) = path.strip_prefix("~/") {
        return env::var_os("HOME").map(|home| PathBuf::from(home).join(rest));
    }
    if path == "~" {
        return env::var_os("HOME").map(PathBuf::from);
    }
    Some(PathBuf::from(path))
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
        if let Some(v) = p.mgr.sort_sensitive {
            self.mgr.sort_sensitive = v;
        }
        if let Some(v) = p.mgr.sort_dir_first {
            self.mgr.sort_dir_first = v;
        }
        if let Some(v) = &p.mgr.sort_fallback {
            match v.as_str() {
                "alphabetical" | "name" => self.mgr.sort_fallback = SortFallback::Alphabetical,
                "natural" => self.mgr.sort_fallback = SortFallback::Natural,
                _ => warnings.push(format!(
                    "config.toml: mgr.sort_fallback '{v}' is not 'alphabetical' or 'natural'"
                )),
            }
        }
        if let Some(v) = p.mgr.show_symlink {
            self.mgr.show_symlink = v;
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
        if let Some(events) = &p.mgr.mouse_events {
            match parse_mouse_events(events) {
                Ok(flags) => self.mgr.mouse_events = flags,
                Err(e) => warnings.push(format!("config.toml: mgr.mouse_events: {e}")),
            }
        }

        if let Some(v) = p.preview.cache_size {
            if v == 0 {
                warnings.push("config.toml: preview.cache_size must be at least 1".into());
            } else {
                self.preview.cache_size = v;
            }
        }
        if let Some(v) = p.preview.max_width {
            if v == 0 {
                warnings.push("config.toml: preview.max_width must be at least 1".into());
            } else {
                self.preview.max_width = v;
            }
        }
        if let Some(v) = p.preview.max_height {
            if v == 0 {
                warnings.push("config.toml: preview.max_height must be at least 1".into());
            } else {
                self.preview.max_height = v;
            }
        }
        if let Some(v) = &p.preview.image_filter {
            match Filter::from_name(v) {
                Some(filter) => self.preview.image_filter = filter,
                None => warnings.push(format!(
                    "config.toml: preview.image_filter '{v}' is not one of \
                     box, nearest, triangle, catmull-rom, gaussian, lanczos3"
                )),
            }
        }
        if let Some(v) = p.preview.image_quality {
            if !(1..=100).contains(&v) {
                warnings.push("config.toml: preview.image_quality must be 1-100".into());
            } else {
                self.preview.image_quality = v as u8;
            }
        }
        if let Some(v) = p.preview.image_delay {
            if v > u32::from(u8::MAX) {
                warnings.push("config.toml: preview.image_delay must be 0-255".into());
            } else {
                self.preview.image_delay = v as u8;
            }
        }
        if let Some(v) = p.preview.tab_size {
            if !(1..=16).contains(&v) {
                warnings.push("config.toml: preview.tab_size must be 1-16".into());
            } else {
                self.preview.tab_size = v as u8;
            }
        }
        if let Some(v) = p.preview.wrap {
            match v {
                WrapValue::Bool(b) => self.preview.wrap = b,
                WrapValue::Str(s) => match s.to_ascii_lowercase().as_str() {
                    "yes" | "true" | "on" => self.preview.wrap = true,
                    "no" | "false" | "off" => self.preview.wrap = false,
                    _ => warnings.push(format!(
                        "config.toml: preview.wrap '{s}' is not 'no' or 'yes'"
                    )),
                },
            }
        }
        if let Some(v) = &p.preview.cache_dir {
            if v.is_empty() {
                self.preview.cache_dir = None;
            } else {
                match expand_tilde(v) {
                    Some(path) if path.is_absolute() => self.preview.cache_dir = Some(path),
                    Some(_) => {
                        warnings.push("config.toml: preview.cache_dir must be absolute".into())
                    }
                    None => {
                        warnings.push("config.toml: preview.cache_dir cannot resolve '~'".into())
                    }
                }
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
    sort_sensitive: Option<bool>,
    sort_dir_first: Option<bool>,
    sort_fallback: Option<String>,
    show_symlink: Option<bool>,
    linemode: Option<String>,
    scrolloff: Option<usize>,
    mouse_events: Option<Vec<String>>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct PartialPreview {
    cache_size: Option<usize>,
    max_width: Option<u32>,
    max_height: Option<u32>,
    image_filter: Option<String>,
    image_quality: Option<u32>,
    image_delay: Option<u32>,
    tab_size: Option<u32>,
    wrap: Option<WrapValue>,
    cache_dir: Option<String>,
}

/// `preview.wrap` accepts Yazi's `"no"`/`"yes"` strings and a plain boolean.
#[derive(Deserialize, Clone)]
#[serde(untagged)]
enum WrapValue {
    Bool(bool),
    Str(String),
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
                    "sort_sensitive",
                    "sort_dir_first",
                    "sort_fallback",
                    "show_symlink",
                    "linemode",
                    "scrolloff",
                    "mouse_events",
                ],
                warnings,
            ),
            "preview" => warn_keys(
                value,
                "preview",
                &[
                    "cache_size",
                    "max_width",
                    "max_height",
                    "image_filter",
                    "image_quality",
                    "image_delay",
                    "tab_size",
                    "wrap",
                    "cache_dir",
                ],
                warnings,
            ),
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
            sort_sensitive = true
            sort_dir_first = false
            sort_fallback = "natural"
            show_symlink = false
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
        assert!(loaded.config.mgr.sort_sensitive);
        assert!(!loaded.config.mgr.sort_dir_first);
        assert_eq!(loaded.config.mgr.sort_fallback, SortFallback::Natural);
        assert!(!loaded.config.mgr.show_symlink);
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
    fn preview_wrap_accepts_yazi_strings_and_bools() {
        let dir = temp_dir("wrap");
        let write = |body: &str| {
            fs::write(dir.join("config.toml"), body).unwrap();
            load_from(&dir)
        };

        let loaded = write("[preview]\nwrap = \"yes\"\n");
        assert!(loaded.warnings.is_empty(), "{:?}", loaded.warnings);
        assert!(loaded.config.preview.wrap);

        let loaded = write("[preview]\nwrap = true\n");
        assert!(loaded.warnings.is_empty(), "{:?}", loaded.warnings);
        assert!(loaded.config.preview.wrap);

        let loaded = write("[preview]\nwrap = \"no\"\n");
        assert!(loaded.warnings.is_empty(), "{:?}", loaded.warnings);
        assert!(!loaded.config.preview.wrap);

        let loaded = write("[preview]\nwrap = \"sometimes\"\n");
        assert!(!loaded.config.preview.wrap);
        assert!(loaded
            .warnings
            .iter()
            .any(|w| w.contains("preview.wrap 'sometimes'")));

        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn preview_cache_dir_accepts_absolute_and_empty() {
        let dir = temp_dir("cache-dir");
        let cache = dir.join("cache");
        let write = |body: &str| {
            fs::write(dir.join("config.toml"), body).unwrap();
            load_from(&dir)
        };

        let loaded = write(&format!("[preview]\ncache_dir = \"{}\"\n", cache.display()));
        assert!(loaded.warnings.is_empty(), "{:?}", loaded.warnings);
        assert_eq!(
            loaded.config.preview.cache_dir.as_deref(),
            Some(cache.as_path())
        );

        // Empty means "use the default temp dir".
        let loaded = write("[preview]\ncache_dir = \"\"\n");
        assert!(loaded.warnings.is_empty(), "{:?}", loaded.warnings);
        assert!(loaded.config.preview.cache_dir.is_none());

        // Relative paths are rejected with a warning.
        let loaded = write("[preview]\ncache_dir = \"relative/cache\"\n");
        assert!(loaded.config.preview.cache_dir.is_none());
        assert!(loaded
            .warnings
            .iter()
            .any(|w| w.contains("preview.cache_dir must be absolute")));

        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn mouse_events_parse_and_round_trip() {
        assert_eq!(
            parse_mouse_events(&["click".into(), "scroll".into(), "drag".into()]).unwrap(),
            MOUSE_CLICK | MOUSE_SCROLL | MOUSE_DRAG
        );
        assert_eq!(parse_mouse_events(&[]).unwrap(), 0);
        assert_eq!(
            parse_mouse_events(&["touch".into(), "move".into()]).unwrap(),
            MOUSE_TOUCH | MOUSE_MOVE
        );
        assert!(parse_mouse_events(&["nope".into()]).is_err());
        assert_eq!(
            mouse_event_names(MOUSE_CLICK | MOUSE_SCROLL | MOUSE_DRAG),
            vec!["click", "scroll", "drag"]
        );
        assert!(mouse_event_names(0).is_empty());
    }

    #[test]
    fn config_toml_mouse_events() {
        let dir = temp_dir("mouse");
        fs::write(
            dir.join("config.toml"),
            "[mgr]\nmouse_events = [\"click\", \"move\"]\n",
        )
        .unwrap();
        let loaded = load_from(&dir);
        assert!(loaded.warnings.is_empty(), "{:?}", loaded.warnings);
        assert_eq!(loaded.config.mgr.mouse_events, MOUSE_CLICK | MOUSE_MOVE);

        fs::write(dir.join("config.toml"), "[mgr]\nmouse_events = []\n").unwrap();
        let loaded = load_from(&dir);
        assert!(loaded.warnings.is_empty(), "{:?}", loaded.warnings);
        assert_eq!(loaded.config.mgr.mouse_events, 0);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn bad_mouse_event_warns_and_keeps_default() {
        let dir = temp_dir("bad-mouse");
        fs::write(
            dir.join("config.toml"),
            "[mgr]\nmouse_events = [\"click\", \"wheel\"]\n",
        )
        .unwrap();
        let loaded = load_from(&dir);
        assert_eq!(
            loaded.config.mgr.mouse_events,
            MgrConfig::default().mouse_events
        );
        assert!(loaded
            .warnings
            .iter()
            .any(|w| w.contains("mouse_events") && w.contains("wheel")));
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn config_toml_preview_settings() {
        let dir = temp_dir("preview");
        fs::write(
            dir.join("config.toml"),
            r#"
            [preview]
            cache_size = 4
            max_width = 1280
            max_height = 720
            image_filter = "lanczos3"
            image_quality = 75
            image_delay = 50
            "#,
        )
        .unwrap();
        let loaded = load_from(&dir);
        assert!(loaded.warnings.is_empty(), "{:?}", loaded.warnings);
        let p = loaded.config.preview;
        assert_eq!(p.cache_size, 4);
        assert_eq!((p.max_width, p.max_height), (1280, 720));
        assert_eq!(p.image_filter, Filter::Lanczos3);
        assert_eq!(p.image_quality, 75);
        assert_eq!(p.image_delay, 50);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn bad_preview_settings_warn_and_keep_defaults() {
        let dir = temp_dir("bad-preview");
        fs::write(
            dir.join("config.toml"),
            r#"
            [preview]
            max_width = 0
            max_height = 0
            image_filter = "mitchell"
            image_quality = 0
            image_delay = 999
            "#,
        )
        .unwrap();
        let loaded = load_from(&dir);
        let p = loaded.config.preview;
        let d = PreviewConfig::default();
        assert_eq!((p.max_width, p.max_height), (d.max_width, d.max_height));
        assert_eq!(p.image_filter, d.image_filter);
        assert_eq!(p.image_quality, d.image_quality);
        assert_eq!(p.image_delay, d.image_delay);
        for key in [
            "max_width",
            "max_height",
            "image_filter",
            "image_quality",
            "image_delay",
        ] {
            assert!(
                loaded.warnings.iter().any(|w| w.contains(key)),
                "missing warning for {key}: {:?}",
                loaded.warnings
            );
        }
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
