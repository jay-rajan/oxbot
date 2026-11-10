//! `run_program`: a small script a bot writes (sh, Python 3 or Node.js),
//! run by oxbot itself, never by the model's engine (issue #24). The
//! person says yes first (the gate's approval request, or the bot's Always
//! rule). Then it runs under macOS's `sandbox-exec` with a profile made
//! for this bot that denies everything not listed:
//!
//! - it may read only the bot's `work/` folder (its run folder, made fresh
//!   inside it and removed after, included) and what the interpreter
//!   itself needs (the system's libraries and tools, the interpreter's own
//!   folder, a few devices);
//! - it may write only in the bot's folder;
//! - no network at all, loopback included; no other programs but the
//!   interpreter and the system's tools in `/bin` and `/usr/bin`; no
//!   system services;
//! - 30 seconds (the whole process group is killed), 30 seconds of CPU,
//!   at most 64 more processes, 1 GB of memory (watched), 256 KB of
//!   output;
//! - a minimal environment: `PATH`, `HOME` and `TMPDIR` (its run folder),
//!   `LANG`.
//!
//! The pure parts (reading the call, finding interpreters, the profile,
//! the report) are unit tested; the run itself is tested on this Mac.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use oxbot_llm::ToolSpec;
use serde_json::{Value, json};
use tokio::io::AsyncReadExt as _;
use tokio_util::sync::CancellationToken;

/// The session environment's name for oxbot's own Node.js, which
/// `run_program` uses first.
pub const NODE_ENV: &str = "OXBOT_PROGRAM_NODE";
/// macOS's sandbox runner.
pub const SANDBOX_EXEC: &str = "/usr/bin/sandbox-exec";
/// The longest a program may run.
pub const WALL_LIMIT: Duration = Duration::from_secs(30);
/// Its CPU seconds.
const CPU_SECONDS: u32 = 30;
/// Processes it may add to the person's own.
const MORE_PROCESSES: usize = 64;
/// The most memory its processes may hold together, in KB.
const MEMORY_KB: u64 = 1024 * 1024;
/// The most output kept (stdout and stderr together).
pub const MAX_OUTPUT: usize = 256 * 1024;
/// The longest program oxbot takes.
const MAX_PROGRAM: usize = 100_000;
/// The most input files.
const MAX_FILES: usize = 20;
/// Where Apple's Command Line Tools and Xcode keep Python 3.
const PYTHONS: [&str; 2] = [
    "/Library/Developer/CommandLineTools/usr/bin/python3",
    "/Applications/Xcode.app/Contents/Developer/usr/bin/python3",
];

/// The system paths every interpreter may read (libraries, tools, the
/// shared cache, time zones, a few devices).
pub const SYSTEM_READ: [&str; 13] = [
    "/usr/lib",
    "/usr/bin",
    "/bin",
    "/usr/share",
    "/System",
    "/Library/Apple",
    "/private/var/db/dyld",
    "/private/var/db/timezone",
    "/private/etc/localtime",
    "/dev/null",
    "/dev/urandom",
    "/dev/random",
    "/dev/zero",
];

/// `run_program` as the model sees it.
#[must_use]
pub fn spec() -> ToolSpec {
    ToolSpec {
        name: super::RUN_PROGRAM.to_owned(),
        description: "Run a small script on the home computer: totals, CSV and spreadsheet \
            work, comparisons, reformatting or editing files in your folder. The person sees \
            the program and says yes first. It runs in your folder (the working directory) and \
            can only read and write files there: no internet, nothing else on the computer, \
            at most 30 seconds and 256 KB of output. language: sh, python3 or node. program: \
            the whole program. files: files in your folder to pass as arguments (sys.argv[1:] \
            / process.argv.slice(2) / \"$@\"). You get its output and exit code. Example: \
            language: python3, program: import csv,sys\\nrows = list(csv.DictReader(open(sys.argv[1])))\\n\
            print(sum(float(r['amount']) for r in rows)), files: [\"inbox/2026-11-06/bills.csv\"]"
            .to_owned(),
        parameters: json!({
            "type": "object",
            "properties": {
                "language": {"type": "string", "enum": ["sh", "python3", "node"], "description": "The interpreter."},
                "program": {"type": "string", "description": "The whole program."},
                "files": {"type": "array", "items": {"type": "string"}, "description": "Files in your folder to pass as arguments, as paths in it."}
            },
            "required": ["language", "program"]
        }),
    }
}

