use std::{
    collections::BTreeSet,
    env, fs,
    io::{self, Write},
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, SystemTime},
};

use ratatui::{
    crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers},
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Clear, List, ListItem, ListState, Paragraph},
    DefaultTerminal, Frame,
};

use crate::{
    preview::{
        detect::{self, Kind},
        Key as PreviewKey, Preview,
    },
    util::human,
};

// ───────────────────────── data ─────────────────────────

#[derive(Clone)]
struct Entry {
    name: String,
    path: PathBuf,
    is_dir: bool,
    size: u64,
    modified: Option<SystemTime>,
    created: Option<SystemTime>,
    mode: u32,
    uid: u32,
    gid: u32,
}

#[derive(Clone, Copy)]
enum Sort {
    Name,
    Natural,
    Size,
    Time,
    Btime,
    Extension,
    Random,
}

#[derive(Clone, Copy, PartialEq)]
enum Prompt {
    Create,
    Rename,
    Delete,
    Cd,
    SearchFd,
    SearchRg,
    Shell,
}

#[derive(Clone, Copy)]
enum Mode {
    Normal,
    Filter,
    Find,
    Prompt(Prompt),
    Help,
}

#[derive(Clone, Copy)]
enum Prefix {
    Goto,
    Sort,
    Copy,
    Linemode,
}

#[derive(Clone, Copy, PartialEq)]
enum Linemode {
    Size,
    Permissions,
    Btime,
    Mtime,
    Owner,
    None,
}

#[derive(Clone, Copy, PartialEq)]
enum SearchKind {
    Fd,
    Rg,
}

#[derive(Clone)]
struct Search {
    kind: SearchKind,
    query: String,
    root: PathBuf,
}

#[derive(Clone, Copy)]
enum CopyKind {
    Path,
    Url,
    DirPath,
    DirUrl,
    FileName,
    NameNoExt,
}

#[derive(Clone, Copy)]
enum VisualKind {
    Select,
    Unselect,
}

struct Visual {
    anchor: usize,
    kind: VisualKind,
    before: BTreeSet<PathBuf>,
}

#[derive(Clone)]
struct Completion {
    display: String,
    path: PathBuf,
}

pub struct App {
    cwd: PathBuf,
    entries: Vec<Entry>,
    state: ListState,
    filter: String,
    show_hidden: bool,
    show_preview: bool,
    sort: Sort,
    sort_reverse: bool,
    selected: BTreeSet<PathBuf>,
    yank: Option<(Vec<PathBuf>, bool)>, // (paths, is_cut)
    mode: Mode,
    prefix: Option<Prefix>,
    visual: Option<Visual>,
    completions: Vec<Completion>,
    completion_index: usize,
    search: Option<Search>,
    find_query: String,
    find_forward: bool,
    find_origin: Option<usize>,
    last_find: Option<String>,
    linemode: Linemode,
    history: Vec<PathBuf>,
    history_pos: usize,
    input: String,
    status: String,
    focus: Option<String>,
    page: usize,
    preview: Preview,
    list: List<'static>,
    list_dirty: bool,
    list_width: u16,
}

// ───────────────────────── helpers ─────────────────────────

fn remove(p: &Path) -> io::Result<()> {
    if fs::symlink_metadata(p)?.is_dir() {
        fs::remove_dir_all(p)
    } else {
        fs::remove_file(p)
    }
}

fn copy_rec(src: &Path, dst: &Path) -> io::Result<()> {
    if fs::symlink_metadata(src)?.is_dir() {
        if dst.starts_with(src) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "cannot copy a directory into itself",
            ));
        }
        fs::create_dir_all(dst)?;
        for e in fs::read_dir(src)? {
            let e = e?;
            copy_rec(&e.path(), &dst.join(e.file_name()))?;
        }
        Ok(())
    } else {
        fs::copy(src, dst).map(|_| ())
    }
}

fn unique(mut p: PathBuf) -> PathBuf {
    while p.exists() {
        let mut s = p.into_os_string();
        s.push(".copy");
        p = PathBuf::from(s);
    }
    p
}

fn extension(name: &str) -> String {
    Path::new(name)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase()
}

/// Smart-case substring match: lowercase needles ignore case, mixed/upper
/// needles are case-sensitive (mirrors Yazi's `--smart` matching).
fn smart_contains(haystack: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return true;
    }
    if needle.chars().any(|c| c.is_ascii_uppercase()) {
        haystack.contains(needle)
    } else {
        haystack.to_lowercase().contains(&needle.to_lowercase())
    }
}

fn entry_for(path: PathBuf, name: String) -> Option<Entry> {
    let md = fs::metadata(&path)
        .or_else(|_| fs::symlink_metadata(&path))
        .ok()?;
    Some(Entry {
        name,
        path,
        is_dir: md.is_dir(),
        size: md.len(),
        modified: md.modified().ok(),
        created: md.created().ok(),
        mode: md.mode(),
        uid: md.uid(),
        gid: md.gid(),
    })
}

fn natural_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    let mut ai = a.chars().peekable();
    let mut bi = b.chars().peekable();
    loop {
        match (ai.peek().copied(), bi.peek().copied()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(x), Some(y)) => {
                if x.is_ascii_digit() && y.is_ascii_digit() {
                    let mut an = String::new();
                    while let Some(c) = ai.peek().copied().filter(|c| c.is_ascii_digit()) {
                        an.push(c);
                        ai.next();
                    }
                    let mut bn = String::new();
                    while let Some(c) = bi.peek().copied().filter(|c| c.is_ascii_digit()) {
                        bn.push(c);
                        bi.next();
                    }
                    let an = an.trim_start_matches('0');
                    let bn = bn.trim_start_matches('0');
                    let ord = an.len().cmp(&bn.len()).then_with(|| an.cmp(bn));
                    if ord != Ordering::Equal {
                        return ord;
                    }
                } else {
                    let ord = x.to_ascii_lowercase().cmp(&y.to_ascii_lowercase());
                    if ord != Ordering::Equal {
                        return ord;
                    }
                    ai.next();
                    bi.next();
                }
            }
        }
    }
}

fn permissions(mode: u32) -> String {
    let bits = mode & 0o777;
    let mut out = String::with_capacity(9);
    for shift in [6, 3, 0] {
        let b = (bits >> shift) & 0o7;
        out.push(if b & 0o4 != 0 { 'r' } else { '-' });
        out.push(if b & 0o2 != 0 { 'w' } else { '-' });
        out.push(if b & 0o1 != 0 { 'x' } else { '-' });
    }
    out
}

fn format_age(t: Option<SystemTime>) -> String {
    let Some(t) = t else {
        return "-".into();
    };
    let secs = match SystemTime::now().duration_since(t) {
        Ok(d) => d.as_secs(),
        Err(_) => return "future".into(),
    };
    const MIN: u64 = 60;
    const HOUR: u64 = 60 * MIN;
    const DAY: u64 = 24 * HOUR;
    const WEEK: u64 = 7 * DAY;
    const MONTH: u64 = 30 * DAY;
    const YEAR: u64 = 365 * DAY;
    match secs {
        0..=59 => "now".into(),
        60..=3599 => format!("{}m", secs / MIN),
        3600..=86399 => format!("{}h", secs / HOUR),
        86400..=604799 => format!("{}d", secs / DAY),
        604800..=2591999 => format!("{}w", secs / WEEK),
        2592000..=31535999 => format!("{}mo", secs / MONTH),
        _ => format!("{}y", secs / YEAR),
    }
}

/// Relative path from `base` to `target` (both absolute), e.g. `../Pictures/x`.
fn relative_path(base: &Path, target: &Path) -> PathBuf {
    let base: Vec<_> = base.components().collect();
    let target: Vec<_> = target.components().collect();
    let mut i = 0;
    while i < base.len() && i < target.len() && base[i] == target[i] {
        i += 1;
    }
    let mut out = PathBuf::new();
    for _ in i..base.len() {
        out.push("..");
    }
    for c in &target[i..] {
        out.push(c.as_os_str());
    }
    if out.as_os_str().is_empty() {
        out.push(".");
    }
    out
}

