use std::{
    io::{self, Write},
    time::{Duration, Instant},
};

mod action;
mod app;
mod config;
mod openwith;
mod preview;
mod util;

fn main() -> io::Result<()> {
    if std::env::var_os("RNN_PROBE").is_some() {
        return probe_graphics();
    }
    let mut app = app::App::new(config::load())?;
    let protocol = detect_protocol();
    let font = probe_cell_size().unwrap_or((8, 16));
    let shm = protocol == preview::ImageProtocol::Kitty && probe_shm();
    if std::env::var_os("RNN_DEBUG").is_some() {
        eprintln!(
            "rnn: protocol={protocol:?} font={font:?} shm={shm} term={:?} tmux={}",
            std::env::var("TERM").unwrap_or_default(),
            preview::tmux_active()
        );
    }
    app.preview_configure(protocol, font, shm);
    let mut terminal = ratatui::init();
    let res = app.run(&mut terminal);
    ratatui::restore();
    res
}

/// `RNN_PROBE=1`: send a tiny kitty transmit and dump whatever the terminal
/// replies for ~3s, so Ghostty can tell us what is wrong with the payload
/// rather than us guessing. Responds are read rather than suppressed (`q=0`:
/// the terminal answers OK on success and an error code on failure). Reply
/// bytes are hex-dumped to stderr (the pane you run it in) and to
/// `/tmp/opencode` so they can be inspected.
fn probe_graphics() -> io::Result<()> {
    use image::{Rgba, RgbaImage};
    use rustix::{
        event::{poll, PollFd, PollFlags},
        io::read,
    };
    use std::os::fd::AsFd;

    let mut img = RgbaImage::from_pixel(8, 8, Rgba([255u8, 0, 0, 255u8]));
    for p in [(0, 0), (7, 0), (0, 7), (7, 7)] {
        img.put_pixel(p.0, p.1, Rgba([255, 255, 255, 255]));
    }
    let id = preview::kitty::next_id();
    let is_tmux = preview::tmux_active();
    eprintln!(
        "rnn-probe: protocol={:?} is_tmux={} id={id} s=8 v=8 cells=(1,1)",
        detect_protocol(),
        is_tmux
    );
    for c in
        preview::kitty::encode_chunks(&image::DynamicImage::ImageRgba8(img.clone()), id, is_tmux)
    {
        io::stdout().write_all(c.as_bytes())?;
        io::stdout().flush()?;
    }

    // Second test: a plain (non-virtual) transmit with q=0. If Ghostty answers
    // this but stayed silent for the U=1 one above, virtual placement transmits
    // are deliberately quiet; if it answers neither, the APC/reply path itself
    // is broken. The result is classified in the token summary below.
    let id2 = preview::kitty::next_id();
    let esc = if is_tmux { "\x1b\x1b" } else { "\x1b" };
    let b64 = base64_simd::STANDARD.encode_to_string(img.as_raw());
    let plain = format!("{esc}_Gq=0,a=T,i={id2},f=32,s=8,v=8,m=0;{b64}{esc}\\");
    let plain = if is_tmux {
        format!("\x1bPtmux;{plain}\x1b\\")
    } else {
        plain
    };
    io::stdout().write_all(plain.as_bytes())?;
    io::stdout().flush()?;
    eprintln!("rnn-probe: also sent plain a=T image id={id2} (q=0, expects an OK/error reply)");

    // The reply arrives as a bare `ESC_G...ESC\` with no newline. In cooked
    // mode the tty line discipline eats/interprets it before `read` can see it,
    // so switch to raw mode for the duration of the read.
    use ratatui::crossterm::terminal::{disable_raw_mode, enable_raw_mode};
    let _ = enable_raw_mode();

    let stdin = std::io::stdin();
    let fd = stdin.as_fd();
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    let deadline = std::time::Instant::now() + Duration::from_millis(3000);
    loop {
        let remain = deadline.saturating_duration_since(std::time::Instant::now());
        if remain.is_zero() {
            break;
        }
        match poll(
            &mut [PollFd::new(&fd, PollFlags::IN)],
            remain.as_millis().min(200) as i32,
        ) {
            Ok(0) => continue,
            Ok(_) => match read(fd, &mut chunk) {
                Ok(0) => break,
                Ok(n) => buf.extend_from_slice(&chunk[..n]),
                Err(_) => break,
            },
            Err(_) => break,
        }
    }
    let _ = disable_raw_mode();

    // Classify the reply tokens:  ESC _ G i=<id>;TOKEN ESC \
    let mut counts = std::collections::BTreeMap::<String, usize>::new();
    let mut rest = buf.as_slice();
    while let Some(i) = rest.iter().position(|&b| b == 0x1b) {
        if rest.get(i + 1..i + 3) == Some(b"_G".as_slice()) {
            let after = &rest[i + 4..];
            let end = after.iter().position(|&b| b == 0x1b).unwrap_or(after.len());
            let body = String::from_utf8_lossy(&after[..end]).into_owned();
            let tok = body.rsplit(';').next().unwrap_or("?").to_string();
            *counts.entry(tok).or_default() += 1;
            rest = &rest[i + 4 + end..];
        } else {
            rest = &rest[i + 1..];
        }
    }
    let path = "/tmp/opencode/rnn_probe_resp.bin";
    if let Ok(mut f) = std::fs::File::create(path) {
        use std::io::Write as _;
        let _ = f.write_all(&buf);
    }
    eprintln!("rnn-probe: reply bytes={} tokens={counts:?}", buf.len());
    if buf.is_empty() {
        eprintln!("rnn-probe: NO reply received (Ghostty silent to both q=0 transmits)");
    }
    for (i, b) in buf.iter().enumerate().take(600) {
        if i % 48 == 0 {
            eprint!("\nrnn-probe:");
        }
        eprint!(" {:02x}", b);
    }
    eprintln!();
    Ok(())
}

