use std::{
    collections::HashSet,
    io::{self, Read},
    path::Path,
    process::{Command, Stdio},
};

use crate::util::human;

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

/// List an archive's contents with 7z, formatted as a tree with sizes.
///
/// Uses `-slt` (one `Key = Value` block per entry) rather than the fixed-width
/// bare listing so paths containing spaces parse correctly. Compressed tarballs
/// (`.tar.gz`, `.tgz`, …) appear to 7z as a single inner `.tar`; those are
/// decompressed through `7z e -so` and listed one level further, like Yazi.
pub fn archive_listing(path: &Path) -> io::Result<String> {
    let mut last: Option<io::Error> = None;
    for prog in ["7zz", "7z"] {
        match list_7z(prog, path) {
            Ok(entries) => {
                if is_inner_tar(&entries) {
                    if let Ok(inner) = list_7z_tar(prog, path) {
                        return Ok(format_archive(&inner));
                    }
                }
                return Ok(format_archive(&entries));
            }
            Err(e) => last = Some(e),
        }
    }
    Err(last.unwrap_or_else(|| io::Error::other("7z: not found")))
}

fn list_7z(prog: &str, path: &Path) -> io::Result<Vec<ArchiveEntry>> {
    let mut c = Command::new(prog);
    c.args(["l", "-ba", "-slt", "-sccUTF-8", "-xr!__MACOSX"])
        .arg(path);
    let (data, _) = capture(&mut c, TEXT_CAP)?;
    let text = String::from_utf8_lossy(&data);
    Ok(parse_7z_slt(&text))
}

/// True when the listing is just the `.tar` inside a compressed tar.
fn is_inner_tar(entries: &[ArchiveEntry]) -> bool {
    entries.len() == 1
        && !entries[0].is_dir
        && entries[0].path.to_ascii_lowercase().ends_with(".tar")
}

/// Decompress a compressed tar through `7z e -so` and list the tar it contains.
fn list_7z_tar(prog: &str, path: &Path) -> io::Result<Vec<ArchiveEntry>> {
    let mut decomp = Command::new(prog);
    decomp
        .args(["e", "-so"])
        .arg(path)
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut decomp = decomp.spawn()?;
    let Some(stdout) = decomp.stdout.take() else {
        let _ = decomp.kill();
        let _ = decomp.wait();
        return Err(io::Error::other("7z: no stdout"));
    };

    let mut list = Command::new(prog);
    list.args(["l", "-ba", "-slt", "-sccUTF-8", "-ttar", "-si"])
        .stdin(Stdio::from(stdout));
    let result = capture(&mut list, TEXT_CAP);
    let _ = decomp.wait();

    let (data, _) = result?;
    let text = String::from_utf8_lossy(&data);
    Ok(parse_7z_slt(&text))
}

#[derive(Debug, PartialEq)]
struct ArchiveEntry {
    path: String,
    size: u64,
    is_dir: bool,
}

/// Parse the blank-line-separated blocks produced by `7z l -ba -slt`.
fn parse_7z_slt(text: &str) -> Vec<ArchiveEntry> {
    fn flush(path: &mut Option<String>, size: u64, is_dir: bool, out: &mut Vec<ArchiveEntry>) {
        if let Some(p) = path.take() {
            if !p.is_empty() {
                out.push(ArchiveEntry {
                    path: p,
                    size,
                    is_dir,
                });
            }
        }
    }

    let mut out = Vec::new();
    let mut path: Option<String> = None;
    let mut size = 0;
    let mut is_dir = false;
    for raw in text.lines() {
        let line = raw.trim_end_matches('\r');
        if line.trim().is_empty() {
            flush(&mut path, size, is_dir, &mut out);
            size = 0;
            is_dir = false;
        } else if let Some(v) = line.strip_prefix("Path = ") {
            path = Some(v.to_string());
        } else if let Some(v) = line.strip_prefix("Size = ") {
            size = v.trim().parse().unwrap_or(0);
        } else if let Some(v) = line.strip_prefix("Folder = ") {
            is_dir = v.trim() == "+";
        } else if let Some(v) = line.strip_prefix("Attributes = ") {
            // Older 7z builds omit `Folder`; the first attribute is `D` for
            // directories.
            if v.trim_start().starts_with('D') {
                is_dir = true;
            }
        }
    }
    flush(&mut path, size, is_dir, &mut out);
    out
}

