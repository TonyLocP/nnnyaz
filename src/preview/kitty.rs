//! Kitty graphics protocol (Kgp) preview backend using Unicode placeholders.
//!
//! Mirrors yazi's driver: the image is transmitted as a chunked APC (`_G`),
//! one complete write per <=4KiB of base64 payload. Inside tmux every chunk is
//! wrapped in a `\ePtmux;...\e\` passthrough, which tmux forwards as a single
//! write (the same wrapper dropped wholesale when it contains a multi-megabyte
//! sequence). The image is then displayed through U+10EEEE placeholder cells
//! whose foreground color encodes the image id.

use std::{
    collections::hash_map::DefaultHasher,
    fs,
    hash::{Hash, Hasher},
    io::{self, Write},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU32, AtomicU64, Ordering},
        mpsc, Arc, OnceLock,
    },
    thread,
    time::SystemTime,
};

use image::{
    imageops::{self, FilterType},
    metadata::Orientation,
    DynamicImage, ImageDecoder, ImageReader, RgbaImage,
};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Style},
};

use super::{media, yazi};

/// Pixel size of a single terminal cell, used to size the transmit canvas.
pub type FontSize = (u16, u16);

/// Default cap on the decoded/transmitted image size.
pub const IMAGE_MAX: (u32, u32) = (1920, 1080);

/// Resize filter for preview scaling. `Box` uses `image::imageops::thumbnail`,
/// which is much faster than the named filters for the large downscales typical
/// of camera images; the other names match Yazi's `preview.image_filter`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Filter {
    Box,
    Nearest,
    Triangle,
    CatmullRom,
    Gaussian,
    Lanczos3,
}

impl Filter {
    pub fn from_name(name: &str) -> Option<Self> {
        Some(match name {
            "box" => Filter::Box,
            "nearest" => Filter::Nearest,
            "triangle" => Filter::Triangle,
            "catmull-rom" | "catmullrom" => Filter::CatmullRom,
            "gaussian" => Filter::Gaussian,
            "lanczos3" => Filter::Lanczos3,
            _ => return None,
        })
    }

    /// The `image` filter, or `None` for the box filter (`thumbnail`).
    fn filter_type(self) -> Option<FilterType> {
        Some(match self {
            Filter::Box => return None,
            Filter::Nearest => FilterType::Nearest,
            Filter::Triangle => FilterType::Triangle,
            Filter::CatmullRom => FilterType::CatmullRom,
            Filter::Gaussian => FilterType::Gaussian,
            Filter::Lanczos3 => FilterType::Lanczos3,
        })
    }
}

/// Preview decode/resize settings, resolved from `[preview]` at startup.
#[derive(Clone, Copy, Debug)]
pub struct Settings {
    pub max: (u32, u32),
    pub filter: Filter,
    pub quality: u8,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            max: IMAGE_MAX,
            filter: Filter::Box,
            quality: 90,
        }
    }
}

/// A per-process image id, masked to 24 bits (the id is carried entirely by the
/// placeholder foreground colour, like yazi's `pid % (0xffffff + 1)`). The
/// counter keeps ids unique within one process across redraws.
pub fn next_id() -> u32 {
    static BASE: OnceLock<u32> = OnceLock::new();
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let base = *BASE.get_or_init(|| (std::process::id() % 0x100_0000).max(1));
    base.wrapping_add(NEXT.fetch_add(1, Ordering::Relaxed)) & 0xFF_FFFF
}

/// Decode an image file, capped to [`IMAGE_MAX`] (aspect preserved), reusing a
/// downscaled on-disk copy when one exists.
///
/// Mirrors yazi's `precache` (`yazi-adapter/src/image.rs`): the first view
/// decodes the (possibly huge) source and writes a small re-encoded copy into a
/// temp cache dir, so later views — even after restarting rnn — skip the source
/// decode entirely. When a yazi cache is available that cache is used
/// exclusively; otherwise rnn keeps its own. `mtime`/`size` are part of rnn's
/// fallback key, so edited files are re-decoded.
pub fn load_image(
    path: &Path,
    mtime: Option<SystemTime>,
    size: u64,
    settings: &Settings,
) -> io::Result<DynamicImage> {
    if let Some(dir) = yazi::cache_dir() {
        return load_shared(&dir, path, settings);
    }
    load_from(&cache_dir(), path, mtime, size, settings)
}

/// Read or fill a preview in yazi's cache. A miss is decoded from the source
/// and written back under yazi's key; rnn's own cache is never touched.
fn load_shared(dir: &Path, path: &Path, settings: &Settings) -> io::Result<DynamicImage> {
    let target = yazi::target(dir, path);
    if let Some(target) = &target {
        if let Some(img) = decode(target) {
            if std::env::var_os("RNN_DEBUG").is_some() {
                eprintln!("cache: yazi hit {}", target.display());
            }
            return Ok(img);
        }
    }

    let img = decode_source(path)?;
    let capped = cap_size(&img, settings.max, settings.filter);
    if let Some(target) = &target {
        schedule_shared(target, &capped, settings.quality);
    }
    Ok(capped)
}

/// Extract a frame from a video and decode it, using yazi's preview cache
/// when available (keyed by the source video, so yazi and rnn share thumbs).
pub fn load_video_thumbnail(
    path: &Path,
    mtime: Option<SystemTime>,
    size: u64,
    settings: &Settings,
) -> io::Result<DynamicImage> {
    if let Some(dir) = yazi::cache_dir() {
        return load_video_shared(&dir, path, settings);
    }
    load_video_from(&cache_dir(), path, mtime, size, settings)
}

fn load_video_shared(dir: &Path, path: &Path, settings: &Settings) -> io::Result<DynamicImage> {
    let target = yazi::target(dir, path);
    if let Some(target) = &target {
        if let Some(img) = decode(target) {
            if std::env::var_os("RNN_DEBUG").is_some() {
                eprintln!("cache: yazi video hit {}", target.display());
            }
            return Ok(img);
        }
    }

    fs::create_dir_all(dir)?;
    let tmp = dir.join(format!(
        ".rnn-video-{}-{}.jpg",
        std::process::id(),
        next_id()
    ));
    let img = media::extract_video_frame(path, &tmp).and_then(|_| decode_source(&tmp));
    let _ = fs::remove_file(&tmp);
    let capped = cap_size(&img?, settings.max, settings.filter);
    if let Some(target) = &target {
        schedule_shared(target, &capped, settings.quality);
    }
    Ok(capped)
}

fn load_video_from(
    dir: &Path,
    path: &Path,
    mtime: Option<SystemTime>,
    size: u64,
    settings: &Settings,
) -> io::Result<DynamicImage> {
    let stem = cache_stem(path, mtime, size);
    if let Some(img) = ["png", "jpg"]
        .iter()
        .find_map(|ext| decode(&dir.join(format!("{stem}.{ext}"))))
    {
        return Ok(img);
    }

    fs::create_dir_all(dir)?;
    let tmp = dir.join(format!(".{stem}.{}.frame.jpg", std::process::id()));
    let img = media::extract_video_frame(path, &tmp).and_then(|_| decode_source(&tmp));
    let _ = fs::remove_file(&tmp);
    let capped = cap_size(&img?, settings.max, settings.filter);
    schedule_cache(dir, &stem, &capped, settings.quality);
    Ok(capped)
}

