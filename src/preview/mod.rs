pub mod detect;
pub mod hex;
pub mod kitty;
pub mod media;
pub mod shm;
pub mod text;

use std::{
    collections::{HashMap, VecDeque},
    fs,
    path::{Path, PathBuf},
    sync::mpsc::{self, Receiver, Sender, TryRecvError},
    thread,
    time::SystemTime,
};

use image::DynamicImage;
use ratatui::{
    style::{Color, Style},
    text::Line,
};

use crate::{
    preview::kitty::KittyState,
    util::{human, sanitize},
};

/// Everything a preview is keyed by: path, mtime and size.
pub type Key = (PathBuf, Option<SystemTime>, u64);

#[derive(Clone)]
pub struct Artifact {
    pub kind: &'static str,
    pub data: Vec<Line<'static>>,
    img: Option<DynamicImage>,
}

enum ImgState {
    Kitty(KittyState),
}

struct Request {
    key: Key,
    preload: bool,
}

pub struct Preview {
    tx: Sender<Request>,
    rx: Receiver<(Key, Artifact)>,
    pending: Option<Key>,
    wanted: Option<Key>,
    active: Option<(Key, Artifact)>,
    cache: HashMap<Key, Artifact>,
    order: VecDeque<Key>,
    changed: bool,
    dead: bool,
    preview_seek: usize,
    kitty: bool,
    font: (u16, u16),
    is_tmux: bool,
    shm: bool,
    image_state: Option<ImgState>,
    last_streamed: u64,
}

const CACHE_MAX: usize = 16;
const DIR_LIST_MAX: usize = 5000;

impl Preview {
    pub fn new() -> Self {
        let (tx, req_rx) = mpsc::channel::<Request>();
        let (res_tx, rx) = mpsc::channel::<(Key, Artifact)>();
        let _ = thread::Builder::new()
            .name("preview-worker".into())
            .spawn(move || worker(req_rx, res_tx));
        Self {
            tx,
            rx,
            pending: None,
            wanted: None,
            active: None,
            cache: HashMap::new(),
            order: VecDeque::new(),
            changed: false,
            dead: false,
            preview_seek: 0,
            kitty: false,
            font: (8, 16),
            is_tmux: false,
            shm: false,
            image_state: None,
            last_streamed: 0,
        }
    }

    /// Enable kitty/ghostty image previews. Call once at startup. `shm` selects
    /// the shared-memory transfer medium, which is far faster under tmux.
    pub fn configure(&mut self, kitty_supported: bool, font: (u16, u16), shm: bool) {
        self.kitty = kitty_supported;
        self.font = font;
        self.is_tmux = std::env::var_os("TMUX").is_some();
        self.shm = shm;
    }

    /// The active kitty image renderer, if any.
    pub fn image_state_mut(&mut self) -> Option<&mut KittyState> {
        match &mut self.image_state {
            Some(ImgState::Kitty(k)) => Some(k),
            None => None,
        }
    }

    /// Whether images are displayed via kitty/ghostty (otherwise preloading
    /// them would be wasted work).
    pub fn kitty_enabled(&self) -> bool {
        self.kitty
    }

    /// Eagerly load `key` (an image) into both caches so navigating to it
    /// later is instant. A preload never changes `pending`, so it does not
    /// affect the loading indicator.
    pub fn preload(&mut self, key: Key) {
        if self.cache.contains_key(&key) {
            return;
        }
        let _ = self.tx.send(Request { key, preload: true });
    }

    fn clear_image(&mut self) {
        if let Some(ImgState::Kitty(old)) = self.image_state.take() {
            kitty::stream_delete(old.id(), self.is_tmux);
        }
    }

    pub fn take_changed(&mut self) -> bool {
        std::mem::take(&mut self.changed)
    }

    pub fn is_loading(&self) -> bool {
        self.pending.is_some()
    }

    pub fn preview_seek(&self) -> usize {
        self.preview_seek
    }

    pub fn set_preview_seek(&mut self, s: usize) {
        self.preview_seek = s;
    }

    /// `(kind, line_count)` of whatever is currently displayed.
    pub fn artifact_meta(&self) -> Option<(&'static str, usize)> {
        self.active.as_ref().map(|(_, a)| (a.kind, a.data.len()))
    }

