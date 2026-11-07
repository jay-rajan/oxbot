//! The schema and its changes, in order.
//!
//! Migrations only go forward: once one has shipped it never changes, and an
//! older oxbot refuses a database a newer one has upgraded (see
//! [`StoreError::Newer`]). Going back means restoring the copy taken before
//! a step that rebuilds tables (`oxbot.db.v<N>.bak`). A step written in Rust
//! uses frozen copies of whatever it computes (the `v9_*` functions), so
//! later changes to the live code can't change what an old upgrade does.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use rusqlite::{Connection, Transaction, params};

use super::{PEOPLE_COLORS, StoreError};
use crate::id::PersonId;

pub(super) const SCHEMA: &str = "
PRAGMA journal_mode = WAL;
CREATE TABLE IF NOT EXISTS events (
    bot   TEXT    NOT NULL,
    seq   INTEGER NOT NULL,
    ts_ms INTEGER NOT NULL,
    body  TEXT    NOT NULL,  -- EventKind as JSON
    PRIMARY KEY (bot, seq)
);
CREATE TABLE IF NOT EXISTS sessions (
    bot            TEXT PRIMARY KEY,
    claude_session TEXT NOT NULL,
    updated_ms     INTEGER NOT NULL
);
";

/// One schema change: SQL, or Rust for what SQL can't do (it gets the
/// migration's transaction).
pub(super) enum Step {
    Sql(&'static str),
    Rust(fn(&Transaction<'_>) -> Result<(), StoreError>),
    /// Rust that rebuilds tables: the database file is copied first.
    Rebuild(fn(&Transaction<'_>) -> Result<(), StoreError>),
}

/// Schema changes after the first release, in order. `PRAGMA user_version`
/// records how many have run.
pub(super) const MIGRATIONS: &[Step] = &[
    // 1: remember the system prompt each session started with.
    Step::Sql("ALTER TABLE sessions ADD COLUMN prompt TEXT NOT NULL DEFAULT ''"),
    // 2: Claude usage per bot, and the plan's limits as last reported.
    Step::Sql(
        "CREATE TABLE usage (
        bot                TEXT PRIMARY KEY,
        turns              INTEGER NOT NULL DEFAULT 0,
        input_tokens       INTEGER NOT NULL DEFAULT 0,
        output_tokens      INTEGER NOT NULL DEFAULT 0,
        cache_read_tokens  INTEGER NOT NULL DEFAULT 0,
        cache_write_tokens INTEGER NOT NULL DEFAULT 0,
        cost_usd           REAL    NOT NULL DEFAULT 0
    );
    CREATE TABLE limits (
        name         TEXT PRIMARY KEY,
        used         REAL    NOT NULL,
        resets_at_ms INTEGER NOT NULL,
        status       TEXT    NOT NULL,
        seen_ms      INTEGER NOT NULL
    );",
    ),
    // 3: the people who send messages, each with their own color.
    Step::Sql(
        "CREATE TABLE people (
        name     TEXT PRIMARY KEY,
        color    INTEGER NOT NULL,
        first_ms INTEGER NOT NULL
    );",
    ),
    // 4: the sites the user signed in to through oxbot's sign-in window.
    Step::Sql(
        "CREATE TABLE sites (
        site         TEXT PRIMARY KEY,
        signed_in_ms INTEGER NOT NULL
    );",
    ),
    // 5: sign-ins from before the sites list, taken from the chat history
    // (as hosts; migration 9 turns them into main domains).
    Step::Sql(
        "INSERT OR IGNORE INTO sites (site, signed_in_ms)
        SELECT json_extract(body, '$.site'), MAX(ts_ms) FROM events
        WHERE json_extract(body, '$.kind') = 'sign_in'
          AND json_extract(body, '$.outcome') = 'done'
          AND json_extract(body, '$.site') IS NOT NULL
        GROUP BY json_extract(body, '$.site');",
    ),
    // 6: each person has their own browser: sites belong to a browser's
    // owner ('' is the home browser every shared bot uses).
    Step::Sql(
        "CREATE TABLE sites_by_owner (
        owner        TEXT    NOT NULL DEFAULT '',
        site         TEXT    NOT NULL,
        signed_in_ms INTEGER NOT NULL,
        PRIMARY KEY (owner, site)
    );
    INSERT INTO sites_by_owner (owner, site, signed_in_ms) SELECT '', site, signed_in_ms FROM sites;
    DROP TABLE sites;
    ALTER TABLE sites_by_owner RENAME TO sites;",
    ),
    // 7: Gates (phones and laptops) paired with this Farm. Only a hash of
    // each Gate's key is kept, so a copy of the database can't pose as one.
    Step::Sql(
        "CREATE TABLE gates (
        id           TEXT    PRIMARY KEY,
        person       TEXT    NOT NULL,
        name         TEXT    NOT NULL,
        key_hash     TEXT    NOT NULL UNIQUE,
        paired_ms    INTEGER NOT NULL,
        last_seen_ms INTEGER NOT NULL
    );",
    ),
    // 8: find each bot's latest reply without reading its whole history.
    Step::Sql(
        "CREATE INDEX IF NOT EXISTS events_replies ON events (bot, seq)
        WHERE json_extract(body, '$.kind') = 'assistant_message';",
    ),
    // 9: people get ids, so a new name never takes over someone's bots or
    // sign-ins. Browser owners and Gates point at ids; sites are named by
    // their main domain once, here, instead of on every start.
    Step::Rebuild(people_get_ids),
    // 10: whether a Claude session has read text oxbot doesn't trust (and
    // what), kept with the session because Claude remembers it on resume.
    Step::Rust(sessions_remember_taint),
    // 11: a browser folder and a Herder belong to one person, and there's
    // one Farm person at most. A folder or Herder held twice (only possible
    // by hand or by an older oxbot) is cleared from the later person, who
    // gets a fresh one when the store opens.
    Step::Sql(
        "UPDATE people SET farm = 0 WHERE farm <> 0
           AND rowid <> (SELECT MIN(rowid) FROM people WHERE farm <> 0);
        UPDATE people SET farm = 1 WHERE farm <> 0;
        UPDATE people SET folder = NULL WHERE folder IS NOT NULL
           AND rowid <> (SELECT MIN(p.rowid) FROM people p WHERE p.folder = people.folder);
        UPDATE people SET herder = NULL WHERE herder IS NOT NULL
           AND rowid <> (SELECT MIN(p.rowid) FROM people p WHERE p.herder = people.herder);
        CREATE UNIQUE INDEX IF NOT EXISTS people_folder ON people (folder);
        CREATE UNIQUE INDEX IF NOT EXISTS people_herder ON people (herder);
        CREATE UNIQUE INDEX IF NOT EXISTS people_one_farm ON people (farm) WHERE farm = 1;",
    ),
    // 12: "Always allow" rules: a bot may do one action on one element
    // label on one exact host without asking. `by_person` is who set it
    // (NULL: the Farm with no person).
    Step::Sql(
        "CREATE TABLE IF NOT EXISTS allow_rules (
            id         INTEGER PRIMARY KEY,
            bot        TEXT    NOT NULL,
            site       TEXT    NOT NULL,
            action     TEXT    NOT NULL,
            label      TEXT    NOT NULL,
            by_person  TEXT,
            created_ms INTEGER NOT NULL,
            UNIQUE (bot, site, action, label)
        );
        CREATE INDEX IF NOT EXISTS allow_rules_bot ON allow_rules (bot);",
    ),
    // 13: each person's own sidebar sections, and which of their sections
    // each bot is in (at most one; no row means not in a section). Every
    // row belongs to one person: nobody sees or changes another's.
    Step::Sql(
        "CREATE TABLE IF NOT EXISTS sections (
        id         TEXT    PRIMARY KEY DEFAULT ('s_' || lower(hex(randomblob(8)))),
        person_id  TEXT    NOT NULL,
        name       TEXT    NOT NULL,
        position   INTEGER NOT NULL,
        created_ms INTEGER NOT NULL
    );
    CREATE UNIQUE INDEX IF NOT EXISTS sections_name ON sections (person_id, name COLLATE NOCASE);
    CREATE TABLE IF NOT EXISTS bot_sections (
        person_id  TEXT NOT NULL,
        bot        TEXT NOT NULL,
        section_id TEXT NOT NULL,
        PRIMARY KEY (person_id, bot)
    );
    CREATE INDEX IF NOT EXISTS bot_sections_section ON bot_sections (section_id);
    CREATE INDEX IF NOT EXISTS bot_sections_bot ON bot_sections (bot);",
    ),
    // 14: research reports, and the web pages each bot's Claude session
    // has opened (a report's sources must be among them). A new session
    // starts with none (see `set_session`).
    Step::Sql(
        "CREATE TABLE IF NOT EXISTS reports (
            id         TEXT    PRIMARY KEY,
            bot        TEXT    NOT NULL,
            seq        INTEGER NOT NULL,  -- its card in the bot's chat
            created_ms INTEGER NOT NULL,
            asked_by   TEXT,              -- person id (NULL: the Farm or unknown)
            question   TEXT    NOT NULL,
            title      TEXT    NOT NULL,
            summary    TEXT    NOT NULL,
            markdown   TEXT    NOT NULL,  -- as the bot wrote it; rendered on every open
            sources    TEXT    NOT NULL,  -- JSON [{n, url, title, site, read_ms}]
            links      TEXT    NOT NULL DEFAULT '[]'  -- JSON [url]: other opened pages it links
        );
        CREATE INDEX IF NOT EXISTS reports_by_bot ON reports (bot, created_ms);
        CREATE TABLE IF NOT EXISTS session_pages (
            bot      TEXT    NOT NULL,
            url      TEXT    NOT NULL,
            title    TEXT    NOT NULL,
            first_ms INTEGER NOT NULL,
            PRIMARY KEY (bot, url)
        );",
    ),
    // 15: the usage alerts already sent for each plan window, app-wide
    // preferences, and the model each bot's Claude last said it uses.
    Step::Rust(usage_alerts_and_prefs),
    // 16: who started each task chain (Claude bots take tasks only from the
    // owner; provenance for knowledge and comparisons), and which engine a
    // saved session belongs to.
    Step::Rust(engines_and_origins),
    // 17: oxbot's own loop: conversations, turns with their checkpoints,
    // and every model request (the meter).
    Step::Sql(
        "CREATE TABLE IF NOT EXISTS loop_sessions (
            id          TEXT    PRIMARY KEY,     -- same id as sessions.claude_session for engine 'openrouter'
            bot         TEXT    NOT NULL,
            prompt_hash TEXT    NOT NULL,        -- SHA-256 of the stable prefix (system prompt + tools)
            context     TEXT    NOT NULL DEFAULT '{}', -- the context builder's state
            created_ms  INTEGER NOT NULL,
            closed_ms   INTEGER                  -- replaced by a newer session of the bot
        );
        CREATE INDEX IF NOT EXISTS loop_sessions_bot ON loop_sessions (bot, created_ms);
        CREATE TABLE IF NOT EXISTS loop_messages (
            session   TEXT    NOT NULL,
            idx       INTEGER NOT NULL,          -- 0, 1, 2...: append-only
            turn      INTEGER NOT NULL,
            role      TEXT    NOT NULL,          -- user | assistant | tool
            body      TEXT    NOT NULL,          -- oxbot_llm::Message as stored JSON (version 1)
            tokens    INTEGER NOT NULL DEFAULT 0,-- size estimate, for the context budget
            untrusted INTEGER NOT NULL DEFAULT 0,-- outside text (web, files, other bots)
            handle    TEXT,                      -- big result saved under work/.oxbot/results/
            meta      TEXT    NOT NULL DEFAULT '{}', -- the context builder's notes on it
            PRIMARY KEY (session, idx)
        );
        CREATE TABLE IF NOT EXISTS loop_turns (
            session    TEXT    NOT NULL,
            turn       INTEGER NOT NULL,
            bot        TEXT    NOT NULL,
            user_seq   INTEGER NOT NULL,         -- the UserMessage event (joins turn_origins)
            origin     TEXT    NOT NULL,         -- who started the task chain, as JSON
            question   TEXT    NOT NULL,
            status     TEXT    NOT NULL,         -- running | waiting_user | done | failed | stopped
            step       INTEGER NOT NULL DEFAULT 0,
            pending    TEXT,                     -- a call that started and whose result isn't saved
            model      TEXT,                     -- last model that answered
            resumes    INTEGER NOT NULL DEFAULT 0,
            dry_run    INTEGER NOT NULL DEFAULT 0,
            started_ms INTEGER NOT NULL,
            updated_ms INTEGER NOT NULL,
            ended_ms   INTEGER,
            PRIMARY KEY (session, turn)
        );
        CREATE INDEX IF NOT EXISTS loop_turns_running ON loop_turns (status) WHERE status = 'running';
        CREATE INDEX IF NOT EXISTS loop_turns_bot ON loop_turns (bot);
        CREATE TABLE IF NOT EXISTS model_calls (
            id                INTEGER PRIMARY KEY,
            ts_ms             INTEGER NOT NULL,
            day               TEXT    NOT NULL,  -- UTC 'YYYY-MM-DD' (free daily counts reset 00:00 UTC)
            bot               TEXT    NOT NULL,
            person            TEXT,              -- the chain's person
            session           TEXT,
            turn              INTEGER,
            step              INTEGER,
            provider          TEXT    NOT NULL,  -- openrouter (local later)
            requested         TEXT    NOT NULL,  -- model asked for
            model             TEXT    NOT NULL,  -- model that answered ('' if none)
            free              INTEGER NOT NULL,
            private           INTEGER NOT NULL,  -- asked for no data collection
            generation_id     TEXT,
            prompt_tokens     INTEGER NOT NULL DEFAULT 0,
            completion_tokens INTEGER NOT NULL DEFAULT 0,
            cached_tokens     INTEGER NOT NULL DEFAULT 0,
            cost_usd          REAL    NOT NULL DEFAULT 0,
            latency_ms        INTEGER NOT NULL DEFAULT 0,
            outcome           TEXT    NOT NULL,  -- ok | rate_limited | busy | daily_limit | error:<kind> | cancelled
            kind              TEXT    NOT NULL DEFAULT 'chat' -- chat | compaction | reflection | judge
        );
        CREATE INDEX IF NOT EXISTS model_calls_bot_day ON model_calls (day, bot);
        CREATE INDEX IF NOT EXISTS model_calls_person_day ON model_calls (day, person);
        CREATE INDEX IF NOT EXISTS model_calls_model ON model_calls (model, ts_ms);",
    ),
    // 18: comparing engines and models (#21): test cases, practice runs and
    // what they did, the oxbot-tool calls of every turn (a case's
    // fixtures), and people's thumbs on replies.
    Step::Rust(comparisons),
    // 19: the loop's context (#19): stored tool results (read back by
    // handle), progress notes that outlive a window, and each model's token
    // calibration. Window state stays in loop_sessions.context (17).
    Step::Sql(
        "CREATE TABLE IF NOT EXISTS loop_results (
            session    TEXT    NOT NULL,             -- loop_sessions.id
            handle     TEXT    NOT NULL,             -- r<N>, counted per session
            bot        TEXT    NOT NULL,
            turn       INTEGER NOT NULL,
            step       INTEGER NOT NULL,
            tool       TEXT    NOT NULL,
            kind       TEXT    NOT NULL DEFAULT 'text', -- page (a browser snapshot) | text
            page_url   TEXT,
            page_title TEXT,
            bytes      INTEGER NOT NULL,
            est_tokens INTEGER NOT NULL,
            tainted_by TEXT,                         -- why it can't be trusted, if so
            created_ms INTEGER NOT NULL,
            PRIMARY KEY (session, handle)
        );
        CREATE INDEX IF NOT EXISTS loop_results_bot ON loop_results (bot, created_ms);
        CREATE TABLE IF NOT EXISTS loop_progress (
            session    TEXT    NOT NULL,
            turn       INTEGER NOT NULL,
            n          INTEGER NOT NULL,             -- 1, 2… within the turn
            ms         INTEGER NOT NULL,
            note       TEXT    NOT NULL,
            tainted_by TEXT,
            PRIMARY KEY (session, turn, n)
        );
        CREATE TABLE IF NOT EXISTS token_calibration (
            model      TEXT    PRIMARY KEY,
            ratio      REAL    NOT NULL,             -- provider tokens per estimated token
            samples    INTEGER NOT NULL,
            updated_ms INTEGER NOT NULL
        );",
    ),
    // 20: how each loop turn's final answer was checked (round 3): sent
    // back once to be checked, and the values in it that nothing the bot
    // read had, before and after. Owner-only; read by the benchmark.
    Step::Sql(
        "CREATE TABLE IF NOT EXISTS loop_checks (
            session           TEXT    NOT NULL,          -- loop_sessions.id
            turn              INTEGER NOT NULL,
            bounced           INTEGER NOT NULL DEFAULT 0,
            ungrounded_before TEXT    NOT NULL DEFAULT '[]', -- JSON list of values
            ungrounded_after  TEXT    NOT NULL DEFAULT '[]', -- JSON list of values
            ms                INTEGER NOT NULL DEFAULT 0,
            PRIMARY KEY (session, turn)
        );",
    ),
    // 21: the Codex engine (step 6): each bot session's Codex thread, so a
    // session goes on after a restart; what oxbot's relay sent to the model
    // provider for each request (the body is a file in the bot's context
    // folder); and which engine made each model request.
    Step::Rust(codex_engine),
    // 22: Claude leaves oxbot (step 6, part 2). Bots that ran on Claude run
    // on Codex now: each is noted once when it moves (so its chat is told
    // once, however often oxbot starts), and what only Claude used stops
    // showing: its plan windows' alerts and limits, and its settings.
    // History stays (chats, sessions, Claude's own usage rows).
    Step::Sql(
        "CREATE TABLE IF NOT EXISTS engine_upgrades (
            bot         TEXT    PRIMARY KEY,
            from_engine TEXT    NOT NULL,   -- `claude`
            to_engine   TEXT    NOT NULL,   -- `codex`
            upgraded_ms INTEGER NOT NULL
        );
        DELETE FROM limit_alerts WHERE name NOT LIKE 'openrouter\\_%' ESCAPE '\\';
        DELETE FROM limits;
        DELETE FROM prefs WHERE key IN ('engines_notice_seen', 'compare.claude_pause');",
    ),
    // 23: oxbot's own loop retires as an engine (step 6, part 3). Its bots
    // run on Codex and are noted once in engine_upgrades (from
    // `openrouter`) as they move. Its turns never go on: the ones still
    // marked running or waiting are stopped, and its conversations closed
    // (their messages go after the usual 30 days). What only the loop read
    // goes now: progress notes and token calibrations. Stored results,
    // checks and the meter stay: Codex bots use the same tables.
    Step::Sql(
        "UPDATE loop_turns
            SET status = 'stopped', pending = NULL,
                ended_ms = COALESCE(ended_ms, CAST(strftime('%s', 'now') AS INTEGER) * 1000)
          WHERE status IN ('running', 'waiting_user');
        UPDATE loop_sessions SET closed_ms = CAST(strftime('%s', 'now') AS INTEGER) * 1000
          WHERE closed_ms IS NULL;
        DELETE FROM loop_progress;
        DELETE FROM token_calibration;",
    ),
    // 24: schedules (step 3): a bot's task that runs on its own at set
    // times, as the person who made it (only ever through their card), and
    // each run's outcome. Rules are small JSON objects (once, daily,
    // weekdays, weekly, monthly), never cron text; times are in `tz`, the
    // home's time zone when it was made.
    Step::Sql(
        "CREATE TABLE IF NOT EXISTS schedules (
            id          INTEGER PRIMARY KEY,
            bot         TEXT    NOT NULL,
            person      TEXT    NOT NULL,   -- person id it runs as
            task_text   TEXT    NOT NULL,
            rule_json   TEXT    NOT NULL,   -- ScheduleRule as JSON
            tz          TEXT    NOT NULL,   -- IANA name, e.g. America/Toronto
            next_run_ms INTEGER,            -- NULL: a one-time schedule that ran
            last_run_ms INTEGER,
            paused      INTEGER NOT NULL DEFAULT 0,
            created_ms  INTEGER NOT NULL,
            updated_ms  INTEGER NOT NULL
        );
        CREATE INDEX IF NOT EXISTS schedules_due ON schedules (next_run_ms) WHERE paused = 0;
        CREATE INDEX IF NOT EXISTS schedules_bot ON schedules (bot);
        CREATE INDEX IF NOT EXISTS schedules_person ON schedules (person);
        CREATE TABLE IF NOT EXISTS schedule_runs (
            id         INTEGER PRIMARY KEY,
            schedule   INTEGER NOT NULL,
            due_ms     INTEGER NOT NULL,
            started_ms INTEGER,
            ended_ms   INTEGER,
            outcome    TEXT    NOT NULL,   -- running | ok | failed | busy | limit | missed | too_soon | no_access
            turn       INTEGER             -- the run's message in the bot's chat (events.seq)
        );
        CREATE INDEX IF NOT EXISTS schedule_runs_schedule ON schedule_runs (schedule, id);",
    ),
    // 25: the people a bot is shared with when its maker picked them
    // (`sharing = \"people\"` in its bot.toml). Bots whose files say only
    // `private` mean just their maker (true) or everyone (false); oxbot
    // writes that out in each file once.
    Step::Sql(
        "CREATE TABLE IF NOT EXISTS bot_members (
            bot        TEXT    NOT NULL,
            person     TEXT    NOT NULL,   -- person id
            added_ms   INTEGER NOT NULL,
            PRIMARY KEY (bot, person)
        );
        CREATE INDEX IF NOT EXISTS bot_members_person ON bot_members (person);",
    ),
    // 26: each person's profile (step 3's welcome screen): their role, a
    // photo, and what bots may know about them, each with whether bots may
    // use it. People oxbot knew before have nothing to fill in (their
    // welcome isn't due); the Farm's person is the owner.
    Step::Sql(
        "CREATE TABLE IF NOT EXISTS profiles (
            person        TEXT    PRIMARY KEY,          -- person id
            role          TEXT    NOT NULL DEFAULT 'adult', -- owner | adult | kid
            age           INTEGER,                      -- kid accounts (later)
            photo         TEXT,                         -- a small JPEG or PNG as a data: URL
            food          TEXT    NOT NULL DEFAULT '',
            food_use      INTEGER NOT NULL DEFAULT 1,
            languages     TEXT    NOT NULL DEFAULT '',
            languages_use INTEGER NOT NULL DEFAULT 1,
            notes         TEXT    NOT NULL DEFAULT '',
            notes_use     INTEGER NOT NULL DEFAULT 1,
            name_set      INTEGER NOT NULL DEFAULT 0,   -- they chose their name themselves
            welcome_due   INTEGER NOT NULL DEFAULT 0,   -- the welcome screen shows until they save it
            owner_ok_ms   INTEGER,                      -- the owner confirmed 18+ and OpenRouter's terms
            updated_ms    INTEGER NOT NULL
        );
        INSERT OR IGNORE INTO profiles (person, role, updated_ms)
          SELECT id, CASE WHEN farm = 1 THEN 'owner' ELSE 'adult' END,
                 CAST(strftime('%s', 'now') AS INTEGER) * 1000
          FROM people;",
    ),
    // 27: Settings → Activity: one row per exchange with the outside world
    // (a question to the AI, a bot's visit to a website, a device's
    // connection, oxbot's own calls). Sizes, places and outcomes only,
    // never what was said; kept 30 days.
    Step::Sql(
        "CREATE TABLE IF NOT EXISTS net_log (
            id         INTEGER PRIMARY KEY,
            ts_ms      INTEGER NOT NULL,
            kind       TEXT    NOT NULL,          -- ai | website | device | oxbot
            who        TEXT    NOT NULL DEFAULT '', -- the name shown when it was written
            bot        TEXT,                      -- bot id
            person     TEXT,                      -- person id
            gate       TEXT,                      -- device (gate) id
            host       TEXT    NOT NULL,
            label      TEXT    NOT NULL,
            bytes_up   INTEGER NOT NULL DEFAULT 0,
            bytes_down INTEGER NOT NULL DEFAULT 0,
            cost_usd   REAL,
            detail     TEXT    NOT NULL DEFAULT '{}' -- JSON: ms, outcome, swapped counts, model, tokens, pages, other_hosts
        );
        CREATE INDEX IF NOT EXISTS net_log_ts ON net_log (ts_ms);
        CREATE INDEX IF NOT EXISTS net_log_kind_ts ON net_log (kind, ts_ms);",
    ),
];