/// Render a PDF page to an image, caching per page in rnn's own cache.
///
/// Yazi's shared cache holds whichever page its PDF previewer last rendered,
/// so it cannot be reused safely here; rnn keys its copy by page as well as by
/// the source file's path/mtime/size.
pub fn load_pdf_page(
    path: &Path,
    page: usize,
    mtime: Option<SystemTime>,
    size: u64,
    settings: &Settings,
) -> io::Result<DynamicImage> {
    let dir = cache_dir();
    let stem = format!("{}-p{}", cache_stem(path, mtime, size), page);
    if let Some(img) = ["png", "jpg"]
        .iter()
        .find_map(|ext| decode(&dir.join(format!("{stem}.{ext}"))))
    {
        return Ok(img);
    }

    fs::create_dir_all(&dir)?;
    let prefix = dir.join(format!(".{stem}.{}", std::process::id()));
    media::render_pdf_page(path, page, &prefix)?;
    let rendered = PathBuf::from(format!("{}.jpg", prefix.to_string_lossy()));
    let img = decode_source(&rendered);
    let _ = fs::remove_file(&rendered);
    let capped = cap_size(&img?, settings.max, settings.filter);
    schedule_cache(&dir, &stem, &capped, settings.quality);
    Ok(capped)
}

/// Render a font's glyph sheet to an image, sharing Yazi's cache when present.
pub fn load_font_preview(
    path: &Path,
    mtime: Option<SystemTime>,
    size: u64,
    settings: &Settings,
) -> io::Result<DynamicImage> {
    if let Some(dir) = yazi::cache_dir() {
        return load_font_shared(&dir, path, settings);
    }
    load_font_from(&cache_dir(), path, mtime, size, settings)
}

fn load_font_shared(dir: &Path, path: &Path, settings: &Settings) -> io::Result<DynamicImage> {
    let target = yazi::target(dir, path);
    if let Some(target) = &target {
        if let Some(img) = decode(target) {
            if std::env::var_os("RNN_DEBUG").is_some() {
                eprintln!("cache: yazi font hit {}", target.display());
            }
            return Ok(img);
        }
    }

    fs::create_dir_all(dir)?;
    let tmp = dir.join(format!(
        ".rnn-font-{}-{}.jpg",
        std::process::id(),
        next_id()
    ));
    let img = media::render_font_preview(path, &tmp).and_then(|_| decode_source(&tmp));
    let _ = fs::remove_file(&tmp);
    let capped = cap_size(&img?, settings.max, settings.filter);
    if let Some(target) = &target {
        schedule_shared(target, &capped, settings.quality);
    }
    Ok(capped)
}

fn load_font_from(
    dir: &Path,
    path: &Path,
    mtime: Option<SystemTime>,
    size: u64,
    settings: &Settings,
) -> io::Result<DynamicImage> {
    let stem = cache_stem(path, mtime, size);
    if let Some(img) = ["png", "jpg"]
        .iter()
        .find_map(|ext| decode(&dir.join(format!("{stem}.{ext}"))))
    {
        return Ok(img);
    }

    fs::create_dir_all(dir)?;
    let tmp = dir.join(format!(
        ".{stem}.{}.{}.font.jpg",
        std::process::id(),
        next_id()
    ));
    let img = media::render_font_preview(path, &tmp).and_then(|_| decode_source(&tmp));
    let _ = fs::remove_file(&tmp);
    let capped = cap_size(&img?, settings.max, settings.filter);
    schedule_cache(dir, &stem, &capped, settings.quality);
    Ok(capped)
}

/// On-disk preview cache root. `RNN_CACHE_DIR` overrides it (tests, config);
/// otherwise a dir under the system temp dir, like yazi's default.
fn cache_dir() -> PathBuf {
    std::env::var_os("RNN_CACHE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("rnn-preview-cache"))
}

/// Stable content key for a source image (path + mtime + size).
fn cache_stem(path: &Path, mtime: Option<SystemTime>, size: u64) -> String {
    let mut h = DefaultHasher::new();
    path.hash(&mut h);
    mtime.hash(&mut h);
    size.hash(&mut h);
    format!("{:016x}", h.finish())
}

fn decode(path: &Path) -> Option<DynamicImage> {
    ImageReader::open(path)
        .ok()?
        .with_guessed_format()
        .ok()?
        .decode()
        .ok()
}

/// Decode a source image, applying its EXIF orientation so phone/camera photos
/// are shown upright (matching Yazi). Cache entries are already upright, so
/// [`decode`] deliberately skips this.
fn decode_source(path: &Path) -> io::Result<DynamicImage> {
    let reader = ImageReader::open(path)?.with_guessed_format()?;
    let mut decoder = reader.into_decoder().map_err(io::Error::other)?;
    let orientation = decoder.orientation().unwrap_or(Orientation::NoTransforms);
    let mut img = DynamicImage::from_decoder(decoder).map_err(io::Error::other)?;
    if orientation != Orientation::NoTransforms {
        img.apply_orientation(orientation);
    }
    Ok(img)
}

fn load_from(
    dir: &Path,
    path: &Path,
    mtime: Option<SystemTime>,
    size: u64,
    settings: &Settings,
) -> io::Result<DynamicImage> {
    let stem = cache_stem(path, mtime, size);
    if let Some(img) = ["png", "jpg"]
        .iter()
        .find_map(|ext| decode(&dir.join(format!("{stem}.{ext}"))))
    {
        return Ok(img);
    }

    let img = decode_source(path)?;
    let capped = cap_size(&img, settings.max, settings.filter);
    schedule_cache(dir, &stem, &capped, settings.quality);
    Ok(capped)
}

/// Re-encode `img` as PNG when it has alpha, otherwise a JPEG at `quality`
/// (yazi makes the same format choice).
fn encode(img: &DynamicImage, quality: u8) -> io::Result<(&'static str, Vec<u8>)> {
    use image::codecs::{jpeg::JpegEncoder, png::PngEncoder};

    let alpha = img.color().has_alpha();
    let ext = if alpha { "png" } else { "jpg" };

    let mut buf = Vec::new();
    if alpha {
        img.write_with_encoder(PngEncoder::new(&mut buf))
            .map_err(io::Error::other)?;
    } else {
        img.write_with_encoder(JpegEncoder::new_with_quality(&mut buf, quality))
            .map_err(io::Error::other)?;
    }
    Ok((ext, buf))
}

/// A deferred cache write. The first view of a file must not wait on
/// re-encoding it for a later view, so cache fills happen on a dedicated
/// writer thread instead of the preview worker.
enum CacheJob {
    Own {
        dir: PathBuf,
        stem: String,
        img: DynamicImage,
        quality: u8,
    },
    Shared {
        target: PathBuf,
        img: DynamicImage,
        quality: u8,
    },
}

fn cache_tx() -> &'static mpsc::Sender<CacheJob> {
    static TX: OnceLock<mpsc::Sender<CacheJob>> = OnceLock::new();
    TX.get_or_init(|| {
        let (tx, rx) = mpsc::channel::<CacheJob>();
        let _ = thread::Builder::new()
            .name("preview-cache".into())
            .spawn(move || {
                for job in rx {
                    match job {
                        CacheJob::Own {
                            dir,
                            stem,
                            img,
                            quality,
                        } => {
                            let _ = write_cache(&dir, &stem, &img, quality);
                        }
                        CacheJob::Shared {
                            target,
                            img,
                            quality,
                        } => {
                            let written = encode(&img, quality)
                                .and_then(|(_, buf)| yazi::write_entry(&target, &buf));
                            if std::env::var_os("RNN_DEBUG").is_some() {
                                match &written {
                                    Ok(()) => eprintln!("cache: yazi store {}", target.display()),
                                    Err(err) => eprintln!(
                                        "cache: yazi store failed for {}: {err}",
                                        target.display()
                                    ),
                                }
                            }
                        }
                    }
                }
            });
        tx
    })
}

