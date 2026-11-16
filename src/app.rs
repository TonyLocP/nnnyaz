use std::{
    collections::{BTreeSet, HashMap},
    env,
    ffi::{OsStr, OsString},
    fs,
    io::{self, BufRead, Read, Write},
    ops::Range,
    os::unix::{ffi::OsStrExt, fs::MetadataExt},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, TryRecvError},
        Arc, Mutex, OnceLock,
    },
    time::{Duration, Instant, SystemTime},
};

use ratatui::{
    crossterm::{
        event::{
            self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind,
            KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
        },
        execute,
    },
    layout::{Constraint, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Clear, List, ListItem, ListState, Paragraph},
    DefaultTerminal, Frame,
};

use crate::{
    action::Action,
    config::{
        self,
        keymap::{Context, KeyChord},
        Config, MOUSE_CLICK, MOUSE_DRAG, MOUSE_MOVE, MOUSE_SCROLL, MOUSE_TOUCH,
    },
    openwith,
    preview::{
        detect::{self, Kind},
        ImageProtocol, Key as PreviewKey, Preview,
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

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Sort {
    Name,
    Natural,
    Size,
    Time,
    Btime,
    Extension,
    Random,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Prompt {
    Create,
    Rename,
    Delete,
    Cd,
    SearchFd,
    SearchRg,
    Shell,
    TabRename,
}

/// A single-line prompt buffer with a cursor and an optional selection.
///
/// Indices are byte offsets on char boundaries, so editing multibyte names is
/// safe. The rename prompt starts with the stem selected and the cursor just
/// before the extension, like nnn and Yazi.
#[derive(Default)]
struct Input {
    text: String,
    cursor: usize,
    /// Selection anchor; the selection runs between this and `cursor`.
    anchor: Option<usize>,
}

impl Input {
    fn new(text: impl Into<String>) -> Self {
        let text = text.into();
        let cursor = text.len();
        Self {
            text,
            cursor,
            anchor: None,
        }
    }

    /// Start renaming `name`: select the stem, leave the extension intact.
    fn rename(name: &str) -> Self {
        let cursor = rename_stem_end(name);
        Self {
            text: name.to_string(),
            cursor,
            anchor: (cursor > 0).then_some(0),
        }
    }

    fn as_str(&self) -> &str {
        &self.text
    }

    fn clear(&mut self) {
        self.text.clear();
        self.cursor = 0;
        self.anchor = None;
    }

    fn set(&mut self, text: impl Into<String>) {
        *self = Self::new(text);
    }

    fn selection(&self) -> Option<Range<usize>> {
        let anchor = self.anchor?;
        (anchor != self.cursor).then(|| anchor.min(self.cursor)..anchor.max(self.cursor))
    }

    /// Replace the selection, if any, with `with`.
    fn replace_selection(&mut self, with: &str) {
        if let Some(range) = self.selection() {
            self.text.replace_range(range.clone(), with);
            self.cursor = range.start + with.len();
            self.anchor = None;
        }
    }

    fn insert(&mut self, c: char) {
        self.replace_selection("");
        self.text.insert(self.cursor, c);
        self.cursor += c.len_utf8();
    }

    fn backspace(&mut self) {
        if self.selection().is_some() {
            self.replace_selection("");
            return;
        }
        if let Some(prev) = self.prev_boundary() {
            self.text.remove(prev);
            self.cursor = prev;
        }
    }

    fn delete(&mut self) {
        if self.selection().is_some() {
            self.replace_selection("");
            return;
        }
        if self.cursor < self.text.len() {
            self.text.remove(self.cursor);
        }
    }

    fn prev_boundary(&self) -> Option<usize> {
        self.text[..self.cursor]
            .char_indices()
            .next_back()
            .map(|(i, _)| i)
    }

    fn next_boundary(&self) -> Option<usize> {
        self.text[self.cursor..]
            .char_indices()
            .nth(1)
            .map(|(i, _)| self.cursor + i)
    }

    fn move_left(&mut self) {
        if let Some(range) = self.selection() {
            self.cursor = range.start;
            self.anchor = None;
        } else if let Some(prev) = self.prev_boundary() {
            self.cursor = prev;
        }
    }

    fn move_right(&mut self) {
        if let Some(range) = self.selection() {
            self.cursor = range.end;
            self.anchor = None;
        } else if let Some(next) = self.next_boundary() {
            self.cursor = next;
        }
    }

    fn move_home(&mut self) {
        self.cursor = 0;
        self.anchor = None;
    }

    fn move_end(&mut self) {
        self.cursor = self.text.len();
        self.anchor = None;
    }

    fn select_left(&mut self) {
        let anchor = *self.anchor.get_or_insert(self.cursor);
        if let Some(prev) = self.prev_boundary() {
            self.cursor = prev;
        }
        if self.cursor == anchor {
            self.anchor = None;
        }
    }

    fn select_right(&mut self) {
        let anchor = *self.anchor.get_or_insert(self.cursor);
        if let Some(next) = self.next_boundary() {
            self.cursor = next;
        }
        if self.cursor == anchor {
            self.anchor = None;
        }
    }

    fn select_home(&mut self) {
        let anchor = *self.anchor.get_or_insert(self.cursor);
        self.cursor = 0;
        if self.cursor == anchor {
            self.anchor = None;
        }
    }

    fn select_end(&mut self) {
        let anchor = *self.anchor.get_or_insert(self.cursor);
        self.cursor = self.text.len();
        if self.cursor == anchor {
            self.anchor = None;
        }
    }

    fn select_all(&mut self) {
        self.cursor = self.text.len();
        self.anchor = (self.cursor > 0).then_some(0);
    }

    /// Delete from the cursor to the start of the line.
    fn kill_to_start(&mut self) {
        if self.selection().is_some() {
            self.replace_selection("");
            return;
        }
        self.text.drain(..self.cursor);
        self.cursor = 0;
    }

    /// Delete from the cursor to the end of the line.
    fn kill_to_end(&mut self) {
        if self.selection().is_some() {
            self.replace_selection("");
            return;
        }
        self.text.truncate(self.cursor);
    }

    /// Delete the word before the cursor (Ctrl+W).
    fn delete_word(&mut self) {
        if self.selection().is_some() {
            self.replace_selection("");
            return;
        }
        let start = self.text[..self.cursor]
            .char_indices()
            .rev()
            .skip_while(|(_, c)| c.is_whitespace())
            .find(|(_, c)| c.is_whitespace())
            .map_or(0, |(i, c)| i + c.len_utf8());
        self.text.drain(start..self.cursor);
        self.cursor = start;
    }
}

/// Byte offset of the last extension dot, or the name length when the whole
/// name is the stem (no extension, a dotfile, or a trailing dot).
fn rename_stem_end(name: &str) -> usize {
    let Some(dot) = name.rfind('.') else {
        return name.len();
    };
    let stem = &name[..dot];
    let suffix = &name[dot + 1..];
    if suffix.is_empty() || stem.is_empty() || stem.chars().all(|c| c == '.') {
        name.len()
    } else {
        dot
    }
}

/// Display width of one character in terminal cells.
fn char_width(c: char) -> usize {
    Span::raw(c.to_string()).width()
}

/// Byte offset of the first character that starts at or after display column
/// `col`.
fn byte_at_col(text: &str, col: usize) -> usize {
    let mut used = 0;
    for (i, c) in text.char_indices() {
        if used >= col {
            return i;
        }
        used += char_width(c);
    }
    text.len()
}

/// What a `d` / `D` confirmation should do when accepted.
#[derive(Clone, Copy, PartialEq)]
enum DeleteMode {
    Trash,
    Permanent,
    FromTrash,
    EmptyTrash,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Mode {
    Normal,
    Filter,
    Find,
    Prompt(Prompt),
    Help,
    Info,
    OpenWith,
    Tasks,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Linemode {
    Size,
    Permissions,
    Btime,
    Mtime,
    Owner,
    None,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum SearchKind {
    Fd,
    Rg,
}

#[derive(Clone)]
struct Search {
    kind: SearchKind,
    query: String,
    root: PathBuf,
    results: Vec<Entry>,
    done: bool,
}

/// Messages streamed from the background `fd`/`rg` search process.
enum SearchMsg {
    Entry(Entry),
    Done,
    Error(String),
}

/// Spawn `fd`/`rg` in the background and stream matching entries back. The
/// returned cancellation sender kills the child when dropped.
fn spawn_search(search: Search, show_hidden: bool) -> (Receiver<SearchMsg>, mpsc::Sender<()>) {
    let (tx, rx) = mpsc::channel();
    let (cancel_tx, cancel_rx) = mpsc::channel();
    std::thread::spawn(move || {
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
                cmd.arg(if show_hidden {
                    "--hidden"
                } else {
                    "--no-hidden"
                });
                cmd.arg("--").arg(&search.query);
            }
            SearchKind::Rg => {
                cmd.args(["--color=never", "--files-with-matches", "--smart-case"]);
                cmd.arg(if show_hidden {
                    "--hidden"
                } else {
                    "--no-hidden"
                });
                cmd.arg("--").arg(&search.query);
            }
        }
        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                let _ = tx.send(SearchMsg::Error(format!("{prog}: {e}")));
                return;
            }
        };
        let Some(stdout) = child.stdout.take() else {
            return;
        };
        let stderr = child.stderr.take().map(|mut s| {
            std::thread::spawn(move || {
                let mut buf = String::new();
                let _ = s.read_to_string(&mut buf);
                buf
            })
        });

        // Kill the child as soon as the app drops the cancellation sender, so
        // cancelling a slow scan does not leave `rg` running in the background.
        let child = Arc::new(Mutex::new(child));
        let killer = Arc::clone(&child);
        std::thread::spawn(move || {
            let _ = cancel_rx.recv();
            if let Ok(mut c) = killer.lock() {
                let _ = c.kill();
            }
        });

        let mut count = 0usize;
        let mut cancelled = false;
        for line in io::BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            let line = line.trim().trim_start_matches("./");
            if line.is_empty() {
                continue;
            }
            let path = search.root.join(line);
            let Some(entry) = entry_for(path, line.to_string()) else {
                continue;
            };
            if tx.send(SearchMsg::Entry(entry)).is_err() {
                cancelled = true;
                break;
            }
            count += 1;
            if count >= 5000 {
                cancelled = true;
                break;
            }
        }
        if cancelled {
            if let Ok(mut c) = child.lock() {
                let _ = c.kill();
            }
        }
        let status = child.lock().ok().and_then(|mut c| c.wait().ok());
        let err = stderr.and_then(|h| h.join().ok()).unwrap_or_default();
        if cancelled {
            return;
        }
        // `rg` exits 1 when nothing matched; that is not an error.
        let failed = status
            .map(|s| !s.success() && s.code() != Some(1))
            .unwrap_or(true);
        if failed && count == 0 {
            let msg = err.lines().next().unwrap_or("search failed");
            let _ = tx.send(SearchMsg::Error(format!("{prog}: {msg}")));
        } else {
            let _ = tx.send(SearchMsg::Done);
        }
    });
    (rx, cancel_tx)
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum CopyKind {
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

#[derive(Clone)]
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

/// A saved tab. The active tab lives in `App`'s fields; inactive tabs are
/// snapshots in `App::tabs`.
#[derive(Clone)]
struct TabSnapshot {
    cwd: PathBuf,
    entries: Vec<Entry>,
    cursor: Option<usize>,
    filter: String,
    show_hidden: bool,
    sort: Sort,
    sort_reverse: bool,
    linemode: Linemode,
    selected: BTreeSet<PathBuf>,
    visual: Option<Visual>,
    search: Option<Search>,
    history: Vec<PathBuf>,
    history_pos: usize,
    focus: Option<String>,
    tab_name: Option<String>,
}

/// One row in the `Tab` (spot/info) overlay.
enum InfoLine {
    Section(&'static str),
    Field(&'static str, String),
    Text(String),
}

/// Metadata shown by the `Tab` (spot/info) overlay.
struct Info {
    lines: Vec<InfoLine>,
}

/// The modal open-with menu (`O`).
struct OpenWith {
    path: PathBuf,
    options: Vec<openwith::Opener>,
    index: usize,
}

pub struct App {
    config: Config,
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
    pending: Vec<KeyChord>,
    visual: Option<Visual>,
    completions: Vec<Completion>,
    completion_index: usize,
    search: Option<Search>,
    search_rx: Option<Receiver<SearchMsg>>,
    search_cancel: Option<mpsc::Sender<()>>,
    find_query: String,
    find_forward: bool,
    find_origin: Option<usize>,
    last_find: Option<String>,
    linemode: Linemode,
    history: Vec<PathBuf>,
    history_pos: usize,
    input: Input,
    status: String,
    focus: Option<String>,
    page: usize,
    tabs: Vec<TabSnapshot>,
    tab_index: usize,
    tab_name: Option<String>,
    info: Option<Info>,
    info_cursor: usize,
    info_scroll: usize,
    info_from_tasks: bool,
    open_with: Option<OpenWith>,
    tasks: Vec<Task>,
    task_next_id: usize,
    task_cursor: usize,
    pending_delete: Option<DeleteMode>,
    quit_warned: bool,
    /// Set when picker mode chooses an entry; the run loop exits at the next
    /// opportunity.
    quit: bool,
    /// Where `quit_cwd` writes the current directory for a shell wrapper.
    cwd_file: Option<PathBuf>,
    /// Where picker mode writes selected paths (`-` means stdout).
    chooser_file: Option<PathBuf>,
    /// Separate picker paths with NUL instead of newline (`-0`).
    chooser_null: bool,
    /// Deferred `-p -` output, flushed after the terminal is restored.
    chooser_stdout: Option<Vec<u8>>,
    /// Local UTC offset captured at startup (before any threads spawn), used
    /// to render mtimes in local time without shelling out to `date`.
    local_offset: time::UtcOffset,
    preview: Preview,
    list: List<'static>,
    list_dirty: bool,
    list_width: u16,
    // Layout rectangles from the last frame, used for mouse hit-testing.
    title_area: Rect,
    list_area: Rect,
    preview_area: Rect,
    tab_spans: Vec<(u16, u16)>,
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

/// Turn relative pane weights (Yazi-style) into percentages that sum to 100.
fn ratio_percentages((list, preview): (u16, u16)) -> (u16, u16) {
    let total = u32::from(list) + u32::from(preview);
    if total == 0 {
        return (50, 50);
    }
    let first = ((u32::from(list) * 100) / total).clamp(1, 99) as u16;
    (first, 100 - first)
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

/// `ls`/nnn-style mode string such as `drwxr-xr-x` or `-rw-r--r--`.
fn mode_string(mode: u32) -> String {
    let kind = match mode & 0o170000 {
        0o040000 => 'd',
        0o120000 => 'l',
        0o010000 => 'p',
        0o020000 => 'c',
        0o060000 => 'b',
        0o140000 => 's',
        _ => '-',
    };
    let mut out = String::with_capacity(10);
    out.push(kind);
    let bits = mode & 0o777;
    for (i, shift) in [6, 3, 0].into_iter().enumerate() {
        let b = (bits >> shift) & 0o7;
        out.push(if b & 0o4 != 0 { 'r' } else { '-' });
        out.push(if b & 0o2 != 0 { 'w' } else { '-' });
        let special = match i {
            0 => mode & 0o4000 != 0,
            1 => mode & 0o2000 != 0,
            _ => mode & 0o1000 != 0,
        };
        let x = if b & 0o1 != 0 { 'x' } else { '-' };
        out.push(if special {
            match (i, x) {
                (2, 'x') => 't',
                (2, _) => 'T',
                (_, 'x') => 's',
                (_, _) => 'S',
            }
        } else {
            x
        });
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

/// Absolute local timestamp, via `date` (no timezone crate in the tree).
fn format_time(t: Option<SystemTime>) -> String {
    let Some(t) = t else {
        return "-".into();
    };
    let secs = t
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    match Command::new("date")
        .arg("-d")
        .arg(format!("@{secs}"))
        .arg("+%Y-%m-%d %H:%M:%S")
        .output()
    {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).trim().to_string(),
        _ => format!("@{secs}"),
    }
}

/// Cache of uid/gid → name lookups, keyed by (`/etc/passwd` or `/etc/group`, id).
type IdCache = HashMap<(String, u32), Option<String>>;

/// Look up a uid/gid name in `/etc/passwd` or `/etc/group` (field 2).
///
/// Results (including misses) are cached: the status line asks for the
/// hovered entry's owner and group on every frame, and re-reading the files
/// each time would be needlessly expensive.
fn id_name(file: &str, id: u32) -> Option<String> {
    static CACHE: OnceLock<Mutex<IdCache>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let key = (file.to_string(), id);
    if let Ok(c) = cache.lock() {
        if let Some(hit) = c.get(&key) {
            return hit.clone();
        }
    }
    let name = id_lookup(file, id);
    if let Ok(mut c) = cache.lock() {
        c.insert(key, name.clone());
    }
    name
}

fn id_lookup(file: &str, id: u32) -> Option<String> {
    let text = fs::read_to_string(file).ok()?;
    for line in text.lines() {
        let mut parts = line.split(':');
        let name = parts.next()?;
        if parts.nth(1)?.parse::<u32>().ok() == Some(id) {
            return Some(name.to_string());
        }
    }
    None
}

/// Local `YYYY-MM-DD HH:MM` timestamp for the status line, like nnn.
fn format_mtime(t: Option<SystemTime>, offset: time::UtcOffset) -> String {
    let Some(t) = t else {
        return "-".into();
    };
    let Ok(age) = t.duration_since(SystemTime::UNIX_EPOCH) else {
        return "-".into();
    };
    let Ok(dt) = time::OffsetDateTime::from_unix_timestamp(age.as_secs() as i64) else {
        return "-".into();
    };
    let dt = dt.to_offset(offset);
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}",
        dt.year(),
        dt.month() as u8,
        dt.day(),
        dt.hour(),
        dt.minute()
    )
}

/// nnn-style trailing field: the symlink target (`->foo`) for links, the
/// extension (`.txt`) for files, and nothing for directories.
fn extension_label(entry: &Entry, md: Option<&fs::Metadata>) -> String {
    if md.map(|m| m.file_type().is_symlink()).unwrap_or(false) {
        return fs::read_link(&entry.path)
            .map(|t| format!("->{}", t.display()))
            .unwrap_or_default();
    }
    if entry.is_dir {
        return String::new();
    }
    match entry.name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() && !ext.is_empty() => format!(".{ext}"),
        _ => String::new(),
    }
}

fn format_duration(secs: f64) -> String {
    let total = secs.max(0.0) as u64;
    let (h, m, s) = (total / 3600, (total % 3600) / 60, total % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

#[derive(Default)]
struct VideoInfo {
    width: Option<u32>,
    height: Option<u32>,
    duration: Option<f64>,
    codec: Option<String>,
}

/// Resolution, duration and codec of a video, via `ffprobe` when installed.
fn video_info(path: &Path) -> VideoInfo {
    let mut info = VideoInfo::default();
    let Ok(out) = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_entries",
            "stream=width,height,codec_name:format=duration",
            "-of",
            "default=noprint_wrappers=1",
        ])
        .arg(path)
        .output()
    else {
        return info;
    };
    if !out.status.success() {
        return info;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    for line in text.lines() {
        if let Some(v) = line.strip_prefix("width=") {
            info.width = v.trim().parse::<u32>().ok();
        } else if let Some(v) = line.strip_prefix("height=") {
            info.height = v.trim().parse::<u32>().ok();
        } else if let Some(v) = line.strip_prefix("duration=") {
            info.duration = v.trim().parse::<f64>().ok();
        } else if let Some(v) = line.strip_prefix("codec_name=") {
            info.codec = Some(v.trim().to_string());
        }
    }
    info
}

/// Recursively total the regular files under `path`, stopping after `limit`
/// so a huge tree can't freeze the UI. The bool is false when the walk was
/// cut short (shown as `(?)`, like yazi's live folder size).
fn dir_size(path: &Path, limit: Duration) -> (u64, bool) {
    let start = std::time::Instant::now();
    let mut total = 0u64;
    let mut stack = vec![path.to_path_buf()];
    while let Some(dir) = stack.pop() {
        if start.elapsed() > limit {
            return (total, false);
        }
        let Ok(rd) = fs::read_dir(&dir) else {
            continue;
        };
        for ent in rd.flatten() {
            let Ok(ft) = ent.file_type() else {
                continue;
            };
            if ft.is_dir() {
                stack.push(ent.path());
            } else if ft.is_file() {
                if let Ok(meta) = ent.metadata() {
                    total = total.saturating_add(meta.len());
                }
            }
        }
    }
    (total, true)
}

fn truncate_chars(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(n.saturating_sub(1)).collect();
        out.push('…');
        out
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

// ───────────────────────── trash ─────────────────────────

/// Freedesktop trash root (`$XDG_DATA_HOME/Trash` or `~/.local/share/Trash`).
fn trash_root() -> Option<PathBuf> {
    if let Some(dir) = env::var_os("XDG_DATA_HOME").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(dir).join("Trash"));
    }
    env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share/Trash"))
}

fn trash_files_dir() -> Option<PathBuf> {
    trash_root().map(|root| root.join("files"))
}

fn trash_info_dir() -> Option<PathBuf> {
    trash_root().map(|root| root.join("info"))
}

/// The trash directory as the app sees it (canonicalized when possible).
fn trash_view_path() -> Option<PathBuf> {
    let files = trash_files_dir()?;
    Some(fs::canonicalize(&files).unwrap_or(files))
}

fn ensure_trash_dirs() -> io::Result<(PathBuf, PathBuf)> {
    let root = trash_root().ok_or_else(|| {
        io::Error::new(io::ErrorKind::NotFound, "HOME and XDG_DATA_HOME are unset")
    })?;
    let files = root.join("files");
    let info = root.join("info");
    fs::create_dir_all(&files)?;
    fs::create_dir_all(&info)?;
    Ok((files, info))
}

/// Percent-encode a path for the `Path=` key of a `.trashinfo` file.
fn percent_encode_path(path: &Path) -> String {
    use std::os::unix::ffi::OsStrExt;
    let mut out = String::new();
    for &b in path.as_os_str().as_bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'/' | b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn percent_decode_path(value: &str) -> PathBuf {
    use std::os::unix::ffi::OsStringExt;
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(hex) = std::str::from_utf8(&bytes[i + 1..i + 3]) {
                if let Ok(v) = u8::from_str_radix(hex, 16) {
                    out.push(v);
                    i += 3;
                    continue;
                }
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    PathBuf::from(std::ffi::OsString::from_vec(out))
}

fn deletion_date() -> String {
    Command::new("date")
        .arg("+%Y-%m-%dT%H:%M:%S")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|| "1970-01-01T00:00:00".into())
}

#[derive(Clone)]
struct TrashInfo {
    name: String,
    original: PathBuf,
    date: String,
}

fn parse_trashinfo(path: &Path) -> Option<TrashInfo> {
    let text = fs::read_to_string(path).ok()?;
    let mut original = None;
    let mut date = String::new();
    for line in text.lines() {
        if let Some(v) = line.strip_prefix("Path=") {
            original = Some(percent_decode_path(v.trim_end()));
        } else if let Some(v) = line.strip_prefix("DeletionDate=") {
            date = v.trim().to_string();
        }
    }
    let name = path
        .file_name()?
        .to_string_lossy()
        .strip_suffix(".trashinfo")?
        .to_string();
    Some(TrashInfo {
        name,
        original: original?,
        date,
    })
}

fn trash_info_for_name(name: &OsStr) -> Option<PathBuf> {
    let info = trash_info_dir()?;
    let mut file = name.to_os_string();
    file.push(".trashinfo");
    let path = info.join(file);
    path.is_file().then_some(path)
}

/// Reserve a unique slot in the trash and write its `.trashinfo` file.
/// Returns `(path inside files/, path of the .trashinfo)`.
fn prepare_trash(path: &Path) -> io::Result<(PathBuf, PathBuf)> {
    let (files, info) = ensure_trash_dirs()?;
    if let Some(root) = trash_root() {
        if path.starts_with(&root) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "already in the trash",
            ));
        }
    }
    let name = path
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "cannot trash a root path"))?;

    let mut candidate = name.to_os_string();
    let mut n = 1usize;
    let (mut dst, mut info_path) = {
        let mut info_name = candidate.clone();
        info_name.push(".trashinfo");
        (files.join(&candidate), info.join(info_name))
    };
    while dst.exists() || info_path.exists() {
        candidate = name.to_os_string();
        candidate.push(format!(".{n}"));
        let mut info_name = candidate.clone();
        info_name.push(".trashinfo");
        dst = files.join(&candidate);
        info_path = info.join(info_name);
        n += 1;
    }

    let original = if path.is_absolute() {
        path.to_path_buf()
    } else {
        env::current_dir()?.join(path)
    };
    let body = format!(
        "[Trash Info]\nPath={}\nDeletionDate={}\n",
        percent_encode_path(&original),
        deletion_date()
    );
    fs::write(&info_path, body)?;
    Ok((dst, info_path))
}

fn unique_restore(path: PathBuf) -> PathBuf {
    if !path.exists() {
        return path;
    }
    let base = path;
    let mut n = 1usize;
    loop {
        let mut candidate = base.clone().into_os_string();
        if n == 1 {
            candidate.push(".restored");
        } else {
            candidate.push(format!(".restored.{n}"));
        }
        let candidate = PathBuf::from(candidate);
        if !candidate.exists() {
            return candidate;
        }
        n += 1;
    }
}

fn restore_trash(info_path: &Path) -> io::Result<PathBuf> {
    let info = parse_trashinfo(info_path)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "invalid .trashinfo file"))?;
    let files = trash_files_dir()
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "trash is unavailable"))?;
    let src = files.join(&info.name);
    let target = unique_restore(info.original);
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)?;
    }
    match fs::rename(&src, &target) {
        Ok(()) => {}
        Err(_) => {
            copy_rec(&src, &target)?;
            remove(&src)?;
        }
    }
    let _ = fs::remove_file(info_path);
    Ok(target)
}

