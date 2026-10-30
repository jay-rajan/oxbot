//! `oxbot-e2e`: oxbot's end-to-end test runner, used by `/pr_test`.
//!
//! ```text
//! oxbot-e2e pick [--base main] [--all]          which tests a change needs (JSON)
//! oxbot-e2e run --out DIR [--engine chrome|safari] [--why TEST=REASON]… TEST.toml…
//! oxbot-e2e app --out DIR                         the Mac app check
//! oxbot-e2e report --out DIR [--intent …] [--fixes FILE] [--notes FILE] [--skipped FILE]
//! oxbot-e2e approve --out DIR [TEST-ID…]          make this run's screens the baselines
//! ```

use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{Context, bail};
use clap::{Parser, Subcommand};
use oxbot_e2e::driver::{Driver, Engine};
use oxbot_e2e::report::TestRun;
use oxbot_e2e::serve::Server;
use oxbot_e2e::spec::{self, Config, Test};
use oxbot_e2e::{app, compare, pick, report, seed, steps};
use serde_json::{Value, json};

#[derive(Debug, Parser)]
#[command(name = "oxbot-e2e", about = "oxbot's end-to-end tests")]
struct Cli {
    /// The repo (default: the current folder).
    #[arg(long, global = true, default_value = ".")]
    repo: PathBuf,
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Debug, Subcommand)]
enum Cmd {
    /// Which tests a change needs, as JSON.
    Pick {
        /// Compare with this branch or commit.
        #[arg(long, default_value = "main")]
        base: String,
        /// Run every test.
        #[arg(long)]
        all: bool,
    },
    /// Run tests, writing screenshots and results.json into --out.
    Run {
        /// The run's folder.
        #[arg(long)]
        out: PathBuf,
        /// The browser.
        #[arg(long, default_value = "chrome")]
        engine: String,
        /// Why a test runs, as `id=reason` (shown in the report).
        #[arg(long)]
        why: Vec<String>,
        /// Test files.
        tests: Vec<PathBuf>,
    },
    /// The Mac app check.
    App {
        /// The run's folder.
        #[arg(long)]
        out: PathBuf,
    },
    /// Make the report page (index.html) in --out.
    Report {
        /// The run's folder.
        #[arg(long)]
        out: PathBuf,
        /// The change's intent, in one line.
        #[arg(long, default_value = "")]
        intent: String,
        /// What was fixed (plain text; `- ` lines become a list).
        #[arg(long)]
        fixes: Option<PathBuf>,
        /// Anything else to say (plain text).
        #[arg(long)]
        notes: Option<PathBuf>,
        /// A file listing the tests not run, one per line.
        #[arg(long)]
        skipped: Option<PathBuf>,
    },
    /// Make this run's screenshots the approved baselines.
    Approve {
        /// The run's folder.
        #[arg(long)]
        out: PathBuf,
        /// Only these tests (default: all in the run).
        tests: Vec<String>,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let repo = std::fs::canonicalize(&cli.repo).context("finding the repo")?;
    match cli.command {
        Cmd::Pick { base, all } => {
            let config = Config::load(&repo)?;
            let tests = spec::all_tests(&repo)?;
            let changed = pick::changed_files(&repo, &base)?;
            let plan = pick::plan(&config, &tests, &changed, all, &repo, |f| {
                !pick::in_base(&repo, &base, f)
            });
            println_json(&serde_json::to_value(plan)?);
        }
        Cmd::Run {
            out,
            engine,
            why,
            tests,
        } => {
            let engine = match engine.as_str() {
                "chrome" => Engine::Chrome,
                "safari" => Engine::Safari,
                other => bail!("unknown browser {other}; use chrome or safari"),
            };
            let broken = run(&repo, &out, engine, &why, &tests).await?;
            if broken > 0 {
                std::process::exit(1);
            }
        }
        Cmd::App { out } => {
            std::fs::create_dir_all(&out)?;
            let config = Config::load(&repo)?;
            let result = app::check(&repo, &config, &out).await?;
            let mut results = report::load(&out)?;
            results["app"] = serde_json::to_value(&result)?;
            report::save(&out, &results)?;
            println_json(&results["app"]);
        }
        Cmd::Report {
            out,
            intent,
            fixes,
            notes,
            skipped,
        } => {
            let read = |p: Option<PathBuf>| {
                p.and_then(|p| std::fs::read_to_string(p).ok())
                    .unwrap_or_default()
            };
            let skipped: Vec<String> = read(skipped)
                .lines()
                .filter(|l| !l.trim().is_empty())
                .map(str::to_owned)
                .collect();
            report::write(&out, &intent, &read(fixes), &read(notes), &skipped)?;
            say(&out.join("index.html").display().to_string());
        }
        Cmd::Approve { out, tests } => {
            let results = report::load(&out)?;
            let mut n = 0;
            for t in results["tests"].as_array().cloned().unwrap_or_default() {
                let id = t["id"].as_str().unwrap_or("");
                if !tests.is_empty() && !tests.iter().any(|x| x == id) {
                    continue;
                }
                let dir = repo
                    .join("e2e/baselines")
                    .join(t["engine"].as_str().unwrap_or("chrome"))
                    .join(id);
                std::fs::create_dir_all(&dir)?;
                for s in t["steps"].as_array().cloned().unwrap_or_default() {
                    if let (Some(shot), "pass") =
                        (s["shot"].as_str(), s["status"].as_str().unwrap_or(""))
                    {
                        let name = Path::new(shot).file_name().unwrap_or_default();
                        std::fs::copy(out.join(shot), dir.join(name))?;
                        n += 1;
                    }
                }
            }
            say(&format!("approved {n} screens"));
        }
    }
    Ok(())
}

#[allow(clippy::print_stdout, reason = "the command's output")]
fn say(text: &str) {
    println!("{text}");
}

#[allow(clippy::print_stdout, reason = "the command's output")]
fn println_json(v: &Value) {
    println!("{}", serde_json::to_string_pretty(v).unwrap_or_default());
}

/// Runs `files` in `engine`; returns how many broke.
async fn run(
    repo: &Path,
    out: &Path,
    engine: Engine,
    why: &[String],
    files: &[PathBuf],
) -> anyhow::Result<usize> {
    let config = Config::load(repo)?;
    std::fs::create_dir_all(out)?;
    let scratch = tempfile::tempdir()?;
    let mut results = report::load(out)?;
    let mut driver = match Driver::start(engine, scratch.path()).await {
        Ok(d) => d,
        Err(err) if engine == Engine::Safari => {
            eprintln!("skipping Safari: {err:#}");
            let mut notes = results["skipped_engines"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            notes.push(json!({ "engine": "safari", "why": format!("{err:#}") }));
            results["skipped_engines"] = Value::Array(notes);
            report::save(out, &results)?;
            return Ok(0);
        }
        Err(err) => return Err(err),
    };
    let mut broken = 0;
    for file in files {
        let file = if file.is_absolute() {
            file.clone()
        } else {
            repo.join(file)
        };
        let test = Test::load(&file)?;
        let reason = why
            .iter()
            .find_map(|w| w.strip_prefix(&format!("{}=", test.id())))
            .unwrap_or("asked for")
            .to_owned();
        eprintln!("▶ {} ({})", test.name, engine.name());
        let run = run_one(repo, &config, out, engine, &mut driver, &test, reason).await?;
        eprintln!("  {}", run.status);
        if run.status == "broken" {
            broken += 1;
        }
        // A test runs once per browser: replace an earlier result.
        let tests = results["tests"]
            .as_array_mut()
            .map(std::mem::take)
            .unwrap_or_default();
        let mut tests: Vec<Value> = tests
            .into_iter()
            .filter(|t| !(t["id"] == run.id.as_str() && t["engine"] == engine.name()))
            .collect();
        tests.push(serde_json::to_value(&run)?);
        results["tests"] = Value::Array(tests);
        report::save(out, &results)?;
    }
    driver.stop().await;
    Ok(broken)
}

/// Runs one test on a fresh seeded oxbot.
async fn run_one(
    repo: &Path,
    config: &Config,
    out: &Path,
    engine: Engine,
    driver: &mut Driver,
    test: &Test,
    why: String,
) -> anyhow::Result<TestRun> {
    let id = test.id();
    let started = Instant::now();
    let home = tempfile::tempdir()?;
    seed::make_home(repo, home.path())?;
    let script = repo.join("e2e/claude").join(&test.claude);
    let server = Server::start(repo, config, home.path(), &script).await?;
    let rel = format!("shots/{}/{id}", engine.name());
    let shots_dir = out.join(&rel);
    std::fs::create_dir_all(&shots_dir)?;
    let mut results = steps::run(driver, &server, &starting_steps(test), &shots_dir, &rel).await;
    for s in &mut results {
        if let Some(shot) = &s.shot {
            let name = Path::new(shot).file_name().unwrap_or_default();
            let baseline = repo
                .join("e2e/baselines")
                .join(engine.name())
                .join(&id)
                .join(name);
            s.compare = compare::compare(out, shot, &baseline).ok();
        }
    }
    let failed = results.iter().any(|s| s.status == "fail");
    let changed = results
        .iter()
        .any(|s| s.compare.as_ref().is_some_and(|c| c.status == "changed"));
    let status = if failed {
        "broken"
    } else if changed {
        "changed"
    } else {
        "pass"
    };
    let log = failed.then(|| server.log_tail(40));
    server.stop().await;
    Ok(TestRun {
        id,
        file: test
            .file
            .strip_prefix(repo)
            .unwrap_or(&test.file)
            .display()
            .to_string(),
        name: test.name.clone(),
        about: test.about.clone(),
        why,
        engine: engine.name().to_owned(),
        as_person: test
            .as_person
            .clone()
            .unwrap_or_else(|| format!("{} (the Farm)", config.person)),
        status: status.to_owned(),
        ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        steps: results,
        log,
    })
}

/// The test's steps, starting on the page as its person.
fn starting_steps(test: &Test) -> Vec<spec::Step> {
    let mut steps = test.steps.clone();
    if steps.first().is_none_or(|s| s.open.is_none()) {
        let path = test
            .as_person
            .as_ref()
            .map_or_else(|| "/".to_owned(), |p| format!("/?as={p}"));
        steps.insert(
            0,
            spec::Step {
                open: Some(path),
                ..spec::Step::default()
            },
        );
    } else if let (Some(p), Some(first)) = (&test.as_person, steps.first_mut())
        && let Some(open) = &mut first.open
        && !open.contains("as=")
    {
        open.push_str(if open.contains('?') { "&as=" } else { "?as=" });
        open.push_str(p);
    }
    steps
}