/// Migration 21: Codex threads, the relay's log, and the engine of each
/// model request (added only where it's missing, so the step can run
/// again).
fn codex_engine(tx: &Transaction<'_>) -> Result<(), StoreError> {
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS codex_threads (
            bot        TEXT    NOT NULL,
            session    TEXT    NOT NULL,             -- sessions.claude_session
            thread_id  TEXT    NOT NULL,             -- Codex's own id
            model      TEXT    NOT NULL DEFAULT '',  -- the model it was started for
            window     INTEGER NOT NULL DEFAULT 0,   -- the window Codex was told
            turns      INTEGER NOT NULL DEFAULT 0,   -- turns so far
            created_ms INTEGER NOT NULL,
            updated_ms INTEGER NOT NULL,
            PRIMARY KEY (bot, session)
        );
        CREATE TABLE IF NOT EXISTS relay_log (
            id        INTEGER PRIMARY KEY,
            ts_ms     INTEGER NOT NULL,
            bot       TEXT    NOT NULL,
            session   TEXT,
            turn      INTEGER,
            model     TEXT    NOT NULL,
            bytes     INTEGER NOT NULL,
            body_path TEXT    NOT NULL               -- under the bot's context folder
        );
        CREATE INDEX IF NOT EXISTS relay_log_bot ON relay_log (bot, ts_ms);
        CREATE INDEX IF NOT EXISTS relay_log_ts ON relay_log (ts_ms);",
    )?;
    let has_engine: bool = tx.query_row(
        "SELECT COUNT(*) > 0 FROM pragma_table_info('model_calls') WHERE name = 'engine'",
        [],
        |r| r.get(0),
    )?;
    if !has_engine {
        tx.execute_batch("ALTER TABLE model_calls ADD COLUMN engine TEXT NOT NULL DEFAULT ''")?;
    }
    Ok(())
}

