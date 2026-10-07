use std::{
    fs,
    io::{self, Read},
    path::Path,
    process::{Command, Stdio},
};

use image::DynamicImage;

const MEDIA_CAP: u64 = 64 * 1024 * 1024;
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

/// Extract a still frame from a video with ffmpeg (first try at 1s in,
/// so seeking-heavy files still work, then a plain first frame).
pub fn video_frame(path: &Path) -> io::Result<DynamicImage> {
    let mut last = String::from("ffmpeg failed");
    for pre in [&["-v", "error", "-ss", "1"][..], &["-v", "error"][..]] {
        let mut cmd = Command::new("ffmpeg");
        cmd.args(pre)
            .arg("-i")
            .arg(path)
            .args(["-frames:v", "1", "-f", "image2pipe", "-vcodec", "png", "-"]);
        match capture(&mut cmd, MEDIA_CAP) {
            Ok((data, _)) if !data.is_empty() => match image::load_from_memory(&data) {
                Ok(img) => return Ok(img),
                Err(e) => last = format!("ffmpeg frame: {e}"),
            },
            Ok(_) => last = "ffmpeg: no frame".into(),
            Err(e) => last = e.to_string(),
        }
    }
    Err(io::Error::other(last))
}

/// Render the first page of a PDF to an image with pdftoppm.
pub fn pdf_page(path: &Path) -> io::Result<DynamicImage> {
    let tmp = std::env::temp_dir().join(format!("rnn-pdf-{}", std::process::id()));
    let status = Command::new("pdftoppm")
        .args(["-f", "1", "-l", "1", "-singlefile", "-scale-to", "1080", "-png"])
        .arg(path)
        .arg(&tmp)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?;
    let png = tmp.with_extension("png");
    if !status.success() {
        let _ = fs::remove_file(&png);
        return Err(io::Error::other(format!("pdftoppm failed ({status})")));
    }
    let data = fs::read(&png).map_err(|e| io::Error::other(format!("pdftoppm: {e}")))?;
    let _ = fs::remove_file(&png);
    image::load_from_memory(&data).map_err(|e| io::Error::other(format!("pdftoppm output: {e}")))
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

/// Decode an image format the `image` crate cannot handle (avif/heic/...)
/// by routing it through ImageMagick.
pub fn magick_image(path: &Path) -> io::Result<DynamicImage> {
    let mut c = Command::new("magick");
    c.arg(path).args(["-auto-orient", "png:-"]);
    let (data, _) = capture(&mut c, MEDIA_CAP)?;
    image::load_from_memory(&data).map_err(|e| io::Error::other(e.to_string()))
}