/// Queue a fallback-cache write; the caller keeps the in-memory image.
fn schedule_cache(dir: &Path, stem: &str, img: &DynamicImage, quality: u8) {
    let _ = cache_tx().send(CacheJob::Own {
        dir: dir.to_path_buf(),
        stem: stem.to_string(),
        img: img.clone(),
        quality,
    });
}

/// Queue a shared (yazi) cache write; the caller keeps the in-memory image.
fn schedule_shared(target: &Path, img: &DynamicImage, quality: u8) {
    let _ = cache_tx().send(CacheJob::Shared {
        target: target.to_path_buf(),
        img: img.clone(),
        quality,
    });
}

/// Write a preview into rnn's fallback cache. The encoded bytes go to a unique
/// temp name and are renamed, so a reader never sees a half-written file.
fn write_cache(dir: &Path, stem: &str, img: &DynamicImage, quality: u8) -> io::Result<()> {
    static SEQ: AtomicU32 = AtomicU32::new(0);

    fs::create_dir_all(dir)?;
    let (ext, buf) = encode(img, quality)?;

    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    let tmp = dir.join(format!("{stem}.{}.{n}.tmp", std::process::id()));
    fs::write(&tmp, &buf)?;
    fs::rename(&tmp, dir.join(format!("{stem}.{ext}")))?;
    prune(dir);
    Ok(())
}

/// Best-effort eviction so the temp cache cannot grow without bound.
fn prune(dir: &Path) {
    const MAX_FILES: usize = 512;
    let Ok(rd) = fs::read_dir(dir) else {
        return;
    };
    let mut files: Vec<(PathBuf, SystemTime)> = rd
        .flatten()
        .filter_map(|e| {
            let p = e.path();
            if p.extension().and_then(|x| x.to_str()) == Some("tmp") {
                return None;
            }
            Some((p, e.metadata().ok()?.modified().ok()?))
        })
        .collect();
    if files.len() <= MAX_FILES {
        return;
    }
    files.sort_by_key(|(_, t)| *t);
    let excess = files.len() - MAX_FILES;
    for (p, _) in files.into_iter().take(excess) {
        let _ = fs::remove_file(p);
    }
}

/// Scale down to fit within `max`, never upscaling.
pub fn cap_size(img: &DynamicImage, max: (u32, u32), filter: Filter) -> DynamicImage {
    let (iw, ih) = (img.width(), img.height());
    let scale = (max.0 as f32 / iw as f32)
        .min(max.1 as f32 / ih as f32)
        .min(1.0);
    if scale >= 1.0 {
        return img.clone();
    }
    let (nw, nh) = (
        (iw as f32 * scale).max(1.0) as u32,
        (ih as f32 * scale).max(1.0) as u32,
    );
    scale_to(img, nw, nh, filter)
}

/// Resize with the configured filter. The box filter (`thumbnail`) is the fast
/// default for downscaling; it falls back to a triangle resize when the target
/// is larger than the source (zoomed-in previews), where it would look blocky.
fn scale_to(img: &DynamicImage, w: u32, h: u32, filter: Filter) -> DynamicImage {
    let down = w < img.width() && h < img.height();
    match (filter.filter_type(), down) {
        (None, true) => img.thumbnail(w, h),
        (None, false) => img.resize_exact(w, h, FilterType::Triangle),
        (Some(ft), _) => img.resize_exact(w, h, ft),
    }
}

/// Pixel dimensions of an area of `cells` cells, capped at `max`.
pub fn cell_pixels(font: FontSize, cells: (u16, u16), max: (u32, u32)) -> (u32, u32) {
    let w = u32::from(cells.0)
        .saturating_mul(u32::from(font.0))
        .min(max.0);
    let h = u32::from(cells.1)
        .saturating_mul(u32::from(font.1))
        .min(max.1);
    (w, h)
}

/// Fit `img` (aspect preserved) into `cells` worth of pixels, scaled by
/// `zoom`, and pad/crop to a transparent canvas that is exactly cell-sized.
/// Zoom values below 1 are treated as fit; values above 1 enlarge the image
/// around the pane centre, cropping the overflow.
pub fn fit_to_cells_zoom(
    img: &DynamicImage,
    font: FontSize,
    cells: (u16, u16),
    zoom: f32,
    max: (u32, u32),
    filter: Filter,
) -> DynamicImage {
    let (cw, ch) = cell_pixels(font, cells, max);
    if cw == 0 || ch == 0 {
        return img.clone();
    }
    let (iw, ih) = (img.width().max(1), img.height().max(1));
    let base = (cw as f32 / iw as f32).min(ch as f32 / ih as f32).min(1.0);
    let scale = base * zoom.max(1.0);
    let nw = ((iw as f32 * scale).round() as u32).max(1);
    let nh = ((ih as f32 * scale).round() as u32).max(1);
    let mut canvas = RgbaImage::new(cw, ch);

    if nw <= cw && nh <= ch {
        let fitted = if nw == iw && nh == ih {
            img.clone()
        } else {
            scale_to(img, nw, nh, filter)
        };
        if nw == cw && nh == ch {
            return fitted;
        }
        imageops::overlay(
            &mut canvas,
            &fitted,
            i64::from((cw - nw) / 2),
            i64::from((ch - nh) / 2),
        );
        return DynamicImage::ImageRgba8(canvas);
    }

    // The scaled image overflows the pane: crop the visible source region
    // first so a high zoom never allocates a huge intermediate image.
    let vw = nw.min(cw);
    let vh = nh.min(ch);
    let dx = nw.saturating_sub(cw) / 2;
    let dy = nh.saturating_sub(ch) / 2;
    let sx = ((dx as f32 / scale).round() as u32).min(iw - 1);
    let sy = ((dy as f32 / scale).round() as u32).min(ih - 1);
    let sw = ((vw as f32 / scale).round() as u32).max(1).min(iw - sx);
    let sh = ((vh as f32 / scale).round() as u32).max(1).min(ih - sy);
    let cropped = img.crop_imm(sx, sy, sw, sh);
    let fitted = scale_to(&cropped, vw, vh, filter);
    imageops::overlay(
        &mut canvas,
        &fitted,
        i64::from((cw - vw) / 2),
        i64::from((ch - vh) / 2),
    );
    DynamicImage::ImageRgba8(canvas)
}