/// Migration 18. Skips what is already there, as 15 does.
pub(super) fn comparisons(tx: &Transaction<'_>) -> Result<(), StoreError> {
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS turn_tools (
            bot    TEXT    NOT NULL,
            seq    INTEGER NOT NULL,  -- the turn's UserMessage event
            n      INTEGER NOT NULL,  -- 1, 2… in the order they ran
            tool   TEXT    NOT NULL,  -- oxbot's own tool, e.g. give_task
            args   TEXT    NOT NULL,  -- JSON, at most 8 KB
            result TEXT    NOT NULL,  -- what the bot was told, at most 8 KB
            ok     INTEGER NOT NULL,
            PRIMARY KEY (bot, seq, n)
        );
        CREATE TABLE IF NOT EXISTS test_cases (
            id            TEXT    PRIMARY KEY,
            bot           TEXT    NOT NULL,
            created_ms    INTEGER NOT NULL,
            created_by    TEXT,                          -- person id
            seq_from      INTEGER NOT NULL,              -- the original turn's UserMessage
            seq_to        INTEGER NOT NULL,              -- and its TurnDone
            title         TEXT    NOT NULL,
            prompt        TEXT    NOT NULL,
            files         TEXT    NOT NULL DEFAULT '[]', -- JSON [Attachment], copied under cases/<id>/files
            instructions  TEXT    NOT NULL,              -- the bot's instructions then
            knowledge     TEXT,                          -- what the bot knew then (JSON), once there is knowledge
            kind          TEXT    NOT NULL CHECK (kind IN ('reads', 'acts')),
            rubric        TEXT    NOT NULL DEFAULT '',   -- the owner's checks, one per line
            expected      TEXT    NOT NULL DEFAULT '[]', -- JSON [{intent, expect}]
            fixtures      TEXT    NOT NULL DEFAULT '{}', -- JSON {give_task: [...], read_reply: [...], ask_user: [...]}
            sites         TEXT    NOT NULL DEFAULT '[]', -- JSON [host]
            uses_sign_ins INTEGER NOT NULL DEFAULT 0,
            browser_owner TEXT    NOT NULL DEFAULT '',   -- '' home, else the owner's person id
            network       TEXT    NOT NULL DEFAULT 'strict' CHECK (network IN ('strict', 'xhr')),
            local_only    INTEGER NOT NULL DEFAULT 0,
            origin        TEXT    NOT NULL DEFAULT '{}', -- JSON {person, on_farm, tainted_by} of the original turn
            archived      INTEGER NOT NULL DEFAULT 0
        );
        CREATE INDEX IF NOT EXISTS test_cases_by_bot ON test_cases (bot, created_ms);
        CREATE TABLE IF NOT EXISTS trial_batches (
            id           TEXT    PRIMARY KEY,
            bot          TEXT    NOT NULL,
            kind         TEXT    NOT NULL CHECK (kind IN ('replay', 'shadow')),
            created_ms   INTEGER NOT NULL,
            targets      TEXT    NOT NULL,                -- JSON [{engine, model}]
            cases        TEXT    NOT NULL,                -- JSON [case id]
            repeats      INTEGER NOT NULL DEFAULT 1,
            est_requests INTEGER NOT NULL DEFAULT 0,
            status       TEXT    NOT NULL                 -- queued | running | paused | done | stopped
        );
        CREATE TABLE IF NOT EXISTS trial_runs (
            id            TEXT    PRIMARY KEY,
            batch         TEXT,
            case_id       TEXT,
            bot           TEXT    NOT NULL,
            kind          TEXT    NOT NULL CHECK (kind IN ('original', 'replay', 'shadow')),
            engine        TEXT    NOT NULL,
            provider      TEXT    NOT NULL,
            model         TEXT    NOT NULL,
            started_ms    INTEGER,
            ended_ms      INTEGER,
            status        TEXT    NOT NULL,  -- queued | running | done | failed | stopped | needs_sign_in | over_budget | rate_limited | judge_error
            steps         INTEGER NOT NULL DEFAULT 0,
            requests      INTEGER NOT NULL DEFAULT 0,
            input_tokens  INTEGER NOT NULL DEFAULT 0,
            output_tokens INTEGER NOT NULL DEFAULT 0,
            cached_tokens INTEGER NOT NULL DEFAULT 0,
            cost_usd      REAL    NOT NULL DEFAULT 0,
            intents       INTEGER NOT NULL DEFAULT 0,  -- cards a live run would have raised
            refusals      INTEGER NOT NULL DEFAULT 0,
            errors        INTEGER NOT NULL DEFAULT 0,
            net_blocked   INTEGER NOT NULL DEFAULT 0,
            answer        TEXT    NOT NULL DEFAULT '',
            report        TEXT,                        -- JSON: a report it would have saved
            intent_list   TEXT    NOT NULL DEFAULT '[]', -- JSON [Intent]
            intent_match  TEXT,                        -- JSON {matched, missing, extra, should_not}
            verdict       TEXT    CHECK (verdict IN ('pass', 'partial', 'fail')),
            verdict_by    TEXT    CHECK (verdict_by IN ('owner', 'judge', 'local_judge')),
            judge         TEXT,                        -- JSON {checks, verdict, score, notes, model}
            owner_verdict TEXT    CHECK (owner_verdict IN ('pass', 'partial', 'fail')),
            web_age_days  INTEGER
        );
        CREATE INDEX IF NOT EXISTS trial_runs_by_bot ON trial_runs (bot, engine, model, started_ms);
        CREATE INDEX IF NOT EXISTS trial_runs_by_case ON trial_runs (case_id);
        CREATE TABLE IF NOT EXISTS trial_events (
            run   TEXT    NOT NULL,
            seq   INTEGER NOT NULL,
            ts_ms INTEGER NOT NULL,
            body  TEXT    NOT NULL,  -- EventKind as JSON, as in events
            PRIMARY KEY (run, seq)
        );
        CREATE TABLE IF NOT EXISTS turn_ratings (
            bot    TEXT    NOT NULL,
            seq    INTEGER NOT NULL,  -- the reply's TurnDone event
            up     INTEGER NOT NULL,
            engine TEXT    NOT NULL,
            model  TEXT    NOT NULL,
            ms     INTEGER NOT NULL,
            PRIMARY KEY (bot, seq)
        );",
    )?;
    Ok(())
}

