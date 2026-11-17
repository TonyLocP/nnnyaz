//! Sixel preview backend for terminals such as foot.
//!
//! Sixels are painted directly on the terminal grid instead of going through
//! Unicode placeholder cells. The encoder mirrors yazi's sixel driver: the
//! image is quantized to at most 255 colours (index 0 is reserved for
//! transparency), then emitted one pixel row at a time with `$`/`-` band
//! separators. Encoding runs on a background thread because a large preview
//! can take a few hundred milliseconds.

use std::{
    fmt::Write as _,
    io::{self, Write},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
    thread,
};

use image::DynamicImage;
use ratatui::layout::Rect;

use super::kitty::{self, FontSize, Settings};

/// Shared slot holding the latest encoded generation.
type Encoded = Arc<Mutex<Option<(u64, Arc<Vec<u8>>)>>>;

/// A sixel image for a fixed cell area and zoom level.
pub struct SixelState {
    font: FontSize,
    is_tmux: bool,
    settings: Settings,
    img: Arc<DynamicImage>,
    cells: Option<((u16, u16), u32)>,
    zoom: u32,
    gen: u64,
    encoded_gen: Arc<AtomicU64>,
    data: Encoded,
    rect: Option<Rect>,
    drawn: Option<(Rect, u64)>,
    force: bool,
}

impl SixelState {
    pub fn new(font: FontSize, is_tmux: bool, settings: Settings, img: DynamicImage) -> Self {
        Self {
            font,
            is_tmux,
            settings,
            img: Arc::new(img),
            cells: None,
            zoom: 100,
            gen: 0,
            encoded_gen: Arc::new(AtomicU64::new(0)),
            data: Arc::new(Mutex::new(None)),
            rect: None,
            drawn: None,
            force: false,
        }
    }

    /// Set the zoom percentage (100 = fit). A change re-encodes the image.
    pub fn set_zoom(&mut self, percent: u32) {
        let percent = percent.max(100);
        if self.zoom != percent {
            self.zoom = percent;
            self.cells = None;
            self.force = true;
        }
    }

    pub fn forget_drawn(&mut self) {
        self.drawn = None;
    }

    pub fn drawn(&self) -> Option<(Rect, u64)> {
        self.drawn
    }

    /// Ensure an encoding for `rect` exists, starting one when the area or
    /// zoom changed. Safe to call every frame.
    pub fn prepare(&mut self, rect: Rect) {
        if rect.width == 0 || rect.height == 0 {
            return;
        }
        self.rect = Some(rect);
        let dims = (rect.width, rect.height);
        if self.cells != Some((dims, self.zoom)) {
            self.cells = Some((dims, self.zoom));
            self.gen += 1;
            let gen = self.gen;
            let img = Arc::clone(&self.img);
            let font = self.font;
            let is_tmux = self.is_tmux;
            let zoom = self.zoom as f32 / 100.0;
            let settings = self.settings;
            let data = Arc::clone(&self.data);
            let encoded_gen = Arc::clone(&self.encoded_gen);
            let _ = thread::Builder::new()
                .name("sixel-encode".into())
                .spawn(move || {
                    let fitted = kitty::fit_to_cells_zoom(
                        &img,
                        font,
                        dims,
                        zoom,
                        settings.max,
                        settings.filter,
                    );
                    let bytes = encode(&fitted, is_tmux);
                    if std::env::var_os("RNN_DEBUG").is_some() {
                        eprintln!(
                            "sixel: encoded gen={gen} cells={dims:?} bytes={}",
                            bytes.len()
                        );
                    }
                    if let Ok(mut slot) = data.lock() {
                        // A newer generation may have finished first; never
                        // replace it with stale pixels.
                        if slot.as_ref().is_none_or(|(old, _)| gen >= *old) {
                            *slot = Some((gen, Arc::new(bytes)));
                        }
                    }
                    encoded_gen.fetch_max(gen, Ordering::SeqCst);
                });
        }
        if self.force {
            self.force = false;
            self.drawn = None;
        }
    }