/// Fit `img` (aspect preserved) into `cells` worth of pixels and pad the
/// remainder with transparency so the transmitted canvas is exactly cell-sized.
#[cfg(test)]
pub fn fit_to_cells(
    img: &DynamicImage,
    font: FontSize,
    cells: (u16, u16),
    filter: Filter,
) -> DynamicImage {
    fit_to_cells_zoom(img, font, cells, 1.0, IMAGE_MAX, filter)
}

/// Encode an image as kitty APC chunks. Each returned string is a complete,
/// self-contained write; in tmux mode every chunk is wrapped so it survives
/// tmux's passthrough as its own small write.
///
/// Mirrors yazi's `kgp.rs` driver: a virtual placement (`U=1`, `C=1` so the
/// cursor is not moved) is transmitted in the first chunk, and continuation
/// chunks carry only `m=`. No `c=`/`r=`: the placement's cell span is taken
/// from the `\u{10EEEE}` placeholder cells, which is what Ghostty does.
pub fn encode_chunks(img: &DynamicImage, id: u32, is_tmux: bool) -> Vec<String> {
    const CHARS_PER_CHUNK: usize = 4096; // base64 chars per APC
    const CHUNK_BYTES: usize = (CHARS_PER_CHUNK / 4) * 3;

    let rgba = img.to_rgba8();
    let (w, h) = (img.width(), img.height());
    let raw = rgba.as_raw();
    let total = raw.chunks(CHUNK_BYTES).count().max(1);
    let esc = if is_tmux { "\x1b\x1b" } else { "\x1b" };

    let mut out = Vec::with_capacity(total);
    for (i, data) in raw.chunks(CHUNK_BYTES).enumerate() {
        let payload = base64_simd::STANDARD.encode_to_string(data);
        let mut a = String::with_capacity(64 + payload.len() + esc.len() * 2);
        a.push_str(esc);
        a.push_str("_G");
        if i == 0 {
            a.push_str(&format!("q=2,i={id},a=T,C=1,U=1,f=32,t=d,s={w},v={h},"));
        }
        a.push_str(&format!("m={};", u8::from(total > i + 1)));
        a.push_str(&payload);
        a.push_str(esc);
        a.push('\\');
        if is_tmux {
            let mut wrapped = String::with_capacity(a.len() + 9);
            wrapped.push_str("\x1bPtmux;");
            wrapped.push_str(&a);
            wrapped.push_str("\x1b\\");
            out.push(wrapped);
        } else {
            out.push(a);
        }
    }
    out
}

/// Encode a full image as a single shared-memory transmit (`t=s`). The raw
/// RGBA pixels are written to a POSIX shm object and only the object's name
/// travels over the pty, so the transmit is a few dozen bytes instead of
/// megabytes — this is what makes previews instant under tmux. Returns `None`
/// when a shared-memory object could not be created (caller falls back to
/// [`encode_chunks`]).
pub fn encode_shm(img: &DynamicImage, id: u32, is_tmux: bool) -> Option<String> {
    let rgba = img.to_rgba8();
    let (w, h) = (img.width(), img.height());
    let raw = rgba.as_raw();
    let name = super::shm::create(raw).ok()?;
    let b64 = base64_simd::STANDARD.encode_to_string(name.as_bytes());
    let esc = if is_tmux { "\x1b\x1b" } else { "\x1b" };
    let inner = format!(
        "{esc}_Gq=2,i={id},a=T,C=1,U=1,f=32,t=s,s={w},v={h},S={};{b64}{esc}\\",
        raw.len()
    );
    Some(if is_tmux {
        format!("\x1bPtmux;{inner}\x1b\\")
    } else {
        inner
    })
}

/// Write a single APC sequence as its own write.
pub fn stream_seq(seq: &str) {
    let mut out = io::stdout().lock();
    let _ = out.write_all(seq.as_bytes());
    let _ = out.flush();
}

/// Write every chunk as its own write syscall. Concatenating them back into
/// one giant write is exactly the form tmux drops, so this must stay per-chunk.
pub fn stream_chunks(chunks: &[String]) {
    let mut out = io::stdout().lock();
    for c in chunks {
        let _ = out.write_all(c.as_bytes());
    }
    let _ = out.flush();
}

/// The APC bytes that free a previous image id.
pub fn delete_seq(id: u32, is_tmux: bool) -> String {
    let inner = format!("\x1b\x1b_Gd=1,q=2,i={id}\x1b\x1b\\");
    if is_tmux {
        format!("\x1bPtmux;{inner}\x1b\\")
    } else {
        inner.replace("\x1b\x1b", "\x1b")
    }
}

/// Tell the terminal to free a previous image id.
pub fn stream_delete(id: u32, is_tmux: bool) {
    let _ = io::stdout().write_all(delete_seq(id, is_tmux).as_bytes());
    let _ = io::stdout().flush();
}

/// Stateful kitty placeholder renderer. The transmit runs on a background
/// thread (it can take hundreds of ms of pty backpressure through tmux), so
/// input keeps flowing while the image uploads. Placeholder cells are only
/// drawn once the current generation's transmit has finished, so the image
/// never appears half-streamed.
pub struct KittyState {
    id: u32,
    font: FontSize,
    is_tmux: bool,
    shm: bool,
    settings: Settings,
    img: Arc<DynamicImage>,
    cells: Option<((u16, u16), u32)>,
    zoom: u32,
    transmit_gen: Arc<AtomicU64>,
    transmitted_gen: Arc<AtomicU64>,
}

impl KittyState {
    pub fn new(
        id: u32,
        font: FontSize,
        is_tmux: bool,
        shm: bool,
        settings: Settings,
        img: DynamicImage,
    ) -> Self {
        Self {
            id,
            font,
            is_tmux,
            shm,
            settings,
            img: Arc::new(img),
            cells: None,
            zoom: 100,
            transmit_gen: Arc::new(AtomicU64::new(0)),
            transmitted_gen: Arc::new(AtomicU64::new(0)),
        }
    }

    pub fn id(&self) -> u32 {
        self.id
    }

    /// Set the zoom percentage (100 = fit). Changing it forces the next
    /// render to retransmit a rescaled image.
    pub fn set_zoom(&mut self, percent: u32) {
        let percent = percent.max(100);
        if self.zoom != percent {
            self.zoom = percent;
            self.cells = None;
        }
    }

    /// Highest transmit generation that has finished streaming. When this
    /// advances past what the UI asked for, the caller redraws.
    pub fn transmitted_gen(&self) -> u64 {
        self.transmitted_gen.load(Ordering::SeqCst)
    }