/// A language oxbot runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Language {
    /// `/bin/sh`.
    Sh,
    /// Python 3 from Apple's developer tools.
    Python3,
    /// Node.js (oxbot's own, bundled with the browser tool).
    Node,
}

impl Language {
    /// Its name, as the model gives it.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Sh => "sh",
            Self::Python3 => "python3",
            Self::Node => "node",
        }
    }

    fn extension(self) -> &'static str {
        match self {
            Self::Sh => "sh",
            Self::Python3 => "py",
            Self::Node => "js",
        }
    }
}

/// A call, read and checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    /// The language.
    pub language: Language,
    /// The program.
    pub program: String,
    /// Input files, as given (relative to the folder).
    pub files: Vec<String>,
}

/// Reads `run_program`'s arguments; every file must be in `work`.
///
/// # Errors
/// What's wrong, for the model.
pub fn parse(args: &Value, work: &Path) -> Result<Request, String> {
    let language = match args.get("language").and_then(Value::as_str).map(str::trim) {
        Some("sh" | "bash" | "shell") => Language::Sh,
        Some("python3" | "python" | "py") => Language::Python3,
        Some("node" | "javascript" | "js") => Language::Node,
        Some(other) => {
            return Err(format!(
                "\"{other}\" isn't a language oxbot runs: use sh, python3 or node"
            ));
        }
        None => return Err("language is needed: sh, python3 or node".to_owned()),
    };
    let program = args
        .get("program")
        .and_then(Value::as_str)
        .filter(|p| !p.trim().is_empty())
        .ok_or("program is needed: the whole program")?;
    if program.len() > MAX_PROGRAM {
        return Err(format!(
            "the program is too long: at most {MAX_PROGRAM} bytes"
        ));
    }
    let files: Vec<String> = match args.get("files") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(list)) => list
            .iter()
            .map(|f| {
                f.as_str()
                    .map(|f| f.trim().to_owned())
                    .filter(|f| !f.is_empty())
                    .ok_or_else(|| "files must be a list of paths in your folder".to_owned())
            })
            .collect::<Result<_, _>>()?,
        Some(_) => return Err("files must be a list of paths in your folder".to_owned()),
    };
    if files.len() > MAX_FILES {
        return Err(format!("at most {MAX_FILES} files"));
    }
    if let Some(bad) = files.iter().find(|f| !inside(work, f)) {
        return Err(format!("{bad} isn't in your folder"));
    }
    Ok(Request {
        language,
        program: program.to_owned(),
        files,
    })
}

/// Whether `given` (relative to `work`, or absolute) is in `work`, as
/// written and after following links (a file that doesn't exist yet
/// counts by its folder).
fn inside(work: &Path, given: &str) -> bool {
    let path = Path::new(given);
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        work.join(path)
    };
    if path
        .components()
        .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return false;
    }
    let real_work = work.canonicalize().unwrap_or_else(|_| work.to_path_buf());
    let real = path
        .canonicalize()
        .or_else(|_| {
            path.parent()
                .map_or_else(|| Ok(path.clone()), Path::canonicalize)
        })
        .unwrap_or(path);
    real.starts_with(&real_work)
}

/// The card's text for a call: the language, the program and its files.
#[must_use]
pub fn card_text(args: &Value) -> String {
    let language = args.get("language").and_then(Value::as_str).unwrap_or("?");
    let program = args.get("program").and_then(Value::as_str).unwrap_or("");
    let files: Vec<&str> = args
        .get("files")
        .and_then(Value::as_array)
        .map(|l| l.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let mut out = format!("{language}:\n{}", program.trim_end());
    if !files.is_empty() {
        let _ = write!(out, "\n\nFiles: {}", files.join(", "));
    }
    out
}

/// An interpreter, as the sandbox lets it run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Interpreter {
    /// The program to start (its real path).
    pub exec: PathBuf,
    /// Its own folder, which it may read and run from (`None` for one in
    /// the system's folders).
    pub home: Option<PathBuf>,
}