    /// The encoded image and generation, when the current request is done.
    pub fn ready(&self) -> Option<(Rect, u64, Arc<Vec<u8>>)> {
        let rect = self.rect?;
        let gen = self.gen;
        if self.encoded_gen.load(Ordering::SeqCst) < gen {
            return None;
        }
        let (data_gen, data) = self.data.lock().ok()?.clone()?;
        if data_gen != gen {
            return None;
        }
        Some((rect, gen, data))
    }

    /// The most recently completed encoding generation.
    pub fn encoded_gen(&self) -> u64 {
        self.encoded_gen.load(Ordering::SeqCst)
    }

    pub fn mark_drawn(&mut self, rect: Rect, gen: u64) {
        self.drawn = Some((rect, gen));
    }
}

/// 32×32×32 nearest-palette lookup (5 bits per channel).
fn palette_lut(palette: &[u8], colors: usize) -> Vec<u8> {
    let mut lut = vec![0u8; 32 * 32 * 32];
    for r in 0..32usize {
        let rv = (r * 8 + 4) as i32;
        for g in 0..32usize {
            let gv = (g * 8 + 4) as i32;
            for b in 0..32usize {
                let bv = (b * 8 + 4) as i32;
                let mut best = 0usize;
                let mut bestd = i32::MAX;
                for i in 0..colors {
                    let dr = rv - i32::from(palette[i * 3]);
                    let dg = gv - i32::from(palette[i * 3 + 1]);
                    let db = bv - i32::from(palette[i * 3 + 2]);
                    let d = dr * dr + dg * dg + db * db;
                    if d < bestd {
                        bestd = d;
                        best = i;
                    }
                }
                lut[(r << 10) | (g << 5) | b] = best as u8;
            }
        }
    }
    lut
}

/// Encode an RGBA image as a sixel sequence. Transparent pixels are emitted as
/// `?` runs, which advance the sixel cursor without painting anything.
pub fn encode(img: &DynamicImage, is_tmux: bool) -> Vec<u8> {
    let rgba = img.to_rgba8();
    let (w, h) = rgba.dimensions();
    if w == 0 || h == 0 {
        return Vec::new();
    }

    let has_alpha = rgba.pixels().any(|p| p[3] == 0);
    let mut sample = Vec::with_capacity(rgba.len());
    for p in rgba.pixels() {
        if p[3] != 0 {
            sample.extend_from_slice(&[p[0], p[1], p[2], 255]);
        }
    }

    if sample.is_empty() {
        return wrap_tmux(&transparent(w, h), is_tmux);
    }

    let colors = if has_alpha { 255 } else { 256 };
    let samplefac = if sample.len() / 4 > 250_000 { 20 } else { 10 };
    let quant = color_quant::NeuQuant::new(samplefac, colors, &sample);
    let palette = quant.color_map_rgb();
    let offset = u8::from(has_alpha);
    // Nearest-palette lookup for every 5-bit-per-channel colour. Building the
    // 32³ table costs a few milliseconds once; mapping each pixel through
    // `NeuQuant::index_of` (a search over the palette) for a full preview can
    // take tens of milliseconds, so the table pays for itself immediately.
    let lut = palette_lut(&palette, colors);

    let mut out = String::with_capacity(1024 + (w as usize * h as usize) / 2);
    let _ = write!(out, "\x1bP9;1q\"1;1;{w};{h}");
    for i in 0..colors {
        let (r, g, b) = (palette[i * 3], palette[i * 3 + 1], palette[i * 3 + 2]);
        let _ = write!(
            out,
            "#{};2;{};{};{}",
            i + offset as usize,
            u16::from(r) * 100 / 255,
            u16::from(g) * 100 / 255,
            u16::from(b) * 100 / 255
        );
    }

    let mut row = vec![None::<u8>; w as usize];
    for y in 0..h {
        for (x, slot) in row.iter_mut().enumerate() {
            let p = rgba.get_pixel(x as u32, y);
            *slot = if p[3] == 0 {
                None
            } else {
                let key = (usize::from(p[0]) >> 3) << 10
                    | (usize::from(p[1]) >> 3) << 5
                    | usize::from(p[2]) >> 3;
                Some(lut[key] + offset)
            };
        }

        let c = (b'?' + (1 << (y % 6))) as char;
        let mut x = 0usize;
        while x < row.len() {
            let value = row[x];
            let mut end = x + 1;
            while end < row.len() && row[end] == value {
                end += 1;
            }
            let count = end - x;
            match value {
                Some(idx) => {
                    let _ = write!(out, "#{idx}");
                    if count > 1 {
                        let _ = write!(out, "!{count}");
                    }
                    out.push(c);
                }
                None => {
                    if count > 1 {
                        let _ = write!(out, "!{count}");
                    }
                    out.push('?');
                }
            }
            x = end;
        }
        out.push('$');
        if y % 6 == 5 {
            out.push('-');
        }
    }
    out.push_str("\x1b\\");
    wrap_tmux(out.as_bytes(), is_tmux)
}

