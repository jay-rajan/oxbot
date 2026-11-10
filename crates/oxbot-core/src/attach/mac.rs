//! The Mac's own tools oxbot uses on files: `textutil` (old Word, RTF,
//! `OpenDocument` text), `sips` (pictures) and oxbot's helper (still frames
//! from video). Each runs as its own process with a time limit and is
//! killed if it runs over; the system's tools get no environment but a
//! plain `PATH`. The helper, which decodes video anyone may send, runs in
//! macOS's sandbox: it may read only that one video (and the system), write
//! only its frames folder, and has no network and can start no program.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use tokio::process::Command;

/// How long `textutil` and `sips` may take.
const TOOL_TIME: Duration = Duration::from_secs(20);
/// How long taking a video's frames may take.
const FRAMES_TIME: Duration = Duration::from_secs(60);
/// macOS's sandbox runner.
const SANDBOX_EXEC: &str = "/usr/bin/sandbox-exec";

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
            return Err(
                "taking still frames from video isn't available on the home computer.".to_owned(),
            );
        };
        if !Path::new(SANDBOX_EXEC).exists() {
            return Err("this Mac can't open video safely (its sandbox is missing).".to_owned());
        }
        // The sandbox matches real paths (`/private/var/…`, not `/var/…`).
        let real =
            |p: &Path| std::fs::canonicalize(p).map_err(|_| "it couldn't be opened.".to_owned());
        std::fs::create_dir_all(to)
            .map_err(|_| "the frames folder couldn't be made.".to_owned())?;
        let (helper, from, to) = (real(helper)?, real(from)?, real(to)?);
        // oxbot's own program: it keeps oxbot's environment.
        let out = run_helper(
            Command::new(SANDBOX_EXEC)
                .arg("-p")
                .arg(frames_profile(&helper, &from, &to))
                .arg(&helper)
                .arg("frames")
                .arg(&from)
                .arg(&to)
                .args(["--max", &max.to_string(), "--size", &size.to_string()]),
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

/// The sandbox profile for the helper (`helper`, its real path) taking
/// frames of `video` into `out`: it reads the system and that one video,
/// writes only `out`, may reach only the system's video decoder, and has
/// no network and no way to start another program.
fn frames_profile(helper: &Path, video: &Path, out: &Path) -> String {
    let own_folder = helper.parent().unwrap_or(helper);
    format!(
        "(version 1)\n\
         (deny default)\n\
         (allow process-exec (literal {helper}))\n\
         (allow sysctl-read)\n\
         (allow file-read-metadata)\n\
         (allow file-read* (literal \"/\") (subpath \"/System\") (subpath \"/usr/lib\") \
            (subpath \"/usr/share\") (subpath \"/Library/Apple\") (literal \"/dev/null\") \
            (literal \"/dev/urandom\") (literal \"/private/etc/localtime\") \
            (subpath \"/private/var/db/timezone\") (subpath {own}) (literal {video}))\n\
         (allow file-write* (subpath {out}))\n\
         (allow mach-lookup (global-name-prefix \"com.apple.coremedia.\") \
            (global-name \"com.apple.cfprefsd.daemon\" \"com.apple.cfprefsd.agent\" \
              \"com.apple.logd\" \"com.apple.system.opendirectoryd.libinfo\"))\n\
         (allow iokit-open (iokit-user-client-class \"IOSurfaceRootUserClient\" \
            \"AGXDeviceUserClient\" \"AppleAVDUserClient\"))\n\
         (allow ipc-posix-shm-read*)\n",
        helper = sbpl(helper),
        own = sbpl(own_folder),
        video = sbpl(video),
        out = sbpl(out),
    )
}

/// A path as an SBPL string.
fn sbpl(path: &Path) -> String {
    let text = path.display().to_string();
    format!("\"{}\"", text.replace('\\', "\\\\").replace('"', "\\\""))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_frames_profile_names_only_the_video_and_its_folder() {
        let profile = frames_profile(
            Path::new("/Applications/oxbot.app/Contents/MacOS/oxbot-mac"),
            Path::new("/b/work/inbox/a \"b\".mov"),
            Path::new("/b/work/inbox/a.mov.frames"),
        );
        assert!(profile.contains("(deny default)"));
        assert!(profile.contains("(allow file-write* (subpath \"/b/work/inbox/a.mov.frames\"))"));
        assert!(
            profile.contains("(literal \"/b/work/inbox/a \\\"b\\\".mov\")"),
            "{profile}"
        );
        assert!(!profile.contains("network"), "no network at all");
        assert_eq!(
            profile.matches("process-exec").count(),
            1,
            "only the helper itself"
        );
    }

    /// The helper built by `scripts/build-mac-helper.sh`, on a Mac with
    /// its sandbox; `None` (the test is skipped) otherwise.
    fn helper() -> Option<PathBuf> {
        let helper =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/mac-helper/oxbot-mac");
        (helper.exists() && Path::new(SANDBOX_EXEC).exists())
            .then(|| std::fs::canonicalize(helper).unwrap())
    }

    #[tokio::test]
    async fn frames_are_taken_inside_the_sandbox() {
        let Some(helper) = helper() else { return };
        let tmp = tempfile::tempdir().unwrap();
        let clip = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../e2e/fixtures/clip.mov");
        let out = tmp.path().join("clip.mov.frames");
        let taken = MacTools::with_helper(Some(helper))
            .frames(&clip, &out, 3, 256)
            .await
            .unwrap();
        assert_eq!(taken.frames, 3);
        assert!(out.join("01.jpg").is_file());
    }

    #[test]
    fn the_sandboxed_helper_cannot_write_outside_its_folder() {
        let Some(helper) = helper() else { return };
        let tmp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(tmp.path()).unwrap();
        let clip = std::fs::canonicalize(
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../e2e/fixtures/clip.mov"),
        )
        .unwrap();
        let (allowed, elsewhere) = (root.join("frames"), root.join("elsewhere"));
        std::fs::create_dir_all(&allowed).unwrap();
        let run = |into: &Path| {
            std::process::Command::new(SANDBOX_EXEC)
                .arg("-p")
                .arg(frames_profile(&helper, &clip, &allowed))
                .arg(&helper)
                .args(["frames".as_ref(), clip.as_os_str(), into.as_os_str()])
                .args(["--max", "2", "--size", "128"])
                .output()
                .unwrap()
        };
        let ok = run(&allowed);
        assert!(
            String::from_utf8_lossy(&ok.stdout).contains("\"frames\":2"),
            "{ok:?}"
        );
        let refused = run(&elsewhere);
        assert!(
            String::from_utf8_lossy(&refused.stdout).contains("error"),
            "{refused:?}"
        );
        assert!(
            !elsewhere.join("01.jpg").exists(),
            "nothing written outside its folder"
        );
        // Nor may it read another file in place of the video.
        let other = root.join("other.mov");
        std::fs::copy(&clip, &other).unwrap();
        let read = std::process::Command::new(SANDBOX_EXEC)
            .arg("-p")
            .arg(frames_profile(&helper, &clip, &allowed))
            .arg(&helper)
            .args(["frames".as_ref(), other.as_os_str(), allowed.as_os_str()])
            .output()
            .unwrap();
        assert!(
            String::from_utf8_lossy(&read.stdout).contains("error"),
            "{read:?}"
        );
    }
}