/// Migration 16. Skips what is already there, as 15 does.
pub(super) fn engines_and_origins(tx: &Transaction<'_>) -> Result<(), StoreError> {
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS turn_origins (
            bot        TEXT    NOT NULL,
            seq        INTEGER NOT NULL,              -- the UserMessage event that started the turn
            person     TEXT,                          -- person id the chain started with (NULL: nobody known)
            on_farm    INTEGER NOT NULL,              -- 1: started on the Farm connection
            entry      TEXT    NOT NULL,              -- 'farm' | 'gate' | 'test' | 'name'
            root_bot   TEXT    NOT NULL,              -- first message of the chain
            root_seq   INTEGER NOT NULL,
            via        TEXT    NOT NULL DEFAULT '[]', -- JSON [bot id]: bots that passed it on, in order
            engine     TEXT    NOT NULL,              -- engine that ran the turn
            tainted_by TEXT,                          -- what a relaying bot had read that oxbot doesn't trust
            PRIMARY KEY (bot, seq)
        );
        CREATE INDEX IF NOT EXISTS turn_origins_root ON turn_origins (root_bot, root_seq);",
    )?;
    let has_engine: bool = tx.query_row(
        "SELECT COUNT(*) > 0 FROM pragma_table_info('sessions') WHERE name = 'engine'",
        [],
        |r| r.get(0),
    )?;
    if !has_engine {
        tx.execute_batch("ALTER TABLE sessions ADD COLUMN engine TEXT NOT NULL DEFAULT 'claude'")?;
    }
    Ok(())
}

