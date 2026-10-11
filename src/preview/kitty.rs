//! Kitty graphics protocol (Kgp) preview backend using Unicode placeholders.
//!
//! Mirrors yazi's driver: the image is transmitted as a chunked APC (`_G`),
//! one complete write per <=4KiB of base64 payload. Inside tmux every chunk is
//! wrapped in a `\ePtmux;...\e\` passthrough, which tmux forwards as a single
//! write (the same wrapper dropped wholesale when it contains a multi-megabyte
//! sequence). The image is then displayed through U+10EEEE placeholder cells
//! whose foreground color encodes the image id.

use std::{
    io::{self, Write},
    path::Path,
    sync::{
        atomic::{AtomicU32, AtomicU64, Ordering},
        Arc, OnceLock,
    },
    thread,
};

use image::{
    DynamicImage, ImageReader,
    imageops::{self, FilterType},
    RgbaImage,
};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Style},
};

/// Pixel size of a single terminal cell, used to size the transmit canvas.
pub type FontSize = (u16, u16);

/// Hard cap on the decoded/transmitted image size.
pub const IMAGE_MAX: (u32, u32) = (1920, 1080);

/// A per-process image id, masked to 24 bits (the id is carried entirely by the
/// placeholder foreground colour, like yazi's `pid % (0xffffff + 1)`). The
/// counter keeps ids unique within one process across redraws.
pub fn next_id() -> u32 {
    static BASE: OnceLock<u32> = OnceLock::new();
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let base = *BASE.get_or_init(|| (std::process::id() % 0x100_0000).max(1));
    base.wrapping_add(NEXT.fetch_add(1, Ordering::Relaxed)) & 0xFF_FFFF
}

/// Decode an image file, capped to [`IMAGE_MAX`] (aspect preserved).
pub fn open_image(path: &Path) -> io::Result<DynamicImage> {
    let img = ImageReader::open(path)?
        .with_guessed_format()?
        .decode()
        .map_err(io::Error::other)?;
    Ok(cap_size(&img, IMAGE_MAX))
}

/// Scale down to fit within `max`, never upscaling.
pub fn cap_size(img: &DynamicImage, max: (u32, u32)) -> DynamicImage {
    let (iw, ih) = (img.width(), img.height());
    let scale = (max.0 as f32 / iw as f32).min(max.1 as f32 / ih as f32).min(1.0);
    if scale >= 1.0 {
        return img.clone();
    }
    let (nw, nh) = ((iw as f32 * scale).max(1.0) as u32, (ih as f32 * scale).max(1.0) as u32);
    img.resize(nw, nh, FilterType::Triangle)
}

/// Pixel dimensions of an area of `cells` cells, capped at [`IMAGE_MAX`].
pub fn cell_pixels(font: FontSize, cells: (u16, u16)) -> (u32, u32) {
    let w = u32::from(cells.0).saturating_mul(u32::from(font.0)).min(IMAGE_MAX.0);
    let h = u32::from(cells.1).saturating_mul(u32::from(font.1)).min(IMAGE_MAX.1);
    (w, h)
}

/// Fit `img` (aspect preserved) into `cells` worth of pixels and pad the
/// remainder with transparency so the transmitted canvas is exactly cell-sized.
pub fn fit_to_cells(img: &DynamicImage, font: FontSize, cells: (u16, u16)) -> DynamicImage {
    let (cw, ch) = cell_pixels(font, cells);
    if cw == 0 || ch == 0 {
        return img.clone();
    }
    let fitted = cap_size(img, (cw, ch));
    let (fw, fh) = (fitted.width(), fitted.height());
    if fw == cw && fh == ch {
        return fitted;
    }
    let mut canvas = RgbaImage::new(cw, ch);
    imageops::overlay(&mut canvas, &fitted, i64::from((cw - fw) / 2), i64::from((ch - fh) / 2));
    DynamicImage::ImageRgba8(canvas)
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
    img: Arc<DynamicImage>,
    cells: Option<(u16, u16)>,
    transmit_gen: Arc<AtomicU64>,
    transmitted_gen: Arc<AtomicU64>,
}