    pub fn lines_slice(&self, rows: usize) -> Option<Vec<Line<'static>>> {
        let (_, a) = self.active.as_ref()?;
        Some(a.data.iter().skip(self.preview_seek).take(rows).cloned().collect())
    }

    /// Drain finished results from the worker without changing the target.
    /// Called from the event loop so previews complete even when no key is
    /// pressed (which is what triggers redraws).
    pub fn poll(&mut self) {
        self.drain();
        if let Some(ImgState::Kitty(k)) = &self.image_state {
            let g = k.transmitted_gen();
            if g != self.last_streamed {
                self.last_streamed = g;
                self.changed = true;
                if std::env::var_os("RNN_DEBUG").is_some() {
                    eprintln!("preview: stream gen {g} -> redraw");
                }
            }
        }
    }

    /// Point the preview at a new entry. Returns true when the target changed
    /// (callers use that to reset preview seek). Results arriving from the
    /// worker are drained and cached here.
    pub fn set_target(&mut self, key: Option<&Key>) -> bool {
        let target_changed = self.wanted.as_ref() != key;
        self.wanted = key.cloned();
        if key.is_none() {
            self.clear_image();
        }
        self.drain();

        if let Some(k) = key {
            let active_is_target = self
                .active
                .as_ref()
                .map(|(ak, _)| ak == k)
                .unwrap_or(false);
            if !active_is_target {
                if self.cache.contains_key(k) {
                    self.activate(k.clone());
                } else if !self.dead && self.pending.as_ref() != Some(k) {
                    self.pending = Some(k.clone());
                    let req = Request { key: k.clone(), preload: false };
                    if self.tx.send(req).is_err() {
                        self.dead = true;
                        self.pending = None;
                    }
                }
            }
        }
        target_changed
    }

    fn drain(&mut self) {
        loop {
            match self.rx.try_recv() {
                Ok((k, art)) => {
                    if self.pending.as_ref() == Some(&k) {
                        self.pending = None;
                    }
                    self.cache_put(k.clone(), art);
                    if self.wanted.as_ref() == Some(&k) {
                        self.activate(k);
                    }
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    self.dead = true;
                    break;
                }
            }
        }
    }

    fn activate(&mut self, k: Key) {
        if self
            .active
            .as_ref()
            .map(|(ak, _)| *ak == k)
            .unwrap_or(false)
        {
            return;
        }
        let art = match self.cache.get(&k) {
            Some(a) => a.clone(),
            None => return,
        };
        if art.img.is_some() {
            if self.kitty {
                if let Some(ImgState::Kitty(old)) = &self.image_state {
                    kitty::stream_delete(old.id(), self.is_tmux);
                }
                if let Some(img) = &art.img {
                    self.image_state = Some(ImgState::Kitty(KittyState::new(
                        kitty::next_id(),
                        self.font,
                        self.is_tmux,
                        self.shm,
                        img.clone(),
                    )));
                    self.last_streamed = 0;
                }
            } else {
                self.image_state = None;
            }
        } else {
            self.image_state = None;
        }
        self.active = Some((k, art));
        self.changed = true;
    }

    fn cache_put(&mut self, k: Key, art: Artifact) {
        if self.cache.insert(k.clone(), art).is_some() {
            if let Some(p) = self.order.iter().position(|x| *x == k) {
                self.order.remove(p);
            }
        }
        self.order.push_back(k.clone());
        while self.cache.len() > CACHE_MAX {
            let n = self.order.len();
            let mut evict = None;
            for _ in 0..n {
                match self.order.pop_front() {
                    Some(front) => {
                        if Some(&front) == self.wanted.as_ref() || front == k {
                            self.order.push_back(front);
                        } else {
                            evict = Some(front);
                            break;
                        }
                    }
                    None => break,
                }
            }
            match evict {
                Some(e) => {
                    self.cache.remove(&e);
                }
                None => break,
            }
        }
    }
}

fn worker(rx: Receiver<Request>, tx: Sender<(Key, Artifact)>) {
    while let Ok(first) = rx.recv() {
        // Batch a few pending requests; the newest display request supersedes
        // older ones, but all preload requests still run.
        let mut batch = vec![first];
        while let Ok(r) = rx.try_recv() {
            if batch.len() >= 16 {
                break;
            }
            batch.push(r);
        }
        let last_display = batch.iter().rposition(|r| !r.preload);
        for (i, req) in batch.into_iter().enumerate() {
            if !req.preload && Some(i) != last_display {
                continue; // superseded by a newer display request
            }
            let art = generate(&req.key);
            if tx.send((req.key, art)).is_err() {
                return;
            }
        }
    }
}

