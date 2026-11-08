use std::{
    collections::BTreeSet,
    env, fs, io,
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
}

#[derive(Clone, Copy)]
enum Sort {
    Name,
    Size,
    Time,
    Extension,
}

#[derive(Clone, Copy, PartialEq)]
enum Prompt {
    Create,
    Rename,
    Delete,
    Cd,
}

#[derive(Clone, Copy)]
enum Mode {
    Normal,
    Filter,
    Prompt(Prompt),
    Help,
}

#[derive(Clone, Copy)]
enum Prefix {
    Goto,
    Sort,
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
        match fs::read_dir(&self.cwd) {
            Ok(rd) => {
                let needle = self.filter.to_lowercase();
                for e in rd.flatten() {
                    let name = e.file_name().to_string_lossy().into_owned();
                    if !self.show_hidden && name.starts_with('.') {
                        continue;
                    }
                    if !needle.is_empty() && !name.to_lowercase().contains(&needle) {
                        continue;
                    }
                    let path = e.path();
                    let md = fs::metadata(&path).or_else(|_| e.metadata()).ok();
                    v.push(Entry {
                        name,
                        path,
                        is_dir: md.as_ref().map(|m| m.is_dir()).unwrap_or(false),
                        size: md.as_ref().map(|m| m.len()).unwrap_or(0),
                        modified: md.and_then(|m| m.modified().ok()),
                    });
                }
            }
            Err(e) => self.status = format!("{}: {e}", self.cwd.display()),
        }

        let sort = self.sort;
        let reverse = self.sort_reverse;
        v.sort_by(|a, b| {
            b.is_dir.cmp(&a.is_dir).then_with(|| {
                let order = match sort {
                    Sort::Name => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
                    Sort::Size => a.size.cmp(&b.size),
                    Sort::Time => a.modified.cmp(&b.modified),
                    Sort::Extension => extension(&a.name)
                        .cmp(&extension(&b.name))
                        .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())),
                };
                if reverse {
                    order.reverse()
                } else {
                    order
                }
            })
        });
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

    fn submit(&mut self, p: Prompt) {
        let name = self.input.trim().to_string();
        if name.is_empty() {
            return;
        }

        if p == Prompt::Cd {
            let path = if name == "~" {
                env::var_os("HOME").map(PathBuf::from)
            } else if let Some(rest) = name.strip_prefix("~/") {
                env::var_os("HOME").map(|h| PathBuf::from(h).join(rest))
            } else {
                Some(PathBuf::from(&name))
            };
            let Some(path) = path else {
                self.status = "HOME is not set".into();
                return;
            };
            let path = if path.is_absolute() {
                path
            } else {
                self.cwd.join(path)
            };
            let path = fs::canonicalize(&path).unwrap_or(path);
            self.cd(path, None);
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
            Prompt::Delete | Prompt::Cd => return,
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
                    }
                    KeyCode::Enter => {
                        self.submit(p);
                        self.mode = Mode::Normal;
                        self.input.clear();
                    }
                    KeyCode::Backspace => {
                        self.input.pop();
                    }
                    KeyCode::Char(c) => self.input.push(c),
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
            KeyCode::Char('~') => self.mode = Mode::Help,
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
            KeyCode::Char('r') => {
                self.visual = None;
                if let Some(e) = self.current() {
                    self.input = e.name.clone();
                    self.mode = Mode::Prompt(Prompt::Rename);
                }
            }
            KeyCode::Char('a') => {
                self.input.clear();
                self.mode = Mode::Prompt(Prompt::Create);
            }
            KeyCode::Char('/') => self.mode = Mode::Filter,
            KeyCode::Char(',') => self.prefix = Some(Prefix::Sort),
            KeyCode::Char('+') => self.zoom_in(),
            KeyCode::Char('-') => self.zoom_out(),
            KeyCode::Char('0') => self.zoom_reset(),
            KeyCode::Esc => {
                if self.visual.is_some() {
                    self.visual = None;
                } else if !self.filter.is_empty() {
                    self.filter.clear();
                    self.reload();
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
                KeyCode::Char(' ') => {
                    self.input.clear();
                    self.mode = Mode::Prompt(Prompt::Cd);
                }
                KeyCode::Char('f') => self.follow_symlink(),
                KeyCode::Char('t') => self.status = "trash is not supported".into(),
                _ => {}
            },
            Prefix::Sort => match key.code {
                KeyCode::Char('a') => self.set_sort(Sort::Name, false),
                KeyCode::Char('A') => self.set_sort(Sort::Name, true),
                KeyCode::Char('s') => self.set_sort(Sort::Size, false),
                KeyCode::Char('S') => self.set_sort(Sort::Size, true),
                KeyCode::Char('m') => self.set_sort(Sort::Time, false),
                KeyCode::Char('M') => self.set_sort(Sort::Time, true),
                KeyCode::Char('e') => self.set_sort(Sort::Extension, false),
                KeyCode::Char('E') => self.set_sort(Sort::Extension, true),
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
                let right = if e.is_dir {
                    String::new()
                } else {
                    human(e.size)
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
            Sort::Size => "size",
            Sort::Time => "time",
            Sort::Extension => "ext",
        };
        let direction = if self.sort_reverse { "↓" } else { "↑" };
        let mut text = match self.mode {
            Mode::Filter => format!("/{}", self.filter),
            Mode::Help => "rnn keys — press any key to close".into(),
            Mode::Prompt(Prompt::Delete) => {
                format!("delete {} item(s)? (y/N)", self.targets().len())
            }
            Mode::Prompt(p) => {
                let label = match p {
                    Prompt::Create => "create",
                    Prompt::Rename => "rename",
                    Prompt::Cd => "go to",
                    Prompt::Delete => "",
                };
                format!("{label}: {}", self.input)
            }
            Mode::Normal if !self.status.is_empty() => self.status.clone(),
            Mode::Normal => format!(
                "{} items | {} selected | sort:{sort}{direction}{}{}",
                self.entries.len(),
                self.selected.len(),
                if self.show_hidden { " | hidden" } else { "" },
                if self.filter.is_empty() {
                    String::new()
                } else {
                    format!(" | filter:{}", self.filter)
                },
            ),
        };
        if let Some(prefix) = self.prefix {
            text.push_str(match prefix {
                Prefix::Goto => " | g…",
                Prefix::Sort => " | ,…",
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

        if matches!(self.mode, Mode::Help) {
            self.draw_help(f);
        }
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
            "Ctrl+a      select all",
            "Ctrl+r      invert selection",
            "v / V       visual select / unset",
            "y / x       yank / cut",
            "p / P       paste / paste-force",
            "Y / X       cancel yank",
            "d / r       delete / rename",
        ];
        let right = [
            "a           create file or dir/",
            "o / Enter   open",
            "e / i       editor / pager",
            "J / K       seek preview",
            "+ / - / 0   zoom in / out / fit",
            "T           toggle preview pane",
            ".           hidden files",
            "/           filter",
            "gh gc gd    home / config / downloads",
            "g<Space>    go to path",
            "gf / gt     follow symlink / trash",
            ",a ,s ,m ,e sort (shift = reverse)",
            "~           close help",
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