/// Migration 15. Skips what is already there (a database rewound to an
/// older version for a test keeps its tables and columns).
pub(super) fn usage_alerts_and_prefs(tx: &Transaction<'_>) -> Result<(), StoreError> {
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS limit_alerts (
            name         TEXT    PRIMARY KEY,
            resets_at_ms INTEGER NOT NULL,
            level        INTEGER NOT NULL,
            used         REAL    NOT NULL,
            alerted_ms   INTEGER NOT NULL
        );
        CREATE TABLE IF NOT EXISTS prefs (
            key   TEXT PRIMARY KEY,
            value TEXT NOT NULL
        );",
    )?;
    let has_model: bool = tx.query_row(
        "SELECT COUNT(*) > 0 FROM pragma_table_info('usage') WHERE name = 'model'",
        [],
        |r| r.get(0),
    )?;
    if !has_model {
        tx.execute_batch("ALTER TABLE usage ADD COLUMN model TEXT NOT NULL DEFAULT ''")?;
    }
    Ok(())
}

/// Migration 10. Skipped if the column is already there (a database
/// rewound to an older version for a test keeps its columns).
pub(super) fn sessions_remember_taint(tx: &Transaction<'_>) -> Result<(), StoreError> {
    let has: bool = tx.query_row(
        "SELECT COUNT(*) > 0 FROM pragma_table_info('sessions') WHERE name = 'tainted_by'",
        [],
        |r| r.get(0),
    )?;
    if !has {
        tx.execute_batch("ALTER TABLE sessions ADD COLUMN tainted_by TEXT")?;
    }
    Ok(())
}

