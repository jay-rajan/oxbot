//! The Mac's own tools oxbot uses on files: `textutil` (old Word, RTF,
//! `OpenDocument` text), `sips` (pictures) and oxbot's helper (still frames
//! from video). Each runs as its own process with a time limit and is
//! killed if it runs over; the system's tools get no environment but a
//! plain `PATH`.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use tokio::process::Command;

/// How long `textutil` and `sips` may take.
const TOOL_TIME: Duration = Duration::from_secs(20);
/// How long taking a video's frames may take.
const FRAMES_TIME: Duration = Duration::from_secs(60);

/// Where the Mac's tools are.
#[derive(Debug, Clone)]
pub struct MacTools {
    /// oxbot's Mac helper (`oxbot-mac`), if this oxbot has one.
    pub helper: Option<PathBuf>,
    /// `textutil`.
    pub textutil: PathBuf,
    /// `sips`.
    pub sips: PathBuf,
}

impl Default for MacTools {
    fn default() -> Self {
        Self::with_helper(None)
    }
}

/// What the helper made of a video.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Frames {
    /// Still frames written.
    pub frames: u32,
    /// How long the video is.
    pub duration_ms: u64,
}

impl MacTools {
    /// The system's tools, and the helper at `helper`.
    #[must_use]
    pub fn with_helper(helper: Option<PathBuf>) -> Self {
        Self {
            helper,
            textutil: PathBuf::from("/usr/bin/textutil"),
            sips: PathBuf::from("/usr/bin/sips"),
        }
    }

    /// Writes `from` (old Word, RTF, `OpenDocument` text, a web archive) to
    /// `to` as plain UTF-8 text.
    ///
    /// # Errors
    /// Says why, in a few words.
    pub async fn to_text(&self, from: &Path, to: &Path) -> Result<(), String> {
        if !self.textutil.exists() {
            return Err("this kind of file can only be read on a Mac".to_owned());
        }
        let out = run(
            Command::new(&self.textutil)
                .args(["-convert", "txt", "-encoding", "UTF-8", "-output"])
                .arg(to)
                .arg(from),
            TOOL_TIME,
        )
        .await?;
        if !out.status.success() || !to.exists() {
            return Err("it may be damaged".to_owned());
        }
        Ok(())
    }

    /// Writes a JPEG of the picture `from` to `to`, at most `max_px` on
    /// its longer side.
    ///
    /// # Errors
    /// Says why, in a few words.
    pub async fn to_jpeg(&self, from: &Path, to: &Path, max_px: u32) -> Result<(), String> {
        if !self.sips.exists() {
            return Err("pictures like this can only be converted on a Mac".to_owned());
        }
        let out = run(
            Command::new(&self.sips)
                .args(["-s", "format", "jpeg", "-Z", &max_px.to_string()])
                .arg(from)
                .arg("--out")
                .arg(to),
            TOOL_TIME,
        )
        .await?;
        if !out.status.success() || !to.exists() {
            return Err("it may be damaged".to_owned());
        }
        Ok(())
    }

    /// A picture's width and height, if `sips` can tell.
    pub async fn pixel_size(&self, path: &Path) -> Option<(u32, u32)> {
        if !self.sips.exists() {
            return None;
        }
        let out = run(
            Command::new(&self.sips)
                .args(["-g", "pixelWidth", "-g", "pixelHeight"])
                .arg(path),
            TOOL_TIME,
        )
        .await
        .ok()?;
        let text = String::from_utf8_lossy(&out.stdout);
        let value = |key: &str| {
            text.lines()
                .find_map(|l| l.trim().strip_prefix(key))
                .and_then(|v| v.trim().parse().ok())
        };
        Some((value("pixelWidth:")?, value("pixelHeight:")?))
    }

    /// Takes up to `max` evenly spaced still frames of the video `from`
    /// into the folder `to` (`01.jpg`, `02.jpg`…), at most `size` pixels on
    /// their longer side.
    ///
    /// # Errors
    /// Says why, for the person who sent it.
    pub async fn frames(
        &self,
        from: &Path,
        to: &Path,
        max: u32,
        size: u32,
    ) -> Result<Frames, String> {
        let Some(helper) = self.helper.as_ref().filter(|h| h.exists()) else {
            return Err("taking still frames from video isn't available on this Mac.".to_owned());
        };
        // oxbot's own program: it keeps oxbot's environment.
        let out = run_helper(
            Command::new(helper).arg("frames").arg(from).arg(to).args([
                "--max",
                &max.to_string(),
                "--size",
                &size.to_string(),
            ]),
            FRAMES_TIME,
        )
        .await?;
        let reply: serde_json::Value =
            serde_json::from_slice(out.stdout.trim_ascii()).unwrap_or_default();
        if let Some(why) = reply["error"].as_str() {
            return Err(why.to_owned());
        }
        let frames = reply["frames"]
            .as_u64()
            .and_then(|n| u32::try_from(n).ok())
            .filter(|n| *n > 0)
            .ok_or_else(|| "no still frames could be taken from it.".to_owned())?;
        Ok(Frames {
            frames: frames.min(max),
            duration_ms: reply["duration_ms"].as_u64().unwrap_or(0),
        })
    }
}

/// Runs a system tool with no input, a plain environment and a time limit.
async fn run(cmd: &mut Command, limit: Duration) -> Result<std::process::Output, String> {
    cmd.env_clear().env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin");
    if let Some(home) = std::env::var_os("HOME") {
        cmd.env("HOME", home);
    }
    run_helper(cmd, limit).await
}

/// Runs `cmd` with no input and a time limit.
async fn run_helper(cmd: &mut Command, limit: Duration) -> Result<std::process::Output, String> {
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let child = cmd.spawn().map_err(|e| format!("couldn't start it: {e}"))?;
    match tokio::time::timeout(limit, child.wait_with_output()).await {
        Ok(Ok(out)) => Ok(out),
        Ok(Err(e)) => Err(format!("it failed: {e}")),
        // Dropping the child (kill_on_drop) stops it.
        Err(_) => Err("it took too long".to_owned()),
    }
}