    /// Ensure the image for `area` is being transmitted, then draw its
    /// placeholder cells if that transmit has completed.
    pub fn render(&mut self, area: Rect, buf: &mut Buffer) {
        if area.width == 0 || area.height == 0 {
            return;
        }
        let dims = (
            area.width.min(DIACRITICS.len() as u16),
            area.height.min(DIACRITICS.len() as u16),
        );
        if self.cells != Some((dims, self.zoom)) {
            self.cells = Some((dims, self.zoom));
            let gen = self.transmit_gen.load(Ordering::SeqCst) + 1;
            self.transmit_gen.store(gen, Ordering::SeqCst);
            if std::env::var_os("RNN_DEBUG").is_some() {
                eprintln!(
                    "kitty: spawn gen={gen} dims={dims:?} zoom={} (t={}ms)",
                    self.zoom,
                    super::debug_stamp()
                );
            }
            stream_delete(self.id, self.is_tmux);
            let img = Arc::clone(&self.img);
            let target = Arc::clone(&self.transmitted_gen);
            let id = self.id;
            let is_tmux = self.is_tmux;
            let shm = self.shm;
            let font = self.font;
            let zoom = self.zoom as f32 / 100.0;
            let settings = self.settings;
            let _ = thread::Builder::new()
                .name("kitty-stream".into())
                .spawn(move || {
                    let start = std::time::Instant::now();
                    let fitted =
                        fit_to_cells_zoom(&img, font, dims, zoom, settings.max, settings.filter);
                    let fit = start.elapsed();
                    let debug = std::env::var_os("RNN_DEBUG").is_some();
                    if shm {
                        if let Some(seq) = encode_shm(&fitted, id, is_tmux) {
                            stream_seq(&seq);
                            target.fetch_max(gen, Ordering::SeqCst);
                            if debug {
                                eprintln!(
                                    "kitty: stream gen={gen} fit={fit:?} total={:?} (shm, t={}ms)",
                                    start.elapsed(),
                                    super::debug_stamp()
                                );
                            }
                            return;
                        }
                        // shared memory unavailable at runtime: fall back
                    }
                    let chunks = encode_chunks(&fitted, id, is_tmux);
                    stream_chunks(&chunks);
                    target.fetch_max(gen, Ordering::SeqCst);
                    if debug {
                        eprintln!(
                            "kitty: stream gen={gen} fit={fit:?} total={:?} (chunks, t={}ms)",
                            start.elapsed(),
                            super::debug_stamp()
                        );
                    }
                });
        }

        if self.transmitted_gen.load(Ordering::SeqCst) < self.transmit_gen.load(Ordering::SeqCst) {
            if std::env::var_os("RNN_DEBUG").is_some() {
                eprintln!("kitty: draw skip (streaming)");
            }
            return; // transmit still in flight; placeholders land next frame
        }
        if std::env::var_os("RNN_DEBUG").is_some() {
            eprintln!("kitty: draw placeholders");
        }

        let bytes = self.id.to_be_bytes();
        let color = Color::Rgb(bytes[1], bytes[2], bytes[3]);
        let style = Style::new().fg(color);

        // Every placeholder cell states its own (row, column) with two
        // combining marks, exactly like yazi. Ghostty does not implement the
        // "marks on the first cell only" continuation shortcut that
        // ratatui-image relies on, so a bare U+10EEEE run renders nothing.
        for y in 0..dims.1 {
            for x in 0..dims.0 {
                let Some(cell) = buf.cell_mut((area.x + x, area.top() + y)) else {
                    continue;
                };
                let mut symbol = String::from('\u{10EEEE}');
                symbol.push(diacritic(usize::from(y)));
                symbol.push(diacritic(usize::from(x)));
                cell.set_symbol(&symbol).set_style(style);
            }
        }
    }
}