/// Migration 9: everyone oxbot knew by name gets an id. Their browser
/// folder and Herder stay what they were (so no sign-in or chat is lost),
/// browser owners and Gates point at the id, and sites are named by their
/// main domain once (and checked against Chrome's cookies next start).
pub(super) fn people_get_ids(tx: &Transaction<'_>) -> Result<(), StoreError> {
    // Everyone known by name, in the order oxbot met them: people who sent
    // messages, then browser owners and Gates' people.
    let mut known: Vec<(String, Option<i64>, i64)> = Vec::new();
    for sql in [
        "SELECT name, color, first_ms FROM people ORDER BY first_ms, rowid",
        "SELECT owner, NULL, MIN(signed_in_ms) FROM sites WHERE owner <> '' GROUP BY owner ORDER BY 3",
        "SELECT person, NULL, MIN(paired_ms) FROM gates GROUP BY person ORDER BY 3",
    ] {
        let mut stmt = tx.prepare(sql)?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
        known.extend(rows.collect::<Result<Vec<_>, _>>()?);
    }
    tx.execute_batch(
        "CREATE TABLE people_v9 (
            id       TEXT    PRIMARY KEY DEFAULT ('p_' || lower(hex(randomblob(8)))),
            name     TEXT    NOT NULL UNIQUE,
            color    INTEGER NOT NULL,
            first_ms INTEGER NOT NULL,
            folder   TEXT,
            herder   TEXT,
            farm     INTEGER NOT NULL DEFAULT 0 CHECK (farm IN (0, 1))
        );
        CREATE TABLE gates_v9 (
            id           TEXT    PRIMARY KEY,
            person_id    TEXT    NOT NULL,
            name         TEXT    NOT NULL,
            key_hash     TEXT    NOT NULL UNIQUE,
            paired_ms    INTEGER NOT NULL,
            last_seen_ms INTEGER NOT NULL
        );
        CREATE TABLE IF NOT EXISTS site_checks (owner TEXT PRIMARY KEY);",
    )?;
    let ids = add_known_people(tx, &known)?;
    let key = |name: &str| ids.get(name).map_or("", PersonId::as_str).to_owned();

    // Sites: owners become ids, hosts become main domains.
    let mut stmt = tx.prepare("SELECT owner, site, signed_in_ms FROM sites")?;
    let sites: Vec<(String, String, i64)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<Result<_, _>>()?;
    drop(stmt);
    tx.execute("DELETE FROM sites", [])?;
    for (owner, site, when) in sites {
        let owner = if owner.is_empty() { owner } else { key(&owner) };
        tx.execute(
            "INSERT INTO sites (owner, site, signed_in_ms) VALUES (?1, ?2, ?3)
             ON CONFLICT(owner, site) DO UPDATE SET signed_in_ms = MAX(signed_in_ms, excluded.signed_in_ms)",
            params![owner, v9_site_of(&site), when],
        )?;
    }
    // Names changed without Chrome being asked: check them on next start.
    tx.execute(
        "INSERT OR IGNORE INTO site_checks (owner) SELECT DISTINCT owner FROM sites",
        [],
    )?;

    let mut stmt =
        tx.prepare("SELECT id, person, name, key_hash, paired_ms, last_seen_ms FROM gates")?;
    let gates: Vec<(String, String, String, String, i64, i64)> = stmt
        .query_map([], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
            ))
        })?
        .collect::<Result<_, _>>()?;
    drop(stmt);
    for (id, person, name, key_hash, paired, seen) in gates {
        tx.execute(
            "INSERT INTO gates_v9 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![id, key(&person), name, key_hash, paired, seen],
        )?;
    }
    tx.execute_batch(
        "DROP TABLE people;
        ALTER TABLE people_v9 RENAME TO people;
        DROP TABLE gates;
        ALTER TABLE gates_v9 RENAME TO gates;",
    )?;
    Ok(())
}