impl KittyState {
    pub fn new(id: u32, font: FontSize, is_tmux: bool, shm: bool, img: DynamicImage) -> Self {
        Self {
            id,
            font,
            is_tmux,
            shm,
            img: Arc::new(img),
            cells: None,
            transmit_gen: Arc::new(AtomicU64::new(0)),
            transmitted_gen: Arc::new(AtomicU64::new(0)),
        }
    }

    pub fn id(&self) -> u32 {
        self.id
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
        if self.cells != Some(dims) {
            self.cells = Some(dims);
            let gen = self.transmit_gen.load(Ordering::SeqCst) + 1;
            self.transmit_gen.store(gen, Ordering::SeqCst);
            if std::env::var_os("RNN_DEBUG").is_some() {
                eprintln!("kitty: spawn gen={gen} dims={dims:?}");
            }
            stream_delete(self.id, self.is_tmux);
            let img = Arc::clone(&self.img);
            let target = Arc::clone(&self.transmitted_gen);
            let id = self.id;
            let is_tmux = self.is_tmux;
            let shm = self.shm;
            let font = self.font;
            let _ = thread::Builder::new()
                .name("kitty-stream".into())
                .spawn(move || {
                    let fitted = fit_to_cells(&img, font, dims);
                    if shm {
                        if let Some(seq) = encode_shm(&fitted, id, is_tmux) {
                            stream_seq(&seq);
                            target.fetch_max(gen, Ordering::SeqCst);
                            return;
                        }
                        // shared memory unavailable at runtime: fall back
                    }
                    let chunks = encode_chunks(&fitted, id, is_tmux);
                    stream_chunks(&chunks);
                    target.fetch_max(gen, Ordering::SeqCst);
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
                img.put_pixel(x, y, Rgba([(x % 251) as u8, (y % 251) as u8, (x + y) as u8, 255]));
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
            assert!(c.starts_with("\x1bPtmux;\x1b\x1b_G"), "bad prefix: {:?}", &c[..c.len().min(24)]);
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
        assert!(first.contains("a=T,C=1,U=1,f=32,t=d,s=64,v=64,m="), "{first}");
        assert!(!first.contains("c=") && !first.contains("r="), "{first}");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn shm_transmit_carries_name_only() {
        let img = make_img(40, 24);
        let seq = encode_shm(&img, 7, false).expect("shm object");
        assert!(seq.starts_with("\x1b_Gq=2,i=7,a=T,C=1,U=1,f=32,t=s,s=40,v=24,S=3840;"), "{seq}");
        assert!(seq.ends_with("\x1b\\"));
        // payload is base64 of a short shm name, not the pixel data
        let payload = seq.rsplit(';').next().unwrap().trim_end_matches(['\u{1b}', '\\']);
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
        let mut st = KittyState::new(id, (8, 16), false, false, make_img(2, 2));
        st.transmit_gen.store(1, Ordering::SeqCst);
        st.transmitted_gen.store(1, Ordering::SeqCst);
        st.cells = Some((3, 2));
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

    #[test]
    fn cap_preserves_aspect() {
        let img = make_img(400, 200);
        let capped = cap_size(&img, (200, 100));
        assert_eq!((capped.width(), capped.height()), (200, 100));
        let capped = cap_size(&img, (100, 50));
        assert_eq!((capped.width(), capped.height()), (100, 50));
    }

    #[test]
    fn fit_pads_to_canvas() {
        let img = make_img(64, 32);
        let fitted = fit_to_cells(&img, (16, 40), (10, 5));
        assert_eq!((fitted.width(), fitted.height()), (160, 200));
    }

    #[test]
    fn delete_seq_shape() {
        assert_eq!(delete_seq(9, false), "\x1b_Gd=1,q=2,i=9\x1b\\");
        assert!(delete_seq(9, true).starts_with("\x1bPtmux;\x1b\x1b_Gd=1,q=2,i=9"));
        assert!(delete_seq(9, true).ends_with("\x1b\\"));
    }
}