/// The interpreter for `language`: `/bin/sh`; Python 3 from Apple's
/// developer tools (never the `/usr/bin/python3` stand-in, which may offer
/// to install them); Node.js on `path` (oxbot's own tools put the bundled
/// one there). `None` if it isn't on this Mac.
#[must_use]
pub fn interpreter(language: Language, path: &str) -> Option<Interpreter> {
    let found = match language {
        Language::Sh => {
            return Some(Interpreter {
                exec: PathBuf::from("/bin/sh"),
                home: None,
            });
        }
        Language::Python3 => PYTHONS.iter().map(PathBuf::from).find(|p| p.is_file())?,
        Language::Node => path
            .split(':')
            .filter(|d| d.starts_with('/'))
            .map(|d| Path::new(d).join("node"))
            .find(|p| p.is_file())?,
    };
    let exec = found.canonicalize().ok()?;
    // `…/bin/node` → `…`; `…/Versions/3.9/bin/python3.9` → `…/Versions/3.9`.
    let home = exec.parent().and_then(Path::parent).map(Path::to_path_buf);
    Some(Interpreter { exec, home })
}

/// Where Homebrew installs: Apple silicon, then Intel Macs.
const HOMEBREW: [&str; 2] = ["/opt/homebrew", "/usr/local"];

/// A path as an SBPL string.
fn sbpl(path: &Path) -> String {
    let text = path.display().to_string();
    format!("\"{}\"", text.replace('\\', "\\\\").replace('"', "\\\""))
}

/// The sandbox profile for a program of the bot whose folder is `work`
/// (its real path), run by `interp`.
#[must_use]
pub fn profile(work: &Path, interp: &Interpreter) -> String {
    let mut read = vec!["(literal \"/\")".to_owned()];
    for p in SYSTEM_READ {
        let kind = if p.starts_with("/dev/") || p.ends_with("localtime") {
            "literal"
        } else {
            "subpath"
        };
        read.push(format!("({kind} {})", sbpl(Path::new(p))));
    }
    let mut exec = vec![
        "(subpath \"/bin\")".to_owned(),
        "(subpath \"/usr/bin\")".to_owned(),
        format!("(literal {})", sbpl(&interp.exec)),
    ];
    if let Some(home) = &interp.home {
        read.push(format!("(subpath {})", sbpl(home)));
        exec.push(format!("(subpath {})", sbpl(home)));
    }
    // A Homebrew interpreter loads its libraries from other Homebrew
    // packages (node needs llhttp, icu4c…): read-only, and they hold no
    // one's data.
    for prefix in HOMEBREW {
        if interp.exec.starts_with(prefix) {
            // …and node reads OpenSSL's settings file at start.
            for dir in ["lib", "opt", "Cellar", "etc/openssl@3"] {
                read.push(format!("(subpath {})", sbpl(&Path::new(prefix).join(dir))));
            }
        }
    }
    read.push(format!("(subpath {})", sbpl(work)));
    format!(
        "(version 1)\n\
         (deny default)\n\
         (allow process-fork)\n\
         (allow signal (target same-sandbox))\n\
         (allow sysctl-read)\n\
         (allow file-read-metadata)\n\
         (allow file-read* {})\n\
         (allow file-write* (subpath {}) (literal \"/dev/null\"))\n\
         (deny file-write-create (vnode-type SYMLINK))\n\
         (allow process-exec {})\n",
        read.join(" "),
        sbpl(work),
        exec.join(" "),
    )
}

/// How a run ended.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Ran {
    /// Its exit code (`None` if a signal ended it).
    pub code: Option<i32>,
    /// What it printed (stdout, then stderr), at most [`MAX_OUTPUT`].
    pub output: String,
    /// Why oxbot stopped it, if it did.
    pub stopped: Option<String>,
    /// The output was longer and was cut.
    pub cut: bool,
}