// ───────────────────────── tasks ─────────────────────────

#[derive(Clone, Copy, PartialEq, Eq)]
enum TaskKind {
    Copy,
    Move,
    Delete,
    Trash,
    Restore,
    EmptyTrash,
    Link,
    Hardlink,
}

impl TaskKind {
    fn label(self) -> &'static str {
        match self {
            TaskKind::Copy => "copy",
            TaskKind::Move => "move",
            TaskKind::Delete => "delete",
            TaskKind::Trash => "trash",
            TaskKind::Restore => "restore",
            TaskKind::EmptyTrash => "empty trash",
            TaskKind::Link => "symlink",
            TaskKind::Hardlink => "hardlink",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum TaskStatus {
    Running,
    Done,
    Failed,
    Cancelled,
}

impl TaskStatus {
    fn label(self) -> &'static str {
        match self {
            TaskStatus::Running => "running",
            TaskStatus::Done => "done",
            TaskStatus::Failed => "failed",
            TaskStatus::Cancelled => "cancelled",
        }
    }
}

#[derive(Clone, Default)]
struct TaskProgress {
    total: usize,
    done: usize,
    current: String,
    errors: Vec<String>,
}

struct TaskState {
    status: TaskStatus,
    progress: TaskProgress,
}

struct Task {
    id: usize,
    kind: TaskKind,
    title: String,
    state: Arc<Mutex<TaskState>>,
    cancel: Arc<AtomicBool>,
    started: Instant,
    finished: Option<Instant>,
}

enum TaskOp {
    Paste {
        pairs: Vec<(PathBuf, PathBuf)>,
        cut: bool,
    },
    Delete {
        paths: Vec<PathBuf>,
    },
    DeleteTrash {
        paths: Vec<PathBuf>,
        infos: Vec<PathBuf>,
    },
    Trash {
        paths: Vec<PathBuf>,
    },
    Restore {
        infos: Vec<PathBuf>,
    },
    EmptyTrash,
    Link {
        pairs: Vec<(PathBuf, PathBuf)>,
        relative: bool,
    },
    Hardlink {
        pairs: Vec<(PathBuf, PathBuf)>,
    },
}

fn task_lock(state: &Arc<Mutex<TaskState>>) -> Option<std::sync::MutexGuard<'_, TaskState>> {
    state.lock().ok()
}

fn task_set_total(state: &Arc<Mutex<TaskState>>, total: usize) {
    if let Some(mut s) = task_lock(state) {
        s.progress.total = total;
    }
}

fn task_add(state: &Arc<Mutex<TaskState>>, n: usize) {
    if let Some(mut s) = task_lock(state) {
        s.progress.done = s.progress.done.saturating_add(n);
    }
}

fn task_current(state: &Arc<Mutex<TaskState>>, path: &Path) {
    if let Some(mut s) = task_lock(state) {
        s.progress.current = path.display().to_string();
    }
}

fn task_error(state: &Arc<Mutex<TaskState>>, message: String) {
    if let Some(mut s) = task_lock(state) {
        if s.progress.errors.len() < 50 {
            s.progress.errors.push(message);
        }
    }
}

fn task_cancelled(cancel: &AtomicBool) -> bool {
    cancel.load(Ordering::Relaxed)
}

fn cancelled_error() -> io::Error {
    io::Error::new(io::ErrorKind::Interrupted, "cancelled")
}

fn task_percent(progress: &TaskProgress) -> u16 {
    if progress.total == 0 {
        return if progress.done > 0 { 100 } else { 0 };
    }
    ((progress.done.min(progress.total) * 100) / progress.total) as u16
}

fn progress_bar(percent: u16, width: usize) -> String {
    let filled = (percent as usize * width / 100).min(width);
    format!("{}{}", "█".repeat(filled), "░".repeat(width - filled))
}

const TASK_COUNT_LIMIT: usize = 200_000;

/// Count entries in a tree (without following symlinks), capped so a huge
/// directory cannot spend forever just preparing a progress total.
fn count_tree(paths: &[PathBuf]) -> usize {
    let mut total = 0usize;
    let mut stack: Vec<PathBuf> = paths.to_vec();
    while let Some(path) = stack.pop() {
        total += 1;
        if total >= TASK_COUNT_LIMIT {
            break;
        }
        if let Ok(md) = fs::symlink_metadata(&path) {
            if md.is_dir() {
                if let Ok(rd) = fs::read_dir(&path) {
                    for entry in rd.flatten() {
                        stack.push(entry.path());
                    }
                }
            }
        }
    }
    total
}

fn copy_task(
    src: &Path,
    dst: &Path,
    state: &Arc<Mutex<TaskState>>,
    cancel: &AtomicBool,
) -> io::Result<()> {
    if task_cancelled(cancel) {
        return Err(cancelled_error());
    }
    let md = fs::symlink_metadata(src)?;
    if md.is_dir() {
        if dst.starts_with(src) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "cannot copy a directory into itself",
            ));
        }
        fs::create_dir_all(dst)?;
        task_add(state, 1);
        for entry in fs::read_dir(src)? {
            let entry = entry?;
            copy_task(&entry.path(), &dst.join(entry.file_name()), state, cancel)?;
        }
        Ok(())
    } else {
        task_current(state, src);
        fs::copy(src, dst)?;
        task_add(state, 1);
        Ok(())
    }
}

/// Remove a path without touching progress (used after a cross-device copy).
fn remove_raw(path: &Path) -> io::Result<()> {
    if fs::symlink_metadata(path)?.is_dir() {
        for entry in fs::read_dir(path)? {
            remove_raw(&entry?.path())?;
        }
        fs::remove_dir(path)
    } else {
        fs::remove_file(path)
    }
}

fn remove_task(path: &Path, state: &Arc<Mutex<TaskState>>, cancel: &AtomicBool) -> io::Result<()> {
    if task_cancelled(cancel) {
        return Err(cancelled_error());
    }
    let md = fs::symlink_metadata(path)?;
    if md.is_dir() {
        for entry in fs::read_dir(path)? {
            let entry = entry?;
            remove_task(&entry.path(), state, cancel)?;
        }
        fs::remove_dir(path)?;
    } else {
        fs::remove_file(path)?;
    }
    task_add(state, 1);
    Ok(())
}

fn run_task(op: TaskOp, state: &Arc<Mutex<TaskState>>, cancel: &Arc<AtomicBool>) {
    let result = match op {
        TaskOp::Paste { pairs, cut } => run_paste(pairs, cut, state, cancel),
        TaskOp::Delete { paths } => run_delete(paths, state, cancel),
        TaskOp::DeleteTrash { paths, infos } => run_delete_trash(paths, infos, state, cancel),
        TaskOp::Trash { paths } => run_trash(paths, state, cancel),
        TaskOp::Restore { infos } => run_restore(infos, state, cancel),
        TaskOp::EmptyTrash => run_empty_trash(state, cancel),
        TaskOp::Link { pairs, relative } => run_link(pairs, relative, state, cancel),
        TaskOp::Hardlink { pairs } => run_hardlink(pairs, state, cancel),
    };
    if let Some(mut s) = task_lock(state) {
        s.status = if task_cancelled(cancel) {
            TaskStatus::Cancelled
        } else if result.is_ok() {
            TaskStatus::Done
        } else {
            TaskStatus::Failed
        };
        if let Err(e) = result {
            if s.progress.errors.len() < 50 {
                s.progress.errors.push(e.to_string());
            }
        }
    }
}

fn run_paste(
    pairs: Vec<(PathBuf, PathBuf)>,
    cut: bool,
    state: &Arc<Mutex<TaskState>>,
    cancel: &Arc<AtomicBool>,
) -> io::Result<()> {
    let counts: Vec<usize> = pairs
        .iter()
        .map(|(src, _)| count_tree(std::slice::from_ref(src)))
        .collect();
    task_set_total(state, counts.iter().sum());
    let mut first_err = None;
    for (i, (src, dst)) in pairs.iter().enumerate() {
        if task_cancelled(cancel) {
            return Err(cancelled_error());
        }
        task_current(state, src);
        let r = if cut {
            match fs::rename(src, dst) {
                Ok(()) => {
                    task_add(state, counts[i]);
                    Ok(())
                }
                Err(_) => copy_task(src, dst, state, cancel).and_then(|_| remove_raw(src)),
            }
        } else {
            copy_task(src, dst, state, cancel)
        };
        if let Err(e) = r {
            if task_cancelled(cancel) {
                return Err(cancelled_error());
            }
            task_error(state, format!("{}: {e}", src.display()));
            first_err.get_or_insert(e);
        }
    }
    first_err.map_or(Ok(()), Err)
}

fn run_delete(
    paths: Vec<PathBuf>,
    state: &Arc<Mutex<TaskState>>,
    cancel: &Arc<AtomicBool>,
) -> io::Result<()> {
    task_set_total(state, count_tree(&paths));
    let mut first_err = None;
    for path in &paths {
        if task_cancelled(cancel) {
            return Err(cancelled_error());
        }
        task_current(state, path);
        if let Err(e) = remove_task(path, state, cancel) {
            if task_cancelled(cancel) {
                return Err(cancelled_error());
            }
            task_error(state, format!("{}: {e}", path.display()));
            first_err.get_or_insert(e);
        }
    }
    first_err.map_or(Ok(()), Err)
}

fn run_delete_trash(
    paths: Vec<PathBuf>,
    infos: Vec<PathBuf>,
    state: &Arc<Mutex<TaskState>>,
    cancel: &Arc<AtomicBool>,
) -> io::Result<()> {
    task_set_total(state, count_tree(&paths) + infos.len());
    let mut first_err = None;
    for path in &paths {
        if task_cancelled(cancel) {
            return Err(cancelled_error());
        }
        task_current(state, path);
        if let Err(e) = remove_task(path, state, cancel) {
            if task_cancelled(cancel) {
                return Err(cancelled_error());
            }
            task_error(state, format!("{}: {e}", path.display()));
            first_err.get_or_insert(e);
        }
    }
    for info in &infos {
        if task_cancelled(cancel) {
            return Err(cancelled_error());
        }
        if let Err(e) = fs::remove_file(info) {
            if e.kind() != io::ErrorKind::NotFound {
                task_error(state, format!("{}: {e}", info.display()));
                first_err.get_or_insert(e);
            }
        }
        task_add(state, 1);
    }
    first_err.map_or(Ok(()), Err)
}

