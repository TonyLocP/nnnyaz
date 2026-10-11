use std::{
    io::{self, Write},
    time::{Duration, Instant},
};

mod app;
mod preview;
mod util;

fn main() -> io::Result<()> {
    if std::env::var_os("RNN_PROBE").is_some() {
        return probe_graphics();
    }
    let mut app = app::App::new()?;
    let graphics = graphics_supported();
    let font = probe_cell_size().unwrap_or((8, 16));
    let shm = graphics && probe_shm();
    if std::env::var_os("RNN_DEBUG").is_some() {
        eprintln!("rnn: graphics={graphics} font={font:?} shm={shm}");
    }
    app.preview_configure(graphics, font, shm);
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
        event::{PollFd, PollFlags, poll},
        io::read,
    };
    use std::os::fd::AsFd;

    let mut img = RgbaImage::from_pixel(8, 8, Rgba([255u8, 0, 0, 255u8]));
    for p in [(0, 0), (7, 0), (0, 7), (7, 7)] {
        img.put_pixel(p.0, p.1, Rgba([255, 255, 255, 255]));
    }
    let id = preview::kitty::next_id();
    let is_tmux = std::env::var_os("TMUX").is_some();
    eprintln!(
        "rnn-probe: graphics={} is_tmux={} id={id} s=8 v=8 cells=(1,1)",
        graphics_supported(),
        is_tmux
    );
    for c in preview::kitty::encode_chunks(&image::DynamicImage::ImageRgba8(img.clone()), id, is_tmux)
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
        match poll(&mut [PollFd::new(&fd, PollFlags::IN)], remain.as_millis().min(200) as i32) {
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
        event::{PollFd, PollFlags, poll},
        io::read,
    };
    use std::os::fd::AsFd;

    let id: u32 = 916472805; // same probe id yazi uses
    let Ok(name) = preview::shm::create(&[0u8; 3]) else {
        return false;
    };
    let b64 = base64_simd::STANDARD.encode_to_string(name.as_bytes());
    let is_tmux = std::env::var_os("TMUX").is_some();
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
        match poll(&mut [PollFd::new(&fd, PollFlags::IN)], remain.as_millis().min(200) as i32) {
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

/// True when the outer terminal is kitty or ghostty (the two terminals with a
/// native kitty graphics protocol implementation this driver targets).
fn graphics_supported() -> bool {
    let nonempty = |k: &str| std::env::var(k).is_ok_and(|v| !v.is_empty());
    if nonempty("KITTY_WINDOW_ID") || nonempty("GHOSTTY_RESOURCES_DIR") {
        return true;
    }
    let term_program = std::env::var("TERM_PROGRAM").unwrap_or_default();
    let term = std::env::var("TERM").unwrap_or_default();
    let k = |s: &str| s.to_ascii_lowercase().contains("ghostty") || s.to_ascii_lowercase().contains("kitty");
    k(&term_program) || k(&term)
}

/// Ask the terminal for its cell size in pixels: `ESC[16t` replies
/// `ESC[6;<height_px>;<width_px>t`.
fn probe_cell_size() -> Option<(u16, u16)> {
    use rustix::{
        event::{PollFd, PollFlags, poll},
        termios::{OptionalActions, tcgetattr, tcsetattr},
    };
    use std::os::fd::AsFd;

    let stdin = std::io::stdin();
    let fd = stdin.as_fd();
    let term = tcgetattr(fd).ok()?;
    let orig = term.clone();

    let mut raw = term;
    raw.make_raw();
    if tcsetattr(fd, OptionalActions::Now, &raw).is_err() {
        return None;
    }

    let mut out = Vec::new();
    let mut so = std::io::stdout();
    let _ = so.write_all(b"\x1b[16t");
    let _ = so.flush();

    let deadline = Instant::now() + Duration::from_millis(1000);
    let mut buf = [0u8; 256];
    loop {
        let remain = deadline.saturating_duration_since(Instant::now());
        if remain.is_zero() {
            break;
        }
        let timeout_ms = remain.as_millis().min(200) as i32;
        match poll(&mut [PollFd::new(&fd, PollFlags::IN)], timeout_ms) {
            Ok(0) => break,
            Ok(_) => match rustix::io::read(fd, &mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    out.extend_from_slice(&buf[..n]);
                    if out.contains(&b't') {
                        break;
                    }
                }
                Err(_) => break,
            },
            Err(_) => break,
        }
    }
    let _ = tcsetattr(fd, OptionalActions::Now, &orig);
    parse_cell_size(&out)
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