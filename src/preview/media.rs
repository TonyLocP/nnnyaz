use std::{
    io::{self, Read},
    path::Path,
    process::{Command, Stdio},
};

const TEXT_CAP: u64 = 1024 * 1024;

/// Run a command and capture up to `cap` bytes of its stdout.
/// Returns the output and whether it was truncated.
fn capture(cmd: &mut Command, cap: u64) -> io::Result<(Vec<u8>, bool)> {
    let program = cmd.get_program().to_string_lossy().into_owned();
    let mut child = cmd.stdout(Stdio::piped()).stderr(Stdio::null()).spawn()?;
    let mut out = Vec::new();
    if let Some(so) = child.stdout.take() {
        so.take(cap).read_to_end(&mut out)?;
    }
    let truncated = out.len() as u64 >= cap;
    if truncated {
        let _ = child.kill();
    }
    let status = child.wait()?;
    if !truncated && !status.success() {
        return Err(io::Error::other(format!("{program} failed ({status})")));
    }
    Ok((out, truncated))
}

/// List an archive's contents with 7z.
pub fn archive_listing(path: &Path) -> io::Result<String> {
    let mut c = Command::new("7z");
    c.args(["l", "-ba"]).arg(path);
    let (data, _) = capture(&mut c, TEXT_CAP)?;
    Ok(String::from_utf8_lossy(&data).into_owned())
}

/// Pretty-print JSON with jq (later syntax-highlighted as JSON).
pub fn json_pretty(path: &Path) -> io::Result<String> {
    let mut c = Command::new("jq");
    c.arg(".").arg(path);
    let (data, _) = capture(&mut c, TEXT_CAP)?;
    Ok(String::from_utf8_lossy(&data).into_owned())
}

/// Number of pages in a PDF, via `pdfinfo`. `None` when the tool is missing or
/// the file cannot be parsed; callers then treat the page bound as unknown.
pub fn pdf_pages(path: &Path) -> Option<usize> {
    let mut c = Command::new("pdfinfo");
    c.arg(path).stdin(Stdio::null()).stderr(Stdio::null());
    let (data, _) = capture(&mut c, 64 * 1024).ok()?;
    parse_pdf_pages(&String::from_utf8_lossy(&data))
}

/// Pull the page count out of `pdfinfo` output.
fn parse_pdf_pages(text: &str) -> Option<usize> {
    text.lines()
        .find_map(|line| line.strip_prefix("Pages:"))
        .and_then(|n| n.trim().parse().ok())
}

/// Render page `page` (zero-based) of a PDF to `<prefix>.jpg` with pdftoppm,
/// the same backend yazi uses for its PDF previewer.
pub fn render_pdf_page(path: &Path, page: usize, prefix: &Path) -> io::Result<()> {
    let n = page.saturating_add(1).to_string();
    let mut c = Command::new("pdftoppm");
    c.args(["-f", &n, "-l", &n, "-singlefile", "-jpeg"])
        .args(["-jpegopt", "quality=75"])
        .arg(path)
        .arg(prefix)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    let out = c.output()?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(io::Error::other(format!(
            "pdftoppm: {}",
            err.lines().next().unwrap_or("failed")
        )));
    }
    Ok(())
}

/// Extract a representative video frame to `dst` (format inferred from the
/// extension). Tries the first frame, then the one-second mark for videos
/// whose opening frame is blank or undecodable.
pub fn extract_video_frame(path: &Path, dst: &Path) -> io::Result<()> {
    let attempt = |seek: &str| -> io::Result<()> {
        let mut c = Command::new("ffmpeg");
        c.args(["-v", "error", "-y", "-ss", seek, "-i"])
            .arg(path)
            .args([
                "-frames:v",
                "1",
                "-an",
                "-vf",
                "scale=min(1920\\,iw):min(1080\\,ih):force_original_aspect_ratio=decrease",
            ])
            .arg(dst)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        let out = c.output()?;
        let nonempty = dst.metadata().map(|m| m.len() > 0).unwrap_or(false);
        if out.status.success() && nonempty {
            Ok(())
        } else {
            let err = String::from_utf8_lossy(&out.stderr);
            Err(io::Error::other(format!(
                "ffmpeg: {}",
                err.lines().next().unwrap_or("failed")
            )))
        }
    };
    attempt("0").or_else(|_| attempt("1"))
}

#[cfg(test)]
mod tests {
    use super::parse_pdf_pages;

    #[test]
    fn parses_pdfinfo_page_count() {
        let out = "Title: x\nCreator: PDFium\nPages:           74\nEncrypted:       no\n";
        assert_eq!(parse_pdf_pages(out), Some(74));
        assert_eq!(parse_pdf_pages("Pages: 1\n"), Some(1));
        assert_eq!(parse_pdf_pages("Title: no pages here\n"), None);
        assert_eq!(parse_pdf_pages("Pages: many\n"), None);
    }
}