/// Probe whether the terminal accepts the kitty shared-memory transfer medium
/// (`t=s`). Writes a tiny object and sends the same `a=q,t=s` query yazi uses;
/// an `OK` reply means the terminal opened the object. The object is removed
/// afterwards (a no-op when the terminal already unlinked it).
#[cfg(target_os = "linux")]
fn probe_shm() -> bool {
    use ratatui::crossterm::terminal::{disable_raw_mode, enable_raw_mode};
    use rustix::{
        event::{poll, PollFd, PollFlags},
        io::read,
    };
    use std::os::fd::AsFd;

    let id: u32 = 916472805; // same probe id yazi uses
    let Ok(name) = preview::shm::create(&[0u8; 3]) else {
        return false;
    };
    let b64 = base64_simd::STANDARD.encode_to_string(name.as_bytes());
    let is_tmux = preview::tmux_active();
    let esc = if is_tmux { "\x1b\x1b" } else { "\x1b" };
    let inner = format!("{esc}_Gi={id},s=1,v=1,a=q,t=s,f=24,S=3;{b64}{esc}\\");
    let seq = if is_tmux {
        format!("\x1bPtmux;{inner}\x1b\\")
    } else {
        inner
    };

    let _ = enable_raw_mode();
    let mut so = std::io::stdout();
    let _ = so.write_all(seq.as_bytes());
    let _ = so.flush();

    let stdin = std::io::stdin();
    let fd = stdin.as_fd();
    let mut buf = Vec::new();
    let mut chunk = [0u8; 512];
    let deadline = Instant::now() + Duration::from_millis(800);
    loop {
        let remain = deadline.saturating_duration_since(Instant::now());
        if remain.is_zero() {
            break;
        }
        match poll(
            &mut [PollFd::new(&fd, PollFlags::IN)],
            remain.as_millis().min(200) as i32,
        ) {
            Ok(0) => continue,
            Ok(_) => match read(fd, &mut chunk) {
                Ok(0) => break,
                Ok(n) => {
                    buf.extend_from_slice(&chunk[..n]);
                    if buf.windows(2).any(|w| w == b"OK") {
                        break;
                    }
                }
                Err(_) => break,
            },
            Err(_) => break,
        }
    }
    let _ = disable_raw_mode();
    let _ = preview::shm::remove(&name);

    String::from_utf8_lossy(&buf).contains(";OK")
}

#[cfg(not(target_os = "linux"))]
fn probe_shm() -> bool {
    false
}