/// What the model is told.
#[must_use]
pub fn report(language: Language, ran: &Ran) -> String {
    let mut out = match (&ran.stopped, ran.code) {
        (Some(why), _) => format!("The {} program was stopped: {why}.\n", language.as_str()),
        (None, Some(code)) => format!(
            "The {} program exited with code {code}.\n",
            language.as_str()
        ),
        (None, None) => format!("The {} program was ended by a signal.\n", language.as_str()),
    };
    if ran.output.trim().is_empty() {
        out.push_str("It printed nothing.");
    } else {
        out.push_str("Its output:\n");
        out.push_str(ran.output.trim_end());
        if ran.cut {
            let _ = write!(out, "\n[cut at {} KB]", MAX_OUTPUT / 1024);
        }
    }
    out
}

/// The shell line that sets the limits, then runs the program.
fn limits_line(processes: usize) -> String {
    format!("ulimit -t {CPU_SECONDS}; ulimit -u {processes}; exec \"$@\"")
}

/// How many processes the folder's owner runs now (the process cap counts
/// all of them).
async fn own_processes(work: &Path) -> usize {
    use std::os::unix::fs::MetadataExt as _;
    let Ok(uid) = std::fs::metadata(work).map(|m| m.uid()) else {
        return 1_000;
    };
    tokio::process::Command::new("/bin/ps")
        .args(["-o", "pid=", "-U", &uid.to_string()])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .await
        .map_or(1_000, |o| {
            String::from_utf8_lossy(&o.stdout).lines().count()
        })
}

/// The memory a process group holds, in KB.
async fn group_memory(pgid: u32) -> u64 {
    tokio::process::Command::new("/bin/ps")
        .args(["-o", "rss=", "-g", &pgid.to_string()])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .await
        .map_or(0, |o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .filter_map(|l| l.trim().parse::<u64>().ok())
                .sum()
        })
}

/// Kills process group `pgid`.
async fn kill_group(pgid: u32) {
    let _ = tokio::process::Command::new("/bin/kill")
        .args(["-9", &format!("-{pgid}")])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await;
}

/// The whole tool: reads the call, runs it, and says how it went (as
/// data: what the program printed may come from any file in the folder).
pub async fn run_tool(
    call: &oxbot_llm::ToolCall,
    work: &Path,
    path: &str,
    cancel: &CancellationToken,
) -> super::ToolOutcome {
    let req = match parse(&call.arguments, work) {
        Ok(req) => req,
        Err(why) => return super::ToolOutcome::note(format!("[tool error] {why}"), true),
    };
    match Box::pin(run(&req, work, path, cancel)).await {
        Ok(ran) => {
            let text = report(req.language, &ran);
            let mut outcome = super::ToolOutcome::note(text.clone(), false);
            outcome.for_model = vec![oxbot_llm::Part::Text(oxbot_engine::text::data_block(
                "program_output",
                &format!(" language=\"{}\"", req.language.as_str()),
                &text,
            ))];
            outcome.meta.tool = Some(call.name.clone());
            outcome
        }
        Err(why) => super::ToolOutcome::note(format!("[tool error] {why}"), true),
    }
}

/// Runs `req` in the bot folder `work`, with Node.js found on `path`.
///
/// # Errors
/// It couldn't start (no sandbox, no interpreter, the folder).
pub async fn run(
    req: &Request,
    work: &Path,
    path: &str,
    cancel: &CancellationToken,
) -> Result<Ran, String> {
    if !Path::new(SANDBOX_EXEC).is_file() {
        return Err("this Mac can't run small scripts safely (its sandbox is missing)".to_owned());
    }
    let interp = interpreter(req.language, path).ok_or_else(|| match req.language {
        Language::Python3 => {
            "Python 3 isn't on the home computer (it comes with Apple's Command Line Tools)"
                .to_owned()
        }
        other => format!("{} isn't on the home computer", other.as_str()),
    })?;
    std::fs::create_dir_all(work).map_err(|e| format!("the folder isn't there: {e}"))?;
    let work = work
        .canonicalize()
        .map_err(|e| format!("the folder isn't there: {e}"))?;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let dir = work.join(format!(".oxbot-run-{}-{stamp}", std::process::id()));
    std::fs::create_dir(&dir).map_err(|e| format!("couldn't make its run folder: {e}"))?;
    let file = dir.join(format!("program.{}", req.language.extension()));
    let result = match std::fs::write(&file, &req.program) {
        Ok(()) => Box::pin(spawn_and_wait(req, &work, &dir, &file, &interp, cancel)).await,
        Err(e) => Err(format!("couldn't save the program: {e}")),
    };
    let _ = std::fs::remove_dir_all(&dir);
    result
}