fn run_trash(
    paths: Vec<PathBuf>,
    state: &Arc<Mutex<TaskState>>,
    cancel: &Arc<AtomicBool>,
) -> io::Result<()> {
    let counts: Vec<usize> = paths
        .iter()
        .map(|path| count_tree(std::slice::from_ref(path)))
        .collect();
    task_set_total(state, counts.iter().sum());
    let mut first_err = None;
    for (i, path) in paths.iter().enumerate() {
        if task_cancelled(cancel) {
            return Err(cancelled_error());
        }
        task_current(state, path);
        let (dst, info) = match prepare_trash(path) {
            Ok(v) => v,
            Err(e) => {
                task_error(state, format!("{}: {e}", path.display()));
                first_err.get_or_insert(e);
                continue;
            }
        };
        let r = match fs::rename(path, &dst) {
            Ok(()) => {
                task_add(state, counts[i]);
                Ok(())
            }
            Err(_) => match copy_task(path, &dst, state, cancel) {
                Ok(()) => remove_raw(path),
                Err(e) => {
                    let _ = remove_raw(&dst);
                    Err(e)
                }
            },
        };
        if let Err(e) = r {
            let _ = fs::remove_file(&info);
            if task_cancelled(cancel) {
                return Err(cancelled_error());
            }
            task_error(state, format!("{}: {e}", path.display()));
            first_err.get_or_insert(e);
        }
    }
    first_err.map_or(Ok(()), Err)
}

fn run_restore(
    infos: Vec<PathBuf>,
    state: &Arc<Mutex<TaskState>>,
    cancel: &Arc<AtomicBool>,
) -> io::Result<()> {
    task_set_total(state, infos.len());
    let mut first_err = None;
    for info in &infos {
        if task_cancelled(cancel) {
            return Err(cancelled_error());
        }
        task_current(state, info);
        if let Err(e) = restore_trash(info) {
            if task_cancelled(cancel) {
                return Err(cancelled_error());
            }
            task_error(state, format!("{}: {e}", info.display()));
            first_err.get_or_insert(e);
        }
        task_add(state, 1);
    }
    first_err.map_or(Ok(()), Err)
}

fn run_empty_trash(state: &Arc<Mutex<TaskState>>, cancel: &Arc<AtomicBool>) -> io::Result<()> {
    let (files, info) = ensure_trash_dirs()?;
    let mut paths = Vec::new();
    if let Ok(rd) = fs::read_dir(&files) {
        for entry in rd.flatten() {
            paths.push(entry.path());
        }
    }
    let mut infos = Vec::new();
    if let Ok(rd) = fs::read_dir(&info) {
        for entry in rd.flatten() {
            infos.push(entry.path());
        }
    }
    task_set_total(state, count_tree(&paths) + infos.len());
    let mut first_err = None;
    for path in &paths {
        if task_cancelled(cancel) {
            return Err(cancelled_error());
        }
        task_current(state, path);
        if let Err(e) = remove_task(path, state, cancel) {
            if task_cancelled(cancel) {
                return Err(cancelled_error());
            }
            task_error(state, format!("{}: {e}", path.display()));
            first_err.get_or_insert(e);
        }
    }
    for info_path in &infos {
        if task_cancelled(cancel) {
            return Err(cancelled_error());
        }
        if let Err(e) = fs::remove_file(info_path) {
            if e.kind() != io::ErrorKind::NotFound {
                task_error(state, format!("{}: {e}", info_path.display()));
                first_err.get_or_insert(e);
            }
        }
        task_add(state, 1);
    }
    first_err.map_or(Ok(()), Err)
}

fn run_link(
    pairs: Vec<(PathBuf, PathBuf)>,
    relative: bool,
    state: &Arc<Mutex<TaskState>>,
    cancel: &Arc<AtomicBool>,
) -> io::Result<()> {
    task_set_total(state, pairs.len());
    let mut first_err = None;
    for (src, dst) in &pairs {
        if task_cancelled(cancel) {
            return Err(cancelled_error());
        }
        task_current(state, src);
        let target = if relative {
            relative_path(dst.parent().unwrap_or(Path::new(".")), src)
        } else {
            src.clone()
        };
        if let Err(e) = std::os::unix::fs::symlink(&target, dst) {
            task_error(state, format!("{}: {e}", dst.display()));
            first_err.get_or_insert(e);
        }
        task_add(state, 1);
    }
    first_err.map_or(Ok(()), Err)
}

fn run_hardlink(
    pairs: Vec<(PathBuf, PathBuf)>,
    state: &Arc<Mutex<TaskState>>,
    cancel: &Arc<AtomicBool>,
) -> io::Result<()> {
    task_set_total(state, pairs.len());
    let mut first_err = None;
    for (src, dst) in &pairs {
        if task_cancelled(cancel) {
            return Err(cancelled_error());
        }
        task_current(state, src);
        if let Err(e) = fs::hard_link(src, dst) {
            task_error(state, format!("{}: {e}", dst.display()));
            first_err.get_or_insert(e);
        }
        task_add(state, 1);
    }
    first_err.map_or(Ok(()), Err)
}

// ───────────────────────── app ─────────────────────────

/// Command-line options. rnn has no argument-parser dependency, so this stays
/// deliberately small: one optional starting path, `--cwd-file`, and the
/// picker options (`--chooser-file` / `-p`, `-0`).
#[derive(Debug, Default, PartialEq)]
struct Cli {
    path: Option<PathBuf>,
    cwd_file: Option<PathBuf>,
    chooser_file: Option<PathBuf>,
    chooser_null: bool,
}

/// Parse rnn's command line. `--cwd-file <path>` and `--cwd-file=<path>` are
/// accepted; so are `--chooser-file <path>`, `--chooser-file=<path>`, nnn's
/// `-p <path>` (or `-p<path>`), and `-0` for NUL-separated picker output. The
/// first non-option argument is the starting path.
fn parse_cli<I: IntoIterator<Item = OsString>>(args: I) -> Result<Cli, String> {
    let mut cli = Cli::default();
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        let text = arg.to_string_lossy();
        if text == "--cwd-file" {
            let value = args
                .next()
                .ok_or_else(|| "--cwd-file requires a path".to_string())?;
            cli.cwd_file = Some(PathBuf::from(value));
        } else if let Some(value) = text.strip_prefix("--cwd-file=") {
            if value.is_empty() {
                return Err("--cwd-file requires a path".into());
            }
            cli.cwd_file = Some(PathBuf::from(value));
        } else if text == "--chooser-file" {
            let value = args
                .next()
                .ok_or_else(|| "--chooser-file requires a path".to_string())?;
            cli.chooser_file = Some(PathBuf::from(value));
        } else if let Some(value) = text.strip_prefix("--chooser-file=") {
            if value.is_empty() {
                return Err("--chooser-file requires a path".into());
            }
            cli.chooser_file = Some(PathBuf::from(value));
        } else if text == "-p" {
            let value = args
                .next()
                .ok_or_else(|| "-p requires a path".to_string())?;
            cli.chooser_file = Some(PathBuf::from(value));
        } else if let Some(value) = text.strip_prefix("-p") {
            if value.is_empty() {
                return Err("-p requires a path".into());
            }
            cli.chooser_file = Some(PathBuf::from(value));
        } else if text == "-0" || text == "--null" {
            cli.chooser_null = true;
        } else if text == "--" {
            if let Some(path) = args.next() {
                if cli.path.replace(PathBuf::from(path)).is_some() {
                    return Err("too many arguments".into());
                }
            }
            if args.next().is_some() {
                return Err("too many arguments".into());
            }
        } else if text.starts_with('-') && text != "-" {
            return Err(format!("unknown option '{text}'"));
        } else if cli.path.replace(PathBuf::from(arg)).is_some() {
            return Err("too many arguments".into());
        }
    }
    Ok(cli)
}

/// Serialize picker paths: one per line, or NUL-separated with `-0`.
fn chooser_bytes(paths: &[PathBuf], null: bool) -> Vec<u8> {
    let sep = if null { b'\0' } else { b'\n' };
    let mut out = Vec::new();
    for path in paths {
        out.extend_from_slice(path.as_os_str().as_bytes());
        out.push(sep);
    }
    out
}

/// Write `cwd` to the cwd file so a shell wrapper can `cd` there after rnn
/// exits. Failures are reported on stderr, which is visible once the terminal
/// is restored, and do not prevent quitting.
fn write_cwd_file(path: &Path, cwd: &Path) {
    if let Err(e) = fs::write(path, format!("{}\n", cwd.display())) {
        eprintln!("rnn: cannot write cwd file {}: {e}", path.display());
    }
}

impl App {
    pub fn new(loaded: config::Loaded) -> io::Result<Self> {
        let cli = parse_cli(env::args_os().skip(1))
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
        let cwd_file = cli.cwd_file.or_else(|| {
            env::var_os("RNN_CWD_FILE")
                .filter(|v| !v.is_empty())
                .map(PathBuf::from)
        });
        let chooser_file = cli.chooser_file.or_else(|| {
            env::var_os("RNN_CHOOSER_FILE")
                .filter(|v| !v.is_empty())
                .map(PathBuf::from)
        });
        let chooser_null = cli.chooser_null;
        let start = cli.path.unwrap_or(env::current_dir()?);
        let cwd = fs::canonicalize(start)?;
        let config::Loaded { config, warnings } = loaded;
        let status = warnings.join(" · ");
        let mgr = config.mgr;
        // `time` refuses to read the local offset once the process is
        // multi-threaded, so capture it now (main is still single-threaded).
        let local_offset = time::UtcOffset::current_local_offset().unwrap_or(time::UtcOffset::UTC);
        let mut app = App {
            config,
            cwd: cwd.clone(),
            entries: vec![],
            state: ListState::default(),
            filter: String::new(),
            show_hidden: mgr.show_hidden,
            show_preview: mgr.show_preview,
            sort: mgr.sort,
            sort_reverse: mgr.sort_reverse,
            selected: BTreeSet::new(),
            yank: None,
            mode: Mode::Normal,
            pending: Vec::new(),
            visual: None,
            completions: Vec::new(),
            completion_index: 0,
            search: None,
            search_rx: None,
            search_cancel: None,
            find_query: String::new(),
            find_forward: true,
            find_origin: None,
            last_find: None,
            linemode: mgr.linemode,
            history: vec![cwd],
            history_pos: 0,
            input: Input::default(),
            status,
            focus: None,
            page: 10,
            tabs: Vec::new(),
            tab_index: 0,
            tab_name: None,
            info: None,
            info_cursor: 0,
            info_scroll: 0,
            info_from_tasks: false,
            open_with: None,
            tasks: Vec::new(),
            task_next_id: 1,
            task_cursor: 0,
            pending_delete: None,
            quit_warned: false,
            quit: false,
            cwd_file,
            chooser_file,
            chooser_null,
            chooser_stdout: None,
            local_offset,
            preview: Preview::new(),
            list: List::new(Vec::<ListItem>::new()),
            list_dirty: true,
            list_width: 0,
            title_area: Rect::default(),
            list_area: Rect::default(),
            preview_area: Rect::default(),
            tab_spans: Vec::new(),
        };
        app.preview.set_cache_size(app.config.preview.cache_size);
        app.reload();
        app.tabs = vec![app.snapshot()];
        Ok(app)
    }

    // ─────────────── tabs ───────────────

    /// Capture the active tab's state. The active tab is also stored as a
    /// (possibly stale) snapshot in `tabs[tab_index]`; it is refreshed before
    /// every switch or structural change.
    fn snapshot(&self) -> TabSnapshot {
        TabSnapshot {
            cwd: self.cwd.clone(),
            entries: self.entries.clone(),
            cursor: self.state.selected(),
            filter: self.filter.clone(),
            show_hidden: self.show_hidden,
            sort: self.sort,
            sort_reverse: self.sort_reverse,
            linemode: self.linemode,
            selected: self.selected.clone(),
            visual: self.visual.clone(),
            search: self.search.clone(),
            history: self.history.clone(),
            history_pos: self.history_pos,
            focus: self.focus.clone(),
            tab_name: self.tab_name.clone(),
        }
    }

    fn save_active(&mut self) {
        let snap = self.snapshot();
        if let Some(slot) = self.tabs.get_mut(self.tab_index) {
            *slot = snap;
        }
    }

    fn restore_tab(&mut self, tab: TabSnapshot) {
        self.cwd = tab.cwd;
        self.entries = tab.entries;
        self.filter = tab.filter;
        self.show_hidden = tab.show_hidden;
        self.sort = tab.sort;
        self.sort_reverse = tab.sort_reverse;
        self.linemode = tab.linemode;
        self.selected = tab.selected;
        self.visual = tab.visual;
        self.search = tab.search;
        self.history = tab.history;
        self.history_pos = tab.history_pos;
        self.focus = tab.focus;
        self.tab_name = tab.tab_name;
        self.stop_search_job();
        self.state
            .select(tab.cursor.filter(|i| *i < self.entries.len()));
        self.pending.clear();
        self.mode = Mode::Normal;
        self.info = None;
        self.input.clear();
        self.completions.clear();
        self.list_dirty = true;
    }

    fn new_tab(&mut self, path: PathBuf) {
        if let Err(e) = fs::read_dir(&path) {
            self.status = format!("{}: {e}", path.display());
            return;
        }
        self.save_active();
        let mut tab = self.snapshot();
        tab.cwd = path.clone();
        tab.entries.clear();
        tab.cursor = None;
        tab.filter.clear();
        tab.selected.clear();
        tab.visual = None;
        tab.search = None;
        tab.history = vec![path];
        tab.history_pos = 0;
        tab.focus = None;
        tab.tab_name = None;
        self.tabs.push(tab);
        self.tab_index = self.tabs.len() - 1;
        let tab = self.tabs[self.tab_index].clone();
        self.restore_tab(tab);
        self.reload();
        self.status = format!("tab {} created", self.tab_index + 1);
    }

    fn switch_tab(&mut self, index: usize) {
        if index >= self.tabs.len() || index == self.tab_index {
            return;
        }
        self.save_active();
        self.tab_index = index;
        let tab = self.tabs[index].clone();
        self.restore_tab(tab);
    }

    fn next_tab(&mut self, delta: isize) {
        if self.tabs.len() <= 1 {
            return;
        }
        let n = self.tabs.len() as isize;
        let index = (self.tab_index as isize + delta).rem_euclid(n) as usize;
        self.switch_tab(index);
    }

    fn close_tab(&mut self) {
        if self.tabs.len() <= 1 {
            self.status = "last tab".into();
            return;
        }
        let closed = self.tab_index;
        self.tabs.remove(closed);
        if self.tab_index >= self.tabs.len() {
            self.tab_index = self.tabs.len() - 1;
        }
        let tab = self.tabs[self.tab_index].clone();
        self.restore_tab(tab);
        self.status = format!("closed tab {}", closed + 1);
    }

    /// Swap the active tab's position with its neighbour (`{` / `}`).
    fn swap_tab(&mut self, delta: isize) {
        if self.tabs.len() <= 1 {
            return;
        }
        self.save_active();
        let n = self.tabs.len() as isize;
        let target = (self.tab_index as isize + delta).rem_euclid(n) as usize;
        self.tabs.swap(self.tab_index, target);
        self.tab_index = target;
    }

    fn tab_cwd(&self, index: usize) -> &Path {
        if index == self.tab_index {
            &self.cwd
        } else {
            &self.tabs[index].cwd
        }
    }

