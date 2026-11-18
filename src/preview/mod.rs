pub mod detect;
pub mod hex;
pub mod kitty;
pub mod media;
pub mod shm;
pub mod sixel;
pub mod text;
pub mod yazi;

use std::{
    collections::{HashMap, HashSet, VecDeque},
    fs,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc::{self, Receiver, Sender, TryRecvError},
        Arc, Mutex,
    },
    thread,
    time::{Duration, SystemTime},
};

use image::DynamicImage;
use ratatui::{
    layout::Rect,
    style::{Color, Style},
    text::Line,
};

use crate::{
    preview::{
        kitty::{KittyState, Settings},
        sixel::SixelState,
    },
    util::{human, sanitize},
};

/// Everything a preview is keyed by: path, mtime and size.
pub type Key = (PathBuf, Option<SystemTime>, u64);

#[derive(Clone)]
pub struct Artifact {
    pub kind: &'static str,
    pub data: Vec<Line<'static>>,
    img: Option<DynamicImage>,
    /// Page count for PDF previews (zero for everything else).
    pages: Option<usize>,
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
    page: usize,
}

pub struct Preview {
    tx: Sender<Request>,
    preload_tx: Sender<Request>,
    /// Decode/resize settings, resolved from `[preview]` at startup.
    settings: Settings,
    /// Bumped whenever the visible target changes. The preload worker uses it
    /// to stay out of the way during fast navigation.
    activity: Arc<AtomicU64>,
    /// The file currently on screen, shared with the preload worker so it
    /// never decodes the same image the display worker is already working on.
    showing: Arc<Mutex<Option<Key>>>,
    rx: Receiver<(Key, usize, Artifact)>,
    pending: Option<(Key, usize)>,
    wanted: Option<Key>,
    /// Page of the current target (PDF only; always zero elsewhere).
    wanted_page: usize,
    active: Option<((Key, usize), Artifact)>,
    cache: HashMap<(Key, usize), Artifact>,
    order: VecDeque<(Key, usize)>,
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
    pub fn new(settings: Settings, image_delay: Duration, tab_size: usize) -> Self {
        let (tx, req_rx) = mpsc::channel::<Request>();
        let (preload_tx, preload_rx) = mpsc::channel::<Request>();
        let (res_tx, rx) = mpsc::channel::<(Key, usize, Artifact)>();
        let activity = Arc::new(AtomicU64::new(0));
        let showing = Arc::new(Mutex::new(None));

        let worker_tx = res_tx.clone();
        let _ = thread::Builder::new()
            .name("preview-worker".into())
            .spawn(move || worker(req_rx, worker_tx, settings, image_delay, tab_size));
        let preload_activity = Arc::clone(&activity);
        let preload_showing = Arc::clone(&showing);
        let _ = thread::Builder::new()
            .name("preview-preload".into())
            .spawn(move || {
                preload_worker(
                    preload_rx,
                    res_tx,
                    preload_activity,
                    preload_showing,
                    settings,
                    tab_size,
                )
            });

        Self {
            tx,
            preload_tx,
            settings,
            activity,
            showing,
            rx,
            pending: None,
            wanted: None,
            wanted_page: 0,
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
        if self.cache.contains_key(&(key.clone(), 0)) {
            return;
        }
        // Preloads live on their own worker: a slow neighbour decode must
        // never delay the image the user is actually looking at.
        let _ = self.preload_tx.send(Request { key, page: 0 });
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

    /// Zero-based page of the current target. Only meaningful for PDFs; other
    /// previews stay on page zero.
    pub fn page(&self) -> usize {
        self.wanted_page
    }

    /// Switch the current PDF preview to another page. The next
    /// [`Self::set_target`] call picks it up.
    pub fn set_page(&mut self, page: usize) {
        if self.wanted_page != page {
            self.wanted_page = page;
            self.activity.fetch_add(1, Ordering::Relaxed);
            self.changed = true;
        }
    }

    /// True when the current target is a PDF (even before its first page has
    /// finished rendering).
    pub fn is_pdf(&self) -> bool {
        if self
            .active
            .as_ref()
            .map(|(_, a)| a.kind == "pdf")
            .unwrap_or(false)
        {
            return true;
        }
        self.wanted
            .as_ref()
            .map(|k| detect::detect(&k.0) == detect::Kind::Pdf)
            .unwrap_or(false)
    }

    /// Page count of the active PDF, when `pdfinfo` could determine it.
    pub fn pdf_pages(&self) -> Option<usize> {
        self.active.as_ref().and_then(|(_, a)| a.pages)
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

    /// Point the preview at a new entry. Returns true when the entry changed
    /// (callers use that to reset preview seek). Page changes do not count as
    /// an entry change. Results arriving from the worker are drained and
    /// cached here.
    pub fn set_target(&mut self, key: Option<&Key>) -> bool {
        let target_changed = self.wanted.as_ref() != key;
        if target_changed {
            self.wanted = key.cloned();
            self.wanted_page = 0;
            self.activity.fetch_add(1, Ordering::Relaxed);
            if let Ok(mut showing) = self.showing.lock() {
                *showing = key.cloned();
            }
        }
        if key.is_none() {
            self.clear_image();
        }
        self.drain();

        if let Some(k) = key {
            let want = (k.clone(), self.wanted_page);
            let active_is_target = self
                .active
                .as_ref()
                .map(|(ak, _)| *ak == want)
                .unwrap_or(false);
            if !active_is_target {
                if self.cache.contains_key(&want) {
                    self.activate(want);
                } else if !self.dead && self.pending.as_ref() != Some(&want) {
                    self.pending = Some(want.clone());
                    let req = Request {
                        key: k.clone(),
                        page: want.1,
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
                Ok((k, page, art)) => {
                    let want = (k.clone(), page);
                    if self.pending.as_ref() == Some(&want) {
                        self.pending = None;
                    }
                    self.cache_put(want.clone(), art);
                    if self.wanted.as_ref() == Some(&k) && self.wanted_page == page {
                        self.activate(want);
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

    fn activate(&mut self, k: (Key, usize)) {
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
                        self.settings,
                        img.clone(),
                    );
                    st.set_zoom(ZOOM_LEVELS[self.zoom_idx]);
                    self.image_state = Some(ImgState::Kitty(st));
                    self.last_streamed = 0;
                }
                ImageProtocol::Sixel => {
                    let mut st =
                        SixelState::new(self.font, self.is_tmux, self.settings, img.clone());
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

    fn cache_put(&mut self, k: (Key, usize), art: Artifact) {
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
                        let keep_wanted =
                            self.wanted.as_ref().map(|w| front.0 == *w).unwrap_or(false);
                        if keep_wanted || front == k {
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

/// How long the preload worker waits for the user to stop moving before it
/// starts decoding neighbours. A j/k burst keeps bumping the activity
/// counter, so speculative work never competes with the visible image.
const PRELOAD_IDLE: Duration = Duration::from_millis(150);

/// Bound on one preload batch so a long j/k burst cannot build a backlog.
const PRELOAD_BATCH_MAX: usize = 8;

/// Millisecond timestamp for `RNN_DEBUG` tracing.
pub(crate) fn debug_stamp() -> u128 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_millis() % 100_000)
        .unwrap_or(0)
}

/// The display worker: only ever renders what the user is looking at, and
/// always collapses a burst to the newest request.
fn worker(
    rx: Receiver<Request>,
    tx: Sender<(Key, usize, Artifact)>,
    settings: Settings,
    image_delay: Duration,
    tab_size: usize,
) {
    while let Ok(mut req) = rx.recv() {
        // Debounce like yazi's `image_delay`: a newer target resets the wait,
        // so holding j only decodes the image where the cursor lands.
        loop {
            match rx.recv_timeout(image_delay) {
                Ok(r) => req = r,
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => break,
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return,
            }
        }
        let start = std::time::Instant::now();
        let art = generate(&req.key, req.page, &settings, tab_size);
        if std::env::var_os("RNN_DEBUG").is_some() {
            eprintln!(
                "preview: show {} in {:?} (t={}ms)",
                req.key.0.display(),
                start.elapsed(),
                debug_stamp()
            );
        }
        if tx.send((req.key, req.page, art)).is_err() {
            return;
        }
    }
}

/// The preload worker: warms neighbours on a separate thread so even a slow
/// decode (a 25 MB camera JPEG can take ~700 ms) can never delay the visible
/// preview. It waits for a short pause before starting, and abandons a batch
/// as soon as the target moves again.
fn preload_worker(
    rx: Receiver<Request>,
    tx: Sender<(Key, usize, Artifact)>,
    activity: Arc<AtomicU64>,
    showing: Arc<Mutex<Option<Key>>>,
    settings: Settings,
    tab_size: usize,
) {
    loop {
        let Ok(first) = rx.recv() else { return };
        let mut batch = vec![first];
        while batch.len() < PRELOAD_BATCH_MAX {
            match rx.try_recv() {
                Ok(r) => batch.push(r),
                Err(_) => break,
            }
        }
        let mut seen = HashSet::new();
        batch.retain(|r| seen.insert(r.key.clone()));

        let seq = activity.load(Ordering::Relaxed);
        thread::sleep(PRELOAD_IDLE);
        if activity.load(Ordering::Relaxed) != seq {
            continue; // user is still moving: drop this batch
        }

        for req in batch {
            if activity.load(Ordering::Relaxed) != seq {
                break;
            }
            // The display worker is already decoding this file (or just
            // showed it); preloading it again would only duplicate the work.
            let on_screen = showing
                .lock()
                .map(|s| s.as_ref() == Some(&req.key))
                .unwrap_or(false);
            if on_screen {
                continue;
            }
            let start = std::time::Instant::now();
            let art = generate(&req.key, req.page, &settings, tab_size);
            if std::env::var_os("RNN_DEBUG").is_some() {
                eprintln!(
                    "preview: preload {} in {:?} (t={}ms)",
                    req.key.0.display(),
                    start.elapsed(),
                    debug_stamp()
                );
            }
            if tx.send((req.key, req.page, art)).is_err() {
                return;
            }
        }
    }
}

fn generate(key: &Key, page: usize, settings: &Settings, tab_size: usize) -> Artifact {
    let path = &key.0;
    let err = |msg: String| Artifact {
        kind: "error",
        data: vec![Line::styled(msg, Style::new().fg(Color::Red))],
        img: None,
        pages: None,
    };

    if path.is_dir() {
        return Artifact {
            kind: "dir",
            data: dir_listing(path),
            img: None,
            pages: None,
        };
    }

    match detect::detect(path) {
        detect::Kind::Text => match text::preview(path, tab_size) {
            Ok(lines) => Artifact {
                kind: "text",
                data: lines,
                img: None,
                pages: None,
            },
            Err(e) => err(format!("{}: {e}", path.display())),
        },
        detect::Kind::Json => match media::json_pretty(path) {
            Ok(s) => Artifact {
                kind: "json",
                data: text::highlight(&s, "json", "", tab_size),
                img: None,
                pages: None,
            },
            Err(_) => match text::preview(path, tab_size) {
                Ok(lines) => Artifact {
                    kind: "json",
                    data: lines,
                    img: None,
                    pages: None,
                },
                Err(e) => err(format!("{}: {e}", path.display())),
            },
        },
        // Images are decoded off-thread and cached both in memory (this
        // `cache`) and on disk (a downscaled copy in the temp cache dir), so
        // revisiting — or restarting rnn — never re-decodes a large source.
        // Videos are turned into a still frame with ffmpeg and PDFs into a
        // rendered page with pdftoppm, then rendered through the same image
        // path. `J`/`K` switch PDF pages.
        detect::Kind::Image => match kitty::load_image(path, key.1, key.2, settings) {
            Ok(img) => Artifact {
                kind: "image",
                data: Vec::new(),
                img: Some(img),
                pages: None,
            },
            Err(_) => binary_preview(path),
        },
        detect::Kind::Video => match kitty::load_video_thumbnail(path, key.1, key.2, settings) {
            Ok(img) => Artifact {
                kind: "video",
                data: Vec::new(),
                img: Some(img),
                pages: None,
            },
            Err(_) => binary_preview(path),
        },
        detect::Kind::Pdf => match kitty::load_pdf_page(path, page, key.1, key.2, settings) {
            Ok(img) => Artifact {
                kind: "pdf",
                data: Vec::new(),
                img: Some(img),
                pages: media::pdf_pages(path),
            },
            Err(e) => err(format!("{}: {e}", path.display())),
        },
        detect::Kind::Font => match kitty::load_font_preview(path, key.1, key.2, settings) {
            Ok(img) => Artifact {
                kind: "font",
                data: Vec::new(),
                img: Some(img),
                pages: None,
            },
            Err(_) => binary_preview(path),
        },
        detect::Kind::Archive => match media::archive_listing(path) {
            Ok(s) => Artifact {
                kind: "archive",
                data: s.lines().map(|l| Line::raw(sanitize(l))).collect(),
                img: None,
                pages: None,
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
            pages: None,
        },
        Err(e) => Artifact {
            kind: "error",
            data: vec![Line::styled(
                format!("{}: {e}", path.display()),
                Style::new().fg(Color::Red),
            )],
            img: None,
            pages: None,
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
