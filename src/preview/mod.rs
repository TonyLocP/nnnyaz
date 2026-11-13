pub mod detect;
pub mod hex;
pub mod kitty;
pub mod media;
pub mod shm;
pub mod sixel;
pub mod text;
pub mod yazi;

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
    layout::Rect,
    style::{Color, Style},
    text::Line,
};

use crate::{
    preview::{kitty::KittyState, sixel::SixelState},
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

/// Which graphics protocol the terminal speaks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImageProtocol {
    None,
    Kitty,
    Sixel,
}

/// True when rnn itself runs inside tmux, not merely launched from a tmux
/// session. A foot window started from tmux inherits `$TMUX`, but its `$TERM`
/// is `foot`, so the terminal name is the reliable signal.
pub fn tmux_active() -> bool {
    if std::env::var_os("TMUX").is_none() {
        return false;
    }
    let term = std::env::var("TERM").unwrap_or_default();
    term.starts_with("tmux") || term.starts_with("screen")
}

enum ImgState {
    Kitty(KittyState),
    Sixel(SixelState),
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
    zoom_idx: usize,
    protocol: ImageProtocol,
    font: (u16, u16),
    is_tmux: bool,
    shm: bool,
    image_state: Option<ImgState>,
    last_streamed: u64,
    sixel_drawn: Option<(Rect, u64)>,
    sixel_clear: bool,
    sixel_force: bool,
    last_sixel_gen: u64,
    cache_max: usize,
}