/// Kitlys `rowcolumn-diacritics.txt`: the combining marks kitty/ghostty map to
/// placeholder row/column/id-extra numbers on a U+10EEEE cell (index 0 = U+0305).
/// Out-of-range indexes fall back to 0, so areas wider than 297 cells are clipped.
static DIACRITICS: [char; 297] = [
    '\u{305}',
    '\u{30D}',
    '\u{30E}',
    '\u{310}',
    '\u{312}',
    '\u{33D}',
    '\u{33E}',
    '\u{33F}',
    '\u{346}',
    '\u{34A}',
    '\u{34B}',
    '\u{34C}',
    '\u{350}',
    '\u{351}',
    '\u{352}',
    '\u{357}',
    '\u{35B}',
    '\u{363}',
    '\u{364}',
    '\u{365}',
    '\u{366}',
    '\u{367}',
    '\u{368}',
    '\u{369}',
    '\u{36A}',
    '\u{36B}',
    '\u{36C}',
    '\u{36D}',
    '\u{36E}',
    '\u{36F}',
    '\u{483}',
    '\u{484}',
    '\u{485}',
    '\u{486}',
    '\u{487}',
    '\u{592}',
    '\u{593}',
    '\u{594}',
    '\u{595}',
    '\u{597}',
    '\u{598}',
    '\u{599}',
    '\u{59C}',
    '\u{59D}',
    '\u{59E}',
    '\u{59F}',
    '\u{5A0}',
    '\u{5A1}',
    '\u{5A8}',
    '\u{5A9}',
    '\u{5AB}',
    '\u{5AC}',
    '\u{5AF}',
    '\u{5C4}',
    '\u{610}',
    '\u{611}',
    '\u{612}',
    '\u{613}',
    '\u{614}',
    '\u{615}',
    '\u{616}',
    '\u{617}',
    '\u{657}',
    '\u{658}',
    '\u{659}',
    '\u{65A}',
    '\u{65B}',
    '\u{65D}',
    '\u{65E}',
    '\u{6D6}',
    '\u{6D7}',
    '\u{6D8}',
    '\u{6D9}',
    '\u{6DA}',
    '\u{6DB}',
    '\u{6DC}',
    '\u{6DF}',
    '\u{6E0}',
    '\u{6E1}',
    '\u{6E2}',
    '\u{6E4}',
    '\u{6E7}',
    '\u{6E8}',
    '\u{6EB}',
    '\u{6EC}',
    '\u{730}',
    '\u{732}',
    '\u{733}',
    '\u{735}',
    '\u{736}',
    '\u{73A}',
    '\u{73D}',
    '\u{73F}',
    '\u{740}',
    '\u{741}',
    '\u{743}',
    '\u{745}',
    '\u{747}',
    '\u{749}',
    '\u{74A}',
    '\u{7EB}',
    '\u{7EC}',
    '\u{7ED}',
    '\u{7EE}',
    '\u{7EF}',
    '\u{7F0}',
    '\u{7F1}',
    '\u{7F3}',
    '\u{816}',
    '\u{817}',
    '\u{818}',
    '\u{819}',
    '\u{81B}',
    '\u{81C}',
    '\u{81D}',
    '\u{81E}',
    '\u{81F}',
    '\u{820}',
    '\u{821}',
    '\u{822}',
    '\u{823}',
    '\u{825}',
    '\u{826}',
    '\u{827}',
    '\u{829}',
    '\u{82A}',
    '\u{82B}',
    '\u{82C}',
    '\u{82D}',
    '\u{951}',
    '\u{953}',
    '\u{954}',
    '\u{F82}',
    '\u{F83}',
    '\u{F86}',
    '\u{F87}',
    '\u{135D}',
    '\u{135E}',
    '\u{135F}',
    '\u{17DD}',
    '\u{193A}',
    '\u{1A17}',
    '\u{1A75}',
    '\u{1A76}',
    '\u{1A77}',
    '\u{1A78}',
    '\u{1A79}',
    '\u{1A7A}',
    '\u{1A7B}',
    '\u{1A7C}',
    '\u{1B6B}',
    '\u{1B6D}',
    '\u{1B6E}',
    '\u{1B6F}',
    '\u{1B70}',
    '\u{1B71}',
    '\u{1B72}',
    '\u{1B73}',
    '\u{1CD0}',
    '\u{1CD1}',
    '\u{1CD2}',
    '\u{1CDA}',
    '\u{1CDB}',
    '\u{1CE0}',
    '\u{1DC0}',
    '\u{1DC1}',
    '\u{1DC3}',
    '\u{1DC4}',
    '\u{1DC5}',
    '\u{1DC6}',
    '\u{1DC7}',
    '\u{1DC8}',
    '\u{1DC9}',
    '\u{1DCB}',
    '\u{1DCC}',
    '\u{1DD1}',
    '\u{1DD2}',
    '\u{1DD3}',
    '\u{1DD4}',
    '\u{1DD5}',
    '\u{1DD6}',
    '\u{1DD7}',
    '\u{1DD8}',
    '\u{1DD9}',
    '\u{1DDA}',
    '\u{1DDB}',
    '\u{1DDC}',
    '\u{1DDD}',
    '\u{1DDE}',
    '\u{1DDF}',
    '\u{1DE0}',
    '\u{1DE1}',
    '\u{1DE2}',
    '\u{1DE3}',
    '\u{1DE4}',
    '\u{1DE5}',
    '\u{1DE6}',
    '\u{1DFE}',
    '\u{20D0}',
    '\u{20D1}',
    '\u{20D4}',
    '\u{20D5}',
    '\u{20D6}',
    '\u{20D7}',
    '\u{20DB}',
    '\u{20DC}',
    '\u{20E1}',
    '\u{20E7}',
    '\u{20E9}',
    '\u{20F0}',
    '\u{2CEF}',
    '\u{2CF0}',
    '\u{2CF1}',
    '\u{2DE0}',
    '\u{2DE1}',
    '\u{2DE2}',
    '\u{2DE3}',
    '\u{2DE4}',
    '\u{2DE5}',
    '\u{2DE6}',
    '\u{2DE7}',
    '\u{2DE8}',
    '\u{2DE9}',
    '\u{2DEA}',
    '\u{2DEB}',
    '\u{2DEC}',
    '\u{2DED}',
    '\u{2DEE}',
    '\u{2DEF}',
    '\u{2DF0}',
    '\u{2DF1}',
    '\u{2DF2}',
    '\u{2DF3}',
    '\u{2DF4}',
    '\u{2DF5}',
    '\u{2DF6}',
    '\u{2DF7}',
    '\u{2DF8}',
    '\u{2DF9}',
    '\u{2DFA}',
    '\u{2DFB}',
    '\u{2DFC}',
    '\u{2DFD}',
    '\u{2DFE}',
    '\u{2DFF}',
    '\u{A66F}',
    '\u{A67C}',
    '\u{A67D}',
    '\u{A6F0}',
    '\u{A6F1}',
    '\u{A8E0}',
    '\u{A8E1}',
    '\u{A8E2}',
    '\u{A8E3}',
    '\u{A8E4}',
    '\u{A8E5}',
    '\u{A8E6}',
    '\u{A8E7}',
    '\u{A8E8}',
    '\u{A8E9}',
    '\u{A8EA}',
    '\u{A8EB}',
    '\u{A8EC}',
    '\u{A8ED}',
    '\u{A8EE}',
    '\u{A8EF}',
    '\u{A8F0}',
    '\u{A8F1}',
    '\u{AAB0}',
    '\u{AAB2}',
    '\u{AAB3}',
    '\u{AAB7}',
    '\u{AAB8}',
    '\u{AABE}',
    '\u{AABF}',
    '\u{AAC1}',
    '\u{FE20}',
    '\u{FE21}',
    '\u{FE22}',
    '\u{FE23}',
    '\u{FE24}',
    '\u{FE25}',
    '\u{FE26}',
    '\u{10A0F}',
    '\u{10A38}',
    '\u{1D185}',
    '\u{1D186}',
    '\u{1D187}',
    '\u{1D188}',
    '\u{1D189}',
    '\u{1D1AA}',
    '\u{1D1AB}',
    '\u{1D1AC}',
    '\u{1D1AD}',
    '\u{1D242}',
    '\u{1D243}',
    '\u{1D244}',
];

