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