/// Encode a path as a `file://` URI suitable for `text/uri-list` clipboards.
fn file_uri(path: &Path) -> String {
    use std::os::unix::ffi::OsStrExt;
    let mut out = String::from("file://");
    for &b in path.as_os_str().as_bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'/' | b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Decode a `file://` URI back into a path (percent-decoding included).
fn uri_path(uri: &str) -> Option<PathBuf> {
    use std::os::unix::ffi::OsStringExt;
    let rest = uri.strip_prefix("file://")?;
    let rest = if rest.starts_with('/') {
        rest
    } else {
        &rest[rest.find('/')?..]
    };
    let bytes = rest.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok();
            if let Some(v) = hex.and_then(|s| u8::from_str_radix(s, 16).ok()) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    Some(PathBuf::from(std::ffi::OsString::from_vec(out)))
}

// ───────────────────────── app ─────────────────────────

impl App {
    pub fn new() -> io::Result<Self> {
        let start = env::args()
            .nth(1)
            .map(PathBuf::from)
            .unwrap_or(env::current_dir()?);
        let cwd = fs::canonicalize(start)?;
        let mut app = App {
            cwd: cwd.clone(),
            entries: vec![],
            state: ListState::default(),
            filter: String::new(),
            show_hidden: false,
            show_preview: true,
            sort: Sort::Name,
            sort_reverse: false,
            selected: BTreeSet::new(),
            yank: None,
            mode: Mode::Normal,
            prefix: None,
            visual: None,
            completions: Vec::new(),
            completion_index: 0,
            search: None,
            find_query: String::new(),
            find_forward: true,
            find_origin: None,
            last_find: None,
            linemode: Linemode::Size,
            history: vec![cwd],
            history_pos: 0,
            input: String::new(),
            status: String::new(),
            focus: None,
            page: 10,
            preview: Preview::new(),
            list: List::new(Vec::<ListItem>::new()),
            list_dirty: true,
            list_width: 0,
        };
        app.reload();
        Ok(app)
    }

    /// Enable kitty image previews (call before `run`).
    pub fn preview_configure(&mut self, kitty_supported: bool, font: (u16, u16), shm: bool) {
        self.preview.configure(kitty_supported, font, shm);
    }

    fn current(&self) -> Option<&Entry> {
        self.state.selected().and_then(|i| self.entries.get(i))
    }

    /// Preload up to two image neighbours on each side (into the preview's
    /// disk + memory caches) so navigating to them is instant, like yazi's
    /// image previewer does.
    fn preload_neighbours(&mut self) {
        if !self.preview.kitty_enabled() {
            return;
        }
        let Some(i) = self.state.selected() else {
            return;
        };
        let n = self.entries.len();
        for j in [i.saturating_sub(2), i.saturating_sub(1), i + 1, i + 2] {
            if j >= n || j == i {
                continue;
            }
            let Some(e) = self.entries.get(j) else {
                continue;
            };
            if e.is_dir || detect::detect(&e.path) != Kind::Image {
                continue;
            }
            self.preview.preload((e.path.clone(), e.modified, e.size));
        }
    }

    fn reload(&mut self) {
        self.visual = None;
        let keep = self
            .focus
            .take()
            .or_else(|| self.current().map(|e| e.name.clone()));

        let mut v: Vec<Entry> = Vec::new();
        if let Some(search) = self.search.clone() {
            v = self.search_entries(&search);
        } else {
            match fs::read_dir(&self.cwd) {
                Ok(rd) => {
                    for e in rd.flatten() {
                        let name = e.file_name().to_string_lossy().into_owned();
                        if !self.show_hidden && name.starts_with('.') {
                            continue;
                        }
                        if let Some(entry) = entry_for(e.path(), name) {
                            v.push(entry);
                        }
                    }
                }
                Err(e) => self.status = format!("{}: {e}", self.cwd.display()),
            }
        }

        if !self.filter.is_empty() {
            let needle = self.filter.clone();
            v.retain(|e| smart_contains(&e.name, &needle));
        }

        let sort = self.sort;
        let reverse = self.sort_reverse;
        if matches!(sort, Sort::Random) {
            // Fisher–Yates with a time-seeded xorshift; no extra dependency.
            let mut seed = SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .map(|d| d.as_nanos() as u64)
                .unwrap_or(0x9e37_79b9_7f4a_7c15)
                | 1;
            for i in (1..v.len()).rev() {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                let j = (seed % (i as u64 + 1)) as usize;
                v.swap(i, j);
            }
        } else {
            v.sort_by(|a, b| {
                b.is_dir.cmp(&a.is_dir).then_with(|| {
                    let order = match sort {
                        Sort::Name => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
                        Sort::Natural => natural_cmp(&a.name, &b.name),
                        Sort::Size => a.size.cmp(&b.size),
                        Sort::Time => a.modified.cmp(&b.modified),
                        Sort::Btime => a.created.cmp(&b.created),
                        Sort::Extension => extension(&a.name)
                            .cmp(&extension(&b.name))
                            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())),
                        Sort::Random => std::cmp::Ordering::Equal,
                    };
                    if reverse {
                        order.reverse()
                    } else {
                        order
                    }
                })
            });
        }
        self.entries = v;

        if self.entries.is_empty() {
            self.state.select(None);
        } else {
            let idx = keep
                .and_then(|k| self.entries.iter().position(|e| e.name == k))
                .unwrap_or(0);
            self.state.select(Some(idx));
        }
        self.list_dirty = true;
    }

    /// Run `fd` or `rg` in the search root and turn the output into entries.
    fn search_entries(&mut self, search: &Search) -> Vec<Entry> {
        let prog = match search.kind {
            SearchKind::Fd => "fd",
            SearchKind::Rg => "rg",
        };
        let mut cmd = Command::new(prog);
        cmd.current_dir(&search.root)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        match search.kind {
            SearchKind::Fd => {
                cmd.args(["--color", "never"]);
                cmd.arg(if self.show_hidden {
                    "--hidden"
                } else {
                    "--no-hidden"
                });
                cmd.arg("--").arg(&search.query);
            }
            SearchKind::Rg => {
                cmd.args(["--color=never", "--files-with-matches", "--smart-case"]);
                cmd.arg(if self.show_hidden {
                    "--hidden"
                } else {
                    "--no-hidden"
                });
                cmd.arg("--").arg(&search.query);
            }
        }
        let out = match cmd.output() {
            Ok(o) => o,
            Err(e) => {
                self.status = format!("{prog}: {e}");
                return Vec::new();
            }
        };
        if !out.status.success() && out.status.code() != Some(1) {
            let err = String::from_utf8_lossy(&out.stderr);
            self.status = format!("{prog}: {}", err.lines().next().unwrap_or("failed"));
            return Vec::new();
        }
        let mut v = Vec::new();
        for line in String::from_utf8_lossy(&out.stdout).lines().take(5000) {
            let line = line.trim().trim_start_matches("./");
            if line.is_empty() {
                continue;
            }
            let path = search.root.join(line);
            if let Some(entry) = entry_for(path, line.to_string()) {
                v.push(entry);
            }
        }
        v
    }

    fn cancel_search(&mut self) {
        if self.search.take().is_some() {
            self.reload();
        }
    }

    fn cd(&mut self, path: PathBuf, focus: Option<String>) {
        if !self.cd_inner(path, focus) {
            return;
        }
        self.history.truncate(self.history_pos + 1);
        if self.history.last() != Some(&self.cwd) {
            self.history.push(self.cwd.clone());
        }
        self.history_pos = self.history.len().saturating_sub(1);
    }

    /// Change directory without touching history (used by `H`/`L`).
    fn cd_inner(&mut self, path: PathBuf, focus: Option<String>) -> bool {
        if let Err(e) = fs::read_dir(&path) {
            self.status = format!("{}: {e}", path.display());
            return false;
        }
        self.cwd = path;
        self.filter.clear();
        self.search = None;
        self.entries.clear();
        self.state.select(None);
        self.prefix = None;
        self.visual = None;
        self.focus = focus;
        self.reload();
        true
    }

    fn history_back(&mut self) {
        if self.history_pos == 0 {
            return;
        }
        let old = self.history_pos;
        self.history_pos -= 1;
        let path = self.history[self.history_pos].clone();
        if !self.cd_inner(path, None) {
            self.history_pos = old;
        }
    }

    fn history_forward(&mut self) {
        if self.history_pos + 1 >= self.history.len() {
            return;
        }
        let old = self.history_pos;
        self.history_pos += 1;
        let path = self.history[self.history_pos].clone();
        if !self.cd_inner(path, None) {
            self.history_pos = old;
        }
    }

    fn go_parent(&mut self) {
        if self.search.is_some() {
            self.cancel_search();
            return;
        }
        if let Some(p) = self.cwd.parent().map(Path::to_path_buf) {
            let name = self
                .cwd
                .file_name()
                .map(|s| s.to_string_lossy().into_owned());
            self.cd(p, name);
        }
    }

    fn move_by(&mut self, d: isize) {
        if self.entries.is_empty() {
            return;
        }
        let i = self.state.selected().unwrap_or(0) as isize;
        let n = (i + d).clamp(0, self.entries.len() as isize - 1);
        self.state.select(Some(n as usize));
        self.apply_visual();
    }

    fn select_all(&mut self) {
        self.visual = None;
        self.selected = self.entries.iter().map(|e| e.path.clone()).collect();
        self.list_dirty = true;
    }

    fn invert_selection(&mut self) {
        self.visual = None;
        self.selected = self
            .entries
            .iter()
            .filter(|e| !self.selected.contains(&e.path))
            .map(|e| e.path.clone())
            .collect();
        self.list_dirty = true;
    }

    fn enter_visual(&mut self, kind: VisualKind) {
        let Some(anchor) = self.state.selected() else {
            return;
        };
        self.visual = Some(Visual {
            anchor,
            kind,
            before: self.selected.clone(),
        });
        self.apply_visual();
    }

    fn apply_visual(&mut self) {
        let Some(visual) = self.visual.as_ref() else {
            return;
        };
        let Some(cur) = self.state.selected() else {
            return;
        };
        let (anchor, kind) = (visual.anchor, visual.kind);
        let mut selected = visual.before.clone();
        let (lo, hi) = if anchor <= cur {
            (anchor, cur)
        } else {
            (cur, anchor)
        };
        for i in lo..=hi {
            if let Some(e) = self.entries.get(i) {
                match kind {
                    VisualKind::Select => {
                        selected.insert(e.path.clone());
                    }
                    VisualKind::Unselect => {
                        selected.remove(&e.path);
                    }
                }
            }
        }
        if selected != self.selected {
            self.selected = selected;
            self.list_dirty = true;
        }
    }

    fn targets(&self) -> Vec<PathBuf> {
        if !self.selected.is_empty() {
            self.selected.iter().cloned().collect()
        } else {
            self.current()
                .map(|e| vec![e.path.clone()])
                .unwrap_or_default()
        }
    }

    /// Resolve a user-typed path, expanding `~` and relative paths.
    fn expand_path(&self, raw: &str) -> PathBuf {
        if raw.is_empty() {
            return self.cwd.clone();
        }
        if raw == "~" {
            if let Some(h) = env::var_os("HOME") {
                return PathBuf::from(h);
            }
        } else if let Some(rest) = raw.strip_prefix("~/") {
            if let Some(h) = env::var_os("HOME") {
                return PathBuf::from(h).join(rest);
            }
        }
        let p = PathBuf::from(raw);
        if p.is_absolute() {
            p
        } else {
            self.cwd.join(p)
        }
    }

    /// Split the current `go to` input into `(directory to list, display
    /// prefix, name prefix)`.
    fn completion_context(&self) -> Option<(PathBuf, String, String)> {
        let raw = self.input.as_str();
        if raw.is_empty() {
            return Some((self.cwd.clone(), String::new(), String::new()));
        }
        if raw == "~" {
            let home = env::var_os("HOME").map(PathBuf::from)?;
            return Some((home, "~/".into(), String::new()));
        }
        if raw == "." {
            return Some((self.cwd.clone(), "./".into(), String::new()));
        }
        if raw == ".." {
            let p = self
                .cwd
                .parent()
                .unwrap_or(self.cwd.as_path())
                .to_path_buf();
            return Some((p, "../".into(), String::new()));
        }
        let (dir_part, prefix) = match raw.rfind('/') {
            Some(i) => (&raw[..=i], &raw[i + 1..]),
            None => ("", raw),
        };
        let base = if dir_part.is_empty() {
            self.cwd.clone()
        } else {
            self.expand_path(dir_part)
        };
        Some((base, dir_part.to_string(), prefix.to_string()))
    }

    fn refresh_completions(&mut self) {
        self.completions.clear();
        self.completion_index = 0;
        if !matches!(self.mode, Mode::Prompt(Prompt::Cd)) {
            return;
        }
        let Some((base, display, prefix)) = self.completion_context() else {
            return;
        };
        let Ok(rd) = fs::read_dir(&base) else {
            return;
        };
        // Yazi matches hidden entries too: exact prefix matches come first,
        // then fuzzy substring matches (so typing `cache` finds `.cache`).
        let smart = !prefix.chars().any(|c| c.is_ascii_uppercase());
        const LIMIT: usize = 30;
        let mut exact = Vec::new();
        let mut fuzzy = Vec::new();
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            let path = e.path();
            if !fs::metadata(&path).map(|m| m.is_dir()).unwrap_or(false) {
                continue;
            }
            let item = Completion {
                display: format!("{display}{name}/"),
                path,
            };
            let starts = if smart {
                name.to_lowercase().starts_with(&prefix.to_lowercase())
            } else {
                name.starts_with(&prefix)
            };
            if starts {
                exact.push(item);
            } else if !prefix.is_empty() && name.contains(&prefix) {
                fuzzy.push(item);
            }
        }
        exact.sort_by_key(|a| a.display.to_lowercase());
        fuzzy.sort_by_key(|a| a.display.to_lowercase());
        exact.extend(fuzzy);
        exact.truncate(LIMIT);
        self.completions = exact;
    }

    fn move_completion(&mut self, delta: isize) {
        if self.completions.is_empty() {
            return;
        }
        let n = self.completions.len() as isize;
        let i = self.completion_index as isize;
        self.completion_index = ((i + delta).rem_euclid(n)) as usize;
    }

    fn accept_completion(&mut self) {
        if let Some(c) = self.completions.get(self.completion_index).cloned() {
            self.input = c.display;
            self.refresh_completions();
        }
    }

    fn submit_cd(&mut self) {
        let name = self.input.trim().to_string();
        if !name.is_empty() {
            let path = self.expand_path(&name);
            let path = fs::canonicalize(&path).unwrap_or(path);
            if path.is_dir() {
                self.cd(path, None);
                return;
            }
        }
        if let Some(c) = self.completions.get(self.completion_index).cloned() {
            self.cd(c.path, None);
            return;
        }
        if !name.is_empty() {
            self.submit(Prompt::Cd);
        }
    }

    fn start_create(&mut self) {
        self.input.clear();
        self.completions.clear();
        self.mode = Mode::Prompt(Prompt::Create);
    }

    fn start_cd(&mut self) {
        self.input.clear();
        self.mode = Mode::Prompt(Prompt::Cd);
        self.refresh_completions();
    }

    fn external(&mut self, term: &mut DefaultTerminal, prog: &str, path: &Path) {
        let mut parts = prog.split_whitespace();
        let Some(p) = parts.next() else { return };
        ratatui::restore();
        let r = Command::new(p).args(parts).arg(path).status();
        *term = ratatui::init();
        if let Err(e) = r {
            self.status = format!("{p}: {e}");
        }
    }

    fn open_external(&mut self, path: &Path) {
        let opener = env::var("OPENER").unwrap_or_else(|_| {
            if cfg!(target_os = "macos") {
                "open"
            } else {
                "xdg-open"
            }
            .to_string()
        });
        let r = Command::new(&opener)
            .arg(path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
        if let Err(err) = r {
            self.status = format!("{opener}: {err}");
        }
    }

    fn open(&mut self, term: &mut DefaultTerminal) {
        let Some(e) = self.current().cloned() else {
            return;
        };
        let _ = term;
        if e.is_dir {
            self.cd(e.path, None);
        } else {
            self.open_external(&e.path);
        }
    }

    fn follow_symlink(&mut self) {
        let Some(e) = self.current().cloned() else {
            return;
        };
        let Ok(target) = fs::read_link(&e.path) else {
            self.status = "not a symlink".into();
            return;
        };
        let target = if target.is_absolute() {
            target
        } else {
            e.path.parent().unwrap_or(self.cwd.as_path()).join(target)
        };
        if target.is_dir() {
            self.cd(target, None);
        } else if target.exists() {
            self.open_external(&target);
        } else {
            self.status = format!("{}: broken symlink", target.display());
        }
    }

    fn fzf_jump(&mut self, term: &mut DefaultTerminal) {
        let preview = env::var_os("HOME").map(|h| {
            PathBuf::from(h)
                .join(".config/yazi/fzf-preview.sh")
                .display()
                .to_string()
        });
        let mut cmd = Command::new("fzf");
        cmd.arg("-m");
        if let Some(p) = preview.filter(|p| Path::new(p).exists()) {
            cmd.arg("--preview")
                .arg(format!("{p} {{}}"))
                .arg("--preview-window")
                .arg("right,50%,border-left");
        }
        cmd.current_dir(&self.cwd)
            .stdin(Stdio::inherit())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        ratatui::restore();
        let out = cmd.output();
        *term = ratatui::init();
        let out = match out {
            Ok(o) => o,
            Err(e) => {
                self.status = format!("fzf: {e}");
                return;
            }
        };
        if !out.status.success() && out.status.code() != Some(130) {
            self.status = format!("fzf exited with {}", out.status);
            return;
        }
        let lines: Vec<String> = String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(str::to_string)
            .collect();
        match lines.len() {
            0 => {}
            1 => {
                let path = self.cwd.join(&lines[0]);
                let path = fs::canonicalize(&path).unwrap_or(path);
                if path.is_dir() {
                    self.cd(path, None);
                } else if let Some(name) = path.file_name() {
                    let name = name.to_string_lossy().into_owned();
                    match path.parent() {
                        Some(parent) if parent != self.cwd.as_path() => {
                            self.cd(parent.to_path_buf(), Some(name));
                        }
                        _ => {
                            self.focus = Some(name);
                            self.reload();
                        }
                    }
                }
            }
            _ => {
                let paths: Vec<PathBuf> = lines
                    .iter()
                    .map(|l| self.cwd.join(l))
                    .map(|p| fs::canonicalize(&p).unwrap_or(p))
                    .filter(|p| p.exists())
                    .collect();
                let parent = paths
                    .first()
                    .and_then(|p| p.parent())
                    .map(Path::to_path_buf);
                let same_parent =
                    parent.is_some() && paths.iter().all(|p| p.parent() == parent.as_deref());
                if let (true, Some(parent)) = (same_parent, parent) {
                    if parent != self.cwd {
                        self.cd(parent.clone(), None);
                    }
                    self.selected = paths
                        .into_iter()
                        .filter(|p| p.parent() == Some(parent.as_path()))
                        .collect();
                } else {
                    self.selected = paths
                        .into_iter()
                        .filter(|p| p.parent() == Some(self.cwd.as_path()))
                        .collect();
                }
                self.list_dirty = true;
                self.status = format!("selected {} item(s)", self.selected.len());
            }
        }
    }

    /// Put `text` on the system clipboard using the given MIME type.
    fn clipboard_set(&mut self, text: String, mime: &str, ok: String) {
        let mut cmd = if env::var_os("WAYLAND_DISPLAY").is_some() {
            let mut c = Command::new("wl-copy");
            c.arg("--type").arg(mime);
            c
        } else if env::var_os("DISPLAY").is_some() {
            let mut c = Command::new("xclip");
            c.args(["-selection", "clipboard", "-t", mime]);
            c
        } else {
            self.status = "no clipboard backend (wl-copy/xclip)".into();
            return;
        };
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        match cmd.spawn() {
            Ok(mut child) => {
                if let Some(mut stdin) = child.stdin.take() {
                    let _ = stdin.write_all(text.as_bytes());
                }
                match child.wait() {
                    Ok(_) => self.status = ok,
                    Err(e) => self.status = format!("clipboard: {e}"),
                }
            }
            Err(e) => self.status = format!("clipboard: {e}"),
        }
    }

    fn clipboard_copy(&mut self) {
        let targets: Vec<PathBuf> = self.targets().into_iter().filter(|p| !p.is_dir()).collect();
        if targets.is_empty() {
            self.status = "nothing to copy".into();
            return;
        }
        let text = targets
            .iter()
            .map(|p| file_uri(p))
            .collect::<Vec<_>>()
            .join("\n")
            + "\n";
        let msg = format!("copied {} file(s)", targets.len());
        self.clipboard_set(text, "text/uri-list", msg);
    }

    /// Yazi's `c` prefix: copy paths, URLs, dir paths, names, etc.
    fn copy_paths(&mut self, kind: CopyKind) {
        let targets = self.targets();
        if targets.is_empty() {
            self.status = "nothing to copy".into();
            return;
        }
        let mut lines = Vec::new();
        for p in &targets {
            let value = match kind {
                CopyKind::Path => p.display().to_string(),
                CopyKind::Url => file_uri(p),
                CopyKind::DirPath => p
                    .parent()
                    .unwrap_or(self.cwd.as_path())
                    .display()
                    .to_string(),
                CopyKind::DirUrl => file_uri(p.parent().unwrap_or(self.cwd.as_path())),
                CopyKind::FileName => p
                    .file_name()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_default(),
                CopyKind::NameNoExt => p
                    .file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_default(),
            };
            if !value.is_empty() {
                lines.push(value);
            }
        }
        if lines.is_empty() {
            self.status = "nothing to copy".into();
            return;
        }
        let text = lines.join("\n") + "\n";
        let msg = format!("copied {} item(s)", lines.len());
        self.clipboard_set(text, "text/plain", msg);
    }

    fn clipboard_paste(&mut self) {
        let (prog, args): (&str, &[&str]) = if env::var_os("WAYLAND_DISPLAY").is_some() {
            ("wl-paste", &["--no-newline"])
        } else if env::var_os("DISPLAY").is_some() {
            ("xclip", &["-selection", "clipboard", "-o"])
        } else {
            self.status = "no clipboard backend (wl-paste/xclip)".into();
            return;
        };
        let out = match Command::new(prog).args(args).output() {
            Ok(o) => o,
            Err(e) => {
                self.status = format!("{prog}: {e}");
                return;
            }
        };
        let text = String::from_utf8_lossy(&out.stdout);
        let mut paths = Vec::new();
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            if line.starts_with("file://") {
                if let Some(p) = uri_path(line) {
                    paths.push(p);
                }
            } else {
                let p = PathBuf::from(line);
                if p.is_absolute() {
                    paths.push(p);
                }
            }
        }
        if paths.is_empty() {
            self.status = "clipboard has no file paths".into();
            return;
        }
        let mut n = 0;
        for src in paths {
            if !src.exists() {
                continue;
            }
            let Some(name) = src.file_name() else {
                continue;
            };
            let dst = unique(self.cwd.join(name));
            if copy_rec(&src, &dst).is_ok() {
                n += 1;
            }
        }
        self.reload();
        self.status = format!("pasted {n} file(s) from clipboard");
    }

    fn submit(&mut self, p: Prompt) {
        let name = self.input.trim().to_string();
        if name.is_empty() {
            return;
        }

        if p == Prompt::Cd {
            let path = self.expand_path(&name);
            let path = fs::canonicalize(&path).unwrap_or(path);
            self.cd(path, None);
            return;
        }

        if matches!(p, Prompt::SearchFd | Prompt::SearchRg) {
            let kind = if p == Prompt::SearchFd {
                SearchKind::Fd
            } else {
                SearchKind::Rg
            };
            self.search = Some(Search {
                kind,
                query: name,
                root: self.cwd.clone(),
            });
            self.reload();
            return;
        }

        let r = match p {
            Prompt::Create => {
                if name.ends_with('/') {
                    fs::create_dir_all(self.cwd.join(name.trim_end_matches('/')))
                } else {
                    fs::OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(self.cwd.join(&name))
                        .map(|_| ())
                }
            }
            Prompt::Rename => match self.current() {
                Some(e) => fs::rename(&e.path, self.cwd.join(&name)),
                None => return,
            },
            Prompt::Delete | Prompt::Cd | Prompt::SearchFd | Prompt::SearchRg | Prompt::Shell => {
                return
            }
        };
        match r {
            Ok(()) => {
                self.focus = Some(name.trim_end_matches('/').to_string());
                self.reload();
            }
            Err(e) => self.status = e.to_string(),
        }
    }

    fn delete(&mut self) {
        let t = self.targets();
        let mut n = 0;
        for p in &t {
            match remove(p) {
                Ok(()) => n += 1,
                Err(e) => self.status = format!("{}: {e}", p.display()),
            }
        }
        self.selected.clear();
        self.reload();
        if self.status.is_empty() {
            self.status = format!("deleted {n} item(s)");
        }
    }

    fn paste(&mut self, force: bool) {
        let Some((paths, cut)) = self.yank.clone() else {
            self.status = "nothing to paste".into();
            return;
        };
        let mut n = 0;
        for src in paths {
            let Some(name) = src.file_name() else {
                continue;
            };
            let target = self.cwd.join(name);
            let dst = if force { target } else { unique(target) };
            let r = if cut {
                fs::rename(&src, &dst).or_else(|_| copy_rec(&src, &dst).and_then(|_| remove(&src)))
            } else {
                copy_rec(&src, &dst)
            };
            match r {
                Ok(()) => n += 1,
                Err(e) => self.status = format!("{}: {e}", src.display()),
            }
        }
        if cut {
            self.yank = None;
        }
        self.reload();
        if self.status.is_empty() {
            self.status = format!("pasted {n} item(s)");
        }
    }

    fn delete_now(&mut self) {
        let t = self.targets();
        if t.is_empty() {
            return;
        }
        self.visual = None;
        let mut n = 0;
        for p in &t {
            match remove(p) {
                Ok(()) => n += 1,
                Err(e) => self.status = format!("{}: {e}", p.display()),
            }
        }
        self.selected.clear();
        self.reload();
        if self.status.is_empty() {
            self.status = format!("deleted {n} item(s)");
        }
    }

    /// Symlink yanked files into the cwd (Yazi's `-` and `_`).
    fn link(&mut self, relative: bool) {
        let Some((paths, _)) = self.yank.clone() else {
            self.status = "nothing to link".into();
            return;
        };
        let mut n = 0;
        for src in paths {
            let Some(name) = src.file_name() else {
                continue;
            };
            let dst = unique(self.cwd.join(name));
            let target = if relative {
                relative_path(&self.cwd, &src)
            } else {
                src.clone()
            };
            match std::os::unix::fs::symlink(&target, &dst) {
                Ok(()) => n += 1,
                Err(e) => self.status = format!("{}: {e}", dst.display()),
            }
        }
        self.reload();
        if self.status.is_empty() {
            self.status = format!("linked {n} item(s)");
        }
    }

    /// Hardlink yanked files into the cwd (Yazi's `Ctrl+-`).
    fn hardlink(&mut self) {
        let Some((paths, _)) = self.yank.clone() else {
            self.status = "nothing to link".into();
            return;
        };
        let mut n = 0;
        for src in paths {
            let Some(name) = src.file_name() else {
                continue;
            };
            let dst = unique(self.cwd.join(name));
            match fs::hard_link(&src, &dst) {
                Ok(()) => n += 1,
                Err(e) => self.status = format!("{}: {e}", dst.display()),
            }
        }
        self.reload();
        if self.status.is_empty() {
            self.status = format!("hardlinked {n} item(s)");
        }
    }

    fn start_search(&mut self, kind: SearchKind) {
        self.input.clear();
        self.completions.clear();
        self.mode = Mode::Prompt(match kind {
            SearchKind::Fd => Prompt::SearchFd,
            SearchKind::Rg => Prompt::SearchRg,
        });
    }

    fn start_shell(&mut self) {
        self.input.clear();
        self.completions.clear();
        self.mode = Mode::Prompt(Prompt::Shell);
    }

    /// Run a shell command in the current directory, temporarily handing the
    /// terminal over to it (Yazi's `;` / `:`).
    fn run_shell(&mut self, term: &mut DefaultTerminal) {
        let cmd = self.input.trim().to_string();
        if cmd.is_empty() {
            return;
        }
        let shell = env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into());
        ratatui::restore();
        let r = Command::new(&shell)
            .arg("-c")
            .arg(&cmd)
            .current_dir(&self.cwd)
            .status();
        *term = ratatui::init();
        match r {
            Ok(s) if !s.success() => self.status = format!("shell: {s}"),
            Err(e) => self.status = format!("shell: {e}"),
            _ => {}
        }
        self.reload();
    }

    fn suspend(&mut self, term: &mut DefaultTerminal) {
        ratatui::restore();
        let _ =
            rustix::process::kill_process(rustix::process::getpid(), rustix::process::Signal::Tstp);
        *term = ratatui::init();
        self.reload();
    }

    fn start_find(&mut self, forward: bool) {
        self.find_origin = self.state.selected();
        self.find_query.clear();
        self.find_forward = forward;
        self.mode = Mode::Find;
    }

    fn find_match(&mut self, from: usize, forward: bool, query: &str, include_from: bool) {
        let n = self.entries.len();
        if n == 0 || query.is_empty() {
            return;
        }
        let start = if include_from {
            from
        } else if forward {
            (from + 1) % n
        } else {
            (from + n - 1) % n
        };
        let mut i = start;
        for _ in 0..n {
            if smart_contains(&self.entries[i].name, query) {
                self.state.select(Some(i));
                return;
            }
            i = if forward {
                (i + 1) % n
            } else {
                (i + n - 1) % n
            };
        }
    }

    fn find_next(&mut self, forward: bool) {
        let Some(query) = self.last_find.clone() else {
            return;
        };
        let from = self.state.selected().unwrap_or(0);
        self.find_match(from, forward, &query, false);
    }

    fn set_linemode(&mut self, mode: Linemode) {
        self.linemode = mode;
        self.list_dirty = true;
    }

    // ─────────────── input ───────────────

    fn handle_key(&mut self, key: KeyEvent, term: &mut DefaultTerminal) -> io::Result<bool> {
        self.status.clear();

        match self.mode {
            Mode::Filter => {
                match key.code {
                    KeyCode::Esc => {
                        self.filter.clear();
                        self.mode = Mode::Normal;
                        self.reload();
                    }
                    KeyCode::Enter => self.mode = Mode::Normal,
                    KeyCode::Backspace => {
                        self.filter.pop();
                        self.reload();
                    }
                    KeyCode::Char(c) => {
                        self.filter.push(c);
                        self.reload();
                    }
                    _ => {}
                }
                return Ok(false);
            }
            Mode::Find => {
                match key.code {
                    KeyCode::Esc => {
                        if let Some(o) = self.find_origin.take() {
                            if !self.entries.is_empty() {
                                self.state.select(Some(o.min(self.entries.len() - 1)));
                            }
                        }
                        self.find_query.clear();
                        self.mode = Mode::Normal;
                    }
                    KeyCode::Enter => {
                        self.last_find = Some(self.find_query.clone());
                        self.find_origin = None;
                        self.mode = Mode::Normal;
                    }
                    KeyCode::Backspace => {
                        self.find_query.pop();
                        if let Some(o) = self.find_origin {
                            if self.find_query.is_empty() {
                                if !self.entries.is_empty() {
                                    self.state.select(Some(o.min(self.entries.len() - 1)));
                                }
                            } else {
                                let fwd = self.find_forward;
                                let q = self.find_query.clone();
                                self.find_match(o, fwd, &q, true);
                            }
                        }
                    }
                    KeyCode::Char(c) => {
                        self.find_query.push(c);
                        if let Some(o) = self.find_origin {
                            let fwd = self.find_forward;
                            let q = self.find_query.clone();
                            self.find_match(o, fwd, &q, true);
                        }
                    }
                    _ => {}
                }
                return Ok(false);
            }
            Mode::Prompt(p) => {
                if p == Prompt::Delete {
                    if key.code == KeyCode::Char('y') {
                        self.delete();
                    }
                    self.mode = Mode::Normal;
                    return Ok(false);
                }
                match key.code {
                    KeyCode::Esc => {
                        self.mode = Mode::Normal;
                        self.input.clear();
                        self.completions.clear();
                    }
                    KeyCode::Enter => {
                        if p == Prompt::Shell {
                            self.run_shell(term);
                        } else if p == Prompt::Cd {
                            self.submit_cd();
                        } else {
                            self.submit(p);
                        }
                        self.mode = Mode::Normal;
                        self.input.clear();
                        self.completions.clear();
                    }
                    KeyCode::Backspace => {
                        self.input.pop();
                        if p == Prompt::Cd {
                            self.refresh_completions();
                        }
                    }
                    KeyCode::Char(c) => {
                        self.input.push(c);
                        if p == Prompt::Cd {
                            self.refresh_completions();
                        }
                    }
                    KeyCode::Up if p == Prompt::Cd => self.move_completion(-1),
                    KeyCode::Down if p == Prompt::Cd => self.move_completion(1),
                    KeyCode::Tab if p == Prompt::Cd => self.accept_completion(),
                    _ => {}
                }
                return Ok(false);
            }
            Mode::Help => {
                self.mode = Mode::Normal;
                return Ok(false);
            }
            Mode::Normal => {}
        }

        if let Some(prefix) = self.prefix.take() {
            self.handle_prefix(prefix, key);
            return Ok(false);
        }

        if key.modifiers.contains(KeyModifiers::CONTROL) {
            match key.code {
                KeyCode::Char('c') => return Ok(true),
                KeyCode::Char('d') => self.move_by((self.page / 2) as isize),
                KeyCode::Char('u') => self.move_by(-((self.page / 2) as isize)),
                KeyCode::Char('b') => self.move_by(-(self.page as isize)),
                KeyCode::Char('f') => self.move_by(self.page as isize),
                KeyCode::Char('a') => self.select_all(),
                KeyCode::Char('r') => self.invert_selection(),
                KeyCode::Char('n') => self.start_create(),
                KeyCode::Char('y') => self.clipboard_copy(),
                KeyCode::Char('p') => self.clipboard_paste(),
                KeyCode::Char('s') => self.cancel_search(),
                // Ctrl+- / Ctrl+_ arrive as `Ctrl+7` from terminals that send
                // the legacy 0x1f byte (crossterm maps 0x1c..=0x1f to 4..=7).
                KeyCode::Char('-') | KeyCode::Char('_') | KeyCode::Char('7') => self.hardlink(),
                KeyCode::Char('z') => self.suspend(term),
                _ => {}
            }
            return Ok(false);
        }

        match key.code {
            KeyCode::Char('q') | KeyCode::Char('Q') => return Ok(true),
            KeyCode::Char('j') | KeyCode::Down => self.move_by(1),
            KeyCode::Char('k') | KeyCode::Up => self.move_by(-1),
            KeyCode::Char('J') => {
                let s = self.preview.preview_seek().saturating_add(5);
                self.preview.set_preview_seek(s);
            }
            KeyCode::Char('K') => {
                let s = self.preview.preview_seek().saturating_sub(5);
                self.preview.set_preview_seek(s);
            }
            KeyCode::PageDown => {
                let n = if key.modifiers.contains(KeyModifiers::SHIFT) {
                    self.page / 2
                } else {
                    self.page
                };
                self.move_by(n as isize);
            }
            KeyCode::PageUp => {
                let n = if key.modifiers.contains(KeyModifiers::SHIFT) {
                    self.page / 2
                } else {
                    self.page
                };
                self.move_by(-(n as isize));
            }
            KeyCode::Char('g') => self.prefix = Some(Prefix::Goto),
            KeyCode::Home => self.select_top(),
            KeyCode::Char('G') | KeyCode::End => self.select_bottom(),
            KeyCode::Char('h') | KeyCode::Left | KeyCode::Backspace => self.go_parent(),
            KeyCode::Char('l') | KeyCode::Right | KeyCode::Enter | KeyCode::Char('o') => {
                self.open(term)
            }
            KeyCode::Char('H') => self.history_back(),
            KeyCode::Char('L') => self.history_forward(),
            KeyCode::Char('~') | KeyCode::F(1) => self.mode = Mode::Help,
            KeyCode::Char('e') => {
                if let Some(e) = self.current().cloned().filter(|e| !e.is_dir) {
                    let ed = env::var("VISUAL")
                        .or_else(|_| env::var("EDITOR"))
                        .unwrap_or_else(|_| "vi".into());
                    self.external(term, &ed, &e.path);
                    self.reload();
                }
            }
            KeyCode::Char('i') => {
                if let Some(e) = self.current().cloned().filter(|e| !e.is_dir) {
                    let pg = env::var("PAGER").unwrap_or_else(|_| "less".into());
                    self.external(term, &pg, &e.path);
                }
            }
            KeyCode::Char(' ') => {
                self.visual = None;
                if let Some(e) = self.current() {
                    let p = e.path.clone();
                    if !self.selected.remove(&p) {
                        self.selected.insert(p);
                    }
                    self.list_dirty = true;
                    self.move_by(1);
                }
            }
            KeyCode::Char('v') => {
                if matches!(
                    self.visual.as_ref().map(|v| v.kind),
                    Some(VisualKind::Select)
                ) {
                    self.visual = None;
                } else {
                    self.enter_visual(VisualKind::Select);
                }
            }
            KeyCode::Char('V') => {
                if matches!(
                    self.visual.as_ref().map(|v| v.kind),
                    Some(VisualKind::Unselect)
                ) {
                    self.visual = None;
                } else {
                    self.enter_visual(VisualKind::Unselect);
                }
            }
            KeyCode::Char('y') | KeyCode::Char('x') => {
                let cut = key.code == KeyCode::Char('x');
                let t = self.targets();
                self.visual = None;
                if !t.is_empty() {
                    self.status =
                        format!("{} item(s) {}", t.len(), if cut { "cut" } else { "yanked" });
                    self.yank = Some((t, cut));
                    self.selected.clear();
                    self.list_dirty = true;
                }
            }
            KeyCode::Char('p') => self.paste(false),
            KeyCode::Char('P') => self.paste(true),
            KeyCode::Char('Y') | KeyCode::Char('X') => {
                if self.yank.take().is_some() {
                    self.status = "unyanked".into();
                }
            }
            KeyCode::Char('d') => {
                let has_targets = !self.targets().is_empty();
                self.visual = None;
                if has_targets {
                    self.mode = Mode::Prompt(Prompt::Delete);
                }
            }
            KeyCode::Char('D') => self.delete_now(),
            KeyCode::Char('r') => {
                self.visual = None;
                if let Some(e) = self.current() {
                    self.input = e.name.clone();
                    self.mode = Mode::Prompt(Prompt::Rename);
                }
            }
            KeyCode::Char('a') => self.start_create(),
            KeyCode::Char('f') => self.mode = Mode::Filter,
            KeyCode::Char('/') => self.start_find(true),
            KeyCode::Char('?') => self.start_find(false),
            KeyCode::Char('n') => self.find_next(true),
            KeyCode::Char('N') => self.find_next(false),
            KeyCode::Char('s') => self.start_search(SearchKind::Fd),
            KeyCode::Char('S') => self.start_search(SearchKind::Rg),
            KeyCode::Char('c') => self.prefix = Some(Prefix::Copy),
            KeyCode::Char('m') => self.prefix = Some(Prefix::Linemode),
            KeyCode::Char('z') => self.fzf_jump(term),
            KeyCode::Char(';') | KeyCode::Char(':') => self.start_shell(),
            KeyCode::Char(',') => self.prefix = Some(Prefix::Sort),
            KeyCode::Char('+') => self.zoom_in(),
            KeyCode::Char('-') if key.modifiers.contains(KeyModifiers::ALT) => self.link(false),
            KeyCode::Char('-') => self.zoom_out(),
            KeyCode::Char('_') => self.link(true),
            KeyCode::Char('0') => self.zoom_reset(),
            KeyCode::Esc => {
                if self.visual.is_some() {
                    self.visual = None;
                } else if !self.filter.is_empty() {
                    self.filter.clear();
                    self.reload();
                } else if self.search.is_some() {
                    self.cancel_search();
                } else {
                    self.selected.clear();
                    self.list_dirty = true;
                }
            }
            KeyCode::Char('.') => {
                self.show_hidden = !self.show_hidden;
                self.reload();
            }
            KeyCode::Char('T') => self.show_preview = !self.show_preview,
            _ => {}
        }
        Ok(false)
    }

    fn handle_prefix(&mut self, prefix: Prefix, key: KeyEvent) {
        if key.code == KeyCode::Esc {
            return;
        }
        match prefix {
            Prefix::Goto => match key.code {
                KeyCode::Char('g') => self.select_top(),
                KeyCode::Char('h') => self.goto("~"),
                KeyCode::Char('c') => self.goto("~/.config"),
                KeyCode::Char('d') => self.goto("~/Downloads"),
                KeyCode::Char('o') => self.goto("~/.local/share/osu-wine/osu!"),
                KeyCode::Char('p') => self.goto("~/Pictures"),
                KeyCode::Char('v') => self.goto("~/Videos"),
                KeyCode::Char(' ') => self.start_cd(),
                KeyCode::Char('f') => self.follow_symlink(),
                KeyCode::Char('t') => self.status = "trash is not supported".into(),
                _ => {}
            },
            Prefix::Sort => match key.code {
                KeyCode::Char('a') => self.set_sort(Sort::Name, false),
                KeyCode::Char('A') => self.set_sort(Sort::Name, true),
                KeyCode::Char('n') => self.set_sort(Sort::Natural, false),
                KeyCode::Char('N') => self.set_sort(Sort::Natural, true),
                KeyCode::Char('s') => {
                    self.set_sort(Sort::Size, false);
                    self.set_linemode(Linemode::Size);
                }
                KeyCode::Char('S') => {
                    self.set_sort(Sort::Size, true);
                    self.set_linemode(Linemode::Size);
                }
                KeyCode::Char('m') => {
                    self.set_sort(Sort::Time, false);
                    self.set_linemode(Linemode::Mtime);
                }
                KeyCode::Char('M') => {
                    self.set_sort(Sort::Time, true);
                    self.set_linemode(Linemode::Mtime);
                }
                KeyCode::Char('b') => {
                    self.set_sort(Sort::Btime, false);
                    self.set_linemode(Linemode::Btime);
                }
                KeyCode::Char('B') => {
                    self.set_sort(Sort::Btime, true);
                    self.set_linemode(Linemode::Btime);
                }
                KeyCode::Char('e') => self.set_sort(Sort::Extension, false),
                KeyCode::Char('E') => self.set_sort(Sort::Extension, true),
                KeyCode::Char('r') => self.set_sort(Sort::Random, false),
                _ => {}
            },
            Prefix::Copy => match key.code {
                KeyCode::Char('c') => self.copy_paths(CopyKind::Path),
                KeyCode::Char('C') => self.copy_paths(CopyKind::Url),
                KeyCode::Char('d') => self.copy_paths(CopyKind::DirPath),
                KeyCode::Char('D') => self.copy_paths(CopyKind::DirUrl),
                KeyCode::Char('f') => self.copy_paths(CopyKind::FileName),
                KeyCode::Char('n') => self.copy_paths(CopyKind::NameNoExt),
                _ => {}
            },
            Prefix::Linemode => match key.code {
                KeyCode::Char('s') => self.set_linemode(Linemode::Size),
                KeyCode::Char('p') => self.set_linemode(Linemode::Permissions),
                KeyCode::Char('b') => self.set_linemode(Linemode::Btime),
                KeyCode::Char('m') => self.set_linemode(Linemode::Mtime),
                KeyCode::Char('o') => self.set_linemode(Linemode::Owner),
                KeyCode::Char('n') => self.set_linemode(Linemode::None),
                _ => {}
            },
        }
    }

    fn goto(&mut self, path: &str) {
        let path = if path == "~" {
            env::var_os("HOME").map(PathBuf::from)
        } else if let Some(rest) = path.strip_prefix("~/") {
            env::var_os("HOME").map(|h| PathBuf::from(h).join(rest))
        } else {
            Some(PathBuf::from(path))
        };
        let Some(path) = path else {
            self.status = "HOME is not set".into();
            return;
        };
        let path = fs::canonicalize(&path).unwrap_or(path);
        self.cd(path, None);
    }

    fn set_sort(&mut self, sort: Sort, reverse: bool) {
        self.sort = sort;
        self.sort_reverse = reverse;
        self.reload();
    }

    fn select_top(&mut self) {
        if !self.entries.is_empty() {
            self.state.select(Some(0));
            self.apply_visual();
        }
    }

    fn select_bottom(&mut self) {
        if !self.entries.is_empty() {
            self.state.select(Some(self.entries.len() - 1));
            self.apply_visual();
        }
    }

    fn zoom_in(&mut self) {
        match self.preview.zoom_in() {
            None => self.status = "zoom: image preview only".into(),
            Some(z) if z <= 1.0 => self.status = "zoom fit".into(),
            Some(_) => {}
        }
    }

    fn zoom_out(&mut self) {
        match self.preview.zoom_out() {
            None => self.status = "zoom: image preview only".into(),
            Some(z) if z <= 1.0 => self.status = "zoom fit".into(),
            Some(_) => {}
        }
    }

    fn zoom_reset(&mut self) {
        if self.preview.zoom_reset() {
            self.status = "zoom fit".into();
        } else {
            self.status = "zoom: image preview only".into();
        }
    }

    // ─────────────── drawing ───────────────

    fn rebuild_list(&mut self, width: u16) {
        let inner = width.saturating_sub(1) as usize;
        let linemode = self.linemode;
        let items: Vec<ListItem> = self
            .entries
            .iter()
            .map(|e| {
                let mark = if self.selected.contains(&e.path) {
                    "▌"
                } else {
                    " "
                };
                let left = format!("{mark}{}{}", e.name, if e.is_dir { "/" } else { "" });
                let right = match linemode {
                    Linemode::None => String::new(),
                    Linemode::Size => {
                        if e.is_dir {
                            String::new()
                        } else {
                            human(e.size)
                        }
                    }
                    Linemode::Permissions => permissions(e.mode),
                    Linemode::Btime => format_age(e.created),
                    Linemode::Mtime => format_age(e.modified),
                    Linemode::Owner => format!("{}:{}", e.uid, e.gid),
                };
                let used = left.chars().count() + right.chars().count();
                let pad = inner.saturating_sub(used).max(1);
                let style = if e.is_dir {
                    Style::new().fg(Color::Blue).add_modifier(Modifier::BOLD)
                } else {
                    Style::new()
                };
                ListItem::new(Line::from(vec![
                    Span::styled(left, style),
                    Span::raw(" ".repeat(pad)),
                    Span::styled(right, Style::new().fg(Color::DarkGray)),
                ]))
            })
            .collect();
        self.list = List::new(items).highlight_style(Style::new().add_modifier(Modifier::REVERSED));
        self.list_width = width;
        self.list_dirty = false;
    }

    fn draw(&mut self, f: &mut Frame) {
        let rows = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).split(f.area());
        let cols = if self.show_preview {
            Layout::horizontal([Constraint::Percentage(40), Constraint::Percentage(60)])
                .split(rows[0])
        } else {
            Layout::horizontal([Constraint::Percentage(100)]).split(rows[0])
        };
        self.page = rows[0].height.saturating_sub(1) as usize;

        // file list
        if self.list_dirty || self.list_width != cols[0].width {
            self.rebuild_list(cols[0].width);
        }
        let title_area = Rect::new(
            cols[0].x,
            cols[0].y,
            cols[0].width.saturating_sub(1),
            cols[0].height.min(1),
        );
        let list_area = Rect::new(
            cols[0].x,
            cols[0].y + 1,
            cols[0].width,
            cols[0].height.saturating_sub(1),
        );
        f.render_widget(
            Paragraph::new(format!(" {} ", self.cwd.display())),
            title_area,
        );
        f.render_stateful_widget(&self.list, list_area, &mut self.state);

        // preview pane
        if self.show_preview {
            self.draw_preview(f, cols[1]);
        } else {
            self.preview.set_target(None);
        }

        // status line
        let sort = match self.sort {
            Sort::Name => "name",
            Sort::Natural => "natural",
            Sort::Size => "size",
            Sort::Time => "time",
            Sort::Btime => "btime",
            Sort::Extension => "ext",
            Sort::Random => "random",
        };
        let direction = if self.sort_reverse { "↓" } else { "↑" };
        let mut text = match self.mode {
            Mode::Filter => format!("filter: {}", self.filter),
            Mode::Find => format!(
                "find{}: {}",
                if self.find_forward { "→" } else { "←" },
                self.find_query
            ),
            Mode::Help => "rnn keys — press any key to close".into(),
            Mode::Prompt(Prompt::Delete) => {
                format!("delete {} item(s)? (y/N)", self.targets().len())
            }
            Mode::Prompt(p) => {
                let label = match p {
                    Prompt::Create => "create",
                    Prompt::Rename => "rename",
                    Prompt::Cd => "go to",
                    Prompt::SearchFd => "fd",
                    Prompt::SearchRg => "rg",
                    Prompt::Shell => "shell",
                    Prompt::Delete => "",
                };
                format!("{label}: {}", self.input)
            }
            Mode::Normal if !self.status.is_empty() => self.status.clone(),
            Mode::Normal => {
                let mut s = format!(
                    "{} items | {} selected | sort:{sort}{direction}",
                    self.entries.len(),
                    self.selected.len()
                );
                if let Some(search) = &self.search {
                    let kind = match search.kind {
                        SearchKind::Fd => "fd",
                        SearchKind::Rg => "rg",
                    };
                    s.push_str(&format!(" | {kind}: {}", search.query));
                }
                if self.show_hidden {
                    s.push_str(" | hidden");
                }
                if !self.filter.is_empty() {
                    s.push_str(&format!(" | filter:{}", self.filter));
                }
                if self.linemode != Linemode::Size {
                    let lm = match self.linemode {
                        Linemode::Size => "size",
                        Linemode::Permissions => "perms",
                        Linemode::Btime => "btime",
                        Linemode::Mtime => "mtime",
                        Linemode::Owner => "owner",
                        Linemode::None => "none",
                    };
                    s.push_str(&format!(" | lm:{lm}"));
                }
                s
            }
        };
        if let Some(prefix) = self.prefix {
            text.push_str(match prefix {
                Prefix::Goto => " | g…",
                Prefix::Sort => " | ,…",
                Prefix::Copy => " | c…",
                Prefix::Linemode => " | m…",
            });
        }
        if let Some(visual) = &self.visual {
            text.push_str(match visual.kind {
                VisualKind::Select => " | VISUAL",
                VisualKind::Unselect => " | VISUAL-UNSET",
            });
        }
        if self.preview.preview_seek() > 0 {
            text.push_str(&format!(" | seek +{}", self.preview.preview_seek()));
        }
        if let Some(z) = self.preview.zoom() {
            if z > 1.0 {
                text.push_str(&format!(" | zoom {z}x"));
            }
        }
        f.render_widget(Paragraph::new(text), rows[1]);
        self.draw_completions(f);

        if matches!(self.mode, Mode::Help) {
            self.draw_help(f);
        }
    }

    fn draw_completions(&self, f: &mut Frame) {
        if !matches!(self.mode, Mode::Prompt(Prompt::Cd)) || self.completions.is_empty() {
            return;
        }
        let area = f.area();
        let visible = self.completions.len().min(10);
        let height = visible as u16 + 2;
        if area.height <= height + 1 || area.width < 20 {
            return;
        }
        let longest = self
            .completions
            .iter()
            .map(|c| c.display.chars().count())
            .max()
            .unwrap_or(0) as u16;
        let width = (longest + 4).max(30).min(area.width.saturating_sub(4));
        let rect = Rect::new(
            area.x + (area.width - width) / 2,
            area.y + area.height - height - 1,
            width,
            height,
        );
        let items: Vec<ListItem> = self
            .completions
            .iter()
            .map(|c| ListItem::new(c.display.clone()))
            .collect();
        let mut state = ListState::default();
        state.select(Some(
            self.completion_index
                .min(self.completions.len().saturating_sub(1)),
        ));
        f.render_widget(Clear, rect);
        f.render_stateful_widget(
            List::new(items)
                .block(Block::bordered().title(" directories "))
                .highlight_style(Style::new().add_modifier(Modifier::REVERSED)),
            rect,
            &mut state,
        );
    }

    fn draw_help(&self, f: &mut Frame) {
        let left = [
            "j/k, ↑/↓     move",
            "gg / G      top / bottom",
            "h/l, ←/→    parent / open",
            "H / L       history back / fwd",
            "Ctrl+u/d    half page",
            "Ctrl+b/f    full page",
            "Space       toggle selection",
            "Ctrl+a/r    select all / invert",
            "v / V       visual select / unset",
            "y / x       yank / cut",
            "p / P       paste / paste-force",
            "Y / X       cancel yank",
            "d / D       delete confirm / now",
            "r / a       rename / create",
            "Ctrl+n      create",
            "Ctrl+y/p    clipboard copy / paste",
            "; / :       run shell command",
            "Ctrl+z      suspend",
        ];
        let right = [
            "o / Enter   open",
            "e / i       editor / pager",
            "J / K       seek preview",
            "+ / - / 0   zoom in / out / fit",
            "T           toggle preview pane",
            ".           hidden files",
            "f           filter",
            "/ ?         find next / previous",
            "n / N       next / previous found",
            "s / S       search fd / rg",
            "z           fzf jump",
            "c …         copy path/url/dir/name",
            "m …         linemode size/perms/time",
            "_ / Alt+-   symlink relative / absolute",
            "Ctrl+-      hardlink",
            "gh gc gd    home / config / downloads",
            "go gp gv    osu / pictures / videos",
            "g<Space>    go to path (live menu)",
            "gf / gt     follow symlink / trash",
            ",a ,n ,s    sort name/natural/size",
            ",m ,b ,e    sort time / btime / ext",
            ",r          sort randomly",
            "~ / F1      help",
        ];
        let area = f.area();
        let width = area.width.saturating_sub(4).min(88);
        let height = area
            .height
            .saturating_sub(4)
            .min((left.len().max(right.len()) + 2) as u16);
        if width == 0 || height == 0 {
            return;
        }
        let rect = Rect::new(
            area.x + (area.width - width) / 2,
            area.y + (area.height - height) / 2,
            width,
            height,
        );
        let lines: Vec<Line<'static>> = (0..left.len().max(right.len()))
            .map(|i| {
                let l = left.get(i).copied().unwrap_or("");
                let r = right.get(i).copied().unwrap_or("");
                Line::from(vec![
                    Span::raw(format!("{l:<44}")),
                    Span::raw(r.to_string()),
                ])
            })
            .collect();
        f.render_widget(Clear, rect);
        f.render_widget(
            Paragraph::new(lines).block(Block::bordered().title(" rnn keys ")),
            rect,
        );
    }

    fn draw_preview(&mut self, f: &mut Frame, area: Rect) {
        let title_area = Rect::new(area.x, area.y, area.width, area.height.min(1));
        let inner = Rect::new(
            area.x,
            area.y + 1,
            area.width,
            area.height.saturating_sub(1),
        );

        let target: Option<PreviewKey> =
            self.current().map(|e| (e.path.clone(), e.modified, e.size));
        if self.preview.set_target(target.as_ref()) {
            self.preview.set_preview_seek(0);
            self.preload_neighbours();
        }
        let meta = self.preview.artifact_meta();
        let name = self
            .current()
            .map(|e| e.name.clone())
            .unwrap_or_else(|| "preview".into());

        let mut title = format!(" {name} ");
        if let Some((kind, total)) = meta {
            title = format!(" {name} · {kind} ");
            let h = inner.height as usize;
            if total > h {
                let max_seek = total.saturating_sub(h);
                let seek = self.preview.preview_seek().min(max_seek);
                title.push_str(&format!(
                    "[{}–{} of {total}] ",
                    seek + 1,
                    (seek + h).min(total)
                ));
            }
        }
        if self.preview.is_loading() {
            title.push('…');
        }
        f.render_widget(Paragraph::new(title), title_area);

        for y in inner.y..inner.y + inner.height {
            for x in inner.x..inner.x + inner.width {
                if let Some(c) = f.buffer_mut().cell_mut((x, y)) {
                    *c = ratatui::buffer::Cell::default();
                }
            }
        }
        let is_image = meta.map(|(k, _)| k == "image").unwrap_or(false);
        if is_image {
            if let Some(s) = self.preview.image_state_mut() {
                s.render(inner, f.buffer_mut());
            } else {
                f.render_widget(
                    Paragraph::new(Line::styled(
                        "image preview needs kitty or ghostty",
                        Style::new().fg(Color::DarkGray),
                    )),
                    inner,
                );
            }
        } else {
            let h = inner.height as usize;
            let lines = self.preview.lines_slice(h).unwrap_or_default();
            f.render_widget(Paragraph::new(lines), inner);
        }
    }

    // ─────────────── main loop ───────────────

    pub fn run(&mut self, term: &mut DefaultTerminal) -> io::Result<()> {
        let time = std::env::var_os("RNN_TIME").is_some();
        term.draw(|f| self.draw(f))?;
        loop {
            let mut dirty = false;
            let mut key_ms = None;
            if event::poll(Duration::from_millis(10))? {
                match event::read()? {
                    Event::Key(k) if k.kind == KeyEventKind::Press => {
                        let t = std::time::Instant::now();
                        if self.handle_key(k, term)? {
                            return Ok(());
                        }
                        if time {
                            key_ms = Some(t.elapsed());
                        }
                        dirty = true;
                    }
                    Event::Resize(_, _) => dirty = true,
                    _ => {}
                }
            }
            self.preview.poll();
            if self.preview.take_changed() {
                dirty = true;
            }
            if dirty {
                let t = std::time::Instant::now();
                let mut closure = Duration::ZERO;
                term.draw(|f| {
                    let tc = std::time::Instant::now();
                    self.draw(f);
                    closure = tc.elapsed();
                })?;
                if time {
                    eprintln!(
                        "rnn-t: key={} draw={:?} render={:?} flush={:?}",
                        key_ms.map(|d| format!("{d:?}")).unwrap_or_default(),
                        t.elapsed(),
                        closure,
                        t.elapsed().saturating_sub(closure)
                    );
                }
            }
        }
    }
}