/// Adds everyone in `known` (name, color, first seen) to `people_v9` with a
/// new id, keeping their color, their old browser folder and their old
/// Herder id. Returns each name's id.
pub(super) fn add_known_people(
    tx: &Transaction<'_>,
    known: &[(String, Option<i64>, i64)],
) -> Result<HashMap<String, PersonId>, StoreError> {
    let mut ids: HashMap<String, PersonId> = HashMap::new();
    let mut colors: Vec<i64> = known.iter().filter_map(|k| k.1).collect();
    let mut herders = HashSet::new();
    let n = i64::try_from(PEOPLE_COLORS.len()).unwrap_or(12);
    for (name, color, first_ms) in known {
        if name.is_empty() || ids.contains_key(name) {
            continue;
        }
        let id = PersonId::random();
        let color = color.unwrap_or_else(|| {
            let c = (0..n)
                .find(|c| !colors.contains(c))
                .unwrap_or_else(|| i64::try_from(colors.len()).unwrap_or(0) % n);
            colors.push(c);
            c
        });
        // `Sam` and `sam` had the same Herder id: the first keeps it.
        let mut herder = v9_herder_id(name);
        if !herders.insert(herder.clone()) {
            herder = v9_herder_id_for(name, &id);
        }
        tx.execute(
            "INSERT INTO people_v9 (id, name, color, first_ms, folder, herder) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![id.as_str(), name, color, first_ms, v9_person_folder(name), herder],
        )?;
        ids.insert(name.clone(), id);
    }
    Ok(ids)
}

