use std::{
    collections::BTreeSet,
    env, fs,
    io::{self, Read},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::SystemTime,
};

use ratatui::{
    crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers},
    layout::{Constraint, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph},
    DefaultTerminal, Frame,
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
}

#[derive(Clone, Copy, PartialEq)]
enum Prompt {
    Mkdir,
    Touch,
    Rename,
    Delete,
}

#[derive(Clone, Copy)]
enum Mode {
    Normal,
    Filter,
    Prompt(Prompt),
}

struct App {
    cwd: PathBuf,
    entries: Vec<Entry>,
    state: ListState,
    filter: String,
    show_hidden: bool,
    show_preview: bool,
    sort: Sort,
    selected: BTreeSet<PathBuf>,
    yank: Option<(Vec<PathBuf>, bool)>, // (paths, is_cut)
    mode: Mode,
    input: String,
    status: String,
    focus: Option<String>,
    page: usize,
}

// ───────────────────────── helpers ─────────────────────────

fn human(n: u64) -> String {
    const U: [&str; 5] = ["B", "K", "M", "G", "T"];
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1024.0 && i < U.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{n}B")
    } else {
        format!("{v:.1}{}", U[i])
    }
}

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

fn sanitize(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            '\t' => ' ',
            c if c.is_control() => '·',
            c => c,
        })
        .collect()
}

// ───────────────────────── preview ─────────────────────────

/// Native preview: directories list their contents, text files show their
/// head, binary files show a hex dump.
fn preview(entry: &Entry, height: usize) -> Vec<Line<'static>> {
    if entry.is_dir {
        let mut names: Vec<(String, bool)> = match fs::read_dir(&entry.path) {
            Ok(rd) => rd
                .flatten()
                .map(|e| {
                    let is_dir = fs::metadata(e.path()).map(|m| m.is_dir()).unwrap_or(false);
                    (e.file_name().to_string_lossy().into_owned(), is_dir)
                })
                .collect(),
            Err(e) => return vec![Line::raw(format!("cannot read directory: {e}"))],
        };
        if names.is_empty() {
            return vec![Line::styled("(empty)", Style::new().fg(Color::DarkGray))];
        }
        names.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.to_lowercase().cmp(&b.0.to_lowercase())));
        return names
            .into_iter()
            .take(height)
            .map(|(n, d)| {
                if d {
                    Line::styled(format!("{}/", sanitize(&n)), Style::new().fg(Color::Blue))
                } else {
                    Line::raw(sanitize(&n))
                }
            })
            .collect();
    }

    let mut buf = Vec::new();
    match fs::File::open(&entry.path) {
        Ok(f) => {
            if let Err(e) = f.take(32 * 1024).read_to_end(&mut buf) {
                return vec![Line::raw(format!("cannot read file: {e}"))];
            }
        }
        Err(e) => return vec![Line::raw(format!("cannot open file: {e}"))],
    }
    if buf.is_empty() {
        return vec![Line::styled("(empty file)", Style::new().fg(Color::DarkGray))];
    }

    let sniff = &buf[..buf.len().min(1024)];
    if sniff.contains(&0) {
        let mut out = vec![
            Line::styled(
                format!("binary file, {}", human(entry.size)),
                Style::new().fg(Color::Yellow),
            ),
            Line::raw(""),
        ];
        for (i, chunk) in buf.chunks(16).take(height.saturating_sub(2)).enumerate() {
            let hex: Vec<String> = chunk.iter().map(|b| format!("{b:02x}")).collect();
            let asc: String = chunk
                .iter()
                .map(|&b| if (0x20..0x7f).contains(&b) { b as char } else { '.' })
                .collect();
            out.push(Line::raw(format!("{:06x}  {:<47}  {}", i * 16, hex.join(" "), asc)));
        }
        return out;
    }

    String::from_utf8_lossy(&buf)
        .lines()
        .take(height)
        .map(|l| Line::raw(sanitize(l)))
        .collect()
}

// ───────────────────────── app ─────────────────────────

impl App {
    fn new() -> io::Result<Self> {
        let start = env::args()
            .nth(1)
            .map(PathBuf::from)
            .unwrap_or(env::current_dir()?);
        let mut app = App {
            cwd: fs::canonicalize(start)?,
            entries: vec![],
            state: ListState::default(),
            filter: String::new(),
            show_hidden: false,
            show_preview: true,
            sort: Sort::Name,
            selected: BTreeSet::new(),
            yank: None,
            mode: Mode::Normal,
            input: String::new(),
            status: String::new(),
            focus: None,
            page: 10,
        };
        app.reload();
        Ok(app)
    }

    fn current(&self) -> Option<&Entry> {
        self.state.selected().and_then(|i| self.entries.get(i))
    }