/// A fully transparent sixel of the requested size.
fn transparent(w: u32, h: u32) -> Vec<u8> {
    format!("\x1bP9;1q\"1;1;{w};{h}\x1b\\").into_bytes()
}

fn wrap_tmux(inner: &[u8], is_tmux: bool) -> Vec<u8> {
    if !is_tmux {
        return inner.to_vec();
    }
    let mut out = Vec::with_capacity(inner.len() + 16);
    out.extend_from_slice(b"\x1bPtmux;");
    for &b in inner {
        if b == 0x1b {
            out.push(0x1b);
        }
        out.push(b);
    }
    out.extend_from_slice(b"\x1b\\");
    out
}

/// Paint a ready sixel at `rect` (cell coordinates), saving and restoring the
/// text cursor so ratatui's cursor tracking stays valid.
pub fn draw(rect: Rect, data: &[u8]) -> io::Result<()> {
    let mut out = io::stdout().lock();
    out.write_all(b"\x1b7")?;
    write!(out, "\x1b[{};{}H", rect.y + 1, rect.x + 1)?;
    out.write_all(data)?;
    out.write_all(b"\x1b8")?;
    out.flush()
}

/// Erase a previously painted sixel by writing spaces over its cells. Foot
/// (and other sixel terminals) drop the image when the cells are overwritten.
pub fn clear(rect: Rect) -> io::Result<()> {
    let mut out = io::stdout().lock();
    out.write_all(b"\x1b7\x1b[0m")?;
    let spaces = " ".repeat(rect.width as usize);
    for y in rect.y..rect.y + rect.height {
        write!(out, "\x1b[{};{}H{}", y + 1, rect.x + 1, spaces)?;
    }
    out.write_all(b"\x1b8")?;
    out.flush()
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Rgba, RgbaImage};

    #[test]
    fn encodes_opaque_pixels_and_transparency() {
        let mut img = RgbaImage::new(4, 7);
        for x in 0..4 {
            img.put_pixel(x, 0, Rgba([255, 0, 0, 255]));
        }
        let out = encode(&DynamicImage::ImageRgba8(img), false);
        let text = String::from_utf8(out).unwrap();
        assert!(text.starts_with("\x1bP9;1q\"1;1;4;7"));
        assert!(text.contains('$'));
        assert!(text.contains('-'));
        assert!(text.contains('?'));
        assert!(text.ends_with("\x1b\\"));
    }

    #[test]
    fn lut_maps_colours_to_nearest_palette_entry() {
        let palette = vec![0, 0, 0, 255, 255, 255, 255, 0, 0];
        let lut = palette_lut(&palette, 3);
        let idx = |r: u8, g: u8, b: u8| {
            lut[(usize::from(r) >> 3) << 10 | (usize::from(g) >> 3) << 5 | usize::from(b) >> 3]
        };
        assert_eq!(idx(250, 2, 2), 2);
        assert_eq!(idx(2, 2, 2), 0);
        assert_eq!(idx(250, 250, 250), 1);
    }

    #[test]
    fn tmux_wrapper_doubles_escapes() {
        let img = RgbaImage::from_pixel(1, 1, Rgba([0, 0, 0, 0]));
        let out = encode(&DynamicImage::ImageRgba8(img), true);
        assert!(out.starts_with(b"\x1bPtmux;\x1b\x1bP9;1q"));
        assert!(out.ends_with(b"\x1b\\"));
    }
}