/// Render entries as an indented tree, synthesizing parent directories that the
/// archive does not store explicitly (Yazi does the same).
fn format_archive(entries: &[ArchiveEntry]) -> String {
    let mut out = String::new();
    let mut seen: HashSet<String> = HashSet::new();
    for e in entries {
        let path = e.path.replace('\\', "/");
        let path = path.trim_start_matches("./").trim_end_matches('/');
        if path.is_empty() || path == "__MACOSX" || path.starts_with("__MACOSX/") {
            continue;
        }
        let parts: Vec<&str> = path.split('/').filter(|p| !p.is_empty()).collect();
        if parts.is_empty() {
            continue;
        }
        for i in 1..parts.len() {
            let dir = parts[..i].join("/");
            if seen.insert(dir) {
                push_archive_line(&mut out, i - 1, parts[i - 1], 0, true);
            }
        }
        let depth = parts.len() - 1;
        let name = parts[depth];
        if e.is_dir {
            if seen.insert(path.to_string()) {
                push_archive_line(&mut out, depth, name, e.size, true);
            }
        } else {
            push_archive_line(&mut out, depth, name, e.size, false);
        }
    }
    out
}

fn push_archive_line(out: &mut String, depth: usize, name: &str, size: u64, is_dir: bool) {
    for _ in 0..depth {
        out.push_str("  ");
    }
    out.push_str(name);
    if is_dir {
        out.push('/');
    } else if size > 0 {
        out.push_str("  ");
        out.push_str(&human(size));
    }
    out.push('\n');
}

/// Glyph sample used by Yazi's font previewer.
const FONT_SAMPLE: &str =
    "ABCDEFGHIJKLM\nNOPQRSTUVWXYZ\nabcdefghijklm\nnopqrstuvwxyz\n1234567890\n!$&*()[]{}";