#[inline]
fn diacritic(i: usize) -> char {
    *DIACRITICS.get(i).unwrap_or(&DIACRITICS[0])
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Rgba, RgbaImage};

    fn make_img(w: u32, h: u32) -> DynamicImage {
        let mut img = RgbaImage::from_pixel(w, h, Rgba([10, 20, 30, 255]));
        for y in 0..h {
            for x in 0..w {
                img.put_pixel(
                    x,
                    y,
                    Rgba([(x % 251) as u8, (y % 251) as u8, (x + y) as u8, 255]),
                );
            }
        }
        DynamicImage::ImageRgba8(img)
    }

    #[test]
    fn ids_are_unique_and_positive() {
        let a = next_id();
        let b = next_id();
        assert_ne!(a, b);
        assert!(a > 0 && b > 0);
    }

    #[test]
    fn chunks_are_self_contained() {
        let img = make_img(300, 200); // 240000 raw bytes => many chunks
        let id = next_id();
        for c in encode_chunks(&img, id, true) {
            assert!(
                c.starts_with("\x1bPtmux;\x1b\x1b_G"),
                "bad prefix: {:?}",
                &c[..c.len().min(24)]
            );
            assert!(c.ends_with("\x1b\\"));
            assert!(c.contains("m=1;") || c.contains("m=0;"));
        }
    }

    #[test]
    fn first_chunk_carries_metadata() {
        let img = make_img(64, 64);
        let id = 7;
        let first = &encode_chunks(&img, id, false)[0];
        assert!(first.starts_with("\x1b_Gq=2,"));
        assert!(first.contains("i=7,"), "{first}");
        assert!(
            first.contains("a=T,C=1,U=1,f=32,t=d,s=64,v=64,m="),
            "{first}"
        );
        assert!(!first.contains("c=") && !first.contains("r="), "{first}");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn shm_transmit_carries_name_only() {
        let img = make_img(40, 24);
        let seq = encode_shm(&img, 7, false).expect("shm object");
        assert!(
            seq.starts_with("\x1b_Gq=2,i=7,a=T,C=1,U=1,f=32,t=s,s=40,v=24,S=3840;"),
            "{seq}"
        );
        assert!(seq.ends_with("\x1b\\"));
        // payload is base64 of a short shm name, not the pixel data
        let payload = seq
            .rsplit(';')
            .next()
            .unwrap()
            .trim_end_matches(['\u{1b}', '\\']);
        let name = base64_simd::STANDARD.decode_to_vec(payload).unwrap();
        let name = String::from_utf8(name).unwrap();
        assert!(name.starts_with("/rnn-"), "{name}");
        let _ = crate::preview::shm::remove(&name);
    }

    #[test]
    fn continuation_chunks_carry_only_m() {
        let img = make_img(300, 200); // forces several chunks
        let chunks = encode_chunks(&img, 7, false);
        assert!(chunks.len() > 1);
        for c in &chunks[1..] {
            assert!(c.starts_with("\x1b_Gm="), "{c}");
            assert!(!c.contains("a=T") && !c.contains("q=2"), "{c}");
        }
    }

    #[test]
    fn diacritics_follow_kitty_table() {
        assert_eq!(diacritic(0), '\u{305}');
        assert_eq!(diacritic(1), '\u{30D}');
        assert_eq!(diacritic(2), '\u{30E}');
        assert_eq!(diacritic(296), '\u{1D244}');
        assert_eq!(diacritic(297), '\u{305}'); // out of range -> index 0
    }

    #[test]
    fn placeholder_marks_on_every_cell() {
        let id = 0x00AB_CDEF;
        let mut st = KittyState::new(
            id,
            (8, 16),
            false,
            false,
            Settings::default(),
            make_img(2, 2),
        );
        st.transmit_gen.store(1, Ordering::SeqCst);
        st.transmitted_gen.store(1, Ordering::SeqCst);
        st.cells = Some(((3, 2), 100));
        let mut buf = Buffer::empty(Rect::new(0, 0, 5, 4));
        st.render(Rect::new(1, 1, 3, 2), &mut buf);

        let ([x, y], color) = ([1u16, 1u16], Color::Rgb(0xAB, 0xCD, 0xEF));
        // each cell states (row, column): U+0305=0, U+030D=1
        assert_eq!(buf[(x, y)].symbol(), "\u{10EEEE}\u{305}\u{305}");
        assert_eq!(buf[(x, y)].style().fg, Some(color));
        assert_eq!(buf[(x + 1, y)].symbol(), "\u{10EEEE}\u{305}\u{30D}");
        assert_eq!(buf[(x, y + 1)].symbol(), "\u{10EEEE}\u{30D}\u{305}");
        assert_eq!(buf[(x + 2, y + 1)].symbol(), "\u{10EEEE}\u{30D}\u{30E}");
    }

    #[test]
    fn roundtrip_matches_pixels() {
        let img = make_img(100, 80);
        let id = next_id();
        let out = encode_chunks(&img, id, false).concat();
        let mut parsed = String::new();
        let mut rest = out.as_str();
        while let Some(e) = rest.find("\x1b\\") {
            let chunk = &rest[..e];
            let Some(semi) = chunk.find(';') else { break };
            parsed.push_str(&chunk[semi + 1..]);
            rest = &rest[e + 2..];
        }
        let payload = parsed.rsplit(';').next().unwrap();
        let bytes = base64_simd::STANDARD.decode_to_vec(payload).unwrap();
        let expected = img.to_rgba8().into_raw();
        assert_eq!(bytes, expected);
    }

    /// Orientation 6 (rotate 90° CW) must be applied on source decode so a
    /// portrait phone photo is not shown sideways. Cache entries are written
    /// upright, so `decode` intentionally skips this.
    #[test]
    fn decode_source_applies_exif_orientation() {
        let base = std::env::temp_dir().join(format!("rnn-exif-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&base).unwrap();
        let path = base.join("rotated.jpg");

        // 40x20 landscape; orientation 6 makes the displayed image 20x40.
        make_img(40, 20).save(&path).unwrap();
        let jpeg = fs::read(&path).unwrap();
        assert_eq!(&jpeg[..2], [0xFF, 0xD8]);

        // Minimal big-endian TIFF header with one IFD entry: Orientation=6.
        let tiff: &[u8] = &[
            0x4D, 0x4D, 0x00, 0x2A, 0x00, 0x00, 0x00, 0x08, // TIFF header
            0x00, 0x01, // one entry
            0x01, 0x12, 0x00, 0x03, 0x00, 0x00, 0x00, 0x01, 0x00, 0x06, 0x00,
            0x00, // tag 0x0112, SHORT, count 1, value 6
            0x00, 0x00, 0x00, 0x00, // no next IFD
        ];
        let mut app1 = vec![0xFF, 0xE1];
        app1.extend_from_slice(&((tiff.len() + 6 + 2) as u16).to_be_bytes());
        app1.extend_from_slice(b"Exif\0\0");
        app1.extend_from_slice(tiff);

        let mut with_exif = Vec::new();
        with_exif.extend_from_slice(&jpeg[..2]);
        with_exif.extend_from_slice(&app1);
        with_exif.extend_from_slice(&jpeg[2..]);
        fs::write(&path, &with_exif).unwrap();

        let img = decode_source(&path).unwrap();
        assert_eq!((img.width(), img.height()), (20, 40));
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn cap_preserves_aspect() {
        let img = make_img(400, 200);
        let capped = cap_size(&img, (200, 100), Filter::Box);
        assert_eq!((capped.width(), capped.height()), (200, 100));
        let capped = cap_size(&img, (100, 50), Filter::Box);
        assert_eq!((capped.width(), capped.height()), (100, 50));
    }

    #[test]
    fn fit_pads_to_canvas() {
        let img = make_img(64, 32);
        let fitted = fit_to_cells(&img, (16, 40), (10, 5), Filter::Box);
        assert_eq!((fitted.width(), fitted.height()), (160, 200));
    }

    #[test]
    fn zoom_scales_and_crops_around_centre() {
        let img = make_img(64, 32);
        let fitted = fit_to_cells_zoom(&img, (16, 40), (10, 5), 2.0, IMAGE_MAX, Filter::Box);
        assert_eq!((fitted.width(), fitted.height()), (160, 200));
        let px = fitted.to_rgba8();
        assert_eq!(px.get_pixel(80, 100)[3], 255);
        assert_eq!(px.get_pixel(0, 0)[3], 0);
    }

    #[test]
    fn zoom_crops_overflow_without_panicking() {
        let img = make_img(64, 32);
        let fitted = fit_to_cells_zoom(&img, (16, 40), (10, 5), 4.0, IMAGE_MAX, Filter::Box);
        assert_eq!((fitted.width(), fitted.height()), (160, 200));
        let px = fitted.to_rgba8();
        assert_eq!(px.get_pixel(80, 100)[3], 255);
    }

    #[test]
    fn delete_seq_shape() {
        assert_eq!(delete_seq(9, false), "\x1b_Gd=1,q=2,i=9\x1b\\");
        assert!(delete_seq(9, true).starts_with("\x1bPtmux;\x1b\x1b_Gd=1,q=2,i=9"));
        assert!(delete_seq(9, true).ends_with("\x1b\\"));
    }

    #[test]
    fn cache_stem_is_stable_and_keyed() {
        let p = |s: &'static str| Path::new(s);
        assert_eq!(
            cache_stem(p("/a/b.png"), None, 10),
            cache_stem(p("/a/b.png"), None, 10)
        );
        assert_ne!(
            cache_stem(p("/a/b.png"), None, 10),
            cache_stem(p("/a/b.png"), None, 11)
        );
        assert_ne!(
            cache_stem(p("/a/b.png"), None, 10),
            cache_stem(p("/a/c.png"), None, 10)
        );
        let t = Some(SystemTime::UNIX_EPOCH);
        assert_ne!(
            cache_stem(p("/a/b.png"), None, 10),
            cache_stem(p("/a/b.png"), t, 10)
        );
    }

    /// Cache fills are asynchronous, so tests poll instead of assuming the
    /// writer thread has finished by the time `load_*` returns.
    fn wait_for(cond: impl Fn() -> bool) -> bool {
        for _ in 0..200 {
            if cond() {
                return true;
            }
            thread::sleep(std::time::Duration::from_millis(5));
        }
        false
    }

    #[test]
    fn disk_cache_roundtrip() {
        let base = std::env::temp_dir().join(format!("rnn-cache-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let cache = base.join("cache");
        let src = base.join("src.png");
        fs::create_dir_all(&base).unwrap();
        make_img(80, 40).save(&src).unwrap();

        let a = load_from(&cache, &src, None, 123, &Settings::default()).unwrap();
        assert_eq!((a.width(), a.height()), (80, 40));
        let cached = wait_for(|| {
            fs::read_dir(&cache)
                .map(|rd| {
                    rd.flatten()
                        .any(|e| e.path().extension().and_then(|x| x.to_str()) != Some("tmp"))
                })
                .unwrap_or(false)
        });
        assert!(cached, "expected a cached copy");

        // Second load must hit the cached file (same dimensions, no re-decode).
        let b = load_from(&cache, &src, None, 123, &Settings::default()).unwrap();
        assert_eq!((b.width(), b.height()), (80, 40));

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn shared_cache_writes_yazi_entry() {
        let base = std::env::temp_dir().join(format!("rnn-yazi-cache-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let cache = base.join("yazi");
        let src = base.join("src.png");
        fs::create_dir_all(&cache).unwrap();
        make_img(80, 40).save(&src).unwrap();

        let a = load_shared(&cache, &src, &Settings::default()).unwrap();
        assert_eq!((a.width(), a.height()), (80, 40));

        let entry = yazi::target(&cache, &src).unwrap();
        let name = entry.file_name().unwrap().to_str().unwrap();
        assert_eq!(name.len(), 26, "yazi keys are 26 base32 chars");
        assert!(name
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit()));
        assert!(
            wait_for(|| entry.is_file()),
            "expected an extensionless yazi cache entry"
        );
        assert_eq!(fs::read_dir(&cache).unwrap().count(), 1);

        let b = decode(&entry).unwrap();
        assert_eq!((b.width(), b.height()), (80, 40));

        let _ = fs::remove_dir_all(&base);
    }

    /// Micro-benchmark for the preview hot path. Run with:
    /// `cargo test --release bench_preview -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn bench_preview() {
        use std::time::Instant;

        let home = std::env::var("HOME").unwrap();
        let path = PathBuf::from(format!("{home}/Pictures/seiabeach.jpg"));
        if !path.exists() {
            eprintln!("skip: {} not found", path.display());
            return;
        }

        let t = Instant::now();
        let src = decode_source(&path).unwrap();
        println!(
            "decode source {:?}: {:?}",
            (src.width(), src.height()),
            t.elapsed()
        );

        let t = Instant::now();
        let capped = cap_size(&src, IMAGE_MAX, Filter::Box);
        println!("cap_size (resize Box): {:?}", t.elapsed());

        let t = Instant::now();
        let thumb = image::imageops::thumbnail(&src.to_rgba8(), IMAGE_MAX.0, IMAGE_MAX.1);
        println!(
            "thumbnail (box): {:?} {:?}",
            t.elapsed(),
            thumb.dimensions()
        );

        let t = Instant::now();
        let (_, buf) = encode(&capped, 90).unwrap();
        println!("encode cache jpeg: {:?} ({} bytes)", t.elapsed(), buf.len());

        let tmp = std::env::temp_dir().join("rnn-bench-cache.jpg");
        fs::write(&tmp, &buf).unwrap();
        let t = Instant::now();
        let cached = decode_source(&tmp).unwrap();
        println!(
            "decode cached jpeg {:?}: {:?}",
            (cached.width(), cached.height()),
            t.elapsed()
        );
        let _ = fs::remove_file(&tmp);

        // Box-filter resize (imageops::thumbnail) vs Triangle for the fit path.
        let rgba = capped.to_rgba8();
        for (nw, nh) in [(960u32, 800u32), (1280, 800)] {
            let t = Instant::now();
            let a = image::imageops::resize(&rgba, nw, nh, FilterType::Triangle);
            println!("resize Triangle -> {nw}x{nh}: {:?}", t.elapsed());
            let t = Instant::now();
            let b = image::imageops::thumbnail(&rgba, nw, nh);
            println!("thumbnail box   -> {nw}x{nh}: {:?}", t.elapsed());
            assert_eq!(a.dimensions(), b.dimensions());
        }

        for cells in [(80u16, 40u16), (120, 50), (160, 60)] {
            let t = Instant::now();
            let fitted = fit_to_cells_zoom(&capped, (8, 16), cells, 1.0, IMAGE_MAX, Filter::Box);
            println!("fit_to_cells {cells:?}: {:?}", t.elapsed());

            let t = Instant::now();
            let rgba = fitted.to_rgba8();
            println!("  to_rgba8: {:?}", t.elapsed());

            let t = Instant::now();
            let chunks = encode_chunks(&fitted, 1, false);
            println!(
                "  encode_chunks: {:?} ({} chunks, {} bytes)",
                t.elapsed(),
                chunks.len(),
                chunks.iter().map(|c| c.len()).sum::<usize>()
            );

            let t = Instant::now();
            if let Some(seq) = encode_shm(&fitted, 1, false) {
                println!("  encode_shm: {:?}", t.elapsed());
                let name = seq
                    .rsplit(';')
                    .next()
                    .unwrap()
                    .trim_end_matches(['\u{1b}', '\\']);
                if let Ok(bytes) = base64_simd::STANDARD.decode_to_vec(name) {
                    if let Ok(name) = String::from_utf8(bytes) {
                        let _ = crate::preview::shm::remove(&name);
                    }
                }
            }
            let _ = rgba;
        }

        let fitted = fit_to_cells_zoom(&capped, (8, 16), (120, 50), 1.0, IMAGE_MAX, Filter::Box);
        let t = Instant::now();
        let bytes = crate::preview::sixel::encode(&fitted, false);
        println!(
            "sixel encode {}x{}: {:?} ({} bytes)",
            fitted.width(),
            fitted.height(),
            t.elapsed(),
            bytes.len()
        );

        // Break down the sixel encode.
        let rgba = fitted.to_rgba8();
        let t = Instant::now();
        let has_alpha = rgba.pixels().any(|p| p[3] == 0);
        println!("  sixel has_alpha scan: {:?}", t.elapsed());
        let t = Instant::now();
        let mut sample = Vec::with_capacity(rgba.len());
        for p in rgba.pixels() {
            if p[3] != 0 {
                sample.extend_from_slice(&[p[0], p[1], p[2], 255]);
            }
        }
        println!("  sixel sample build: {:?}", t.elapsed());
        let t = Instant::now();
        let quant = color_quant::NeuQuant::new(20, 256, &sample);
        println!("  sixel neuquant: {:?}", t.elapsed());
        let t = Instant::now();
        let mut sum = 0usize;
        for p in rgba.pixels() {
            if p[3] != 0 {
                sum += quant.index_of(&[p[0], p[1], p[2], 255]);
            }
        }
        println!(
            "  sixel index_of loop: {:?} (sum {sum}, alpha {has_alpha})",
            t.elapsed()
        );
    }
}