/// FNV-1a over `bytes`, as oxbot used it when migration 9 shipped.
fn v9_fnv(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325_u64, |h, b| {
        (h ^ u64::from(*b)).wrapping_mul(0x0100_0000_01b3)
    })
}

/// The browser folder oxbot made for `name` before people had ids. Frozen
/// copy of `person_folder` as migration 9 shipped.
pub(super) fn v9_person_folder(name: &str) -> String {
    let safe: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    let safe = safe.trim_matches('-');
    let safe = if safe.is_empty() { "person" } else { safe };
    format!(
        "{}-{:08x}",
        safe.chars().take(32).collect::<String>(),
        v9_fnv(name.as_bytes()) >> 32
    )
}

/// The letters and digits of `name` a Herder id starts with (at most 20).
fn v9_slug(name: &str) -> String {
    name.trim()
        .to_lowercase()
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .take(20)
        .collect()
}

/// `assistant--<slug>-<tail>`, or `assistant--<tail>` without a slug.
fn v9_herder(slug: &str, tail: &str) -> String {
    if slug.is_empty() {
        format!("assistant--{tail}")
    } else {
        format!("assistant--{slug}-{tail}")
    }
}

/// A person's Herder id from their name. Frozen copy of
/// `personal_herder_id` as migration 9 shipped.
pub(super) fn v9_herder_id(name: &str) -> String {
    let hash = v9_fnv(name.trim().to_lowercase().as_bytes());
    v9_herder(&v9_slug(name), &format!("{:06x}", hash & 0xff_ffff))
}

/// A person's Herder id from their id, when the one from their name was
/// taken. Frozen copy of `personal_herder_id_for` as migration 9 shipped.
pub(super) fn v9_herder_id_for(name: &str, id: &PersonId) -> String {
    let hex = id.as_str().trim_start_matches("p_");
    v9_herder(&v9_slug(name), hex.get(..8).unwrap_or(hex))
}

/// A host's main domain, the way migration 9 named sites. Frozen copy of
/// `oxbot_browser::site_of` (the public suffix list itself comes from the
/// `psl` crate; the golden test notices if an update changes an answer).
pub(super) fn v9_site_of(host: &str) -> String {
    let host = host.trim().trim_start_matches('.').to_ascii_lowercase();
    if host.parse::<std::net::IpAddr>().is_ok() {
        return host;
    }
    psl::domain_str(&host).map_or(host.clone(), str::to_owned)
}

/// Runs the schema changes not run yet, up to `upto`. A database a newer
/// oxbot has upgraded is refused; with `can_back_up` (a file that was
/// there before), it's copied to `<file>.v<version>.bak` before a step that
/// rebuilds tables.
pub(super) fn migrate(
    conn: &mut Connection,
    upto: usize,
    can_back_up: bool,
) -> Result<(), StoreError> {
    let done: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    let done = usize::try_from(done).unwrap_or(0);
    if done > MIGRATIONS.len() {
        return Err(StoreError::Newer {
            found: done,
            known: MIGRATIONS.len(),
        });
    }
    let mut pending = MIGRATIONS.iter().take(upto).skip(done);
    if can_back_up && pending.any(|step| matches!(step, Step::Rebuild(_))) {
        back_up(conn, done)?;
    }
    for (i, step) in MIGRATIONS.iter().enumerate().take(upto).skip(done) {
        // A migration and its version bump commit together, so a crash
        // mid-way (6 and 9 rebuild tables) leaves the database as it was.
        let tx = conn.transaction()?;
        match step {
            Step::Sql(sql) => tx.execute_batch(sql)?,
            Step::Rust(run) | Step::Rebuild(run) => run(&tx)?,
        }
        tx.execute_batch(&format!("PRAGMA user_version = {}", i + 1))?;
        tx.commit()?;
        if i + 1 == MIGRATIONS.len() && done > 0 {
            tracing::info!(from = done, to = i + 1, "upgraded the database");
        }
    }
    Ok(())
}

/// Where the copy of `db` taken at schema `version` goes.
pub(super) fn backup_path(db: &Path, version: usize) -> PathBuf {
    let mut name = db.as_os_str().to_owned();
    name.push(format!(".v{version}.bak"));
    PathBuf::from(name)
}

/// Copies a file database, as it is at schema `version`, next to itself
/// before an upgrade rebuilds tables. `VACUUM INTO` writes one consistent
/// file, WAL included. An in-memory database has nothing to keep, and a
/// copy already there (from an upgrade that failed) is kept as it is.
fn back_up(conn: &Connection, version: usize) -> Result<(), StoreError> {
    let Some(db) = conn.path().filter(|p| !p.is_empty()) else {
        return Ok(());
    };
    let to = backup_path(Path::new(db), version);
    if to.exists() {
        tracing::info!(copy = %to.display(), "a copy from before this upgrade is already there");
        return Ok(());
    }
    let failed = |why: String| StoreError::Backup {
        to: to.clone(),
        why,
    };
    let target = to
        .to_str()
        .ok_or_else(|| failed("the path isn't valid UTF-8".to_owned()))?;
    conn.execute("VACUUM INTO ?1", [target])
        .map_err(|e| failed(e.to_string()))?;
    tracing::info!(copy = %to.display(), "copied the database before upgrading it");
    Ok(())
}