/// Render a font's glyphs to a JPEG with ImageMagick, mirroring Yazi's font
/// previewer. Tries `magick` (ImageMagick 7) then `convert` (ImageMagick 6).
pub fn render_font_preview(path: &Path, dst: &Path) -> io::Result<()> {
    let mut last: Option<io::Error> = None;
    for prog in ["magick", "convert"] {
        let mut c = Command::new(prog);
        c.args(["-size", "800x560", "-gravity", "center", "-font"])
            .arg(path)
            .args([
                "-pointsize",
                "64",
                "xc:white",
                "-fill",
                "black",
                "-annotate",
                "+0+0",
                FONT_SAMPLE,
            ])
            .arg(format!("JPG:{}", dst.display()))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        match c.output() {
            Ok(out) if out.status.success() => return Ok(()),
            Ok(out) => {
                let err = String::from_utf8_lossy(&out.stderr);
                last = Some(io::Error::other(format!(
                    "{prog}: {}",
                    err.lines().next().unwrap_or("failed")
                )));
            }
            Err(e) => last = Some(e),
        }
    }
    Err(last.unwrap_or_else(|| io::Error::other("magick: not found")))
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
    use super::{format_archive, parse_7z_slt, parse_pdf_pages};

    #[test]
    fn parses_pdfinfo_page_count() {
        let out = "Title: x\nCreator: PDFium\nPages:           74\nEncrypted:       no\n";
        assert_eq!(parse_pdf_pages(out), Some(74));
        assert_eq!(parse_pdf_pages("Pages: 1\n"), Some(1));
        assert_eq!(parse_pdf_pages("Title: no pages here\n"), None);
        assert_eq!(parse_pdf_pages("Pages: many\n"), None);
    }

    #[test]
    fn archive_slt_parses_blocks() {
        let text = "Path = a.txt\r\nSize = 6\r\nAttributes =  -rw-r--r--\r\n\r\n\
                    Path = sub\r\nFolder = +\r\nSize = 0\r\n\r\n\
                    Path = sub/b.txt\r\nSize = 1\r\nAttributes =  -rw-r--r--\r\n";
        let entries = parse_7z_slt(text);
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].path, "a.txt");
        assert_eq!(entries[0].size, 6);
        assert!(!entries[0].is_dir);
        assert_eq!(entries[1].path, "sub");
        assert!(entries[1].is_dir);
        assert_eq!(entries[2].path, "sub/b.txt");
    }

    #[test]
    fn archive_attributes_mark_directories() {
        // Some 7z builds omit `Folder`; the leading D in Attributes still
        // identifies a directory.
        let text = "Path = dir\nSize = 0\nAttributes = D drwxr-xr-x\n";
        let entries = parse_7z_slt(text);
        assert!(entries[0].is_dir);
    }

    #[test]
    fn archive_formats_as_tree_with_sizes() {
        let text = "Path = a.txt\nSize = 6\n\nPath = sub\nFolder = +\nSize = 0\n\n\
                    Path = sub/b.txt\nSize = 1024\n\nPath = __MACOSX/junk\nSize = 2\n";
        let out = format_archive(&parse_7z_slt(text));
        assert_eq!(out, "a.txt  6B\nsub/\n  b.txt  1.0K\n");
    }

    #[test]
    fn archive_synthesizes_missing_parents() {
        let text = "Path = x/y/z.txt\nSize = 3\n";
        let out = format_archive(&parse_7z_slt(text));
        assert_eq!(out, "x/\n  y/\n    z.txt  3B\n");
    }

    #[test]
    fn archive_listing_formats_a_real_archive() {
        use std::process::Command;

        let dir = std::env::temp_dir().join(format!(
            "nnnyaz-archive-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("a.txt"), b"hello\n").unwrap();
        std::fs::write(dir.join("sub/b.txt"), b"x").unwrap();

        let zip = dir.join("test.zip");
        let created = Command::new("7z")
            .args(["a", "-bd"])
            .arg(&zip)
            .arg("a.txt")
            .arg("sub")
            .current_dir(&dir)
            .output();
        let Ok(created) = created else {
            let _ = std::fs::remove_dir_all(&dir);
            return; // 7z not installed; unit tests cover the parser
        };
        if !created.status.success() {
            let _ = std::fs::remove_dir_all(&dir);
            return;
        }

        let text = super::archive_listing(&zip).unwrap();
        assert!(text.contains("a.txt  6B"), "{text}");
        assert!(text.contains("sub/"), "{text}");
        assert!(text.contains("  b.txt  1B"), "{text}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn archive_listing_descends_into_compressed_tar() {
        use std::process::Command;

        let dir = std::env::temp_dir().join(format!(
            "nnnyaz-targz-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("a.txt"), b"hello\n").unwrap();
        std::fs::write(dir.join("sub/b.txt"), b"x").unwrap();

        let tar = dir.join("test.tar");
        let created = Command::new("7z")
            .args(["a", "-bd", "-ttar"])
            .arg(&tar)
            .arg("a.txt")
            .arg("sub")
            .current_dir(&dir)
            .output();
        let Ok(created) = created else {
            let _ = std::fs::remove_dir_all(&dir);
            return; // 7z not installed; unit tests cover the parser
        };
        if !created.status.success() {
            let _ = std::fs::remove_dir_all(&dir);
            return;
        }

        let gz = dir.join("test.tar.gz");
        let compressed = Command::new("7z")
            .args(["a", "-bd", "-tgzip"])
            .arg(&gz)
            .arg("test.tar")
            .current_dir(&dir)
            .output();
        let Ok(compressed) = compressed else {
            let _ = std::fs::remove_dir_all(&dir);
            return;
        };
        if !compressed.status.success() {
            let _ = std::fs::remove_dir_all(&dir);
            return;
        }

        let text = super::archive_listing(&gz).unwrap();
        assert!(text.contains("a.txt  6B"), "{text}");
        assert!(text.contains("sub/"), "{text}");
        assert!(text.contains("  b.txt  1B"), "{text}");
        assert!(!text.contains("test.tar"), "{text}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