    fn reload(&mut self) {
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
        v.sort_by(|a, b| {
            b.is_dir.cmp(&a.is_dir).then_with(|| match sort {
                Sort::Name => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
                Sort::Size => b.size.cmp(&a.size),
                Sort::Time => b.modified.cmp(&a.modified),
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
    }

    fn cd(&mut self, path: PathBuf, focus: Option<String>) {
        if let Err(e) = fs::read_dir(&path) {
            self.status = format!("{}: {e}", path.display());
            return;
        }
        self.cwd = path;
        self.filter.clear();
        self.entries.clear();
        self.state.select(None);
        self.focus = focus;
        self.reload();
    }

    fn go_parent(&mut self) {
        if let Some(p) = self.cwd.parent().map(Path::to_path_buf) {
            let name = self.cwd.file_name().map(|s| s.to_string_lossy().into_owned());
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
    }

    fn targets(&self) -> Vec<PathBuf> {
        if !self.selected.is_empty() {
            self.selected.iter().cloned().collect()
        } else {
            self.current().map(|e| vec![e.path.clone()]).unwrap_or_default()
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

    fn open(&mut self, term: &mut DefaultTerminal) {
        let Some(e) = self.current().cloned() else { return };
        if e.is_dir {
            self.cd(e.path, None);
            return;
        }
        let opener = env::var("OPENER").unwrap_or_else(|_| {
            if cfg!(target_os = "macos") { "open" } else { "xdg-open" }.to_string()
        });
        let _ = term;
        let r = Command::new(&opener)
            .arg(&e.path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
        if let Err(err) = r {
            self.status = format!("{opener}: {err}");
        }
    }

    fn submit(&mut self, p: Prompt) {
        let name = self.input.trim().to_string();
        if name.is_empty() {
            return;
        }
        let r = match p {
            Prompt::Mkdir => fs::create_dir_all(self.cwd.join(&name)),
            Prompt::Touch => fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(self.cwd.join(&name))
                .map(|_| ()),
            Prompt::Rename => match self.current() {
                Some(e) => fs::rename(&e.path, self.cwd.join(&name)),
                None => return,
            },
            Prompt::Delete => return,
        };
        match r {
            Ok(()) => {
                self.focus = Some(name);
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

    fn paste(&mut self) {
        let Some((paths, cut)) = self.yank.clone() else {
            self.status = "nothing to paste".into();
            return;
        };
        let mut n = 0;
        for src in paths {
            let Some(name) = src.file_name() else { continue };
            let dst = unique(self.cwd.join(name));
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
            Mode::Normal => {}
        }

        if key.modifiers.contains(KeyModifiers::CONTROL) {
            match key.code {
                KeyCode::Char('c') => return Ok(true),
                KeyCode::Char('d') => self.move_by((self.page / 2) as isize),
                KeyCode::Char('u') => self.move_by(-((self.page / 2) as isize)),
                _ => {}
            }
            return Ok(false);
        }

        match key.code {
            KeyCode::Char('q') => return Ok(true),
            KeyCode::Char('j') | KeyCode::Down => self.move_by(1),
            KeyCode::Char('k') | KeyCode::Up => self.move_by(-1),
            KeyCode::PageDown => self.move_by(self.page as isize),
            KeyCode::PageUp => self.move_by(-(self.page as isize)),
            KeyCode::Char('g') | KeyCode::Home => self.state.select(if self.entries.is_empty() {
                None
            } else {
                Some(0)
            }),
            KeyCode::Char('G') | KeyCode::End => {
                if !self.entries.is_empty() {
                    self.state.select(Some(self.entries.len() - 1));
                }
            }
            KeyCode::Char('h') | KeyCode::Left | KeyCode::Backspace => self.go_parent(),
            KeyCode::Char('l') | KeyCode::Right | KeyCode::Enter => self.open(term),
            KeyCode::Char('~') => {
                if let Some(h) = env::var_os("HOME") {
                    self.cd(PathBuf::from(h), None);
                }
            }
            KeyCode::Char('e') => {
                if let Some(e) = self.current().cloned().filter(|e| !e.is_dir) {
                    let ed = env::var("VISUAL")
                        .or_else(|_| env::var("EDITOR"))
                        .unwrap_or_else(|_| "vi".into());
                    self.external(term, &ed, &e.path);
                    self.reload();
                }
            }
            KeyCode::Char('p') => {
                if let Some(e) = self.current().cloned().filter(|e| !e.is_dir) {
                    let pg = env::var("PAGER").unwrap_or_else(|_| "less".into());
                    self.external(term, &pg, &e.path);
                }
            }
            KeyCode::Char(' ') => {
                if let Some(e) = self.current() {
                    let p = e.path.clone();
                    if !self.selected.remove(&p) {
                        self.selected.insert(p);
                    }
                    self.move_by(1);
                }
            }
            KeyCode::Char('a') => self.selected = self.entries.iter().map(|e| e.path.clone()).collect(),
            KeyCode::Char('u') => self.selected.clear(),
            KeyCode::Char('y') | KeyCode::Char('x') => {
                let t = self.targets();
                if !t.is_empty() {
                    self.status = format!("{} item(s) {}", t.len(), if key.code == KeyCode::Char('x') { "cut" } else { "yanked" });
                    self.yank = Some((t, key.code == KeyCode::Char('x')));
                    self.selected.clear();
                }
            }
            KeyCode::Char('P') => self.paste(),
            KeyCode::Char('d') => {
                if !self.targets().is_empty() {
                    self.mode = Mode::Prompt(Prompt::Delete);
                }
            }
            KeyCode::Char('r') => {
                if let Some(e) = self.current() {
                    self.input = e.name.clone();
                    self.mode = Mode::Prompt(Prompt::Rename);
                }
            }
            KeyCode::Char('n') => {
                self.input.clear();
                self.mode = Mode::Prompt(Prompt::Mkdir);
            }
            KeyCode::Char('t') => {
                self.input.clear();
                self.mode = Mode::Prompt(Prompt::Touch);
            }
            KeyCode::Char('/') => self.mode = Mode::Filter,
            KeyCode::Esc => {
                self.filter.clear();
                self.reload();
            }
            KeyCode::Char('.') => {
                self.show_hidden = !self.show_hidden;
                self.reload();
            }
            KeyCode::Char('v') => self.show_preview = !self.show_preview,
            KeyCode::Char('s') => {
                self.sort = match self.sort {
                    Sort::Name => Sort::Size,
                    Sort::Size => Sort::Time,
                    Sort::Time => Sort::Name,
                };
                self.reload();
            }
            _ => {}
        }
        Ok(false)
    }

    // ─────────────── drawing ───────────────

    fn draw(&mut self, f: &mut Frame) {
        let rows = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).split(f.area());
        let cols = if self.show_preview {
            Layout::horizontal([Constraint::Percentage(45), Constraint::Percentage(55)]).split(rows[0])
        } else {
            Layout::horizontal([Constraint::Percentage(100)]).split(rows[0])
        };
        self.page = rows[0].height.saturating_sub(2) as usize;

        // file list
        let width = cols[0].width.saturating_sub(2) as usize;
        let items: Vec<ListItem> = self
            .entries
            .iter()
            .map(|e| {
                let mark = if self.selected.contains(&e.path) { "▌" } else { " " };
                let left = format!("{mark}{}{}", e.name, if e.is_dir { "/" } else { "" });
                let right = if e.is_dir { String::new() } else { human(e.size) };
                let used = left.chars().count() + right.chars().count();
                let pad = width.saturating_sub(used).max(1);
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

        let list = List::new(items)
            .block(Block::default().borders(Borders::ALL).title(format!(" {} ", self.cwd.display())))
            .highlight_style(Style::new().add_modifier(Modifier::REVERSED));
        f.render_stateful_widget(list, cols[0], &mut self.state);

        // preview pane
        if self.show_preview {
            let h = cols[1].height.saturating_sub(2) as usize;
            let (title, lines) = match self.current() {
                Some(e) => (format!(" {} ", e.name), preview(e, h)),
                None => (" preview ".to_string(), vec![]),
            };
            f.render_widget(
                Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title(title)),
                cols[1],
            );
        }

        // status line
        let sort = match self.sort {
            Sort::Name => "name",
            Sort::Size => "size",
            Sort::Time => "time",
        };
        let text = match self.mode {
            Mode::Filter => format!("/{}", self.filter),
            Mode::Prompt(Prompt::Delete) => format!("delete {} item(s)? (y/N)", self.targets().len()),
            Mode::Prompt(p) => {
                let label = match p {
                    Prompt::Mkdir => "new dir",
                    Prompt::Touch => "new file",
                    Prompt::Rename => "rename",
                    Prompt::Delete => "",
                };
                format!("{label}: {}", self.input)
            }
            Mode::Normal if !self.status.is_empty() => self.status.clone(),
            Mode::Normal => format!(
                "{} items | {} selected | sort:{sort}{}{}",
                self.entries.len(),
                self.selected.len(),
                if self.show_hidden { " | hidden" } else { "" },
                if self.filter.is_empty() { String::new() } else { format!(" | filter:{}", self.filter) },
            ),
        };
        f.render_widget(Paragraph::new(text), rows[1]);
    }

    fn run(&mut self, term: &mut DefaultTerminal) -> io::Result<()> {
        loop {
            term.draw(|f| self.draw(f))?;
            if let Event::Key(k) = event::read()? {
                if k.kind != KeyEventKind::Press {
                    continue;
                }
                if self.handle_key(k, term)? {
                    return Ok(());
                }
            }
        }
    }
}

fn main() -> io::Result<()> {
    let mut app = App::new()?;
    let mut terminal = ratatui::init();
    let res = app.run(&mut terminal);
    ratatui::restore();
    res
}