fn generate(key: &Key) -> Artifact {
    let path = &key.0;
    let err = |msg: String| Artifact {
        kind: "error",
        data: vec![Line::styled(msg, Style::new().fg(Color::Red))],
        img: None,
    };

    if path.is_dir() {
        return Artifact {
            kind: "dir",
            data: dir_listing(path),
            img: None,
        };
    }

    match detect::detect(path) {
        detect::Kind::Text => match text::preview(path) {
            Ok(lines) => Artifact { kind: "text", data: lines, img: None },
            Err(e) => err(format!("{}: {e}", path.display())),
        },
        detect::Kind::Json => match media::json_pretty(path) {
            Ok(s) => Artifact {
                kind: "json",
                data: text::highlight(&s, "json", ""),
                img: None,
            },
            Err(_) => match text::preview(path) {
                Ok(lines) => Artifact { kind: "json", data: lines, img: None },
                Err(e) => err(format!("{}: {e}", path.display())),
            },
        },
        // Images are decoded off-thread and cached both in memory (this
        // `cache`) and on disk (a downscaled copy in the temp cache dir), so
        // revisiting — or restarting rnn — never re-decodes a large source.
        // Video and PDFs show as binary.
        detect::Kind::Image => match kitty::load_image(path, key.1, key.2) {
            Ok(img) => Artifact {
                kind: "image",
                data: Vec::new(),
                img: Some(img),
            },
            Err(_) => binary_preview(path),
        },
        detect::Kind::Video | detect::Kind::Pdf => binary_preview(path),
        detect::Kind::Archive => match media::archive_listing(path) {
            Ok(s) => Artifact {
                kind: "archive",
                data: s.lines().map(|l| Line::raw(sanitize(l))).collect(),
                img: None,
            },
            Err(e) => err(format!("{e}")),
        },
        detect::Kind::Binary => binary_preview(path),
    }
}

fn binary_preview(path: &Path) -> Artifact {
    match hex::preview(path) {
        Ok(lines) => Artifact {
            kind: "binary",
            data: lines,
            img: None,
        },
        Err(e) => Artifact {
            kind: "error",
            data: vec![Line::styled(
                format!("{}: {e}", path.display()),
                Style::new().fg(Color::Red),
            )],
            img: None,
        },
    }
}

fn dir_listing(path: &Path) -> Vec<Line<'static>> {
    let rd = match fs::read_dir(path) {
        Ok(rd) => rd,
        Err(e) => {
            return vec![Line::styled(
                format!("cannot read directory: {e}"),
                Style::new().fg(Color::Red),
            )]
        }
    };
    let mut items: Vec<(String, bool, u64)> = rd
        .flatten()
        .map(|e| {
            let is_dir = e.file_type().map(|t| t.is_dir()).unwrap_or(false);
            let size = e.metadata().map(|m| m.len()).unwrap_or(0);
            (e.file_name().to_string_lossy().into_owned(), is_dir, size)
        })
        .collect();
    if items.is_empty() {
        return vec![Line::styled("(empty)", Style::new().fg(Color::DarkGray))];
    }
    items.sort_by(|a, b| {
        b.1.cmp(&a.1)
            .then_with(|| a.0.to_lowercase().cmp(&b.0.to_lowercase()))
    });

    let total = items.len();
    let name_w = items
        .iter()
        .map(|(n, d, _)| n.chars().count() + usize::from(*d))
        .max()
        .unwrap_or(0);

    let mut out: Vec<Line<'static>> = items
        .into_iter()
        .take(DIR_LIST_MAX)
        .map(|(n, d, sz)| {
            let name = if d {
                format!("{}/", sanitize(&n))
            } else {
                sanitize(&n)
            };
            let pad = name_w.saturating_sub(name.chars().count()) + 1;
            let size = if d {
                String::new()
            } else {
                human(sz)
            };
            let style = if d {
                Style::new().fg(Color::Blue).add_modifier(ratatui::style::Modifier::BOLD)
            } else {
                Style::new()
            };
            Line::from(vec![
                ratatui::text::Span::styled(name, style),
                ratatui::text::Span::raw(" ".repeat(pad)),
                ratatui::text::Span::styled(size, Style::new().fg(Color::DarkGray)),
            ])
        })
        .collect();
    if total > DIR_LIST_MAX {
        out.push(Line::styled(
            format!("… ({} more)", total - DIR_LIST_MAX),
            Style::new().fg(Color::DarkGray),
        ));
    }
    out
}