const CACHE_MAX: usize = 16;
const DIR_LIST_MAX: usize = 5000;
/// Zoom percentages: fit, 1.5x, 2x, 3x, 4x.
const ZOOM_LEVELS: [u32; 5] = [100, 150, 200, 300, 400];

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
            zoom_idx: 0,
            protocol: ImageProtocol::None,
            font: (8, 16),
            is_tmux: false,
            shm: false,
            image_state: None,
            last_streamed: 0,
            sixel_drawn: None,
            sixel_clear: false,
            sixel_force: false,
            last_sixel_gen: 0,
            cache_max: CACHE_MAX,
        }
    }

    /// Set how many preview artifacts to keep before evicting the oldest.
    pub fn set_cache_size(&mut self, size: usize) {
        self.cache_max = size.max(1);
    }

    /// Enable image previews. Call once at startup. `shm` selects the kitty
    /// shared-memory transfer medium, which is far faster under tmux.
    pub fn configure(&mut self, protocol: ImageProtocol, font: (u16, u16), shm: bool) {
        self.protocol = protocol;
        self.font = font;
        self.is_tmux = tmux_active();
        self.shm = shm;
    }

    pub fn protocol(&self) -> ImageProtocol {
        self.protocol
    }

    /// The active kitty image renderer, if any.
    pub fn image_state_mut(&mut self) -> Option<&mut KittyState> {
        match &mut self.image_state {
            Some(ImgState::Kitty(k)) => Some(k),
            _ => None,
        }
    }

    /// Whether images are displayed at all (otherwise preloading them would be
    /// wasted work).
    pub fn images_enabled(&self) -> bool {
        self.protocol != ImageProtocol::None
    }

    /// Backwards-compatible alias used by the kitty preloading path.
    pub fn kitty_enabled(&self) -> bool {
        self.images_enabled()
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
        match self.image_state.take() {
            Some(ImgState::Kitty(old)) => kitty::stream_delete(old.id(), self.is_tmux),
            Some(ImgState::Sixel(_)) if self.sixel_drawn.is_some() => {
                self.sixel_clear = true;
            }
            Some(ImgState::Sixel(_)) | None => {}
        }
    }

    /// Ensure the sixel renderer has an encoding for `rect`. Called from the
    /// draw callback, so the actual terminal write happens after ratatui's
    /// buffer has been flushed.
    pub fn prepare_sixel(&mut self, rect: Rect) {
        if let Some(ImgState::Sixel(s)) = &mut self.image_state {
            s.prepare(rect);
        }
    }

    /// Erase a painted sixel that no longer matches the preview area, before
    /// the new layout's text is rendered.
    pub fn clear_sixel_if_moved(&mut self, rect: Rect) {
        if let Some((old, _)) = self.sixel_drawn {
            if old != rect {
                let _ = sixel::clear(old);
                self.sixel_drawn = None;
                if let Some(ImgState::Sixel(s)) = &mut self.image_state {
                    s.forget_drawn();
                }
            }
        }
    }

    /// Erase the painted sixel immediately when a clear is pending. Called
    /// from `draw_preview` after `set_target`, so the erase happens before the
    /// new frame's text is written and cannot wipe it.
    pub fn clear_sixel_pending(&mut self) {
        if !self.sixel_clear {
            return;
        }
        self.sixel_clear = false;
        if let Some((rect, _)) = self.sixel_drawn.take() {
            let _ = sixel::clear(rect);
        }
        if let Some(ImgState::Sixel(s)) = &mut self.image_state {
            s.forget_drawn();
        }
    }

    /// Called before each frame. Erases the sixel when it must be repainted
    /// (resize, terminal re-init) or while an overlay covers the pane.
    pub fn begin_sixel(&mut self, allow: bool) {
        if self.sixel_force {
            self.sixel_force = false;
            if self.sixel_drawn.is_some() {
                self.sixel_clear = true;
            }
            if let Some(ImgState::Sixel(s)) = &mut self.image_state {
                s.forget_drawn();
            }
        }
        if !allow {
            if self.sixel_drawn.is_some() {
                self.sixel_clear = true;
            }
            if let Some(ImgState::Sixel(s)) = &mut self.image_state {
                s.forget_drawn();
            }
        }
        self.clear_sixel_pending();
    }

    /// Paint a ready sixel after a ratatui frame. `allow` is false while a
    /// full-screen overlay is open, so the overlay can cover the pane and the
    /// image is repainted when it closes.
    pub fn flush_sixel(&mut self, allow: bool) {
        if !allow {
            return;
        }
        let Some(ImgState::Sixel(s)) = &mut self.image_state else {
            return;
        };
        let Some((rect, gen, data)) = s.ready() else {
            return;
        };
        if s.drawn() == Some((rect, gen)) {
            return;
        }
        if let Some((old, old_gen)) = self.sixel_drawn {
            if old != rect || old_gen != gen {
                let _ = sixel::clear(old);
            }
        }
        let r = sixel::draw(rect, &data);
        if std::env::var_os("RNN_DEBUG").is_some() {
            eprintln!(
                "sixel: draw gen={gen} rect={rect:?} bytes={} result={:?}",
                data.len(),
                r.as_ref().err()
            );
        }
        if r.is_ok() {
            s.mark_drawn(rect, gen);
            self.sixel_drawn = Some((rect, gen));
        }
    }

    /// Repaint the sixel on the next flush, e.g. after a resize or after
    /// leaving the alternate screen.
    pub fn force_sixel_redraw(&mut self) {
        self.sixel_force = true;
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

    /// Current zoom factor, when an image is actually being displayed.
    pub fn zoom(&self) -> Option<f32> {
        self.image_state
            .as_ref()
            .map(|_| ZOOM_LEVELS[self.zoom_idx] as f32 / 100.0)
    }

    /// Zoom in one step. Returns the new factor, or `None` when the active
    /// preview is not an image.
    pub fn zoom_in(&mut self) -> Option<f32> {
        self.zoom()?;
        let idx = (self.zoom_idx + 1).min(ZOOM_LEVELS.len() - 1);
        self.apply_zoom(idx)
    }

    /// Zoom out one step. Returns the new factor, or `None` when the active
    /// preview is not an image.
    pub fn zoom_out(&mut self) -> Option<f32> {
        self.zoom()?;
        let idx = self.zoom_idx.saturating_sub(1);
        self.apply_zoom(idx)
    }

    /// Reset back to fit. Returns false when the active preview is not an
    /// image.
    pub fn zoom_reset(&mut self) -> bool {
        if self.zoom().is_none() {
            return false;
        }
        self.zoom_idx = 0;
        match &mut self.image_state {
            Some(ImgState::Kitty(k)) => k.set_zoom(ZOOM_LEVELS[0]),
            Some(ImgState::Sixel(s)) => s.set_zoom(ZOOM_LEVELS[0]),
            None => {}
        }
        self.changed = true;
        true
    }

    fn apply_zoom(&mut self, idx: usize) -> Option<f32> {
        self.zoom_idx = idx;
        match &mut self.image_state {
            Some(ImgState::Kitty(k)) => k.set_zoom(ZOOM_LEVELS[self.zoom_idx]),
            Some(ImgState::Sixel(s)) => s.set_zoom(ZOOM_LEVELS[self.zoom_idx]),
            None => {}
        }
        self.changed = true;
        Some(ZOOM_LEVELS[self.zoom_idx] as f32 / 100.0)
    }

    /// `(kind, line_count)` of whatever is currently displayed.
    pub fn artifact_meta(&self) -> Option<(&'static str, usize)> {
        self.active.as_ref().map(|(_, a)| (a.kind, a.data.len()))
    }

    pub fn lines_slice(&self, rows: usize) -> Option<Vec<Line<'static>>> {
        let (_, a) = self.active.as_ref()?;
        Some(
            a.data
                .iter()
                .skip(self.preview_seek)
                .take(rows)
                .cloned()
                .collect(),
        )
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
        if let Some(ImgState::Sixel(s)) = &self.image_state {
            let g = s.encoded_gen();
            if g != self.last_sixel_gen {
                self.last_sixel_gen = g;
                self.changed = true;
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
            let active_is_target = self.active.as_ref().map(|(ak, _)| ak == k).unwrap_or(false);
            if !active_is_target {
                if self.cache.contains_key(k) {
                    self.activate(k.clone());
                } else if !self.dead && self.pending.as_ref() != Some(k) {
                    self.pending = Some(k.clone());
                    let req = Request {
                        key: k.clone(),
                        preload: false,
                    };
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
        self.clear_image();
        if let Some(img) = &art.img {
            match self.protocol {
                ImageProtocol::Kitty => {
                    let mut st = KittyState::new(
                        kitty::next_id(),
                        self.font,
                        self.is_tmux,
                        self.shm,
                        img.clone(),
                    );
                    st.set_zoom(ZOOM_LEVELS[self.zoom_idx]);
                    self.image_state = Some(ImgState::Kitty(st));
                    self.last_streamed = 0;
                }
                ImageProtocol::Sixel => {
                    let mut st = SixelState::new(self.font, self.is_tmux, img.clone());
                    st.set_zoom(ZOOM_LEVELS[self.zoom_idx]);
                    self.image_state = Some(ImgState::Sixel(st));
                    self.last_sixel_gen = 0;
                }
                ImageProtocol::None => {}
            }
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
        while self.cache.len() > self.cache_max {
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
            Ok(lines) => Artifact {
                kind: "text",
                data: lines,
                img: None,
            },
            Err(e) => err(format!("{}: {e}", path.display())),
        },
        detect::Kind::Json => match media::json_pretty(path) {
            Ok(s) => Artifact {
                kind: "json",
                data: text::highlight(&s, "json", ""),
                img: None,
            },
            Err(_) => match text::preview(path) {
                Ok(lines) => Artifact {
                    kind: "json",
                    data: lines,
                    img: None,
                },
                Err(e) => err(format!("{}: {e}", path.display())),
            },
        },
        // Images are decoded off-thread and cached both in memory (this
        // `cache`) and on disk (a downscaled copy in the temp cache dir), so
        // revisiting — or restarting rnn — never re-decodes a large source.
        // Videos are turned into a still frame with ffmpeg and then rendered
        // through the same image path. PDFs still show as binary.
        detect::Kind::Image => match kitty::load_image(path, key.1, key.2) {
            Ok(img) => Artifact {
                kind: "image",
                data: Vec::new(),
                img: Some(img),
            },
            Err(_) => binary_preview(path),
        },
        detect::Kind::Video => match kitty::load_video_thumbnail(path, key.1, key.2) {
            Ok(img) => Artifact {
                kind: "video",
                data: Vec::new(),
                img: Some(img),
            },
            Err(_) => binary_preview(path),
        },
        detect::Kind::Pdf => binary_preview(path),
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
            let size = if d { String::new() } else { human(sz) };
            let style = if d {
                Style::new()
                    .fg(Color::Blue)
                    .add_modifier(ratatui::style::Modifier::BOLD)
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