    /// Label shown for a tab in the tab bar (custom name, else cwd name).
    fn tab_label(&self, index: usize) -> String {
        let name = if index == self.tab_index {
            self.tab_name.as_ref()
        } else {
            self.tabs.get(index).and_then(|t| t.tab_name.as_ref())
        };
        if let Some(name) = name {
            return name.clone();
        }
        let cwd = self.tab_cwd(index);
        if trash_view_path().as_deref() == Some(cwd) {
            return "trash".into();
        }
        cwd.file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| cwd.display().to_string())
    }

    /// Enable image previews (call before `run`).
    pub fn preview_configure(&mut self, protocol: ImageProtocol, font: (u16, u16), shm: bool) {
        self.preview.configure(protocol, font, shm);
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
            if e.is_dir || !matches!(detect::detect(&e.path), Kind::Image | Kind::Video) {
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
        if let Some(search) = &self.search {
            v = search.results.clone();
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

    /// Stop any running search job without clearing the search view.
    fn stop_search_job(&mut self) {
        self.search_rx = None;
        self.search_cancel = None;
        if let Some(search) = self.search.as_mut() {
            search.done = true;
        }
    }

    /// Start a streaming `fd`/`rg` search in the background.
    fn start_search_job(&mut self, search: Search) {
        self.stop_search_job();
        self.search = Some(search.clone());
        self.reload();
        let (rx, cancel) = spawn_search(search, self.show_hidden);
        self.search_rx = Some(rx);
        self.search_cancel = Some(cancel);
    }

    /// Drain any results the background search has produced so far.
    fn poll_search(&mut self) -> bool {
        let Some(rx) = self.search_rx.as_ref() else {
            return false;
        };
        let mut incoming = Vec::new();
        let mut done = false;
        let mut error = None;
        loop {
            match rx.try_recv() {
                Ok(SearchMsg::Entry(e)) => incoming.push(e),
                Ok(SearchMsg::Done) => {
                    done = true;
                    break;
                }
                Ok(SearchMsg::Error(e)) => {
                    error = Some(e);
                    done = true;
                    break;
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    done = true;
                    break;
                }
            }
        }
        let got = !incoming.is_empty() || done;
        if let Some(search) = self.search.as_mut() {
            search.results.extend(incoming);
            if done {
                search.done = true;
            }
        }
        if done {
            self.search_rx = None;
            self.search_cancel = None;
        }
        if let Some(e) = error {
            self.status = e;
        }
        if got {
            self.reload();
        }
        got
    }

    fn cancel_search(&mut self) {
        self.search_rx = None;
        self.search_cancel = None;
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
        self.search_rx = None;
        self.search_cancel = None;
        self.entries.clear();
        self.state.select(None);
        self.pending.clear();
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
            self.input.set(c.display);
            self.refresh_completions();
        }
    }

    fn submit_cd(&mut self) {
        let name = self.input.as_str().trim().to_string();
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
        self.restore_terminal();
        let r = Command::new(p).args(parts).arg(path).status();
        *term = self.init_terminal();
        if let Err(e) = r {
            self.status = format!("{p}: {e}");
        }
    }

    /// `A`: edit a temporary file and create every non-empty line in it.
    fn bulk_create(&mut self, term: &mut DefaultTerminal) {
        let editor = env::var("VISUAL")
            .or_else(|_| env::var("EDITOR"))
            .unwrap_or_else(|_| "vi".into());
        let tmp = env::temp_dir().join(format!("rnn-bulk-create-{}.txt", std::process::id()));
        if let Err(e) = fs::write(&tmp, "") {
            self.status = format!("bulk create: {e}");
            return;
        }
        self.external(term, &editor, &tmp);
        let content = fs::read_to_string(&tmp).unwrap_or_default();
        let _ = fs::remove_file(&tmp);

        let mut n = 0;
        let mut last = None;
        for line in content.lines() {
            let name = line.trim();
            if name.is_empty() {
                continue;
            }
            let target = self.cwd.join(name);
            let r = if name.ends_with('/') {
                fs::create_dir_all(&target)
            } else {
                fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&target)
                    .map(|_| ())
            };
            match r {
                Ok(()) => {
                    n += 1;
                    last = Some(name.trim_end_matches('/').to_string());
                }
                Err(e) => self.status = format!("{}: {e}", target.display()),
            }
        }
        self.focus = last;
        self.reload();
        if n > 0 {
            self.status = format!("created {n} item(s)");
        }
    }

    fn open_external(&mut self, path: &Path) {
        // Browser-associated files (PDFs, XML, …) should follow the browser the
        // user chose as the system default, not whatever browser owns the MIME
        // type. A user-set OPENER is an explicit override, so leave it alone.
        if env::var_os("OPENER").is_none() {
            if let Some((browser, uri)) = openwith::browser_redirect(path) {
                if openwith::launch_desktop(&browser, &uri) {
                    return;
                }
            }
        }
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
        } else if self.chooser_file.is_some() {
            // Picker mode: opening a file picks it (and any selection) and
            // exits, like nnn's `-p` and Yazi's `--chooser-file`.
            self.write_chooser(true);
            self.quit = true;
        } else {
            self.open_external(&e.path);
        }
    }

    /// `O`: build the open-with menu from the user's Yazi config.
    fn start_open_with(&mut self) {
        let Some(e) = self.current().cloned() else {
            return;
        };
        let mut options = Vec::new();
        if self.is_trash_view() {
            options.push(openwith::Opener {
                name: "trash-restore".into(),
                desc: Some("Restore selected files".into()),
                command: "rnn:trash-restore".into(),
                block: false,
            });
            options.push(openwith::Opener {
                name: "trash-empty".into(),
                desc: Some("Empty trash bin".into()),
                command: "rnn:trash-empty".into(),
                block: false,
            });
        }
        options.extend(openwith::options(&e.path, self.config.open.as_ref()));
        if options.is_empty() {
            self.status = "open with: no openers configured".into();
            return;
        }
        self.open_with = Some(OpenWith {
            path: e.path,
            options,
            index: 0,
        });
        self.mode = Mode::OpenWith;
    }

    fn handle_open_with_key(&mut self, key: KeyEvent, term: &mut DefaultTerminal) {
        let Some(state) = self.open_with.as_ref() else {
            self.mode = Mode::Normal;
            return;
        };
        let len = state.options.len();
        if len == 0 {
            self.open_with = None;
            self.mode = Mode::Normal;
            return;
        }
        let mut index = state.index.min(len - 1);
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                self.open_with = None;
                self.mode = Mode::Normal;
                return;
            }
            KeyCode::Char('j') | KeyCode::Down => index = (index + 1) % len,
            KeyCode::Char('k') | KeyCode::Up => index = (index + len - 1) % len,
            KeyCode::Char('g') | KeyCode::Home => index = 0,
            KeyCode::Char('G') | KeyCode::End => index = len - 1,
            KeyCode::Char(c @ '1'..='9') => {
                let i = c as usize - '1' as usize;
                if i >= len {
                    return;
                }
                index = i;
                if let Some(s) = self.open_with.as_mut() {
                    s.index = index;
                }
                self.run_opener(term, index);
                return;
            }
            KeyCode::Enter => {
                self.run_opener(term, index);
                return;
            }
            _ => return,
        }
        if let Some(s) = self.open_with.as_mut() {
            s.index = index;
        }
    }

    fn run_opener(&mut self, term: &mut DefaultTerminal, index: usize) {
        let Some(state) = self.open_with.take() else {
            return;
        };
        let Some(op) = state.options.get(index).cloned() else {
            self.mode = Mode::Normal;
            return;
        };
        self.mode = Mode::Normal;
        if op.command.starts_with("rnn:trash-restore") {
            self.restore_selected();
            return;
        }
        if op.command.starts_with("rnn:trash-empty") {
            self.start_delete(DeleteMode::EmptyTrash);
            return;
        }
        if op.command == "rnn:open" {
            self.open_external(&state.path);
            return;
        }
        let cmd = op.command_for(&state.path);
        let shell = env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into());
        if op.block {
            self.restore_terminal();
            let r = Command::new(&shell)
                .arg("-c")
                .arg(&cmd)
                .current_dir(&self.cwd)
                .status();
            *term = self.init_terminal();
            match r {
                Ok(s) if !s.success() => self.status = format!("{}: {s}", op.name),
                Err(e) => self.status = format!("{}: {e}", op.name),
                _ => self.status = format!("opened with {}", op.name),
            }
            self.reload();
        } else {
            let r = Command::new(&shell)
                .arg("-c")
                .arg(&cmd)
                .current_dir(&self.cwd)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn();
            match r {
                Ok(_) => self.status = format!("opened with {}", op.name),
                Err(e) => self.status = format!("{}: {e}", op.name),
            }
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
        self.restore_terminal();
        let out = cmd.output();
        *term = self.init_terminal();
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

    /// Jump to a directory from zoxide's database using its interactive
    /// picker (which delegates to fzf when available).
    fn zoxide_jump(&mut self, term: &mut DefaultTerminal) {
        let mut cmd = Command::new("zoxide");
        cmd.args(["query", "--interactive"])
            .stdin(Stdio::inherit())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        self.restore_terminal();
        let out = cmd.output();
        *term = self.init_terminal();
        let out = match out {
            Ok(o) => o,
            Err(e) => {
                self.status = format!("zoxide: {e}");
                return;
            }
        };
        if !out.status.success() {
            if out.status.code() != Some(130) {
                self.status = format!("zoxide exited with {}", out.status);
            }
            return;
        }
        let path = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if path.is_empty() {
            return;
        }
        let path = PathBuf::from(path);
        if path.is_dir() {
            self.cd(path, None);
        } else {
            self.status = format!("zoxide: not a directory: {}", path.display());
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
        let pairs: Vec<(PathBuf, PathBuf)> = paths
            .into_iter()
            .filter(|src| src.exists())
            .filter_map(|src| {
                let name = src.file_name()?;
                let dst = unique(self.cwd.join(name));
                Some((src, dst))
            })
            .collect();
        if pairs.is_empty() {
            self.status = "clipboard has no file paths".into();
            return;
        }
        let n = pairs.len();
        self.spawn_task(
            TaskKind::Copy,
            format!("Paste {n} item(s) from clipboard"),
            TaskOp::Paste { pairs, cut: false },
        );
        self.status = format!("pasting {n} file(s) from clipboard…");
    }

    fn submit(&mut self, p: Prompt) {
        let name = self.input.as_str().trim().to_string();

        if p == Prompt::TabRename {
            self.tab_name = if name.is_empty() { None } else { Some(name) };
            self.save_active();
            return;
        }

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
            self.start_search_job(Search {
                kind,
                query: name,
                root: self.cwd.clone(),
                results: Vec::new(),
                done: false,
            });
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
            Prompt::TabRename => return,
        };
        match r {
            Ok(()) => {
                self.focus = Some(name.trim_end_matches('/').to_string());
                self.reload();
            }
            Err(e) => self.status = e.to_string(),
        }
    }

    /// Begin a `d` / `D` confirmation.
    fn start_delete(&mut self, mode: DeleteMode) {
        if mode != DeleteMode::EmptyTrash && self.targets().is_empty() {
            return;
        }
        self.visual = None;
        self.pending_delete = Some(mode);
        self.mode = Mode::Prompt(Prompt::Delete);
    }

    /// The user answered `y` to a delete confirmation.
    fn confirm_delete(&mut self) {
        match self.pending_delete.take() {
            Some(DeleteMode::Trash) => self.trash_selected(),
            Some(DeleteMode::Permanent) => self.delete_selected(),
            Some(DeleteMode::FromTrash) => self.delete_from_trash(),
            Some(DeleteMode::EmptyTrash) => self.empty_trash(),
            None => {}
        }
    }

    fn is_trash_view(&self) -> bool {
        trash_view_path().as_deref() == Some(self.cwd.as_path())
    }

    /// `d` in the trash view permanently removes the selected entries.
    fn delete_from_trash(&mut self) {
        let targets = self.targets();
        if targets.is_empty() {
            return;
        }
        let infos: Vec<PathBuf> = targets
            .iter()
            .filter_map(|p| p.file_name().and_then(trash_info_for_name))
            .collect();
        self.selected.clear();
        self.visual = None;
        let n = targets.len();
        self.spawn_task(
            TaskKind::Delete,
            format!("Purge {n} item(s) from trash"),
            TaskOp::DeleteTrash {
                paths: targets,
                infos,
            },
        );
        self.status = format!("purging {n} item(s) from trash…");
    }

    fn empty_trash(&mut self) {
        self.selected.clear();
        self.visual = None;
        self.spawn_task(
            TaskKind::EmptyTrash,
            "Empty trash".into(),
            TaskOp::EmptyTrash,
        );
        self.status = "emptying trash…".into();
    }

    /// Restore the selected trash entries to their original locations.
    fn restore_selected(&mut self) {
        let targets = self.targets();
        let infos: Vec<PathBuf> = targets
            .iter()
            .filter_map(|p| p.file_name().and_then(trash_info_for_name))
            .collect();
        if infos.is_empty() {
            self.status = "no trash info for the selection".into();
            return;
        }
        self.selected.clear();
        self.visual = None;
        let n = infos.len();
        self.spawn_task(
            TaskKind::Restore,
            format!("Restore {n} item(s)"),
            TaskOp::Restore { infos },
        );
        self.status = format!("restoring {n} item(s)…");
    }

    fn trash_selected(&mut self) {
        let targets = self.targets();
        if targets.is_empty() {
            return;
        }
        self.selected.clear();
        self.visual = None;
        let n = targets.len();
        self.spawn_task(
            TaskKind::Trash,
            format!("Trash {n} item(s)"),
            TaskOp::Trash { paths: targets },
        );
        self.status = format!("trashing {n} item(s)…");
    }

    fn delete_selected(&mut self) {
        let targets = self.targets();
        if targets.is_empty() {
            return;
        }
        self.selected.clear();
        self.visual = None;
        let n = targets.len();
        self.spawn_task(
            TaskKind::Delete,
            format!("Delete {n} item(s)"),
            TaskOp::Delete { paths: targets },
        );
        self.status = format!("deleting {n} item(s)…");
    }

    fn paste(&mut self, force: bool) {
        let Some((paths, cut)) = self.yank.clone() else {
            self.status = "nothing to paste".into();
            return;
        };
        let pairs: Vec<(PathBuf, PathBuf)> = paths
            .into_iter()
            .filter_map(|src| {
                let name = src.file_name()?;
                let target = self.cwd.join(name);
                let dst = if force { target } else { unique(target) };
                Some((src, dst))
            })
            .collect();
        if pairs.is_empty() {
            self.status = "nothing to paste".into();
            return;
        }
        let n = pairs.len();
        self.selected.clear();
        self.visual = None;
        self.list_dirty = true;
        self.spawn_task(
            if cut { TaskKind::Move } else { TaskKind::Copy },
            format!("{} {n} item(s)", if cut { "Move" } else { "Copy" }),
            TaskOp::Paste { pairs, cut },
        );
        if cut {
            self.yank = None;
        }
        self.status = format!("{} {n} item(s)…", if cut { "moving" } else { "copying" });
    }

    /// Symlink yanked files into the cwd (Yazi's `-` and `_`).
    fn link(&mut self, relative: bool) {
        let Some((paths, _)) = self.yank.clone() else {
            self.status = "nothing to link".into();
            return;
        };
        let pairs: Vec<(PathBuf, PathBuf)> = paths
            .into_iter()
            .filter_map(|src| {
                let name = src.file_name()?;
                let dst = unique(self.cwd.join(name));
                Some((src, dst))
            })
            .collect();
        if pairs.is_empty() {
            self.status = "nothing to link".into();
            return;
        }
        let n = pairs.len();
        self.spawn_task(
            TaskKind::Link,
            format!("Symlink {n} item(s)"),
            TaskOp::Link { pairs, relative },
        );
        self.status = format!("linking {n} item(s)…");
    }

    /// Hardlink yanked files into the cwd (Yazi's `Ctrl+-`).
    fn hardlink(&mut self) {
        let Some((paths, _)) = self.yank.clone() else {
            self.status = "nothing to link".into();
            return;
        };
        let pairs: Vec<(PathBuf, PathBuf)> = paths
            .into_iter()
            .filter_map(|src| {
                let name = src.file_name()?;
                let dst = unique(self.cwd.join(name));
                Some((src, dst))
            })
            .collect();
        if pairs.is_empty() {
            self.status = "nothing to link".into();
            return;
        }
        let n = pairs.len();
        self.spawn_task(
            TaskKind::Hardlink,
            format!("Hardlink {n} item(s)"),
            TaskOp::Hardlink { pairs },
        );
        self.status = format!("hardlinking {n} item(s)…");
    }

    fn spawn_task(&mut self, kind: TaskKind, title: String, op: TaskOp) {
        let state = Arc::new(Mutex::new(TaskState {
            status: TaskStatus::Running,
            progress: TaskProgress::default(),
        }));
        let cancel = Arc::new(AtomicBool::new(false));
        let thread_state = Arc::clone(&state);
        let thread_cancel = Arc::clone(&cancel);
        std::thread::spawn(move || run_task(op, &thread_state, &thread_cancel));
        self.tasks.push(Task {
            id: self.task_next_id,
            kind,
            title,
            state,
            cancel,
            started: Instant::now(),
            finished: None,
        });
        self.task_next_id = self.task_next_id.wrapping_add(1);
    }

    /// Refresh task progress. Returns true while the overlay needs redrawing.
    fn poll_tasks(&mut self) -> bool {
        let mut changed = false;
        let mut finished = None;
        for task in &mut self.tasks {
            let status = match task.state.lock() {
                Ok(s) => s.status,
                Err(_) => continue,
            };
            if status == TaskStatus::Running {
                changed = true;
            } else if task.finished.is_none() {
                task.finished = Some(Instant::now());
                changed = true;
                finished = Some((task.title.clone(), status));
            }
        }
        if finished.is_some() {
            self.reload();
        }
        if let Some((title, status)) = finished {
            self.status = format!("{title}: {}", status.label());
        }
        if !self.tasks_running() {
            self.quit_warned = false;
        }
        changed
    }

    /// Whether quitting is allowed now. With unfinished tasks running, warn
    /// once and return false; `retry` is the key named in the warning.
    fn quit_allowed(&mut self, retry: &str) -> bool {
        if self.config.confirm.quit_with_tasks && self.tasks_running() && !self.quit_warned {
            self.quit_warned = true;
            self.status = format!("unfinished tasks — press {retry} again to quit");
            false
        } else {
            true
        }
    }

    fn tasks_running(&self) -> bool {
        self.tasks.iter().any(|task| {
            task.state
                .lock()
                .map(|s| s.status == TaskStatus::Running)
                .unwrap_or(false)
        })
    }

    /// Write the current selection to the chooser file. On a plain quit only
    /// an existing selection is written (like nnn); picker-mode `Enter` also
    /// includes the hovered entry when nothing is selected.
    fn write_chooser(&mut self, include_hovered: bool) {
        let Some(dest) = self.chooser_file.clone() else {
            return;
        };
        let mut paths: Vec<PathBuf> = self.selected.iter().cloned().collect();
        if paths.is_empty() && include_hovered {
            if let Some(e) = self.current() {
                paths.push(e.path.clone());
            }
        }
        if paths.is_empty() {
            return;
        }
        let out = chooser_bytes(&paths, self.chooser_null);
        if dest == Path::new("-") {
            self.chooser_stdout = Some(out);
        } else if let Err(e) = fs::write(&dest, &out) {
            eprintln!("rnn: cannot write chooser file {}: {e}", dest.display());
        }
    }

    /// When picker output goes to stdout, point fd 1 at the controlling
    /// terminal for the duration of the TUI so escape sequences do not pollute
    /// the picked paths. Returns the original stdout fd for restoration.
    pub fn redirect_stdout_for_chooser(&self) -> Option<rustix::fd::OwnedFd> {
        if self.chooser_file.as_deref() != Some(Path::new("-")) {
            return None;
        }
        let tty = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/tty")
            .ok()?;
        let saved = rustix::io::dup(rustix::stdio::stdout()).ok()?;
        rustix::stdio::dup2_stdout(&tty).ok()?;
        Some(saved)
    }

    /// Flush deferred `-p -` output after the terminal has been restored, so
    /// the picked paths are visible in the caller's shell.
    pub fn flush_chooser_stdout(&mut self) {
        if let Some(out) = self.chooser_stdout.take() {
            let mut stdout = io::stdout();
            let _ = stdout.write_all(&out);
            let _ = stdout.flush();
        }
    }

    fn task_move(&mut self, delta: isize) {
        if self.tasks.is_empty() {
            self.task_cursor = 0;
            return;
        }
        let current = self.task_cursor.min(self.tasks.len() - 1) as isize;
        self.task_cursor = (current + delta).clamp(0, self.tasks.len() as isize - 1) as usize;
    }

    fn cancel_task(&mut self) {
        let Some(task) = self.tasks.get(self.task_cursor) else {
            return;
        };
        let running = task
            .state
            .lock()
            .map(|s| s.status == TaskStatus::Running)
            .unwrap_or(false);
        if running {
            task.cancel.store(true, Ordering::Relaxed);
            self.status = format!("cancelling: {}", task.title);
        } else {
            self.status = "task already finished".into();
        }
    }

    fn inspect_task(&mut self) {
        let Some(task) = self.tasks.get(self.task_cursor) else {
            return;
        };
        let (status, progress, elapsed) = {
            let state = match task.state.lock() {
                Ok(s) => s,
                Err(_) => return,
            };
            (
                state.status,
                state.progress.clone(),
                task.started.elapsed().as_secs_f64(),
            )
        };
        let mut lines = vec![
            InfoLine::Section("Task"),
            InfoLine::Field("ID", task.id.to_string()),
            InfoLine::Field("Title", task.title.clone()),
            InfoLine::Field("Kind", task.kind.label().to_string()),
            InfoLine::Field("Status", status.label().to_string()),
            InfoLine::Field(
                "Progress",
                format!(
                    "{} / {} ({}%)",
                    progress.done.min(progress.total),
                    progress.total,
                    task_percent(&progress)
                ),
            ),
            InfoLine::Field("Elapsed", format_duration(elapsed)),
        ];
        if !progress.current.is_empty() {
            lines.push(InfoLine::Field("Current", progress.current.clone()));
        }
        lines.push(InfoLine::Section("Errors"));
        if progress.errors.is_empty() {
            lines.push(InfoLine::Text("  (none)".into()));
        } else {
            for error in &progress.errors {
                lines.push(InfoLine::Text(format!("  {error}")));
            }
        }
        self.info = Some(Info { lines });
        self.info_cursor = 0;
        self.info_scroll = 0;
        self.info_from_tasks = true;
        self.mode = Mode::Info;
    }

    fn close_info(&mut self) {
        self.info = None;
        self.mode = if self.info_from_tasks {
            Mode::Tasks
        } else {
            Mode::Normal
        };
        self.info_from_tasks = false;
    }

    fn goto_trash(&mut self) {
        if let Err(e) = ensure_trash_dirs() {
            self.status = format!("trash: {e}");
            return;
        }
        let Some(files) = trash_view_path() else {
            self.status = "trash: HOME is not set".into();
            return;
        };
        self.cd(files, None);
        if self.entries.is_empty() {
            self.status = "trash is empty".into();
        }
    }

    fn start_search(&mut self, kind: SearchKind) {
        self.stop_search_job();
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
        let cmd = self.input.as_str().trim().to_string();
        if cmd.is_empty() {
            return;
        }
        let shell = env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into());
        self.restore_terminal();
        let r = Command::new(&shell)
            .arg("-c")
            .arg(&cmd)
            .current_dir(&self.cwd)
            .status();
        *term = self.init_terminal();
        match r {
            Ok(s) if !s.success() => self.status = format!("shell: {s}"),
            Err(e) => self.status = format!("shell: {e}"),
            _ => {}
        }
        self.reload();
    }

    fn suspend(&mut self, term: &mut DefaultTerminal) {
        self.restore_terminal();
        let _ =
            rustix::process::kill_process(rustix::process::getpid(), rustix::process::Signal::Tstp);
        *term = self.init_terminal();
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

    /// 1-based position of the cursor among entries matching `query`, plus the
    /// total number of matches. Returns `(0, 0)` when the query is empty.
    fn find_progress(&self, query: &str) -> (usize, usize) {
        if query.is_empty() {
            return (0, 0);
        }
        let current = self.state.selected();
        let mut pos = 0;
        let mut total = 0;
        for (i, entry) in self.entries.iter().enumerate() {
            if smart_contains(&entry.name, query) {
                total += 1;
                if Some(i) == current {
                    pos = total;
                }
            }
        }
        (pos, total)
    }

    fn set_linemode(&mut self, mode: Linemode) {
        self.linemode = mode;
        self.list_dirty = true;
    }

    /// Build the `Tab` overlay for the hovered entry.
    fn open_info(&mut self) {
        let Some(e) = self.current().cloned() else {
            return;
        };
        let meta = fs::symlink_metadata(&e.path).ok();
        let is_link = meta
            .as_ref()
            .map(|m| m.file_type().is_symlink())
            .unwrap_or(false);
        let kind = if is_link {
            "symlink"
        } else if e.is_dir {
            "directory"
        } else {
            match detect::detect(&e.path) {
                Kind::Text => "text",
                Kind::Json => "json",
                Kind::Image => "image",
                Kind::Video => "video",
                Kind::Pdf => "pdf",
                Kind::Archive => "archive",
                Kind::Binary => "binary",
            }
        };

        let mut lines: Vec<InfoLine> = vec![InfoLine::Section("Base")];
        lines.push(InfoLine::Field("Name", e.name.clone()));
        lines.push(InfoLine::Field("Path", e.path.display().to_string()));
        lines.push(InfoLine::Field("Type", kind.to_string()));
        if let Some(mime) = openwith::file_mime(&e.path) {
            lines.push(InfoLine::Field("Mimetype", mime));
        }
        if let Ok(target) = fs::read_link(&e.path) {
            lines.push(InfoLine::Field("Link to", target.display().to_string()));
        }
        if self.is_trash_view() {
            if let Some(name) = e.path.file_name() {
                if let Some(info_path) = trash_info_for_name(name) {
                    if let Some(info) = parse_trashinfo(&info_path) {
                        lines.push(InfoLine::Section("Trash"));
                        lines.push(InfoLine::Field(
                            "Original",
                            info.original.display().to_string(),
                        ));
                        lines.push(InfoLine::Field("Deleted", info.date));
                    }
                }
            }
        }
        lines.push(InfoLine::Field(
            "Created",
            format!("{} ({})", format_time(e.created), format_age(e.created)),
        ));
        lines.push(InfoLine::Field(
            "Modified",
            format!("{} ({})", format_time(e.modified), format_age(e.modified)),
        ));
        if let Some(m) = &meta {
            if let Ok(accessed) = m.accessed() {
                lines.push(InfoLine::Field("Accessed", format_time(Some(accessed))));
            }
        }

        if e.is_dir {
            lines.push(InfoLine::Section("Folder"));
            let (bytes, complete) = dir_size(&e.path, Duration::from_millis(400));
            let size = if complete {
                human(bytes)
            } else {
                format!("{} (?)", human(bytes))
            };
            lines.push(InfoLine::Field("Size", size));
            if let Ok(rd) = fs::read_dir(&e.path) {
                lines.push(InfoLine::Field("Entries", rd.flatten().count().to_string()));
            }
        } else {
            lines.push(InfoLine::Section("File"));
            lines.push(InfoLine::Field(
                "Size",
                format!("{} ({} bytes)", human(e.size), e.size),
            ));
        }

        match detect::detect(&e.path) {
            Kind::Image => {
                lines.push(InfoLine::Section("Image"));
                if let Ok(Some(fmt)) = infer::get_from_path(&e.path) {
                    lines.push(InfoLine::Field("Format", fmt.extension().to_uppercase()));
                }
                if let Ok((w, h)) = image::image_dimensions(&e.path) {
                    lines.push(InfoLine::Field("Size", format!("{w}×{h}")));
                }
            }
            Kind::Video => {
                lines.push(InfoLine::Section("Video"));
                let v = video_info(&e.path);
                if let Some(d) = v.duration {
                    lines.push(InfoLine::Field("Duration", format_duration(d)));
                }
                if let (Some(w), Some(h)) = (v.width, v.height) {
                    lines.push(InfoLine::Field("Size", format!("{w}×{h}")));
                }
                if let Some(codec) = v.codec {
                    lines.push(InfoLine::Field("Codec", codec));
                }
            }
            _ => {}
        }

        if let Some(m) = &meta {
            lines.push(InfoLine::Section("Metadata"));
            let owner = id_name("/etc/passwd", e.uid)
                .map(|n| format!("{n} ({})", e.uid))
                .unwrap_or_else(|| e.uid.to_string());
            let group = id_name("/etc/group", e.gid)
                .map(|n| format!("{n} ({})", e.gid))
                .unwrap_or_else(|| e.gid.to_string());
            lines.push(InfoLine::Field("Owner", format!("{owner} : {group}")));
            lines.push(InfoLine::Field(
                "Permissions",
                format!("{} ({:04o})", permissions(e.mode), e.mode & 0o7777),
            ));
            lines.push(InfoLine::Field(
                "Inode",
                format!("{} · {} link(s)", m.ino(), m.nlink()),
            ));
        }

        if let Some((pkind, n)) = self.preview.artifact_meta() {
            lines.push(InfoLine::Section("Preview"));
            lines.push(InfoLine::Field("Kind", pkind.to_string()));
            lines.push(InfoLine::Field("Lines", n.to_string()));
        }
        self.info = Some(Info { lines });
        self.info_cursor = 0;
        self.info_scroll = 0;
        self.info_from_tasks = false;
        self.mode = Mode::Info;
    }

    fn info_move(&mut self, delta: isize) {
        let Some(info) = &self.info else {
            return;
        };
        let len = info.lines.len();
        if len == 0 {
            return;
        }
        let cur = self.info_cursor.min(len - 1) as isize;
        self.info_cursor = (cur + delta).clamp(0, len as isize - 1) as usize;
        let page = self.page.max(1);
        if self.info_cursor < self.info_scroll {
            self.info_scroll = self.info_cursor;
        } else if self.info_cursor >= self.info_scroll + page {
            self.info_scroll = self.info_cursor + 1 - page;
        }
    }

    fn info_swipe(&mut self, delta: isize) {
        self.move_by(delta);
        self.open_info();
    }

    fn info_copy_cell(&mut self) {
        let Some(info) = &self.info else {
            return;
        };
        let Some(line) = info.lines.get(self.info_cursor) else {
            return;
        };
        let text = match line {
            InfoLine::Field(_, value) => value.clone(),
            InfoLine::Section(title) => title.to_string(),
            InfoLine::Text(value) => value.clone(),
        };
        self.clipboard_set(text + "\n", "text/plain", "copied cell".into());
    }

    // ─────────────── input ───────────────

    fn handle_key(&mut self, key: KeyEvent, term: &mut DefaultTerminal) -> io::Result<bool> {
        self.status.clear();

        let ctrl_c =
            key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c');

        match self.mode {
            Mode::Filter => {
                if ctrl_c {
                    self.filter.clear();
                    self.mode = Mode::Normal;
                    self.reload();
                    return Ok(false);
                }
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
                if ctrl_c {
                    if let Some(o) = self.find_origin.take() {
                        if !self.entries.is_empty() {
                            self.state.select(Some(o.min(self.entries.len() - 1)));
                        }
                    }
                    self.find_query.clear();
                    self.mode = Mode::Normal;
                    return Ok(false);
                }
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
                if ctrl_c {
                    self.mode = Mode::Normal;
                    self.input.clear();
                    self.completions.clear();
                    self.pending_delete = None;
                    return Ok(false);
                }
                if p == Prompt::Delete {
                    if key.code == KeyCode::Char('y') {
                        self.confirm_delete();
                    } else {
                        self.pending_delete = None;
                    }
                    self.mode = Mode::Normal;
                    return Ok(false);
                }
                let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
                let shift = key.modifiers.contains(KeyModifiers::SHIFT);
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
                        self.input.backspace();
                        if p == Prompt::Cd {
                            self.refresh_completions();
                        }
                    }
                    KeyCode::Delete => {
                        self.input.delete();
                        if p == Prompt::Cd {
                            self.refresh_completions();
                        }
                    }
                    KeyCode::Left if shift => self.input.select_left(),
                    KeyCode::Right if shift => self.input.select_right(),
                    KeyCode::Home if shift => self.input.select_home(),
                    KeyCode::End if shift => self.input.select_end(),
                    KeyCode::Left => self.input.move_left(),
                    KeyCode::Right => self.input.move_right(),
                    KeyCode::Home => self.input.move_home(),
                    KeyCode::End => self.input.move_end(),
                    KeyCode::Char('a') if ctrl => self.input.select_all(),
                    KeyCode::Char('e') if ctrl => self.input.move_end(),
                    KeyCode::Char('u') if ctrl => self.input.kill_to_start(),
                    KeyCode::Char('k') if ctrl => self.input.kill_to_end(),
                    KeyCode::Char('w') if ctrl => self.input.delete_word(),
                    KeyCode::Char(c) if !ctrl => {
                        self.input.insert(c);
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
            Mode::OpenWith => {
                self.handle_open_with_key(key, term);
                return Ok(false);
            }
            Mode::Info | Mode::Tasks | Mode::Normal => {}
        }

        // Ctrl+c is a safety net: it always cancels or quits, even when the
        // user has rebound it.
        if ctrl_c {
            match self.mode {
                Mode::Info => self.close_info(),
                Mode::Tasks => self.mode = Mode::Normal,
                Mode::Normal => {
                    if self.tabs.len() > 1 {
                        self.close_tab();
                    } else if self.config.confirm.quit_with_tasks
                        && self.tasks_running()
                        && !self.quit_warned
                    {
                        self.quit_warned = true;
                        self.status = "unfinished tasks — press Ctrl+c again to quit".into();
                    } else {
                        return Ok(true);
                    }
                }
                _ => {}
            }
            return Ok(false);
        }

        if !self.pending.is_empty() && key.code == KeyCode::Esc {
            self.pending.clear();
            return Ok(false);
        }

        let ctx = match self.mode {
            Mode::Normal => Context::Mgr,
            Mode::Tasks => Context::Tasks,
            Mode::Info => Context::Info,
            _ => return Ok(false),
        };
        if let Some(quit) = self.handle_mapped_key(ctx, key, term)? {
            return Ok(quit);
        }
        Ok(false)
    }

    /// Feed a key into the active keymap trie. Returns `Some(quit)` when an
    /// action decided to exit rnn.
    fn handle_mapped_key(
        &mut self,
        ctx: Context,
        key: KeyEvent,
        term: &mut DefaultTerminal,
    ) -> io::Result<Option<bool>> {
        let next = {
            let root = self.config.keymap.node(ctx);
            let Some(node) = root.walk(&self.pending) else {
                self.pending.clear();
                return Ok(None);
            };
            node.child(&key)
                .map(|(chord, child)| (*chord, child.has_children(), child.action().cloned()))
        };
        let Some((chord, has_children, action)) = next else {
            self.pending.clear();
            return Ok(None);
        };
        if has_children {
            self.pending.push(chord);
            return Ok(None);
        }
        if let Some(action) = action {
            self.pending.clear();
            return self.dispatch(action, term).map(Some);
        }
        self.pending.clear();
        Ok(None)
    }

    fn dispatch(&mut self, action: Action, term: &mut DefaultTerminal) -> io::Result<bool> {
        match action {
            Action::Chain(actions) => {
                for action in actions {
                    if self.dispatch(action, term)? {
                        return Ok(true);
                    }
                }
            }
            Action::Nop => {}
            Action::Quit => {
                if self.quit_allowed("q") {
                    self.write_chooser(false);
                    return Ok(true);
                }
            }
            Action::QuitCwd => {
                if self.quit_allowed("Q") {
                    self.write_chooser(false);
                    if let Some(path) = &self.cwd_file {
                        write_cwd_file(path, &self.cwd);
                    }
                    return Ok(true);
                }
            }
            Action::QuitOrClose => {
                if self.tabs.len() > 1 {
                    self.close_tab();
                } else if self.quit_allowed("Ctrl+c") {
                    self.write_chooser(false);
                    return Ok(true);
                }
            }
            Action::Close => match self.mode {
                Mode::Tasks => self.mode = Mode::Normal,
                Mode::Info => self.close_info(),
                _ => {}
            },
            Action::Escape => {
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
            Action::Help => self.mode = Mode::Help,
            Action::Tasks => {
                self.task_cursor = self.task_cursor.min(self.tasks.len().saturating_sub(1));
                self.mode = Mode::Tasks;
            }
            Action::Info => self.open_info(),
            Action::Move(n) => match self.mode {
                Mode::Tasks => self.task_move(n),
                Mode::Info => self.info_move(n),
                _ => self.move_by(n),
            },
            Action::HalfPage(n) => self.move_by((self.page / 2) as isize * n as isize),
            Action::Page(n) => self.move_by(self.page as isize * n as isize),
            Action::Top => match self.mode {
                Mode::Tasks => self.task_cursor = 0,
                Mode::Info => self.info_move(isize::MIN / 2),
                _ => self.select_top(),
            },
            Action::Bottom => match self.mode {
                Mode::Tasks => self.task_cursor = self.tasks.len().saturating_sub(1),
                Mode::Info => self.info_move(isize::MAX / 2),
                _ => self.select_bottom(),
            },
            Action::Parent => self.go_parent(),
            Action::Open => self.open(term),
            Action::OpenWith => self.start_open_with(),
            Action::History(n) => {
                if n < 0 {
                    self.history_back();
                } else {
                    self.history_forward();
                }
            }
            Action::Follow => self.follow_symlink(),
            Action::Goto(path) => self.goto(&path),
            Action::GotoTrash => self.goto_trash(),
            Action::Cd => self.start_cd(),
            Action::Hidden => {
                self.show_hidden = !self.show_hidden;
                self.reload();
            }
            Action::TogglePreview => self.show_preview = !self.show_preview,
            Action::Zoom(n) => match n {
                0 => self.zoom_reset(),
                n if n > 0 => self.zoom_in(),
                _ => self.zoom_out(),
            },
            Action::Seek(n) => self.seek_preview(n as isize),
            Action::Toggle => {
                self.visual = None;
                if let Some(e) = self.current() {
                    let p = e.path.clone();
                    if !self.selected.remove(&p) {
                        self.selected.insert(p);
                    }
                    self.list_dirty = true;
                }
            }
            Action::ToggleSelect => {
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
            Action::SelectAll => self.select_all(),
            Action::InvertSelection => self.invert_selection(),
            Action::VisualSelect => {
                if matches!(
                    self.visual.as_ref().map(|v| v.kind),
                    Some(VisualKind::Select)
                ) {
                    self.visual = None;
                } else {
                    self.enter_visual(VisualKind::Select);
                }
            }
            Action::VisualUnset => {
                if matches!(
                    self.visual.as_ref().map(|v| v.kind),
                    Some(VisualKind::Unselect)
                ) {
                    self.visual = None;
                } else {
                    self.enter_visual(VisualKind::Unselect);
                }
            }
            Action::Yank => self.yank_selection(false),
            Action::Cut => self.yank_selection(true),
            Action::Paste(force) => self.paste(force),
            Action::Unyank => {
                if self.yank.take().is_some() {
                    self.status = "unyanked".into();
                }
            }
            Action::ClipboardCopy => self.clipboard_copy(),
            Action::ClipboardPaste => self.clipboard_paste(),
            Action::Trash => {
                let mode = if self.is_trash_view() {
                    DeleteMode::FromTrash
                } else {
                    DeleteMode::Trash
                };
                self.start_delete(mode);
            }
            Action::Delete => {
                let mode = if self.is_trash_view() {
                    DeleteMode::FromTrash
                } else {
                    DeleteMode::Permanent
                };
                self.start_delete(mode);
            }
            Action::Rename => {
                self.visual = None;
                if let Some(e) = self.current() {
                    self.input = Input::rename(&e.name);
                    self.mode = Mode::Prompt(Prompt::Rename);
                }
            }
            Action::Create => self.start_create(),
            Action::BulkCreate => self.bulk_create(term),
            Action::Filter => self.mode = Mode::Filter,
            Action::Find(forward) => self.start_find(forward),
            Action::FindNext(forward) => self.find_next(forward),
            Action::Search(kind) => self.start_search(kind),
            Action::CancelSearch => self.cancel_search(),
            Action::Shell => self.start_shell(),
            Action::Fzf => self.fzf_jump(term),
            Action::Zoxide => self.zoxide_jump(term),
            Action::Suspend => self.suspend(term),
            Action::Editor => {
                if let Some(e) = self.current().cloned().filter(|e| !e.is_dir) {
                    let ed = env::var("VISUAL")
                        .or_else(|_| env::var("EDITOR"))
                        .unwrap_or_else(|_| "vi".into());
                    self.external(term, &ed, &e.path);
                    self.reload();
                }
            }
            Action::Pager => {
                if let Some(e) = self.current().cloned().filter(|e| !e.is_dir) {
                    let pg = env::var("PAGER").unwrap_or_else(|_| "less".into());
                    self.external(term, &pg, &e.path);
                }
            }
            Action::CopyPath(kind) => self.copy_paths(kind),
            Action::SetLinemode(mode) => self.set_linemode(mode),
            Action::SetSort(sort, reverse) => {
                self.set_sort(sort, reverse);
                match sort {
                    Sort::Size => self.set_linemode(Linemode::Size),
                    Sort::Time => self.set_linemode(Linemode::Mtime),
                    Sort::Btime => self.set_linemode(Linemode::Btime),
                    _ => {}
                }
            }
            Action::Link(relative) => self.link(relative),
            Action::Hardlink => self.hardlink(),
            Action::NewTab(home) => {
                let path = if home {
                    match env::var_os("HOME") {
                        Some(home) => PathBuf::from(home),
                        None => {
                            self.status = "HOME is not set".into();
                            return Ok(false);
                        }
                    }
                } else {
                    self.cwd.clone()
                };
                self.new_tab(path);
            }
            Action::RenameTab => {
                self.input.set(
                    self.tab_name
                        .clone()
                        .unwrap_or_else(|| self.tab_label(self.tab_index)),
                );
                self.mode = Mode::Prompt(Prompt::TabRename);
            }
            Action::CloseTab => self.close_tab(),
            Action::SwitchTab(index) => self.switch_tab(index),
            Action::NextTab(delta) => self.next_tab(delta),
            Action::SwapTab(delta) => self.swap_tab(delta),
            Action::TaskMove(delta) => self.task_move(delta),
            Action::TaskTop => self.task_cursor = 0,
            Action::TaskBottom => self.task_cursor = self.tasks.len().saturating_sub(1),
            Action::TaskInspect => self.inspect_task(),
            Action::TaskCancel => self.cancel_task(),
            Action::InfoMove(delta) => self.info_move(delta),
            Action::InfoSwipe(delta) => {
                if !self.info_from_tasks {
                    self.info_swipe(delta);
                }
            }
            Action::InfoCopyCell => self.info_copy_cell(),
            Action::Custom(name) => {
                let result = self
                    .config
                    .lua
                    .as_ref()
                    .map(|lua| config::lua::run_action(lua, &name));
                match result {
                    Some(Ok(Some(message))) => {
                        self.status = message;
                    }
                    Some(Ok(None)) => {}
                    Some(Err(e)) => {
                        self.status = e;
                    }
                    None => {
                        self.status = format!("unknown action '{name}'");
                    }
                }
            }
        }
        Ok(false)
    }

    fn yank_selection(&mut self, cut: bool) {
        let targets = self.targets();
        self.visual = None;
        if !targets.is_empty() {
            self.status = format!(
                "{} item(s) {}",
                targets.len(),
                if cut { "cut" } else { "yanked" }
            );
            self.yank = Some((targets, cut));
            self.selected.clear();
            self.list_dirty = true;
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
                    self.config.theme.directory
                } else {
                    self.config.theme.fg
                };
                ListItem::new(Line::from(vec![
                    Span::styled(left, style),
                    Span::raw(" ".repeat(pad)),
                    Span::styled(right, self.config.theme.dim),
                ]))
            })
            .collect();
        self.list = List::new(items).highlight_style(self.config.theme.selection);
        self.list_width = width;
        self.list_dirty = false;
    }

    /// Keep `scrolloff` rows of context above and below the cursor. Ratatui
    /// only scrolls far enough to keep the selection visible, so adjust the
    /// offset ourselves before rendering.
    fn apply_scrolloff(&mut self, height: usize) {
        let Some(selected) = self.state.selected() else {
            return;
        };
        let offset = self.state.offset();
        let next = scrolloff_offset(
            selected,
            offset,
            height,
            self.entries.len(),
            self.config.mgr.scrolloff,
        );
        if next != offset {
            *self.state.offset_mut() = next;
        }
    }

    fn draw(&mut self, f: &mut Frame) {
        let rows = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).split(f.area());
        let cols = if self.show_preview {
            let (list_pct, preview_pct) = ratio_percentages(self.config.mgr.ratio);
            Layout::horizontal([
                Constraint::Percentage(list_pct),
                Constraint::Percentage(preview_pct),
            ])
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
        self.title_area = title_area;
        self.list_area = list_area;
        self.preview_area = if self.show_preview {
            cols.get(1).copied().unwrap_or_default()
        } else {
            Rect::default()
        };

        let mut tab_spans = Vec::with_capacity(self.tabs.len());
        let mut spans = Vec::with_capacity(self.tabs.len());
        let mut x = title_area.x;
        for i in 0..self.tabs.len() {
            let name = self.tab_label(i);
            let style = if i == self.tab_index {
                self.config.theme.tab_active
            } else {
                self.config.theme.tab_inactive
            };
            let span = Span::styled(format!(" {}:{} ", i + 1, name), style);
            let width = span.width() as u16;
            tab_spans.push((x, x.saturating_add(width)));
            x = x.saturating_add(width);
            spans.push(span);
        }
        self.tab_spans = tab_spans;
        f.render_widget(Paragraph::new(Line::from(spans)), title_area);
        self.apply_scrolloff(list_area.height as usize);
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
            Mode::Find => {
                let arrow = if self.find_forward { "→" } else { "←" };
                if self.find_query.is_empty() {
                    format!("find{arrow}: ")
                } else {
                    let (pos, total) = self.find_progress(&self.find_query);
                    format!("find{arrow}: {}  [{pos}/{total}]", self.find_query)
                }
            }
            Mode::Help => "rnn keys — press any key to close".into(),
            Mode::Info => {
                if self.info_from_tasks {
                    "task — j/k scroll · c c copy · Esc close".into()
                } else {
                    "info — j/k scroll · h/l swipe · c c copy · Esc close".into()
                }
            }
            Mode::Tasks => "tasks — j/k move · Enter inspect · x cancel · Esc/w close".into(),
            Mode::OpenWith => "open with — j/k move · Enter open · Esc cancel".into(),
            Mode::Prompt(Prompt::Delete) => match self.pending_delete {
                Some(DeleteMode::Trash) => {
                    format!("trash {} item(s)? (y/N)", self.targets().len())
                }
                Some(DeleteMode::Permanent) => {
                    format!("permanently delete {} item(s)? (y/N)", self.targets().len())
                }
                Some(DeleteMode::FromTrash) => format!(
                    "permanently delete {} item(s) from trash? (y/N)",
                    self.targets().len()
                ),
                Some(DeleteMode::EmptyTrash) => "empty trash? (y/N)".into(),
                None => "delete? (y/N)".into(),
            },
            Mode::Prompt(p) => {
                let label = match p {
                    Prompt::Create => "create",
                    Prompt::Rename => "rename",
                    Prompt::Cd => "go to",
                    Prompt::SearchFd => "fd",
                    Prompt::SearchRg => "rg",
                    Prompt::Shell => "shell",
                    Prompt::TabRename => "rename tab",
                    Prompt::Delete => "",
                };
                format!("{label}: {}", self.input.as_str())
            }
            Mode::Normal if !self.status.is_empty() => self.status.clone(),
            Mode::Normal => {
                let mut s = String::new();
                if let Some(entry) = self.current() {
                    // nnn-style: cursor position, local mtime, then mode/size.
                    // lstat supplies the type, link length, and mtime; the
                    // permission bits come from the followed metadata so a
                    // valid symlink shows its target's bits (like nnn).
                    let md = entry.path.symlink_metadata().ok();
                    let lmode = md.as_ref().map(|m| m.mode()).unwrap_or(entry.mode);
                    let mode = (lmode & 0o170000) | (entry.mode & 0o7777);
                    let size = md.as_ref().map(|m| m.len()).unwrap_or(entry.size);
                    let mtime = md
                        .as_ref()
                        .and_then(|m| m.modified().ok())
                        .or(entry.modified);
                    let pos = self.state.selected().map_or(0, |i| i + 1);
                    s.push_str(&format!("{pos}/{}  ", self.entries.len()));
                    s.push_str(&format_mtime(mtime, self.local_offset));
                    s.push(' ');
                    s.push_str(&mode_string(mode));
                    s.push(' ');
                    s.push_str(&human(size));
                    let ext = extension_label(entry, md.as_ref());
                    if !ext.is_empty() {
                        s.push(' ');
                        s.push_str(&ext);
                    }
                    s.push_str(" | ");
                } else {
                    s.push_str(&format!("{} items | ", self.entries.len()));
                }
                s.push_str(&format!(
                    "{} selected | sort:{sort}{direction}",
                    self.selected.len()
                ));
                if self.chooser_file.is_some() {
                    s.push_str(" | picker");
                }
                if let Some(query) = self.last_find.as_deref().filter(|q| !q.is_empty()) {
                    let (pos, total) = self.find_progress(query);
                    if pos > 0 {
                        s.push_str(&format!(" | find: {query} [{pos}/{total}]"));
                    }
                }
                if let Some(search) = &self.search {
                    let kind = match search.kind {
                        SearchKind::Fd => "fd",
                        SearchKind::Rg => "rg",
                    };
                    s.push_str(&format!(
                        " | {kind}: {}{}",
                        search.query,
                        if search.done { "" } else { " …" }
                    ));
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
                let running = self
                    .tasks
                    .iter()
                    .filter(|task| {
                        task.state
                            .lock()
                            .map(|s| s.status == TaskStatus::Running)
                            .unwrap_or(false)
                    })
                    .count();
                if running > 0 {
                    s.push_str(&format!(" | tasks {running}"));
                }
                s
            }
        };
        if !self.pending.is_empty() {
            let keys: Vec<String> = self.pending.iter().map(|c| c.label()).collect();
            text.push_str(&format!(" | {}…", keys.join(" ")));
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
        let status_style = match self.mode {
            Mode::Prompt(Prompt::Delete) => self.config.theme.status_error,
            Mode::Prompt(_) | Mode::Filter | Mode::Find => self.config.theme.status_info,
            Mode::Normal if !self.status.is_empty() => self.config.theme.status_info,
            _ => self.config.theme.status,
        };
        match self.mode {
            Mode::Prompt(Prompt::Delete) => {
                f.render_widget(Paragraph::new(text).style(status_style), rows[1]);
            }
            Mode::Prompt(p) => self.draw_prompt_status(f, rows[1], p),
            _ => f.render_widget(Paragraph::new(text).style(status_style), rows[1]),
        }
        self.draw_completions(f);
        if !self.pending.is_empty() {
            self.draw_prefix_menu(f);
        }

        if matches!(self.mode, Mode::Help) {
            self.draw_help(f);
        }
        if matches!(self.mode, Mode::Info) {
            self.draw_info(f);
        }
        if matches!(self.mode, Mode::OpenWith) {
            self.draw_open_with(f);
        }
        if matches!(self.mode, Mode::Tasks) {
            self.draw_tasks(f);
        }
    }

    fn draw_prefix_menu(&self, f: &mut Frame) {
        if self.pending.is_empty() {
            return;
        }
        let ctx = match self.mode {
            Mode::Tasks => Context::Tasks,
            Mode::Info => Context::Info,
            _ => Context::Mgr,
        };
        let root = self.config.keymap.node(ctx);
        let Some(node) = root.walk(&self.pending) else {
            return;
        };
        let items: Vec<(String, String)> = node
            .children()
            .iter()
            .map(|(chord, child)| {
                let desc = child
                    .desc()
                    .map(str::to_string)
                    .or_else(|| child.action().map(|action| action.label()))
                    .unwrap_or_else(|| {
                        if child.has_children() {
                            "…".to_string()
                        } else {
                            String::new()
                        }
                    });
                (chord.label(), desc)
            })
            .collect();
        if items.is_empty() {
            return;
        }
        let area = f.area();
        if area.width < 16 || area.height < 5 {
            return;
        }

        let max_rows = area.height.saturating_sub(3).max(1) as usize;
        let cols = items.len().div_ceil(max_rows).max(1);
        let rows = items.len().div_ceil(cols);

        let key_w = items
            .iter()
            .map(|(k, _)| k.chars().count())
            .max()
            .unwrap_or(1);
        let desc_w = items
            .iter()
            .map(|(_, d)| d.chars().count())
            .max()
            .unwrap_or(0);
        let gap = 2usize;
        let max_width = area.width.saturating_sub(2) as usize;
        let overhead = 2 + cols * (key_w + 2) + cols.saturating_sub(1) * gap;
        let desc_w = desc_w.min(max_width.saturating_sub(overhead) / cols).max(4);
        let col_w = key_w + 2 + desc_w;
        let width = (cols * col_w + cols.saturating_sub(1) * gap + 2).min(max_width.max(8)) as u16;
        let height = (rows + 2).min(area.height.saturating_sub(1) as usize) as u16;
        let rect = Rect::new(
            area.x + area.width - width,
            area.y + area.height - 1 - height,
            width,
            height,
        );

        let mut lines = Vec::new();
        for r in 0..rows {
            let mut spans = Vec::new();
            for c in 0..cols {
                let Some((key, desc)) = items.get(c * rows + r) else {
                    continue;
                };
                if c > 0 {
                    spans.push(Span::raw(" ".repeat(gap)));
                }
                let key_pad = key_w - key.chars().count();
                spans.push(Span::styled(
                    format!("{key}{}", " ".repeat(key_pad + 2)),
                    self.config.theme.prefix_key,
                ));
                let desc_pad = desc_w.saturating_sub(desc.chars().count());
                spans.push(Span::styled(
                    format!("{desc}{}", " ".repeat(desc_pad)),
                    self.config.theme.prefix_desc,
                ));
            }
            lines.push(Line::from(spans));
        }

        let keys: Vec<String> = self.pending.iter().map(|c| c.label()).collect();
        let title = Line::from(vec![
            Span::styled(
                format!(" {} ", keys.join(" ")),
                self.config.theme.prefix_key,
            ),
            Span::styled("· Esc ", self.config.theme.dim),
        ]);
        f.render_widget(Clear, rect);
        f.render_widget(
            Paragraph::new(lines).block(
                Block::bordered()
                    .border_style(self.config.theme.border)
                    .title(title),
            ),
            rect,
        );
    }

    fn draw_info(&mut self, f: &mut Frame) {
        let Some(info) = &self.info else {
            return;
        };
        let area = f.area();
        if area.width < 24 || area.height < 5 {
            return;
        }
        let longest = info
            .lines
            .iter()
            .filter_map(|line| match line {
                InfoLine::Field(label, value) => {
                    Some(2 + label.chars().count() + 2 + value.chars().count())
                }
                InfoLine::Text(value) => Some(value.chars().count()),
                InfoLine::Section(_) => None,
            })
            .max()
            .unwrap_or(20)
            .min(110) as u16;
        let width = (longest + 4).max(60).min(area.width.saturating_sub(4));
        let avail = width.saturating_sub(4) as usize;
        let total = info.lines.len();
        let cursor = self.info_cursor.min(total.saturating_sub(1));

        // Expand sections into display rows (sections get a blank separator).
        let mut rows: Vec<(Option<usize>, Line<'static>)> = Vec::new();
        for (i, line) in info.lines.iter().enumerate() {
            let selected = i == cursor;
            let base = if selected {
                self.config.theme.selection
            } else {
                Style::new()
            };
            match line {
                InfoLine::Section(title) => {
                    if !rows.is_empty() {
                        rows.push((None, Line::from("")));
                    }
                    rows.push((
                        Some(i),
                        Line::from(Span::styled(*title, self.config.theme.info_section))
                            .style(base),
                    ));
                }
                InfoLine::Field(label, value) => rows.push((
                    Some(i),
                    Line::from(vec![
                        Span::styled(format!("  {label:<12}"), self.config.theme.info_label),
                        Span::raw(truncate_chars(value, avail.saturating_sub(16))),
                    ])
                    .style(base),
                )),
                InfoLine::Text(value) => rows.push((
                    Some(i),
                    Line::from(Span::raw(truncate_chars(value, avail))).style(base),
                )),
            }
        }

        let height = (rows.len() as u16 + 2).min(area.height.saturating_sub(2));
        let rect = Rect::new(
            area.x + (area.width - width) / 2,
            area.y + (area.height - height) / 2,
            width,
            height,
        );
        let max = height.saturating_sub(2) as usize;
        let cursor_row = rows
            .iter()
            .position(|(i, _)| *i == Some(cursor))
            .unwrap_or(0);
        let mut scroll = self.info_scroll;
        if cursor_row < scroll {
            scroll = cursor_row;
        } else if max > 0 && cursor_row >= scroll + max {
            scroll = cursor_row + 1 - max;
        }
        scroll = scroll.min(rows.len().saturating_sub(max));
        self.info_scroll = scroll;

        let lines: Vec<Line<'static>> = rows
            .into_iter()
            .skip(scroll)
            .take(max)
            .map(|(_, line)| line)
            .collect();
        let title = if self.info_from_tasks {
            " task ".to_string()
        } else {
            format!(" info {}/{} ", cursor + 1, total)
        };
        f.render_widget(Clear, rect);
        f.render_widget(
            Paragraph::new(lines).block(
                Block::bordered()
                    .border_style(self.config.theme.border)
                    .title(title),
            ),
            rect,
        );
    }

    fn draw_tasks(&mut self, f: &mut Frame) {
        let area = f.area();
        if area.width < 30 || area.height < 6 {
            return;
        }
        let width = (area.width as u32 * 80 / 100)
            .max(40)
            .min(area.width as u32) as u16;
        let height = (area.height as u32 * 80 / 100)
            .max(8)
            .min(area.height as u32) as u16;
        let rect = Rect::new(
            area.x + (area.width - width) / 2,
            area.y + (area.height - height) / 2,
            width,
            height,
        );
        let inner_width = width.saturating_sub(4) as usize;
        let inner_height = height.saturating_sub(2) as usize;

        let total = self.tasks.len();
        let running = self
            .tasks
            .iter()
            .filter(|task| {
                task.state
                    .lock()
                    .map(|s| s.status == TaskStatus::Running)
                    .unwrap_or(false)
            })
            .count();
        let cursor = self.task_cursor.min(total.saturating_sub(1));
        let visible = (inner_height / 2).max(1);
        let start = if cursor >= visible {
            cursor + 1 - visible
        } else {
            0
        };

        let mut lines: Vec<Line<'static>> = Vec::new();
        if total == 0 {
            lines.push(Line::from(Span::styled(
                "  no tasks",
                self.config.theme.dim,
            )));
        } else {
            for (i, task) in self.tasks.iter().enumerate().skip(start).take(visible) {
                let (status, progress) = match task.state.lock() {
                    Ok(state) => (state.status, state.progress.clone()),
                    Err(_) => (TaskStatus::Failed, TaskProgress::default()),
                };
                let percent = if status == TaskStatus::Done {
                    100
                } else {
                    task_percent(&progress)
                };
                let (icon, color) = match status {
                    TaskStatus::Running => ("▶", self.config.theme.task_running),
                    TaskStatus::Done => ("✓", self.config.theme.task_done),
                    TaskStatus::Failed => ("✗", self.config.theme.task_failed),
                    TaskStatus::Cancelled => ("⊘", self.config.theme.task_cancelled),
                };
                let selected = i == cursor;
                let base = if selected {
                    self.config.theme.selection
                } else {
                    Style::new()
                };
                let marker = if selected { "▸ " } else { "  " };
                let show_progress = self.config.tasks.show_progress;
                let bar_width = 20usize.min(inner_width.saturating_sub(30)).max(6);
                let bar = if show_progress {
                    progress_bar(percent, bar_width)
                } else {
                    String::new()
                };
                let title_width = inner_width
                    .saturating_sub(if show_progress { bar_width + 12 } else { 8 })
                    .max(8);
                let title = truncate_chars(&task.title, title_width);
                let mut spans = vec![
                    Span::styled(
                        format!("{marker}{icon} "),
                        color.add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(title, base),
                ];
                if show_progress {
                    spans.push(Span::raw(" "));
                    spans.push(Span::styled(bar, color));
                    spans.push(Span::styled(
                        format!(" {percent:>3}%"),
                        self.config.theme.task_percent,
                    ));
                }
                lines.push(Line::from(spans).style(base));
                let detail = if let Some(error) = progress.errors.last() {
                    error.clone()
                } else if !progress.current.is_empty() {
                    progress.current.clone()
                } else {
                    match status {
                        TaskStatus::Running => "…".into(),
                        TaskStatus::Done => format!("{} item(s)", progress.done),
                        TaskStatus::Cancelled => "cancelled".into(),
                        TaskStatus::Failed => "failed".into(),
                    }
                };
                lines.push(
                    Line::from(Span::styled(
                        format!(
                            "    {}",
                            truncate_chars(&detail, inner_width.saturating_sub(6))
                        ),
                        self.config.theme.dim,
                    ))
                    .style(base),
                );
            }
        }

        let title = format!(" tasks {running} running · {total} total ");
        f.render_widget(Clear, rect);
        f.render_widget(
            Paragraph::new(lines).block(
                Block::bordered()
                    .border_style(self.config.theme.border)
                    .title(title),
            ),
            rect,
        );
    }

    fn draw_open_with(&self, f: &mut Frame) {
        let Some(state) = &self.open_with else {
            return;
        };
        let area = f.area();
        if area.width < 30 || area.height < 5 {
            return;
        }
        let height = (state.options.len() as u16 + 2).min(area.height.saturating_sub(2));
        let width = 68.min(area.width.saturating_sub(4));
        let rect = Rect::new(
            area.x + (area.width - width) / 2,
            area.y + (area.height - height) / 2,
            width,
            height,
        );
        let inner = width.saturating_sub(4) as usize;
        let visible = height.saturating_sub(2) as usize;
        let start = if state.index >= visible {
            state.index + 1 - visible
        } else {
            0
        };
        let lines: Vec<Line<'static>> = state
            .options
            .iter()
            .enumerate()
            .skip(start)
            .take(visible)
            .map(|(i, op)| {
                let marker = if i == state.index { "▸ " } else { "  " };
                let desc = op.desc.as_deref().unwrap_or("");
                let text = if desc.is_empty() {
                    format!("{marker}{}. {}", i + 1, op.name)
                } else {
                    format!("{marker}{}. {} — {}", i + 1, op.name, desc)
                };
                let style = if i == state.index {
                    Style::new().add_modifier(Modifier::REVERSED | Modifier::BOLD)
                } else {
                    Style::new()
                };
                Line::from(Span::styled(truncate_chars(&text, inner), style))
            })
            .collect();
        f.render_widget(Clear, rect);
        f.render_widget(
            Paragraph::new(lines).block(Block::bordered().title(" open with ")),
            rect,
        );
    }

    /// Render a prompt label and editable buffer, highlighting the selection
    /// and placing the terminal cursor so the caret stays visible.
    fn draw_prompt_status(&self, f: &mut Frame, area: Rect, p: Prompt) {
        let label = match p {
            Prompt::Create => "create",
            Prompt::Rename => "rename",
            Prompt::Cd => "go to",
            Prompt::SearchFd => "fd",
            Prompt::SearchRg => "rg",
            Prompt::Shell => "shell",
            Prompt::TabRename => "rename tab",
            Prompt::Delete => "",
        };
        let prefix = format!("{label}: ");
        let prefix_w = Span::raw(prefix.as_str()).width();
        if prefix_w >= area.width as usize {
            f.render_widget(
                Paragraph::new(prefix).style(self.config.theme.status_info),
                area,
            );
            return;
        }
        let avail = area.width as usize - prefix_w;
        let text = self.input.as_str();
        let cursor_col = Span::raw(&text[..self.input.cursor]).width();
        let scroll = cursor_col.saturating_sub(avail.saturating_sub(1));
        let start = byte_at_col(text, scroll);
        let end = byte_at_col(text, scroll + avail);

        let mut spans = vec![Span::raw(prefix)];
        let mut pos = start;
        if let Some(sel) = self.input.selection() {
            let sel_start = sel.start.max(start).min(end);
            let sel_end = sel.end.min(end).max(sel_start);
            if sel_start > pos {
                spans.push(Span::raw(&text[pos..sel_start]));
            }
            if sel_end > sel_start {
                spans.push(Span::styled(
                    &text[sel_start..sel_end],
                    self.config.theme.selection,
                ));
            }
            pos = sel_end.max(pos);
        }
        if end > pos {
            spans.push(Span::raw(&text[pos..end]));
        }

        f.render_widget(
            Paragraph::new(Line::from(spans)).style(self.config.theme.status_info),
            area,
        );
        let x = area.x + (prefix_w + cursor_col - scroll) as u16;
        f.set_cursor_position((x.min(area.right().saturating_sub(1)), area.y));
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
            "d / D       trash / delete (confirm)",
            "r / a       rename / create",
            "A           bulk create",
            "w           task manager",
            "Ctrl+n      create",
            "Ctrl+y/p    clipboard copy / paste",
            "; / :       run shell command",
            "Ctrl+z      suspend",
            "t t / t n   new tab / at home",
            "t r / t c   rename / close tab",
            "1-9 [ ]     switch tab",
            "{ / }       swap tab",
            "Tab         info (j/k h/l c c)",
            "Ctrl+c      close tab / quit",
        ];
        let right = [
            "o / Enter   open",
            "O / S-Enter open with…",
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
            "Z           zoxide jump",
            "c …         copy path/url/dir/name",
            "m …         linemode size/perms/time",
            "_ / Alt+-   symlink relative / absolute",
            "Ctrl+-      hardlink",
            "gh gc gd    home / config / downloads",
            "go gp gv    osu / pictures / videos",
            "g<Space>    go to path (live menu)",
            "gf / gt     follow symlink / trash",
            "O in trash  restore / empty",
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
        // Erase a stale sixel before this frame's text is written, and erase
        // it when the pane moved (ratio change, preview toggled).
        self.preview.clear_sixel_pending();
        self.preview.clear_sixel_if_moved(inner);
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
        f.render_widget(
            Paragraph::new(title).style(self.config.theme.preview_title),
            title_area,
        );

        for y in inner.y..inner.y + inner.height {
            for x in inner.x..inner.x + inner.width {
                if let Some(c) = f.buffer_mut().cell_mut((x, y)) {
                    *c = ratatui::buffer::Cell::default();
                }
            }
        }
        let is_image = meta
            .map(|(k, _)| k == "image" || k == "video")
            .unwrap_or(false);
        if is_image {
            match self.preview.protocol() {
                ImageProtocol::Kitty => {
                    if let Some(s) = self.preview.image_state_mut() {
                        s.render(inner, f.buffer_mut());
                    } else {
                        f.render_widget(
                            Paragraph::new(Line::styled(
                                "image preview needs kitty or ghostty",
                                self.config.theme.dim,
                            )),
                            inner,
                        );
                    }
                }
                ImageProtocol::Sixel => {
                    // Cells stay blank; the sixel is painted directly after
                    // ratatui flushes this frame (see `flush_sixel`).
                    self.preview.prepare_sixel(inner);
                }
                ImageProtocol::None => {
                    f.render_widget(
                        Paragraph::new(Line::styled(
                            "image preview needs a kitty or sixel terminal",
                            self.config.theme.dim,
                        )),
                        inner,
                    );
                }
            }
        } else {
            let h = inner.height as usize;
            let lines = self.preview.lines_slice(h).unwrap_or_default();
            f.render_widget(Paragraph::new(lines), inner);
        }
    }

    /// Full-screen views that cover the preview pane, so sixels must not be
    /// painted over them.
    fn overlay_open(&self) -> bool {
        if !self.pending.is_empty() {
            return true;
        }
        if matches!(
            self.mode,
            Mode::Help | Mode::Info | Mode::Tasks | Mode::OpenWith
        ) {
            return true;
        }
        matches!(self.mode, Mode::Prompt(Prompt::Cd)) && !self.completions.is_empty()
    }

    // ─────────────── terminal / mouse ───────────────

    /// Mouse capture is enabled iff at least one event kind is configured,
    /// matching Yazi's `mgr.mouse_events` semantics.
    pub fn mouse_enabled(&self) -> bool {
        self.config.mgr.mouse_events != 0
    }

    pub fn enable_mouse(&self) {
        if self.mouse_enabled() {
            let _ = execute!(io::stdout(), EnableMouseCapture);
        }
    }

    pub fn disable_mouse(&self) {
        if self.mouse_enabled() {
            let _ = execute!(io::stdout(), DisableMouseCapture);
        }
    }

    /// Leave the alternate screen and release the mouse before handing the
    /// terminal to an external program or suspending.
    fn restore_terminal(&self) {
        self.disable_mouse();
        ratatui::restore();
    }

    /// Re-enter the alternate screen and restore mouse capture afterwards.
    fn init_terminal(&mut self) -> DefaultTerminal {
        let term = ratatui::init();
        self.enable_mouse();
        self.preview.force_sixel_redraw();
        term
    }

    /// Translate a crossterm mouse event into its `mgr.mouse_events` flag.
    fn mouse_flag(kind: MouseEventKind) -> u8 {
        match kind {
            MouseEventKind::Down(_) | MouseEventKind::Up(_) => MOUSE_CLICK,
            MouseEventKind::ScrollDown | MouseEventKind::ScrollUp => MOUSE_SCROLL,
            MouseEventKind::ScrollLeft | MouseEventKind::ScrollRight => MOUSE_TOUCH,
            MouseEventKind::Moved => MOUSE_MOVE,
            MouseEventKind::Drag(_) => MOUSE_DRAG,
        }
    }

    /// Handle a mouse event in the manager view. Returns whether the screen
    /// needs to be redrawn. Overlays and prompts ignore mouse input, mirroring
    /// Yazi routing events to the `mgr` layer only.
    fn handle_mouse(&mut self, mouse: MouseEvent, term: &mut DefaultTerminal) -> io::Result<bool> {
        if self.config.mgr.mouse_events & Self::mouse_flag(mouse.kind) == 0 {
            return Ok(false);
        }
        if std::env::var_os("RNN_DEBUG").is_some() {
            eprintln!(
                "mouse: {:?} at ({}, {}) mode={:?}",
                mouse.kind, mouse.column, mouse.row, self.mode
            );
        }
        if self.mode != Mode::Normal {
            return Ok(false);
        }

        let x = mouse.column;
        let y = mouse.row;
        let inside = |r: Rect| {
            r.width > 0 && r.height > 0 && x >= r.x && x < r.right() && y >= r.y && y < r.bottom()
        };
        let left = mouse.kind == MouseEventKind::Down(MouseButton::Left);
        let right = mouse.kind == MouseEventKind::Down(MouseButton::Right);

        // Tab bar: left or right click switches to the clicked tab.
        if (left || right) && inside(self.title_area) {
            if let Some(index) = tab_at(&self.tab_spans, x) {
                self.begin_mouse_action();
                self.switch_tab(index);
                return Ok(true);
            }
        }

        if inside(self.list_area) {
            match mouse.kind {
                MouseEventKind::Down(MouseButton::Left)
                | MouseEventKind::Down(MouseButton::Right) => {
                    if let Some(index) = list_index_at(
                        self.list_area,
                        self.state.offset(),
                        self.entries.len(),
                        x,
                        y,
                    ) {
                        self.begin_mouse_action();
                        self.state.select(Some(index));
                        self.apply_visual();
                        if std::env::var_os("RNN_DEBUG").is_some() {
                            eprintln!("mouse: select {index} ({})", self.entries[index].name);
                        }
                        if right {
                            self.open(term);
                        }
                        return Ok(true);
                    }
                }
                MouseEventKind::ScrollDown => {
                    self.begin_mouse_action();
                    self.move_by(1);
                    return Ok(true);
                }
                MouseEventKind::ScrollUp => {
                    self.begin_mouse_action();
                    self.move_by(-1);
                    return Ok(true);
                }
                _ => {}
            }
        }

        if inside(self.preview_area) {
            match mouse.kind {
                MouseEventKind::Down(MouseButton::Left) => {
                    self.begin_mouse_action();
                    self.open(term);
                    return Ok(true);
                }
                MouseEventKind::ScrollDown => {
                    self.begin_mouse_action();
                    self.seek_preview(1);
                    return Ok(true);
                }
                MouseEventKind::ScrollUp => {
                    self.begin_mouse_action();
                    self.seek_preview(-1);
                    return Ok(true);
                }
                _ => {}
            }
        }

        Ok(false)
    }

    fn begin_mouse_action(&mut self) {
        self.pending.clear();
        self.status.clear();
    }

    fn seek_preview(&mut self, delta: isize) {
        let current = self.preview.preview_seek();
        let seek = if delta >= 0 {
            current.saturating_add(delta as usize)
        } else {
            current.saturating_sub((-delta) as usize)
        };
        self.preview.set_preview_seek(seek);
    }

    // ─────────────── main loop ───────────────

    pub fn run(&mut self, term: &mut DefaultTerminal) -> io::Result<()> {
        let time = std::env::var_os("RNN_TIME").is_some();
        let mut last_search_poll = std::time::Instant::now();
        let mut last_task_poll = std::time::Instant::now();
        self.preview.begin_sixel(!self.overlay_open());
        term.draw(|f| self.draw(f))?;
        self.preview.flush_sixel(!self.overlay_open());
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
                    Event::Mouse(m) => {
                        if self.handle_mouse(m, term)? {
                            dirty = true;
                        }
                    }
                    Event::Resize(_, _) => {
                        self.preview.force_sixel_redraw();
                        dirty = true;
                    }
                    _ => {}
                }
            }
            if self.quit {
                return Ok(());
            }
            self.preview.poll();
            if self.preview.take_changed() {
                dirty = true;
            }
            if self.search_rx.is_some() && last_search_poll.elapsed() >= Duration::from_millis(50) {
                last_search_poll = std::time::Instant::now();
                if self.poll_search() {
                    dirty = true;
                }
            }
            if last_task_poll.elapsed() >= Duration::from_millis(100) {
                last_task_poll = std::time::Instant::now();
                if self.poll_tasks() {
                    dirty = true;
                }
            }
            if dirty {
                let t = std::time::Instant::now();
                let mut closure = Duration::ZERO;
                self.preview.begin_sixel(!self.overlay_open());
                term.draw(|f| {
                    let tc = std::time::Instant::now();
                    self.draw(f);
                    closure = tc.elapsed();
                })?;
                self.preview.flush_sixel(!self.overlay_open());
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

/// The tab at `x`. Like Yazi, pick the last tab whose start is at or before
/// `x`, so clicking empty space to the right of the last tab selects it.
fn tab_at(spans: &[(u16, u16)], x: u16) -> Option<usize> {
    spans.iter().rposition(|&(start, _)| x >= start)
}

/// The entry index at a screen position inside the list area. `offset` is the
/// first visible row (as reported by `ListState`), so this stays correct after
/// scrolloff scrolling.
fn list_index_at(area: Rect, offset: usize, len: usize, x: u16, y: u16) -> Option<usize> {
    if area.width == 0
        || area.height == 0
        || x < area.x
        || x >= area.right()
        || y < area.y
        || y >= area.bottom()
    {
        return None;
    }
    let index = offset + (y - area.y) as usize;
    (index < len).then_some(index)
}

/// Compute the list offset that keeps `margin` rows of context above and below
/// the selected item, clamped so the list never scrolls past its end.
fn scrolloff_offset(
    selected: usize,
    offset: usize,
    height: usize,
    len: usize,
    margin: usize,
) -> usize {
    if height == 0 || margin == 0 {
        return offset;
    }
    let margin = margin.min(height.saturating_sub(1) / 2);
    let mut next = offset;
    if selected < offset + margin {
        next = selected.saturating_sub(margin);
    } else if selected + margin >= offset + height {
        next = selected + margin + 1 - height;
    }
    next.min(len.saturating_sub(height))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_dir(tag: &str) -> PathBuf {
        let dir = env::temp_dir().join(format!(
            "rnn-test-{tag}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn mode_string_matches_ls_style() {
        assert_eq!(mode_string(0o100644), "-rw-r--r--");
        assert_eq!(mode_string(0o040755), "drwxr-xr-x");
        assert_eq!(mode_string(0o120777), "lrwxrwxrwx");
        assert_eq!(mode_string(0o104755), "-rwsr-xr-x");
        assert_eq!(mode_string(0o102755), "-rwxr-sr-x");
        assert_eq!(mode_string(0o041777), "drwxrwxrwt");
        assert_eq!(mode_string(0o104644), "-rwSr--r--");
    }

    #[test]
    fn rename_stem_stops_before_extension() {
        assert_eq!(rename_stem_end("file.txt"), 4);
        assert_eq!(rename_stem_end("archive.tar.gz"), 11);
        assert_eq!(rename_stem_end("README"), 6);
        assert_eq!(rename_stem_end("file."), 5);
        assert_eq!(rename_stem_end(""), 0);
    }

    #[test]
    fn rename_stem_keeps_dotfiles_whole() {
        assert_eq!(rename_stem_end(".bashrc"), 7);
        assert_eq!(rename_stem_end(".."), 2);
        assert_eq!(rename_stem_end("..."), 3);
    }

    #[test]
    fn rename_input_selects_stem_and_keeps_extension() {
        let mut input = Input::rename("notes.txt");
        assert_eq!(input.as_str(), "notes.txt");
        assert_eq!(input.cursor, 5);
        assert_eq!(input.selection(), Some(0..5));
        input.insert('x');
        assert_eq!(input.as_str(), "x.txt");
        assert_eq!(input.cursor, 1);
        assert_eq!(input.selection(), None);
    }

    #[test]
    fn rename_input_selects_whole_dotfile() {
        let input = Input::rename(".bashrc");
        assert_eq!(input.cursor, 7);
        assert_eq!(input.selection(), Some(0..7));
    }

    #[test]
    fn input_editing_respects_char_boundaries() {
        let mut input = Input::new("héllo");
        assert_eq!(input.cursor, 6);
        input.backspace();
        assert_eq!(input.as_str(), "héll");
        input.move_home();
        input.delete();
        assert_eq!(input.as_str(), "éll");
        assert_eq!(input.cursor, 0);
        input.insert('x');
        assert_eq!(input.as_str(), "xéll");
    }

    #[test]
    fn input_selection_edits_and_collapses() {
        let mut input = Input::new("abc.def");
        input.select_all();
        assert_eq!(input.selection(), Some(0..7));
        input.insert('z');
        assert_eq!(input.as_str(), "z");
        assert_eq!(input.cursor, 1);
        assert_eq!(input.selection(), None);

        let mut input = Input::new("abc.def");
        input.move_end();
        for _ in 0..3 {
            input.select_left();
        }
        assert_eq!(input.selection(), Some(4..7));
        input.backspace();
        assert_eq!(input.as_str(), "abc.");
        assert_eq!(input.cursor, 4);

        let mut input = Input::new("abc");
        input.select_left();
        input.select_home();
        assert_eq!(input.selection(), Some(0..3));
        input.move_right();
        assert_eq!(input.cursor, 3);
        assert_eq!(input.selection(), None);
    }

    #[test]
    fn input_kill_and_word_delete() {
        let mut input = Input::new("alpha beta ");
        input.move_end();
        input.delete_word();
        assert_eq!(input.as_str(), "alpha ");
        input.insert('g');
        assert_eq!(input.as_str(), "alpha g");
        input.kill_to_start();
        assert_eq!(input.as_str(), "");
        assert_eq!(input.cursor, 0);

        let mut input = Input::new("one two three");
        input.move_home();
        input.kill_to_end();
        assert_eq!(input.as_str(), "");

        let mut input = Input::new("one two");
        input.move_end();
        input.kill_to_end();
        assert_eq!(input.as_str(), "one two");
    }

    #[test]
    fn format_mtime_uses_supplied_offset() {
        let epoch = Some(SystemTime::UNIX_EPOCH);
        assert_eq!(
            format_mtime(epoch, time::UtcOffset::UTC),
            "1970-01-01 00:00"
        );
        let mst = time::UtcOffset::from_hms(-7, 0, 0).unwrap();
        assert_eq!(format_mtime(epoch, mst), "1969-12-31 17:00");
        assert_eq!(format_mtime(None, mst), "-");
    }

    #[test]
    fn extension_label_matches_nnn() {
        let dir = tmp_dir("extlabel");
        let file = dir.join("a.tar.gz");
        fs::write(&file, b"x").unwrap();
        let entry = entry_for(file.clone(), "a.tar.gz".into()).unwrap();
        let md = fs::symlink_metadata(&file).unwrap();
        assert_eq!(extension_label(&entry, Some(&md)), ".gz");

        let plain = dir.join("plain.");
        fs::write(&plain, b"x").unwrap();
        let entry = entry_for(plain.clone(), "plain.".into()).unwrap();
        let md = fs::symlink_metadata(&plain).unwrap();
        assert_eq!(extension_label(&entry, Some(&md)), "");

        let link = dir.join("link.sh");
        std::os::unix::fs::symlink("a.tar.gz", &link).unwrap();
        let entry = entry_for(link.clone(), "link.sh".into()).unwrap();
        let md = fs::symlink_metadata(&link).unwrap();
        assert_eq!(extension_label(&entry, Some(&md)), "->a.tar.gz");

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn percent_path_round_trip() {
        for path in [
            Path::new("/tmp/plain.txt"),
            Path::new("/tmp/with space/and#hash"),
            Path::new("/tmp/unicode-λ/文件"),
            Path::new("/tmp/percent%25"),
        ] {
            let encoded = percent_encode_path(path);
            assert_eq!(percent_decode_path(&encoded), path);
        }
        assert_eq!(percent_encode_path(Path::new("/a b")), "/a%20b");
        assert_eq!(percent_decode_path("/a%20b"), Path::new("/a b"));
    }

    #[test]
    fn parse_trashinfo_reads_path_and_date() {
        let dir = tmp_dir("trashinfo");
        let info = dir.join("notes.txt.trashinfo");
        fs::write(
            &info,
            "[Trash Info]\nPath=/home/user/My%20Notes/notes.txt\nDeletionDate=2024-01-02T03:04:05\n",
        )
        .unwrap();
        let parsed = parse_trashinfo(&info).unwrap();
        assert_eq!(parsed.name, "notes.txt");
        assert_eq!(parsed.original, Path::new("/home/user/My Notes/notes.txt"));
        assert_eq!(parsed.date, "2024-01-02T03:04:05");
        assert!(parse_trashinfo(&dir.join("missing.trashinfo")).is_none());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn task_percent_clamps_and_handles_empty() {
        let empty = TaskProgress::default();
        assert_eq!(task_percent(&empty), 0);

        let done_only = TaskProgress {
            done: 3,
            ..Default::default()
        };
        assert_eq!(task_percent(&done_only), 100);

        let half = TaskProgress {
            total: 4,
            done: 2,
            ..Default::default()
        };
        assert_eq!(task_percent(&half), 50);

        let over = TaskProgress {
            total: 4,
            done: 9,
            ..Default::default()
        };
        assert_eq!(task_percent(&over), 100);
    }

    #[test]
    fn count_tree_walks_directories() {
        let dir = tmp_dir("count");
        fs::write(dir.join("a"), b"a").unwrap();
        fs::create_dir_all(dir.join("sub/deep")).unwrap();
        fs::write(dir.join("sub/b"), b"b").unwrap();
        fs::write(dir.join("sub/deep/c"), b"c").unwrap();

        // dir + a + sub + b + deep + c
        assert_eq!(count_tree(std::slice::from_ref(&dir)), 6);
        assert_eq!(count_tree(&[dir.join("a")]), 1);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn scrolloff_keeps_context_around_cursor() {
        // 20 rows, margin 5: the cursor stays at least five rows from the edge.
        assert_eq!(scrolloff_offset(14, 0, 20, 100, 5), 0);
        assert_eq!(scrolloff_offset(15, 0, 20, 100, 5), 1);
        assert_eq!(scrolloff_offset(16, 0, 20, 100, 5), 2);
        // Scrolling up keeps the cursor five rows below the top.
        assert_eq!(scrolloff_offset(14, 10, 20, 100, 5), 9);
        assert_eq!(scrolloff_offset(3, 10, 20, 100, 5), 0);
        // The list never scrolls past its end.
        assert_eq!(scrolloff_offset(99, 80, 20, 100, 5), 80);
        assert_eq!(scrolloff_offset(98, 0, 20, 100, 5), 80);
        // A short list cannot scroll.
        assert_eq!(scrolloff_offset(4, 0, 20, 10, 5), 0);
        // Margin is capped at half the height and 0 disables it.
        assert_eq!(scrolloff_offset(4, 0, 6, 100, 5), 1);
        assert_eq!(scrolloff_offset(10, 0, 20, 100, 0), 0);
    }

    #[test]
    fn mouse_hit_testing() {
        let area = Rect::new(0, 1, 32, 10);
        assert_eq!(list_index_at(area, 0, 5, 2, 1), Some(0));
        assert_eq!(list_index_at(area, 0, 5, 2, 5), Some(4));
        // Below the last entry or outside the pane.
        assert_eq!(list_index_at(area, 0, 5, 2, 6), None);
        assert_eq!(list_index_at(area, 0, 5, 2, 0), None);
        assert_eq!(list_index_at(area, 0, 5, 32, 2), None);
        // The visible offset is added to the clicked row.
        assert_eq!(list_index_at(area, 3, 10, 2, 2), Some(4));
        assert_eq!(list_index_at(area, 3, 10, 2, 9), None);
        // An empty pane never hits.
        assert_eq!(list_index_at(Rect::default(), 0, 5, 0, 0), None);

        let tabs = [(0, 13), (13, 26), (26, 39)];
        assert_eq!(tab_at(&tabs, 0), Some(0));
        assert_eq!(tab_at(&tabs, 12), Some(0));
        assert_eq!(tab_at(&tabs, 13), Some(1));
        assert_eq!(tab_at(&tabs, 38), Some(2));
        // Empty space right of the last tab selects it, as in Yazi.
        assert_eq!(tab_at(&tabs, 100), Some(2));
        assert_eq!(tab_at(&[], 0), None);
    }

    #[test]
    fn mouse_flags_match_event_kinds() {
        assert_eq!(
            App::mouse_flag(MouseEventKind::Down(MouseButton::Left)),
            MOUSE_CLICK
        );
        assert_eq!(
            App::mouse_flag(MouseEventKind::Up(MouseButton::Right)),
            MOUSE_CLICK
        );
        assert_eq!(App::mouse_flag(MouseEventKind::ScrollDown), MOUSE_SCROLL);
        assert_eq!(App::mouse_flag(MouseEventKind::ScrollUp), MOUSE_SCROLL);
        assert_eq!(App::mouse_flag(MouseEventKind::ScrollLeft), MOUSE_TOUCH);
        assert_eq!(App::mouse_flag(MouseEventKind::ScrollRight), MOUSE_TOUCH);
        assert_eq!(App::mouse_flag(MouseEventKind::Moved), MOUSE_MOVE);
        assert_eq!(
            App::mouse_flag(MouseEventKind::Drag(MouseButton::Left)),
            MOUSE_DRAG
        );
    }

    #[test]
    fn cli_parses_path_and_cwd_file() {
        let args = |list: &[&str]| list.iter().map(OsString::from).collect::<Vec<_>>();

        assert_eq!(parse_cli(args(&[])).unwrap(), Cli::default());
        assert_eq!(
            parse_cli(args(&["--cwd-file", "/tmp/cwd", "/tmp/start"])).unwrap(),
            Cli {
                path: Some(PathBuf::from("/tmp/start")),
                cwd_file: Some(PathBuf::from("/tmp/cwd")),
                ..Cli::default()
            }
        );
        assert_eq!(
            parse_cli(args(&["--cwd-file=/tmp/cwd"])).unwrap(),
            Cli {
                cwd_file: Some(PathBuf::from("/tmp/cwd")),
                ..Cli::default()
            }
        );
        assert_eq!(
            parse_cli(args(&["--", "-weird-dir"])).unwrap(),
            Cli {
                path: Some(PathBuf::from("-weird-dir")),
                ..Cli::default()
            }
        );
        assert!(parse_cli(args(&["--cwd-file"])).is_err());
        assert!(parse_cli(args(&["--cwd-file="])).is_err());
        assert!(parse_cli(args(&["--nope"])).is_err());
        assert!(parse_cli(args(&["one", "two"])).is_err());
    }

    #[test]
    fn cli_parses_picker_options() {
        let args = |list: &[&str]| list.iter().map(OsString::from).collect::<Vec<_>>();
        let expected = |path: Option<&str>, null: bool| Cli {
            chooser_file: path.map(PathBuf::from),
            chooser_null: null,
            ..Cli::default()
        };

        assert_eq!(
            parse_cli(args(&["--chooser-file", "/tmp/pick"])).unwrap(),
            expected(Some("/tmp/pick"), false)
        );
        assert_eq!(
            parse_cli(args(&["--chooser-file=/tmp/pick"])).unwrap(),
            expected(Some("/tmp/pick"), false)
        );
        assert_eq!(
            parse_cli(args(&["-p", "/tmp/pick"])).unwrap(),
            expected(Some("/tmp/pick"), false)
        );
        assert_eq!(
            parse_cli(args(&["-p/tmp/pick"])).unwrap(),
            expected(Some("/tmp/pick"), false)
        );
        assert_eq!(
            parse_cli(args(&["-p", "-", "-0"])).unwrap(),
            expected(Some("-"), true)
        );
        assert!(parse_cli(args(&["--chooser-file"])).is_err());
        assert!(parse_cli(args(&["--chooser-file="])).is_err());
        assert!(parse_cli(args(&["-p"])).is_err());
    }

    #[test]
    fn chooser_bytes_uses_the_requested_separator() {
        let paths = vec![PathBuf::from("/tmp/a"), PathBuf::from("/tmp/b c")];
        assert_eq!(chooser_bytes(&paths, false), b"/tmp/a\n/tmp/b c\n");
        assert_eq!(chooser_bytes(&paths, true), b"/tmp/a\0/tmp/b c\0");
    }

    #[test]
    fn cwd_file_gets_the_current_directory() {
        let path = std::env::temp_dir().join(format!(
            "rnn-cwd-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        write_cwd_file(&path, Path::new("/tmp/some dir"));
        assert_eq!(fs::read_to_string(&path).unwrap(), "/tmp/some dir\n");
        let _ = fs::remove_file(path);
    }
}