/// Decide which graphics protocol the terminal speaks. An explicit
/// `RNN_IMAGE_PROTOCOL=kitty|sixel|none` wins; otherwise kitty/ghostty
/// environment hints are checked first, then a DA1 query for sixel support.
fn detect_protocol() -> preview::ImageProtocol {
    use preview::ImageProtocol;

    if let Ok(v) = std::env::var("RNN_IMAGE_PROTOCOL") {
        match v.trim().to_ascii_lowercase().as_str() {
            "none" | "off" | "0" | "false" => return ImageProtocol::None,
            "kitty" => return ImageProtocol::Kitty,
            "sixel" => return ImageProtocol::Sixel,
            _ => {}
        }
    }
    if kitty_supported() {
        return ImageProtocol::Kitty;
    }
    if sixel_supported() {
        return ImageProtocol::Sixel;
    }
    ImageProtocol::None
}

/// True when the outer terminal is kitty or ghostty (the two terminals with a
/// native kitty graphics protocol implementation this driver targets).
fn kitty_supported() -> bool {
    let nonempty = |k: &str| std::env::var(k).is_ok_and(|v| !v.is_empty());
    if nonempty("KITTY_WINDOW_ID") || nonempty("GHOSTTY_RESOURCES_DIR") {
        return true;
    }
    let term_program = std::env::var("TERM_PROGRAM").unwrap_or_default();
    let term = std::env::var("TERM").unwrap_or_default();
    let k = |s: &str| {
        s.to_ascii_lowercase().contains("ghostty") || s.to_ascii_lowercase().contains("kitty")
    };
    k(&term_program) || k(&term)
}

/// Sixel support: a `TERM` hint, or the DA1 reply advertising attribute 4.
fn sixel_supported() -> bool {
    let term = std::env::var("TERM")
        .unwrap_or_default()
        .to_ascii_lowercase();
    if term.contains("foot") || term.contains("sixel") {
        return true;
    }
    let reply = query_terminal(b"\x1b[c", 500, b'c');
    da1_has_sixel(&reply)
}

/// `ESC[?...;4;...c` — DA1 attribute 4 means sixel graphics.
fn da1_has_sixel(bytes: &[u8]) -> bool {
    let Ok(s) = std::str::from_utf8(bytes) else {
        return false;
    };
    let Some(start) = s.find("\x1b[?") else {
        return false;
    };
    let rest = &s[start + 3..];
    let Some(end) = rest.find('c') else {
        return false;
    };
    rest[..end].split(';').any(|p| p == "4")
}

/// Send a query sequence and collect the reply for up to `timeout_ms`.
fn query_terminal(seq: &[u8], timeout_ms: u64, until: u8) -> Vec<u8> {
    use rustix::{
        event::{poll, PollFd, PollFlags},
        termios::{tcgetattr, tcsetattr, OptionalActions},
    };
    use std::os::fd::AsFd;

    let stdin = std::io::stdin();
    let fd = stdin.as_fd();
    let Ok(term) = tcgetattr(fd) else {
        return Vec::new();
    };
    let orig = term.clone();
    let mut raw = term;
    raw.make_raw();
    if tcsetattr(fd, OptionalActions::Now, &raw).is_err() {
        return Vec::new();
    }

    let mut so = std::io::stdout();
    let _ = so.write_all(seq);
    let _ = so.flush();

    let mut out = Vec::new();
    let mut buf = [0u8; 256];
    let deadline = Instant::now() + Duration::from_millis(timeout_ms);
    loop {
        let remain = deadline.saturating_duration_since(Instant::now());
        if remain.is_zero() {
            break;
        }
        let timeout = remain.as_millis().min(200) as i32;
        match poll(&mut [PollFd::new(&fd, PollFlags::IN)], timeout) {
            Ok(0) => break,
            Ok(_) => match rustix::io::read(fd, &mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    out.extend_from_slice(&buf[..n]);
                    if out.contains(&until) {
                        break;
                    }
                }
                Err(_) => break,
            },
            Err(_) => break,
        }
    }
    let _ = tcsetattr(fd, OptionalActions::Now, &orig);
    out
}