/// The sandboxed command for `req`: `sandbox-exec` with the bot's
/// profile, a shell that sets the limits, then the interpreter.
fn command(
    req: &Request,
    work: &Path,
    dir: &Path,
    file: &Path,
    interp: &Interpreter,
    processes: usize,
) -> tokio::process::Command {
    let mut path = "/usr/bin:/bin".to_owned();
    if req.language == Language::Node
        && let Some(bin) = interp.exec.parent()
    {
        path = format!("{}:{path}", bin.display());
    }
    let mut cmd = tokio::process::Command::new(SANDBOX_EXEC);
    cmd.arg("-p")
        .arg(profile(work, interp))
        .args(["/bin/sh", "-c", &limits_line(processes), "sh"])
        .arg(&interp.exec)
        .arg(file)
        .args(&req.files)
        .env_clear()
        .env("PATH", path)
        .env("HOME", dir)
        .env("TMPDIR", dir)
        .env("LANG", "en_US.UTF-8")
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .current_dir(work)
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    cmd
}

async fn spawn_and_wait(
    req: &Request,
    work: &Path,
    dir: &Path,
    file: &Path,
    interp: &Interpreter,
    cancel: &CancellationToken,
) -> Result<Ran, String> {
    let processes = own_processes(work).await + MORE_PROCESSES;
    let mut cmd = command(req, work, dir, file, interp, processes);
    let mut child = cmd.spawn().map_err(|e| format!("couldn't start it: {e}"))?;
    let pgid = child.id().unwrap_or(0);
    let (Some(mut stdout), Some(mut stderr)) = (child.stdout.take(), child.stderr.take()) else {
        kill_group(pgid).await;
        return Err("couldn't read its output".to_owned());
    };
    let mut out = Vec::new();
    let mut err = Vec::new();
    let (mut out_done, mut err_done) = (false, false);
    let mut stopped: Option<String> = None;
    let mut cut = false;
    let deadline = tokio::time::sleep(WALL_LIMIT);
    tokio::pin!(deadline);
    let mut watch = tokio::time::interval(Duration::from_millis(250));
    let mut buf_out = vec![0u8; 8192];
    let mut buf_err = vec![0u8; 8192];
    while !(out_done && err_done) {
        tokio::select! {
            n = stdout.read(&mut buf_out), if !out_done => match n {
                Ok(0) | Err(_) => out_done = true,
                Ok(n) => out.extend_from_slice(&buf_out[..n]),
            },
            n = stderr.read(&mut buf_err), if !err_done => match n {
                Ok(0) | Err(_) => err_done = true,
                Ok(n) => err.extend_from_slice(&buf_err[..n]),
            },
            () = &mut deadline => {
                stopped = Some(format!("it ran for {} seconds", WALL_LIMIT.as_secs()));
                break;
            }
            () = cancel.cancelled() => {
                stopped = Some("the task was stopped".to_owned());
                break;
            }
            _ = watch.tick() => {
                if group_memory(pgid).await > MEMORY_KB {
                    stopped = Some(format!("it used more than {} MB of memory", MEMORY_KB / 1024));
                    break;
                }
            }
        }
        if out.len() + err.len() > MAX_OUTPUT {
            cut = true;
            stopped = Some(format!("it printed more than {} KB", MAX_OUTPUT / 1024));
            break;
        }
    }
    if stopped.is_some() {
        kill_group(pgid).await;
    }
    let status = tokio::time::timeout(Duration::from_secs(5), child.wait())
        .await
        .ok()
        .and_then(Result::ok);
    // Anything it left running goes too.
    kill_group(pgid).await;
    let mut output = String::from_utf8_lossy(&out).into_owned();
    if !err.is_empty() {
        if !output.is_empty() && !output.ends_with('\n') {
            output.push('\n');
        }
        output.push_str(&String::from_utf8_lossy(&err));
    }
    if output.len() > MAX_OUTPUT {
        let mut end = MAX_OUTPUT;
        while !output.is_char_boundary(end) {
            end -= 1;
        }
        output.truncate(end);
        cut = true;
    }
    Ok(Ran {
        code: status.and_then(|s| s.code()),
        output,
        stopped,
        cut,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn work() -> (tempfile::TempDir, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let work = tmp.path().join("bots/a/work");
        std::fs::create_dir_all(&work).unwrap();
        let work = work.canonicalize().unwrap();
        (tmp, work)
    }

    #[test]
    fn calls_are_read_and_files_stay_in_the_folder() {
        let (_tmp, w) = work();
        std::fs::write(w.join("a.csv"), "x").unwrap();
        let r = parse(
            &json!({"language": "python", "program": "print(1)", "files": ["a.csv"]}),
            &w,
        )
        .unwrap();
        assert_eq!(r.language, Language::Python3);
        assert_eq!(r.files, ["a.csv"]);
        let e = |args: Value| parse(&args, &w).unwrap_err();
        assert!(e(json!({"program": "x"})).contains("language is needed"));
        assert!(e(json!({"language": "ruby", "program": "x"})).contains("isn't a language"));
        assert!(e(json!({"language": "sh", "program": "  "})).contains("program is needed"));
        assert!(
            e(json!({"language": "sh", "program": "x", "files": ["../../b/work/s.txt"]}))
                .contains("isn't in your folder")
        );
        assert!(
            e(json!({"language": "sh", "program": "x", "files": ["/etc/passwd"]}))
                .contains("isn't in your folder")
        );
        assert!(e(json!({"language": "sh", "program": "x", "files": "a.csv"})).contains("list"));
        // A link out of the folder doesn't count as in it.
        std::os::unix::fs::symlink("/etc", w.join("etc")).unwrap();
        assert!(
            e(json!({"language": "sh", "program": "x", "files": ["etc/hosts"]}))
                .contains("isn't in your folder")
        );
    }

    #[test]
    fn a_homebrew_interpreter_may_read_homebrew_libraries_only() {
        let (_tmp, w) = work();
        let node = Interpreter {
            exec: PathBuf::from("/opt/homebrew/Cellar/node/26.7.0/bin/node"),
            home: Some(PathBuf::from("/opt/homebrew/Cellar/node/26.7.0")),
        };
        let p = profile(&w, &node);
        let read = p
            .lines()
            .find(|l| l.starts_with("(allow file-read* "))
            .unwrap();
        for want in [
            "(subpath \"/opt/homebrew/lib\")",
            "(subpath \"/opt/homebrew/opt\")",
            "(subpath \"/opt/homebrew/Cellar\")",
        ] {
            assert!(read.contains(want), "{want}");
        }
        let write = p
            .lines()
            .find(|l| l.starts_with("(allow file-write* "))
            .unwrap();
        assert!(!write.contains("homebrew"), "never writable");
        let exec = p
            .lines()
            .find(|l| l.starts_with("(allow process-exec "))
            .unwrap();
        assert!(
            !exec.contains("/opt/homebrew/opt"),
            "libraries are read, not run"
        );
    }

    #[test]
    fn the_profile_denies_by_default_and_lists_what_it_allows() {
        let (_tmp, w) = work();
        let node = Interpreter {
            exec: PathBuf::from("/opt/oxbot/tools/node/bin/node"),
            home: Some(PathBuf::from("/opt/oxbot/tools/node")),
        };
        let p = profile(&w, &node);
        assert!(p.starts_with("(version 1)\n(deny default)\n"));
        // Reading: the system, the interpreter's folder, the bot's folder.
        let read = p
            .lines()
            .find(|l| l.starts_with("(allow file-read* "))
            .unwrap();
        for want in [
            "(subpath \"/usr/lib\")",
            "(subpath \"/System\")",
            "(literal \"/dev/urandom\")",
            "(subpath \"/opt/oxbot/tools/node\")",
        ] {
            assert!(read.contains(want), "{want}");
        }
        assert!(read.contains(&format!("(subpath \"{}\")", w.display())));
        assert!(
            !read.contains("\"/Users\"")
                && !read.contains("/Library/Application Support")
                && !read.contains("/Volumes")
        );
        // Writing: the bot's folder only.
        let write = p
            .lines()
            .find(|l| l.starts_with("(allow file-write* "))
            .unwrap();
        assert_eq!(
            write,
            format!(
                "(allow file-write* (subpath \"{}\") (literal \"/dev/null\"))",
                w.display()
            )
        );
        // No links: oxbot reads and uploads files in the folder, and a
        // link there could point anywhere.
        assert!(p.contains("\n(deny file-write-create (vnode-type SYMLINK))\n"));
        // Running: the system's tools and the interpreter only.
        let exec = p
            .lines()
            .find(|l| l.starts_with("(allow process-exec "))
            .unwrap();
        assert!(exec.contains("(literal \"/opt/oxbot/tools/node/bin/node\")"));
        assert!(
            !read.contains("/opt/homebrew"),
            "not a Homebrew interpreter"
        );
        // No network, no system services.
        assert!(!p.contains("network"));
        assert!(!p.contains("mach-lookup"));
        assert!(!p.contains("(allow default)"));
    }

    #[test]
    fn paths_are_quoted_safely() {
        assert_eq!(sbpl(Path::new("/a \"b\"\\c")), "\"/a \\\"b\\\"\\\\c\"");
        let sh = Interpreter {
            exec: PathBuf::from("/bin/sh"),
            home: None,
        };
        let p = profile(Path::new("/x/\")(allow default)(\"/work"), &sh);
        assert!(!p.contains("\n(allow default)"));
        assert!(p.contains("\\\")(allow default)(\\\""));
    }

    #[test]
    fn the_report_says_how_it_ended() {
        let ok = Ran {
            code: Some(0),
            output: "3 rows\n".into(),
            ..Ran::default()
        };
        assert_eq!(
            report(Language::Sh, &ok),
            "The sh program exited with code 0.\nIts output:\n3 rows"
        );
        let slow = Ran {
            stopped: Some("it ran for 30 seconds".into()),
            ..Ran::default()
        };
        assert_eq!(
            report(Language::Node, &slow),
            "The node program was stopped: it ran for 30 seconds.\nIt printed nothing."
        );
        let big = Ran {
            code: Some(0),
            output: "x".into(),
            cut: true,
            ..Ran::default()
        };
        assert!(report(Language::Python3, &big).ends_with("[cut at 256 KB]"));
        assert!(limits_line(600).contains("ulimit -u 600"));
        assert_eq!(
            card_text(&json!({"language": "python3", "program": "print(1)\n", "files": ["a.csv"]})),
            "python3:\nprint(1)\n\nFiles: a.csv"
        );
    }

    #[test]
    fn interpreters_are_found_by_their_real_path() {
        let sh = interpreter(Language::Sh, "").unwrap();
        assert_eq!(sh.exec, PathBuf::from("/bin/sh"));
        assert_eq!(sh.home, None);
        let tmp = tempfile::tempdir().unwrap();
        let bin = tmp.path().join("node/bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(bin.join("node"), "").unwrap();
        let node = interpreter(Language::Node, &format!("relative:{}", bin.display())).unwrap();
        assert_eq!(
            node.home,
            Some(tmp.path().join("node").canonicalize().unwrap())
        );
        assert_eq!(interpreter(Language::Node, "/nowhere"), None);
    }

    /// Runs on this Mac with its sandbox: what a program may and may not
    /// do. Skipped where `sandbox-exec` or Apple's Python is missing.
    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn real_sandbox_keeps_a_program_in_its_folder() {
        if !Path::new(SANDBOX_EXEC).is_file() {
            return;
        }
        let (tmp, w) = work();
        let sibling = tmp.path().join("bots/b/work");
        std::fs::create_dir_all(&sibling).unwrap();
        std::fs::write(sibling.join("secret.txt"), "theirs").unwrap();
        std::fs::write(w.join("in.txt"), "a\nb\nc\n").unwrap();
        let home = std::env::var("HOME").unwrap_or_default();
        // oxbot's own Node.js, where the build keeps it.
        let node_path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/tools-bundle/node/bin")
            .display()
            .to_string();
        let cancel = CancellationToken::new();
        let go = |language: Language, program: &str, files: &[&str]| {
            let req = Request {
                language,
                program: program.to_owned(),
                files: files.iter().map(|f| (*f).to_owned()).collect(),
            };
            let (w, node_path, cancel) = (w.clone(), node_path.clone(), cancel.clone());
            async move { run(&req, &w, &node_path, &cancel).await.unwrap() }
        };
        // Its own folder: read and write.
        let ran = go(
            Language::Sh,
            "wc -l < \"$1\"; echo made > out.txt; cat out.txt",
            &["in.txt"],
        )
        .await;
        assert_eq!(
            (ran.code, ran.output.split_whitespace().collect::<Vec<_>>()),
            (Some(0), vec!["3", "made"])
        );
        // Nothing outside it.
        let ran = go(
            Language::Sh,
            &format!("cat {}/secret.txt", sibling.display()),
            &[],
        )
        .await;
        assert!(ran.output.contains("Operation not permitted"), "{ran:?}");
        let ran = go(
            Language::Sh,
            &format!(
                "ls {home}/.ssh; ls {home}/Library; ls /Volumes; echo x > {home}/oxbot-sandbox-test"
            ),
            &[],
        )
        .await;
        assert_eq!(
            ran.output.matches("Operation not permitted").count(),
            4,
            "{ran:?}"
        );
        assert!(!Path::new(&format!("{home}/oxbot-sandbox-test")).exists());
        // No links (a hard link out is a write outside, so refused too).
        let ran = go(
            Language::Sh,
            &format!(
                "ln -s {0}/secret.txt l1; ln {0}/secret.txt l2; ln -s in.txt l3; ls",
                sibling.display()
            ),
            &[],
        )
        .await;
        assert_eq!(
            ran.output.matches("Operation not permitted").count(),
            3,
            "{ran:?}"
        );
        assert!(std::fs::symlink_metadata(w.join("l1")).is_err());
        assert!(std::fs::symlink_metadata(w.join("l3")).is_err());
        // No network, loopback included.
        let ran = go(
            Language::Sh,
            "curl -sS --max-time 3 http://127.0.0.1:9/ || echo no-net",
            &[],
        )
        .await;
        assert!(ran.output.contains("no-net"), "{ran:?}");
        // An endless loop is stopped (after the wall limit), its group killed.
        let started = std::time::Instant::now();
        let ran = go(Language::Sh, "while true; do :; done", &[]).await;
        assert!(ran.stopped.is_some() || ran.code != Some(0), "{ran:?}");
        assert!(started.elapsed() < WALL_LIMIT + Duration::from_secs(10));
        // The run folder is gone after.
        assert!(std::fs::read_dir(&w).unwrap().all(|e| {
            !e.unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".oxbot-run")
        }));
        if interpreter(Language::Python3, "").is_some() {
            let ran = go(
                Language::Python3,
                &format!(
                    "import sys, csv, json, urllib.request\nprint(len(open(sys.argv[1]).read().split()))\n\
                     try:\n    open('{}/secret.txt')\nexcept Exception as e:\n    print('blocked', e)\n\
                     try:\n    urllib.request.urlopen('http://127.0.0.1:9/', timeout=3)\nexcept Exception as e:\n    print('net', e)",
                    sibling.display()
                ),
                &["in.txt"],
            )
            .await;
            assert!(
                ran.output
                    .starts_with("3\nblocked [Errno 1] Operation not permitted"),
                "{ran:?}"
            );
            assert!(
                ran.output
                    .contains("net <urlopen error [Errno 1] Operation not permitted>"),
                "{ran:?}"
            );
        }
        if interpreter(Language::Node, &node_path).is_some() {
            let ran = go(
                Language::Node,
                "const fs = require('fs');\nconsole.log(fs.readFileSync(process.argv[2], 'utf8').trim().split('\\n').length);\n\
                 try { fs.readFileSync(process.env.HOME + '/../../../../.ssh/id_rsa'); } catch (e) { console.log('blocked', e.code); }\n\
                 fetch('http://127.0.0.1:9/').then(() => console.log('got'), () => console.log('no-net'));",
                &["in.txt"],
            )
            .await;
            assert!(ran.output.starts_with("3\nblocked"), "{ran:?}");
            assert!(ran.output.contains("no-net"), "{ran:?}");
        }
    }
}
