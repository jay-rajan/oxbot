//! A stand-in for the browser tool (`chrome-devtools-mcp`) in end-to-end
//! tests: an MCP server over stdio answering
//! from the reply script in `OXBOT_FAKE_SCRIPT`. See
//! [`oxbot_e2e::fake_devtools`]. Of the real tool's flags it reads only
//! `--browserUrl`.

use std::io::{BufRead as _, Write as _};

use oxbot_e2e::fake_devtools::Devtools;
use oxbot_e2e::script::Script;
use serde_json::Value;

fn main() {
    let log = std::env::var_os("OXBOT_FAKE_DT_LOG").map(Into::into);
    let mut devtools = Devtools::new(Script::from_env(), log);
    if let Some(url) =
        std::env::args().find_map(|a| a.strip_prefix("--browserUrl=").map(str::to_owned))
    {
        devtools = devtools.with_browser_url(&url);
    }
    let stdout = std::io::stdout();
    for line in std::io::stdin().lock().lines() {
        let Ok(line) = line else { break };
        let Ok(msg) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if let Some(reply) = devtools.handle(&msg) {
            let mut out = stdout.lock();
            if writeln!(out, "{reply}").and_then(|()| out.flush()).is_err() {
                break;
            }
        }
    }
}