/// Cell size from `TIOCGWINSZ` (physical pixels / cells). Foot reports zero
/// pixels at startup, then a scaled value, then the final one, so wait for two
/// consecutive equal readings before trusting it.
fn winsize_cell_size() -> Option<(u16, u16)> {
    use std::os::fd::AsFd;
    let stdin = std::io::stdin();
    let mut last = None;
    for _ in 0..20 {
        if let Ok(ws) = rustix::termios::tcgetwinsize(stdin.as_fd()) {
            let (cols, rows) = (ws.ws_col, ws.ws_row);
            let (xpix, ypix) = (ws.ws_xpixel, ws.ws_ypixel);
            if cols > 0 && rows > 0 && xpix > 0 && ypix > 0 {
                let size = (xpix / cols, ypix / rows);
                if size.0 > 0 && size.1 > 0 {
                    if last == Some(size) {
                        return Some(size);
                    }
                    last = Some(size);
                }
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    last
}

/// Ask the terminal for its cell size in pixels. Prefers `TIOCGWINSZ`; when
/// the pty has not been sized yet (foot reports zero pixels during startup),
/// derive it from the window pixel size (`ESC[14t`) and text area
/// (`ESC[18t`). The `ESC[16t` cell query is a last resort because some
/// terminals report it at a different scale.
fn probe_cell_size() -> Option<(u16, u16)> {
    if preview::tmux_active() {
        if let Some(v) = tmux_cell_size() {
            return Some(v);
        }
    }
    if let Some(v) = winsize_cell_size() {
        return Some(v);
    }
    if let Some(v) = query_window_and_text_size() {
        return Some(v);
    }
    parse_cell_size(&query_terminal(b"\x1b[16t", 1000, b't'))
}

/// Inside tmux the pty reports tmux's (sometimes scaled) pixel size. Ask tmux
/// for the outer terminal's cell size instead, which is what the sixel is
/// rendered with after passthrough.
fn tmux_cell_size() -> Option<(u16, u16)> {
    // The tmux client can still report the terminal's pre-map cell size for a
    // few frames after the window is created, so wait for two equal readings.
    let mut last = None;
    for _ in 0..20 {
        let size = query_tmux_cell_size();
        if let Some(size) = size {
            if last == Some(size) {
                return Some(size);
            }
            last = Some(size);
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    last
}

fn query_tmux_cell_size() -> Option<(u16, u16)> {
    let out = std::process::Command::new("tmux")
        .args([
            "display-message",
            "-p",
            "#{client_cell_width}x#{client_cell_height}",
        ])
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let (w, h) = text.trim().split_once('x')?;
    let w = w.parse::<u16>().ok()?;
    let h = h.parse::<u16>().ok()?;
    (w > 0 && h > 0).then_some((w, h))
}

/// `ESC[14t` -> `ESC[4;<height_px>;<width_px>t` and
/// `ESC[18t` -> `ESC[8;<rows>;<cols>t`.
fn query_window_and_text_size() -> Option<(u16, u16)> {
    let win = query_terminal(b"\x1b[14t", 500, b't');
    let text = query_terminal(b"\x1b[18t", 500, b't');
    let (hpx, wpx) = parse_report(&win, "\x1b[4;")?;
    let (rows, cols) = parse_report(&text, "\x1b[8;")?;
    if rows == 0 || cols == 0 {
        return None;
    }
    let (w, h) = (wpx / cols, hpx / rows);
    (w > 0 && h > 0).then_some((w, h))
}

/// Parse `<a>;<b>t` after `marker`, returning `(a, b)`.
fn parse_report(bytes: &[u8], marker: &str) -> Option<(u16, u16)> {
    let s = std::str::from_utf8(bytes).ok()?;
    let start = s.find(marker)?;
    let rest = &s[start + marker.len()..];
    let end = rest.find('t')?;
    let mut parts = rest[..end].split(';');
    let a = parts.next()?.parse::<u16>().ok()?;
    let b = parts.next()?.parse::<u16>().ok()?;
    Some((a, b))
}

fn parse_cell_size(bytes: &[u8]) -> Option<(u16, u16)> {
    let s = std::str::from_utf8(bytes).ok()?;
    let marker = s.find("\x1b[6;")?;
    let rest = &s[marker + 4..];
    let end = rest.find('t')?;
    let mut parts = rest[..end].split(';');
    let h = parts.next()?.parse::<u16>().ok()?;
    let w = parts.next()?.parse::<u16>().ok()?;
    Some((w, h))
}
