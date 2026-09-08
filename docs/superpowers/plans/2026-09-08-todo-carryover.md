# Todo carry-over Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Surface an unfinished todo from the last N days (default 7, configurable) in `/progress add` autocomplete, `/team status`, `/team report`, and `/todo list`, until a `done` report closes it or it ages out.

**Architecture:** Carry-over is a *derived query* over existing `entries` rows — no schema change. Three new pure DB helpers in `entries.rs` (`carryover_todos`, `carryover_count`, `carryover_report`) each take a `lookback_days` argument; `0` disables the feature and every helper short-circuits to empty. A new config key `[carryover] lookback_days` rides on `Config`, onto the discord `Handler` struct, and into the four command handlers that need it.

**Tech Stack:** Rust, `rusqlite` (SQLite, `PRAGMA foreign_keys=ON`, WAL), `serenity` for Discord, `chrono` / `chrono-tz` for dates, `serde` + `toml` for config. Tests: plain `#[test]` against a real `tempfile` DB (never `:memory:`).

**Spec:** `docs/superpowers/specs/2026-09-08-todo-carryover-design.md` — read it alongside this plan.

## Global Constraints

- `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, and `cargo test` must all be clean before any task is considered done. CI clippy is not `--all-targets`, so `#[cfg(test)]` warnings only fail locally — always run the `--all-targets` form.
- DB-backed tests open a fresh `tempfile::tempdir()` path via the module's `open_test_db()` helper — never `:memory:` (WAL pragma is silently ignored in-memory).
- Any test that mutates a `DISPATCHD_*` / `XDG_CONFIG_HOME` env var must hold `crate::test_support::ENV_LOCK` for the mutation. The config tests here follow the existing pattern in `src/config.rs`.
- DB logic stays free of `serenity` types and lives in `entries.rs` / `status.rs`; `src/discord/*.rs` only wires it to interactions.
- The carry-over window is `[today - lookback_days, today - 1]` inclusive — SQL `date >= date(?today, '-<lookback_days> days') AND date < ?today`. For the default 7 that is `D-7 … D-1`.
- Eligibility rule: a `type='todo'` row carries over iff **no** `type='update'` row exists with that `todo_id` and `status='done'` (newer non-done updates do not matter).
- `lookback_days` type is `u32` on `Config`/`Handler`; the `entries::` helpers take `i64` (cast at the call site with `as i64`).
- Attribution: commit messages get **no** `Co-Authored-By` / "Generated with" trailer.

---

### Task 1: Config key `[carryover] lookback_days`

**Files:**
- Modify: `src/config.rs` (add `DEFAULT_CARRYOVER_LOOKBACK_DAYS`, `Config.carryover_lookback_days`, `RawCarryover`, `RawConfig.carryover`, `from_raw` wiring, and two existing tests)
- Modify: `config.example.toml` (append a `[carryover]` block)
- Modify: `src/main.rs:201-225` (add one line to the effective-config print block)
- Test: `src/config.rs` (inline `#[cfg(test)] mod tests`)

**Interfaces:**
- Consumes: nothing.
- Produces: `Config { carryover_lookback_days: u32, .. }`, default `7`.

- [ ] **Step 1: Write the failing tests**

In `src/config.rs` `mod tests`, add:

```rust
    #[test]
    fn carryover_lookback_defaults_to_seven_and_is_overridable() {
        assert_eq!(Config::default().carryover_lookback_days, 7);

        let (_dir, path) = write_config("[carryover]\nlookback_days = 3\n");
        let raw = read_raw_config(&path).unwrap();
        let config = Config::from_raw(raw).unwrap();
        assert_eq!(config.carryover_lookback_days, 3);
    }

    #[test]
    fn carryover_lookback_accepts_zero() {
        let (_dir, path) = write_config("[carryover]\nlookback_days = 0\n");
        let raw = read_raw_config(&path).unwrap();
        let config = Config::from_raw(raw).unwrap();
        assert_eq!(config.carryover_lookback_days, 0);
    }
```

In the existing `partial_schedule_override_changes_only_that_field` test, add one more assertion next to the other `assert_eq!(config.*, defaults.*)` lines:

```rust
        assert_eq!(
            config.carryover_lookback_days,
            defaults.carryover_lookback_days
        );
```

In the existing `full_override_changes_every_field` test, add `[carryover]\nlookback_days = 2` to the TOML string it writes and assert the result:

```rust
        assert_eq!(config.carryover_lookback_days, 2);
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --lib config::`
Expected: FAIL — `no field carryover_lookback_days on type Config`.

- [ ] **Step 3: Implement the config plumbing**

In `src/config.rs`, next to the other `DEFAULT_*` consts:

```rust
const DEFAULT_CARRYOVER_LOOKBACK_DAYS: u32 = 7;
```

Add the field to `struct Config` (after `update_followup_delay_minutes`):

```rust
    /// How many days back `/progress add` and the `/team` views look for
    /// unfinished todos to carry forward. `0` disables carry-over.
    pub carryover_lookback_days: u32,
```

Add to the `impl Default for Config` block:

```rust
            carryover_lookback_days: DEFAULT_CARRYOVER_LOOKBACK_DAYS,
```

Add the raw table. Next to `RawFollowup`:

```rust
#[derive(Debug, Default, Deserialize)]
struct RawCarryover {
    lookback_days: Option<u32>,
}
```

Add to `struct RawConfig` (next to `followup`):

```rust
    #[serde(default)]
    carryover: RawCarryover,
```

In `from_raw`, in the returned `Ok(Config { .. })` literal (next to `update_followup_delay_minutes`):

```rust
            carryover_lookback_days: raw
                .carryover
                .lookback_days
                .unwrap_or(defaults.carryover_lookback_days),
```

In `config.example.toml`, append after the `[followup]` block:

```toml

[carryover]
# How many days back /progress add and the /team views look for
# unfinished todos to carry forward. A todo keeps carrying over until a
# "Done" progress report is filed against it, or until it ages past this
# window. Set to 0 to turn carry-over off.
# lookback_days = 7
```

In `src/main.rs`, in the `println!` block that prints the effective config (right after the `ticker_interval_seconds` line):

```rust
    println!(
        "  carryover_lookback_days:      {}",
        config.carryover_lookback_days
    );
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --lib config::` then `cargo fmt --check && cargo clippy --all-targets -- -D warnings`
Expected: PASS, clean.

- [ ] **Step 5: Commit**

```bash
git add src/config.rs config.example.toml src/main.rs
git commit -m "feat: add [carryover] lookback_days config key (default 7)"
```

---

### Task 2: `entries::short_date` + `entries::carryover_todos`

**Files:**
- Modify: `src/entries.rs:1` (imports) and add the two functions + `CarryoverTodo` after `list_todos` (around line 132)
- Test: `src/entries.rs` (inline `mod tests`)

**Interfaces:**
- Consumes: `Config.carryover_lookback_days` conceptually (passed as `i64` by later tasks).
- Produces:
  - `pub fn short_date(ymd: &str) -> String` — `"2026-08-27"` → `"Aug 27"`, non-parseable input returned unchanged.
  - `pub struct CarryoverTodo { pub id: i64, pub task: String, pub sow_ref: Option<String>, pub origin_date: String }` — derives `Debug, Clone, PartialEq, Eq`.
  - `pub fn carryover_todos(conn: &Connection, discord_user_id: &str, today: &str, lookback_days: i64, partial: &str) -> Result<Vec<CarryoverTodo>>` — oldest-first (`ORDER BY date, id`), capped at 25, empty when `lookback_days <= 0`.

- [ ] **Step 1: Write the failing tests**

In `src/entries.rs` `mod tests`, add:

```rust
    #[test]
    fn short_date_formats_or_passes_through() {
        assert_eq!(short_date("2026-08-27"), "Aug 27");
        assert_eq!(short_date("2026-12-01"), "Dec 1");
        assert_eq!(short_date("not a date"), "not a date");
    }

    #[test]
    fn carryover_todos_includes_recent_unfinished_todos_oldest_first() {
        let conn = open_test_db();
        let today = "2026-09-08";
        let a = insert_todo(&conn, "42", "2026-09-07", "Alpha", None, None).unwrap();
        let b = insert_todo(&conn, "42", "2026-09-06", "Bravo", None, Some("M1")).unwrap();
        insert_update(&conn, "42", "2026-09-06", "Bravo", Some(b), "blocked", "stuck", Some("ops"))
            .unwrap();

        let got = carryover_todos(&conn, "42", today, 7, "").unwrap();
        assert_eq!(
            got,
            vec![
                CarryoverTodo {
                    id: b,
                    task: "Bravo".into(),
                    sow_ref: Some("M1".into()),
                    origin_date: "2026-09-06".into(),
                },
                CarryoverTodo {
                    id: a,
                    task: "Alpha".into(),
                    sow_ref: None,
                    origin_date: "2026-09-07".into(),
                },
            ]
        );
    }

    #[test]
    fn carryover_todos_excludes_done_today_and_other_users() {
        let conn = open_test_db();
        let today = "2026-09-08";
        // has a done report -> excluded (even with a later non-done one)
        let done = insert_todo(&conn, "42", "2026-09-06", "Done one", None, None).unwrap();
        insert_update(&conn, "42", "2026-09-06", "Done one", Some(done), "done", "shipped", None)
            .unwrap();
        insert_update(&conn, "42", "2026-09-07", "Done one", Some(done), "blocked", "regressed", None)
            .unwrap();
        // today's todo -> excluded
        insert_todo(&conn, "42", today, "Today one", None, None).unwrap();
        // another user -> excluded
        let other = insert_todo(&conn, "99", "2026-09-07", "Theirs", None, None).unwrap();
        let _ = other;

        assert!(carryover_todos(&conn, "42", today, 7, "").unwrap().is_empty());
    }

    #[test]
    fn carryover_todos_window_boundary_is_inclusive() {
        let conn = open_test_db();
        let today = "2026-09-08";
        let inside = insert_todo(&conn, "42", "2026-09-01", "Inside", None, None).unwrap(); // D-7
        insert_todo(&conn, "42", "2026-08-31", "Outside", None, None).unwrap(); // D-8

        let got = carryover_todos(&conn, "42", today, 7, "").unwrap();
        assert_eq!(got.iter().map(|c| c.id).collect::<Vec<_>>(), vec![inside]);
    }

    #[test]
    fn carryover_todos_filters_by_substring() {
        let conn = open_test_db();
        insert_todo(&conn, "42", "2026-09-07", "Write parser", None, None).unwrap();
        insert_todo(&conn, "42", "2026-09-07", "Ship release", None, None).unwrap();

        let got = carryover_todos(&conn, "42", "2026-09-08", 7, "parser").unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].task, "Write parser");
    }

    #[test]
    fn carryover_todos_disabled_when_lookback_zero() {
        let conn = open_test_db();
        insert_todo(&conn, "42", "2026-09-07", "Alpha", None, None).unwrap();
        assert!(carryover_todos(&conn, "42", "2026-09-08", 0, "").unwrap().is_empty());
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --lib entries::tests::carryover entries::tests::short_date`
Expected: FAIL — `cannot find function carryover_todos` / `short_date`.

- [ ] **Step 3: Implement**

In `src/entries.rs`, change the top import line:

```rust
use chrono::{NaiveDate, Utc};
```

(The `use chrono::Utc;` becomes `use chrono::{NaiveDate, Utc};`. `chrono_tz::Tz` and the `rusqlite` line stay.)

After `list_todos` (ends around line 132), add:

```rust
/// `"2026-08-27"` -> `"Aug 27"`. Input that doesn't parse as `YYYY-MM-DD`
/// is returned unchanged. Used to label carried-over todos in the
/// `/progress add` autocomplete, `/todo list`, and `/team report`.
pub fn short_date(ymd: &str) -> String {
    NaiveDate::parse_from_str(ymd, "%Y-%m-%d")
        .map(|d| d.format("%b %-d").to_string())
        .unwrap_or_else(|_| ymd.to_string())
}

/// A still-open todo from a recent past day, carried forward into today's
/// `/progress add` autocomplete and `/todo list`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CarryoverTodo {
    pub id: i64,
    pub task: String,
    pub sow_ref: Option<String>,
    /// The `YYYY-MM-DD` the todo was originally created on.
    pub origin_date: String,
}

/// This user's still-open todos from the `lookback_days` calendar days
/// before `today` (window `[today - lookback_days, today - 1]`) -
/// `type = 'todo'` rows with no `status = 'done'` update against them
/// (matched via `todo_id`), optionally filtered by a `task` substring.
/// Ordered oldest-first, capped at 25 (Discord's autocomplete limit).
/// Empty when `lookback_days <= 0` (carry-over disabled).
pub fn carryover_todos(
    conn: &Connection,
    discord_user_id: &str,
    today: &str,
    lookback_days: i64,
    partial: &str,
) -> Result<Vec<CarryoverTodo>> {
    if lookback_days <= 0 {
        return Ok(Vec::new());
    }
    let window_start = format!("-{lookback_days} days");
    let mut stmt = conn.prepare(
        "SELECT t.id, t.task, t.sow_ref, t.date FROM entries t
         WHERE t.type = 'todo' AND t.discord_user_id = ?1
           AND t.date >= date(?2, ?3) AND t.date < ?2
           AND t.task LIKE '%' || ?4 || '%'
           AND NOT EXISTS (
             SELECT 1 FROM entries u
             WHERE u.type = 'update' AND u.todo_id = t.id AND u.status = 'done'
           )
         ORDER BY t.date, t.id
         LIMIT 25",
    )?;
    let rows = stmt
        .query_map(
            params![discord_user_id, today, window_start, partial],
            |row| {
                Ok(CarryoverTodo {
                    id: row.get(0)?,
                    task: row.get(1)?,
                    sow_ref: row.get(2)?,
                    origin_date: row.get(3)?,
                })
            },
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --lib entries::` then `cargo fmt --check && cargo clippy --all-targets -- -D warnings`
Expected: PASS, clean.

- [ ] **Step 5: Commit**

```bash
git add src/entries.rs
git commit -m "feat: entries::carryover_todos + short_date helper"
```

---

### Task 3: `entries::carryover_count`

**Files:**
- Modify: `src/entries.rs` (add `carryover_count` right after `carryover_todos`)
- Test: `src/entries.rs` (inline `mod tests`)

**Interfaces:**
- Consumes: the carry-over rule / window (Task 2).
- Produces: `pub fn carryover_count(conn: &Connection, discord_user_id: &str, today: &str, lookback_days: i64) -> Result<i64>` — `0` when `lookback_days <= 0`.

- [ ] **Step 1: Write the failing test**

```rust
    #[test]
    fn carryover_count_matches_the_list_length() {
        let conn = open_test_db();
        let today = "2026-09-08";
        insert_todo(&conn, "42", "2026-09-07", "Alpha", None, None).unwrap();
        let b = insert_todo(&conn, "42", "2026-09-06", "Bravo", None, None).unwrap();
        insert_update(&conn, "42", "2026-09-06", "Bravo", Some(b), "in_progress", "wip", None)
            .unwrap();
        let c = insert_todo(&conn, "42", "2026-09-05", "Charlie", None, None).unwrap();
        insert_update(&conn, "42", "2026-09-05", "Charlie", Some(c), "done", "done", None).unwrap();

        assert_eq!(carryover_count(&conn, "42", today, 7).unwrap(), 2);
        assert_eq!(carryover_count(&conn, "42", today, 0).unwrap(), 0);
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test --lib entries::tests::carryover_count`
Expected: FAIL — `cannot find function carryover_count`.

- [ ] **Step 3: Implement**

In `src/entries.rs`, immediately after `carryover_todos`:

```rust
/// Count of this user's still-open carried-over todos (see
/// `carryover_todos`). `0` when `lookback_days <= 0`.
pub fn carryover_count(
    conn: &Connection,
    discord_user_id: &str,
    today: &str,
    lookback_days: i64,
) -> Result<i64> {
    if lookback_days <= 0 {
        return Ok(0);
    }
    let window_start = format!("-{lookback_days} days");
    let n = conn.query_row(
        "SELECT COUNT(*) FROM entries t
         WHERE t.type = 'todo' AND t.discord_user_id = ?1
           AND t.date >= date(?2, ?3) AND t.date < ?2
           AND NOT EXISTS (
             SELECT 1 FROM entries u
             WHERE u.type = 'update' AND u.todo_id = t.id AND u.status = 'done'
           )",
        params![discord_user_id, today, window_start],
        |row| row.get(0),
    )?;
    Ok(n)
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --lib entries::` then `cargo fmt --check && cargo clippy --all-targets -- -D warnings`
Expected: PASS, clean.

- [ ] **Step 5: Commit**

```bash
git add src/entries.rs
git commit -m "feat: entries::carryover_count"
```

---

### Task 4: `entries::carryover_report`

**Files:**
- Modify: `src/entries.rs` (add `CarryoverDetail` + `carryover_report` after `carryover_count`)
- Test: `src/entries.rs` (inline `mod tests`)

**Interfaces:**
- Consumes: `carryover_todos` (Task 2).
- Produces:
  - `pub struct CarryoverDetail { pub task: String, pub sow_ref: Option<String>, pub origin_date: String, pub latest_status: Option<String>, pub latest_progress: Option<String>, pub latest_blocker: Option<String>, pub updated_today: bool }` — derives `Debug, Clone, PartialEq, Eq`.
  - `pub fn carryover_report(conn: &Connection, discord_user_id: &str, today: &str, lookback_days: i64) -> Result<Vec<CarryoverDetail>>` — same ordering/window/eligibility as `carryover_todos`; empty when `lookback_days <= 0`.

- [ ] **Step 1: Write the failing test**

```rust
    #[test]
    fn carryover_report_carries_latest_status_and_updated_today_flag() {
        let conn = open_test_db();
        let today = "2026-09-08";
        let a = insert_todo(&conn, "42", "2026-09-05", "Alpha", None, Some("M2")).unwrap();
        insert_update(&conn, "42", "2026-09-05", "Alpha", Some(a), "in_progress", "day 1", None)
            .unwrap();
        insert_update(
            &conn, "42", today, "Alpha", Some(a), "blocked", "hit a wall", Some("need creds"),
        )
        .unwrap();
        // carried but never reported on
        insert_todo(&conn, "42", "2026-09-06", "Bravo", None, None).unwrap();

        let got = carryover_report(&conn, "42", today, 7).unwrap();
        assert_eq!(got.len(), 2);

        assert_eq!(got[0].task, "Alpha");
        assert_eq!(got[0].sow_ref.as_deref(), Some("M2"));
        assert_eq!(got[0].origin_date, "2026-09-05");
        assert_eq!(got[0].latest_status.as_deref(), Some("blocked"));
        assert_eq!(got[0].latest_progress.as_deref(), Some("hit a wall"));
        assert_eq!(got[0].latest_blocker.as_deref(), Some("need creds"));
        assert!(got[0].updated_today);

        assert_eq!(got[1].task, "Bravo");
        assert_eq!(got[1].latest_status, None);
        assert_eq!(got[1].latest_progress, None);
        assert_eq!(got[1].latest_blocker, None);
        assert!(!got[1].updated_today);
    }

    #[test]
    fn carryover_report_empty_when_disabled() {
        let conn = open_test_db();
        insert_todo(&conn, "42", "2026-09-07", "Alpha", None, None).unwrap();
        assert!(carryover_report(&conn, "42", "2026-09-08", 0).unwrap().is_empty());
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test --lib entries::tests::carryover_report`
Expected: FAIL — `cannot find function carryover_report`.

- [ ] **Step 3: Implement**

In `src/entries.rs`, after `carryover_count`:

```rust
/// A carried-over todo plus its latest progress report and whether it
/// moved today - the `/team report` "Carried over" view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CarryoverDetail {
    pub task: String,
    pub sow_ref: Option<String>,
    pub origin_date: String,
    /// Status of the most recent linked update (any date), or `None` if
    /// the todo has never had a progress report.
    pub latest_status: Option<String>,
    pub latest_progress: Option<String>,
    pub latest_blocker: Option<String>,
    /// True when at least one linked update is dated `today`.
    pub updated_today: bool,
}

/// Per-todo detail for the `/team report` "Carried over" block. Same
/// window / eligibility / ordering as `carryover_todos`. Empty when
/// `lookback_days <= 0`.
pub fn carryover_report(
    conn: &Connection,
    discord_user_id: &str,
    today: &str,
    lookback_days: i64,
) -> Result<Vec<CarryoverDetail>> {
    let todos = carryover_todos(conn, discord_user_id, today, lookback_days, "")?;
    let mut out = Vec::with_capacity(todos.len());
    for t in todos {
        let latest = conn
            .query_row(
                "SELECT status, progress, blocker FROM entries
                 WHERE type = 'update' AND todo_id = ?1
                 ORDER BY id DESC LIMIT 1",
                params![t.id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<String>>(2)?,
                    ))
                },
            )
            .optional()?;
        let updated_today: bool = conn.query_row(
            "SELECT EXISTS(
               SELECT 1 FROM entries
               WHERE type = 'update' AND todo_id = ?1 AND date = ?2
             )",
            params![t.id, today],
            |row| row.get(0),
        )?;
        let (latest_status, latest_progress, latest_blocker) = match latest {
            Some((s, p, b)) => (Some(s), Some(p), b),
            None => (None, None, None),
        };
        out.push(CarryoverDetail {
            task: t.task,
            sow_ref: t.sow_ref,
            origin_date: t.origin_date,
            latest_status,
            latest_progress,
            latest_blocker,
            updated_today,
        });
    }
    Ok(out)
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --lib entries::` then `cargo fmt --check && cargo clippy --all-targets -- -D warnings`
Expected: PASS, clean.

- [ ] **Step 5: Commit**

```bash
git add src/entries.rs
git commit -m "feat: entries::carryover_report"
```

---

### Task 5: `/team status` carry-over aware (end to end)

**Files:**
- Modify: `src/status.rs` — `MemberStatus` gains `carried_count`; `team_status` gains a `carryover_lookback_days: i64` param, fixes `matched_update_count`, and populates `carried_count`; `format_status_line` gains the `+N carried` / `no new todo` rendering. Update the ~9 existing `team_status(&conn, DATE)` test calls to `team_status(&conn, DATE, 0)`.
- Modify: `src/discord/team.rs` — `handle_status` gains `carryover_lookback_days: u32`, passes `as i64` to `team_status`.
- Modify: `src/discord/mod.rs` — `Handler` struct gains `carryover_lookback_days: u32`; `run()` sets it from `config`; the `"status"` dispatch arm passes `self.carryover_lookback_days`.
- Test: `src/status.rs` (inline `mod tests`)

**Interfaces:**
- Consumes: `entries::carryover_count` (Task 3), `Config.carryover_lookback_days` (Task 1).
- Produces:
  - `pub struct MemberStatus { .., pub carried_count: i64 }`
  - `pub fn team_status(conn: &Connection, date: &str, carryover_lookback_days: i64) -> Result<Vec<MemberStatus>>`
  - `Handler { .., carryover_lookback_days: u32 }` (used by Tasks 6–8 too).

- [ ] **Step 1: Write the failing tests**

In `src/status.rs` `mod tests`, add:

```rust
    #[test]
    fn matched_count_ignores_updates_against_past_day_todos() {
        let conn = open_test_db();
        seed_member(&conn, "1", "Alice", "lead");
        entries::insert_todo(&conn, "1", DATE, "today task", None, None).unwrap();
        let past = entries::insert_todo(&conn, "1", "2026-08-20", "old task", None, None).unwrap();
        entries::insert_update(&conn, "1", DATE, "old task", Some(past), "in_progress", "wip", None)
            .unwrap();

        let statuses = team_status(&conn, DATE, 0).unwrap();
        assert_eq!(statuses[0].todo_count, 1);
        assert_eq!(statuses[0].matched_update_count, 0);
    }

    #[test]
    fn carried_count_appends_suffix_when_there_are_todos_today() {
        let conn = open_test_db();
        seed_member(&conn, "1", "Alice", "lead");
        entries::insert_todo(&conn, "1", DATE, "today task", None, None).unwrap();
        entries::insert_todo(&conn, "1", "2026-08-27", "old task", None, None).unwrap();

        let statuses = team_status(&conn, DATE, 7).unwrap();
        assert_eq!(statuses[0].carried_count, 1);
        assert_eq!(
            format_status_line(&statuses[0]),
            "❌ Alice - 0/1 updated +1 carried"
        );
    }

    #[test]
    fn no_new_todo_but_carried_shows_a_warning_line() {
        let conn = open_test_db();
        seed_member(&conn, "1", "Alice", "lead");
        entries::insert_todo(&conn, "1", "2026-08-27", "old task", None, None).unwrap();

        let statuses = team_status(&conn, DATE, 7).unwrap();
        assert_eq!(
            format_status_line(&statuses[0]),
            "⚠️ Alice - no new todo (1 carried)"
        );
    }

    #[test]
    fn carried_count_zero_leaves_the_line_unchanged() {
        let conn = open_test_db();
        seed_member(&conn, "1", "Alice", "lead");
        entries::insert_todo(&conn, "1", DATE, "today task", None, None).unwrap();

        let statuses = team_status(&conn, DATE, 7).unwrap();
        assert_eq!(statuses[0].carried_count, 0);
        assert_eq!(format_status_line(&statuses[0]), "❌ Alice - 0/1 updated");
    }
```

Then update every existing `team_status(&conn, DATE)` call in this test module to `team_status(&conn, DATE, 0)` (9 sites: the tests `fully_matched_member_shows_green_check`, `partially_matched_member_shows_warning`, `member_with_no_todos_shows_no_fraction`, `member_with_todos_but_zero_matches_shows_red`, `ad_hoc_update_does_not_count_toward_matched`, `two_updates_against_the_same_todo_still_count_as_one_match`, `sow_refs_are_appended_in_first_seen_order`, `repeated_sow_ref_across_todos_appears_once`, `no_sow_refs_leaves_the_line_unchanged`).

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --lib status::`
Expected: FAIL — `team_status` takes 2 arguments but 3 were supplied / `no field carried_count`.

- [ ] **Step 3: Implement**

In `src/status.rs`:

Add to `struct MemberStatus` (after `sow_refs`):

```rust
    /// Count of this member's still-open carried-over todos (see
    /// `entries::carryover_count`). `0` when carry-over is disabled.
    pub carried_count: i64,
```

Change `team_status`'s signature:

```rust
pub fn team_status(
    conn: &Connection,
    date: &str,
    carryover_lookback_days: i64,
) -> Result<Vec<MemberStatus>> {
```

Change the `matched_update_count` query string (add the last `AND`):

```rust
        let matched_update_count: i64 = conn.query_row(
            "SELECT COUNT(DISTINCT todo_id) FROM entries
             WHERE type = 'update' AND date = ?1 AND discord_user_id = ?2 AND todo_id IS NOT NULL
               AND todo_id IN (
                 SELECT id FROM entries
                 WHERE type = 'todo' AND date = ?1 AND discord_user_id = ?2
               )",
            params![date, discord_user_id],
            |row| row.get(0),
        )?;
```

Just before `result.push(MemberStatus { .. })`:

```rust
        let carried_count = crate::entries::carryover_count(
            conn,
            &discord_user_id,
            date,
            carryover_lookback_days,
        )?;
```

Add `carried_count,` to that `MemberStatus { .. }` literal.

Replace `format_status_line`:

```rust
pub fn format_status_line(status: &MemberStatus) -> String {
    let base = if status.todo_count == 0 {
        if status.carried_count > 0 {
            format!(
                "⚠️ {} - no new todo ({} carried)",
                status.name, status.carried_count
            )
        } else {
            format!("❌ {} - no todo posted", status.name)
        }
    } else {
        let emoji = if status.matched_update_count == status.todo_count {
            "✅"
        } else if status.matched_update_count == 0 {
            "❌"
        } else {
            "⚠️"
        };
        format!(
            "{emoji} {} - {}/{} updated",
            status.name, status.matched_update_count, status.todo_count
        )
    };
    let base = if status.sow_refs.is_empty() {
        base
    } else {
        format!("{base} ({})", status.sow_refs.join(", "))
    };
    if status.todo_count > 0 && status.carried_count > 0 {
        format!("{base} +{} carried", status.carried_count)
    } else {
        base
    }
}
```

In `src/discord/team.rs`, `handle_status`:

```rust
pub async fn handle_status(
    ctx: &SerenityContext,
    command: &CommandInteraction,
    db: &Arc<Mutex<Connection>>,
    timezone: &Tz,
    carryover_lookback_days: u32,
) {
```

and change the call:

```rust
            Ok(true) => match status::team_status(&conn, &date, carryover_lookback_days as i64) {
```

In `src/discord/mod.rs`:

Add the field to `struct Handler` (after `timezone: Tz,`):

```rust
    carryover_lookback_days: u32,
```

In `pub async fn run(...)`, in the `.event_handler(Handler { .. })` literal (after `timezone: config.timezone,`):

```rust
            carryover_lookback_days: config.carryover_lookback_days,
```

In the `"status"` arm of the `"team"` match:

```rust
                    Some(("status", _)) => {
                        team::handle_status(
                            &ctx,
                            &command,
                            &self.db,
                            &self.timezone,
                            self.carryover_lookback_days,
                        )
                        .await
                    }
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --lib` then `cargo fmt --check && cargo clippy --all-targets -- -D warnings`
Expected: PASS, clean. (The whole lib builds — `team_report` still has its old arity, that's Task 6.)

- [ ] **Step 5: Commit**

```bash
git add src/status.rs src/discord/team.rs src/discord/mod.rs
git commit -m "feat: /team status shows +N carried and stops counting carry-over updates as today's"
```

---

### Task 6: `/team report` carry-over aware (end to end)

**Files:**
- Modify: `src/status.rs` — `MemberReport` gains `carried: Vec<crate::entries::CarryoverDetail>`; `team_report` gains a `carryover_lookback_days: i64` param and populates it; `format_report`'s empty-check and body render the `Carried over:` block. Fix the 3 test `MemberReport { .. }` literals (`sample_report`, and the two inline ones) + update the ~7 existing `team_report(&conn, DATE)` calls to `team_report(&conn, DATE, 0)`.
- Modify: `src/discord/team.rs` — `handle_report` gains `carryover_lookback_days: u32`, passes `as i64`.
- Modify: `src/discord/mod.rs` — the `"report"` dispatch arm passes `self.carryover_lookback_days`.
- Test: `src/status.rs` (inline `mod tests`)

**Interfaces:**
- Consumes: `entries::carryover_report` + `entries::short_date` (Tasks 2, 4), `Handler.carryover_lookback_days` (Task 5).
- Produces:
  - `pub struct MemberReport { .., pub carried: Vec<crate::entries::CarryoverDetail> }`
  - `pub fn team_report(conn: &Connection, date: &str, carryover_lookback_days: i64) -> Result<Vec<MemberReport>>`

- [ ] **Step 1: Write the failing tests**

In `src/status.rs` `mod tests`, add:

```rust
    #[test]
    fn format_report_renders_the_carried_over_block() {
        let report = MemberReport {
            name: "Alice".into(),
            todos: vec![],
            ad_hoc: vec![],
            carried: vec![
                entries::CarryoverDetail {
                    task: "Refactor auth".into(),
                    sow_ref: Some("M2".into()),
                    origin_date: "2026-08-27".into(),
                    latest_status: Some("in_progress".into()),
                    latest_progress: Some("traced the leak".into()),
                    latest_blocker: None,
                    updated_today: true,
                },
                entries::CarryoverDetail {
                    task: "Write migration".into(),
                    sow_ref: None,
                    origin_date: "2026-08-25".into(),
                    latest_status: None,
                    latest_progress: None,
                    latest_blocker: None,
                    updated_today: false,
                },
            ],
        };
        assert_eq!(
            format_report(&report),
            "**Alice**\n\
             Carried over:\n\
             • Refactor auth [M2] — from Aug 27 · ⏳ in progress · updated today\n\
             \u{20}\u{20}traced the leak\n\
             • Write migration — from Aug 25 · no report yet"
        );
    }

    #[test]
    fn format_report_carried_over_shows_blocker() {
        let report = MemberReport {
            name: "Budi".into(),
            todos: vec![],
            ad_hoc: vec![],
            carried: vec![entries::CarryoverDetail {
                task: "Ship it".into(),
                sow_ref: None,
                origin_date: "2026-08-26".into(),
                latest_status: Some("blocked".into()),
                latest_progress: Some("waiting".into()),
                latest_blocker: Some("DBA review".into()),
                updated_today: false,
            }],
        };
        assert_eq!(
            format_report(&report),
            "**Budi**\n\
             Carried over:\n\
             • Ship it — from Aug 26 · ⛔ blocked\n\
             \u{20}\u{20}waiting\n\
             \u{20}\u{20}(blocker: DBA review)"
        );
    }

    #[test]
    fn team_report_populates_carried_from_past_days() {
        let conn = open_test_db();
        seed_member(&conn, "1", "Alice", "lead");
        entries::insert_todo(&conn, "1", "2026-08-27", "old task", None, None).unwrap();

        let reports = team_report(&conn, DATE, 7).unwrap();
        assert_eq!(reports[0].carried.len(), 1);
        assert_eq!(reports[0].carried[0].task, "old task");
        // still renders even though nothing was posted *today*
        assert!(format_report(&reports[0]).contains("Carried over:"));
    }
```

Then:
- add `carried: vec![],` to the `MemberReport { .. }` returned by `sample_report()`.
- add `carried: vec![],` to the inline `MemberReport { .. }` in `format_report_empty_member_is_a_single_line` and in `format_report_unknown_status_falls_back_to_verbatim`.
- update the 7 existing `team_report(&conn, DATE)` calls to `team_report(&conn, DATE, 0)` (tests `team_report_nests_updates_under_their_todo_in_id_order`, `team_report_puts_unmatched_updates_in_ad_hoc`, `team_report_includes_members_with_nothing_ordered_by_name`, `team_report_todo_with_no_update_has_empty_updates`, `team_report_pipeline_chunks_without_losing_content`, `team_report_separates_matched_and_ad_hoc_for_one_member`, `team_report_does_not_leak_one_members_updates_into_another`).

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --lib status::`
Expected: FAIL — `team_report` arity / `missing field carried`.

- [ ] **Step 3: Implement**

In `src/status.rs`:

Add to `struct MemberReport` (after `ad_hoc`):

```rust
    /// Still-open todos from recent past days (see
    /// `entries::carryover_report`). Empty when carry-over is disabled.
    pub carried: Vec<crate::entries::CarryoverDetail>,
```

Change `team_report`'s signature:

```rust
pub fn team_report(
    conn: &Connection,
    date: &str,
    carryover_lookback_days: i64,
) -> Result<Vec<MemberReport>> {
```

Just before `reports.push(MemberReport { .. })`:

```rust
        let carried = crate::entries::carryover_report(
            conn,
            &discord_user_id,
            date,
            carryover_lookback_days,
        )?;
```

Add `carried,` to that `MemberReport { .. }` literal.

In `format_report`, change the empty guard:

```rust
    if report.todos.is_empty() && report.ad_hoc.is_empty() && report.carried.is_empty() {
        return format!("**{}** - nothing posted today", report.name);
    }
```

At the end of `format_report`, after the `for update in &report.ad_hoc { .. }` loop and before `out`:

```rust
    if !report.carried.is_empty() {
        out.push_str("\nCarried over:");
        for c in &report.carried {
            out.push('\n');
            let sow = c
                .sow_ref
                .as_deref()
                .map(|r| format!(" [{r}]"))
                .unwrap_or_default();
            out.push_str(&format!(
                "• {}{} — from {}",
                c.task,
                sow,
                crate::entries::short_date(&c.origin_date)
            ));
            match &c.latest_status {
                Some(status) => {
                    let (glyph, label) = status_glyph_label(status);
                    out.push_str(&format!(" · {glyph} {label}"));
                    if c.updated_today {
                        out.push_str(" · updated today");
                    }
                    if let Some(progress) = &c.latest_progress {
                        out.push_str(&format!("\n  {progress}"));
                    }
                    if let Some(blocker) = &c.latest_blocker {
                        out.push_str(&format!("\n  (blocker: {blocker})"));
                    }
                }
                None => out.push_str(" · no report yet"),
            }
        }
    }
```

In `src/discord/team.rs`, `handle_report`:

```rust
pub async fn handle_report(
    ctx: &SerenityContext,
    command: &CommandInteraction,
    db: &Arc<Mutex<Connection>>,
    timezone: &Tz,
    carryover_lookback_days: u32,
) {
```

and the call:

```rust
            Ok(true) => match status::team_report(&conn, &date, carryover_lookback_days as i64) {
```

In `src/discord/mod.rs`, the `"report"` arm:

```rust
                    Some(("report", _)) => {
                        team::handle_report(
                            &ctx,
                            &command,
                            &self.db,
                            &self.timezone,
                            self.carryover_lookback_days,
                        )
                        .await
                    }
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --lib` then `cargo fmt --check && cargo clippy --all-targets -- -D warnings`
Expected: PASS, clean.

- [ ] **Step 5: Commit**

```bash
git add src/status.rs src/discord/team.rs src/discord/mod.rs
git commit -m "feat: /team report shows a Carried over block per member"
```

---

### Task 7: `/progress add` autocomplete unions carried-over todos

**Files:**
- Modify: `src/discord/progress.rs` — add `use std::collections::HashSet;`; add pure `merge_task_choices`; `handle_autocomplete` gains `carryover_lookback_days: u32` and the `"add"` branch unions `list_open_todos` + `carryover_todos`.
- Modify: `src/discord/mod.rs` — the `"progress"` autocomplete dispatch passes `self.carryover_lookback_days`.
- Test: `src/discord/progress.rs` (inline `mod tests`)

**Interfaces:**
- Consumes: `entries::list_open_todos` (existing), `entries::carryover_todos` + `entries::CarryoverTodo` + `entries::short_date` (Task 2), `Handler.carryover_lookback_days` (Task 5).
- Produces: `fn merge_task_choices(today: Vec<(i64, String)>, carried: Vec<entries::CarryoverTodo>) -> Vec<(String, String)>` — `(label, option_value)` pairs, today's first, deduped by id, capped at 25.

- [ ] **Step 1: Write the failing tests**

In `src/discord/progress.rs` `mod tests`:

```rust
    #[test]
    fn merge_task_choices_puts_today_first_then_carried_labelled() {
        let today = vec![(1, "Write parser".to_string())];
        let carried = vec![crate::entries::CarryoverTodo {
            id: 2,
            task: "Refactor auth".to_string(),
            sow_ref: Some("M2".to_string()),
            origin_date: "2026-08-27".to_string(),
        }];
        let got = merge_task_choices(today, carried);
        assert_eq!(
            got,
            vec![
                ("Write parser".to_string(), "id:1".to_string()),
                (
                    "Refactor auth · carried from Aug 27".to_string(),
                    "id:2".to_string()
                ),
            ]
        );
    }

    #[test]
    fn merge_task_choices_dedups_by_id_and_caps_at_25() {
        let today: Vec<(i64, String)> = (0..30).map(|i| (i, format!("t{i}"))).collect();
        let carried = vec![crate::entries::CarryoverTodo {
            id: 0, // already in `today`
            task: "dup".to_string(),
            sow_ref: None,
            origin_date: "2026-08-27".to_string(),
        }];
        let got = merge_task_choices(today, carried);
        assert_eq!(got.len(), 25);
        assert_eq!(got.iter().filter(|(_, v)| v == "id:0").count(), 1);
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --lib discord::progress::tests::merge_task_choices`
Expected: FAIL — `cannot find function merge_task_choices`.

- [ ] **Step 3: Implement**

In `src/discord/progress.rs`, add to the imports at the top:

```rust
use std::collections::HashSet;
```

Add the helper (near `encode_task_for_modal`):

```rust
/// Merges today's open todos with carried-over ones for the `/progress
/// add` task autocomplete: today's first, then carry-over, deduped by
/// todo id, capped at Discord's 25-choice limit. Returns
/// `(label, option_value)` pairs; carry-over labels carry the origin
/// date. Pure - no serenity types.
fn merge_task_choices(
    today: Vec<(i64, String)>,
    carried: Vec<entries::CarryoverTodo>,
) -> Vec<(String, String)> {
    let mut seen: HashSet<i64> = HashSet::new();
    let mut out: Vec<(String, String)> = Vec::new();
    for (id, task) in today {
        if out.len() >= 25 {
            break;
        }
        if seen.insert(id) {
            out.push((task, format!("id:{id}")));
        }
    }
    for c in carried {
        if out.len() >= 25 {
            break;
        }
        if seen.insert(c.id) {
            out.push((
                format!(
                    "{} · carried from {}",
                    c.task,
                    entries::short_date(&c.origin_date)
                ),
                format!("id:{}", c.id),
            ));
        }
    }
    out
}
```

Change `handle_autocomplete`'s signature:

```rust
pub async fn handle_autocomplete(
    ctx: &SerenityContext,
    autocomplete: &AutocompleteInteraction,
    db: &Arc<Mutex<Connection>>,
    timezone: &Tz,
    carryover_lookback_days: u32,
) {
```

Replace the `"add" => { .. }` arm body with:

```rust
        "add" => {
            let partial = get_option_string(options, "task").unwrap_or_default();
            let result = {
                let conn = db.lock().expect("db mutex poisoned");
                let today =
                    entries::list_open_todos(&conn, &discord_user_id, &date, &partial);
                let carried = entries::carryover_todos(
                    &conn,
                    &discord_user_id,
                    &date,
                    carryover_lookback_days as i64,
                    &partial,
                );
                today.and_then(|t| carried.map(|c| (t, c)))
            };
            match result {
                Ok((today, carried)) => CreateAutocompleteResponse::new().set_choices(
                    merge_task_choices(today, carried)
                        .into_iter()
                        .map(|(label, value)| AutocompleteChoice::new(label, value))
                        .collect(),
                ),
                Err(e) => {
                    eprintln!("failed to list todos for /progress add autocomplete: {e}");
                    CreateAutocompleteResponse::new()
                }
            }
        }
```

In `src/discord/mod.rs`, the `"progress"` autocomplete arm:

```rust
                "progress" => {
                    progress::handle_autocomplete(
                        &ctx,
                        &autocomplete,
                        &self.db,
                        &self.timezone,
                        self.carryover_lookback_days,
                    )
                    .await
                }
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --lib` then `cargo fmt --check && cargo clippy --all-targets -- -D warnings`
Expected: PASS, clean.

- [ ] **Step 5: Commit**

```bash
git add src/discord/progress.rs src/discord/mod.rs
git commit -m "feat: /progress add autocomplete offers carried-over todos"
```

---

### Task 8: `/todo list` shows a "Carried over" section

**Files:**
- Modify: `src/discord/todo.rs` — add pure `format_todo_list`; `handle_list` gains `carryover_lookback_days: u32`, fetches carry-over, renders via the helper.
- Modify: `src/discord/mod.rs` — the `"list"` dispatch arm for `"todo"` passes `self.carryover_lookback_days`.
- Test: `src/discord/todo.rs` (inline `mod tests`)

**Interfaces:**
- Consumes: `entries::list_todos` (existing), `entries::carryover_todos` + `entries::CarryoverTodo` + `entries::short_date` (Task 2), `Handler.carryover_lookback_days` (Task 5).
- Produces: `fn format_todo_list(today: &[(i64, String, Option<String>)], carried: &[entries::CarryoverTodo]) -> String`.

- [ ] **Step 1: Write the failing tests**

In `src/discord/todo.rs` `mod tests`:

```rust
    #[test]
    fn format_todo_list_today_only() {
        let today = vec![
            (12, "Write tests".to_string(), Some("M1D2".to_string())),
            (13, "Ship the release".to_string(), None),
        ];
        assert_eq!(
            format_todo_list(&today, &[]),
            "**Today's todos:**\n`12` Write tests [M1D2]\n`13` Ship the release"
        );
    }

    #[test]
    fn format_todo_list_appends_carried_over_section() {
        let today = vec![(13, "Ship the release".to_string(), None)];
        let carried = vec![
            crate::entries::CarryoverTodo {
                id: 1,
                task: "Refactor auth".to_string(),
                sow_ref: Some("M2".to_string()),
                origin_date: "2026-08-27".to_string(),
            },
            crate::entries::CarryoverTodo {
                id: 2,
                task: "Write migration".to_string(),
                sow_ref: None,
                origin_date: "2026-08-25".to_string(),
            },
        ];
        assert_eq!(
            format_todo_list(&today, &carried),
            "**Today's todos:**\n`13` Ship the release\n\n\
             **Carried over (still open):**\n\
             Refactor auth [M2] — from Aug 27\n\
             Write migration — from Aug 25"
        );
    }

    #[test]
    fn format_todo_list_no_todos_today_but_carried() {
        let carried = vec![crate::entries::CarryoverTodo {
            id: 1,
            task: "Refactor auth".to_string(),
            sow_ref: None,
            origin_date: "2026-08-27".to_string(),
        }];
        assert_eq!(
            format_todo_list(&[], &carried),
            "You haven't submitted a todo today yet - use `/todo add`.\n\n\
             **Carried over (still open):**\n\
             Refactor auth — from Aug 27"
        );
    }

    #[test]
    fn format_todo_list_completely_empty() {
        assert_eq!(
            format_todo_list(&[], &[]),
            "You haven't submitted a todo today yet - use `/todo add`."
        );
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --lib discord::todo::tests::format_todo_list`
Expected: FAIL — `cannot find function format_todo_list`.

- [ ] **Step 3: Implement**

In `src/discord/todo.rs`, add the helper (near `handle_list`):

```rust
/// Builds the `/todo list` reply: today's todos with ids, then a
/// read-only "Carried over" section for still-open todos from recent
/// past days. Pure.
fn format_todo_list(
    today: &[(i64, String, Option<String>)],
    carried: &[entries::CarryoverTodo],
) -> String {
    let mut sections: Vec<String> = Vec::new();
    if today.is_empty() {
        sections.push(
            "You haven't submitted a todo today yet - use `/todo add`.".to_string(),
        );
    } else {
        let lines: Vec<String> = today
            .iter()
            .map(|(id, task, sow_ref)| match sow_ref {
                Some(r) => format!("`{id}` {task} [{r}]"),
                None => format!("`{id}` {task}"),
            })
            .collect();
        sections.push(format!("**Today's todos:**\n{}", lines.join("\n")));
    }
    if !carried.is_empty() {
        let lines: Vec<String> = carried
            .iter()
            .map(|c| match &c.sow_ref {
                Some(r) => format!(
                    "{} [{r}] — from {}",
                    c.task,
                    entries::short_date(&c.origin_date)
                ),
                None => format!("{} — from {}", c.task, entries::short_date(&c.origin_date)),
            })
            .collect();
        sections.push(format!(
            "**Carried over (still open):**\n{}",
            lines.join("\n")
        ));
    }
    sections.join("\n\n")
}
```

Replace `handle_list` with:

```rust
pub async fn handle_list(
    ctx: &SerenityContext,
    command: &CommandInteraction,
    db: &Arc<Mutex<Connection>>,
    timezone: &Tz,
    carryover_lookback_days: u32,
) {
    let discord_user_id = command.user.id.to_string();
    let date = entries::today_in(timezone);

    let result = {
        let conn = db.lock().expect("db mutex poisoned");
        let today = entries::list_todos(&conn, &discord_user_id, &date, "");
        let carried = entries::carryover_todos(
            &conn,
            &discord_user_id,
            &date,
            carryover_lookback_days as i64,
            "",
        );
        today.and_then(|t| carried.map(|c| (t, c)))
    };

    let reply_text = match result {
        Ok((today, carried)) => format_todo_list(&today, &carried),
        Err(e) => {
            eprintln!("failed to list todos: {e}");
            "⚠️ Something went wrong - please try again.".to_string()
        }
    };
    reply_ephemeral(ctx, command, reply_text, "/todo list").await;
}
```

In `src/discord/mod.rs`, the `"list"` arm of the `"todo"` match:

```rust
                    Some(("list", _)) => {
                        todo::handle_list(
                            &ctx,
                            &command,
                            &self.db,
                            &self.timezone,
                            self.carryover_lookback_days,
                        )
                        .await
                    }
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --lib` then `cargo fmt --check && cargo clippy --all-targets -- -D warnings`
Expected: PASS, clean.

- [ ] **Step 5: Commit**

```bash
git add src/discord/todo.rs src/discord/mod.rs
git commit -m "feat: /todo list shows a Carried over section"
```

---

### Task 9: Documentation

**Files:**
- Modify: `docs/user-guide.md` — new `## Carrying work over` section + touch four command sections
- Modify: `CLAUDE.md` — config-key mention + project-layout bullets
- Test: none (docs). Run the markdown-touching build check only.

**Interfaces:**
- Consumes: the finished behavior from Tasks 1–8.
- Produces: nothing code-facing.

- [ ] **Step 1: Update `docs/user-guide.md`**

Insert a new section immediately **before** `## \`/team status\` - tech lead only`:

```markdown
## Carrying work over

A todo you don't finish doesn't vanish at midnight. As long as no
**Done** progress report has been filed against it, a todo from the last
7 days keeps showing up as "still open" (the tech lead can change the
window in `config.toml`, or set it to `0` to switch this off):

- **`/progress add`** lists it in the `task` autocomplete, marked
  `· carried from <date>`. Picking it attaches your report to the
  original todo - and its SOW ref - instead of logging it as unplanned
  work.
- **`/todo list`** shows it in a read-only **Carried over (still open)**
  block below today's todos. You can't `edit` or `delete` a carried-over
  todo - close it out by filing a Done report, or leave it to age past
  the window.
- The tech lead sees it in **`/team status`** (a `+N carried` marker on
  your line) and **`/team report`** (a **Carried over** block per
  person, showing each todo's latest status and whether it moved today).

Carry-over is per-person and only ever looks at your own todos.
```

In `## \`/todo\` - your task list for the day`, at the end of the
`/todo list` bullet, add:

```markdown
  Still-open todos from earlier days appear below this in a read-only
  **Carried over** section - see "Carrying work over" below.
```

In `## \`/progress\` - report progress against a todo`, in the `task`
sub-bullet of `/progress add`, after the sentence about typing something
else for unplanned work, add:

```markdown
    Todos carried over from earlier days show up here too, marked
    `· carried from <date>`.
```

In `## \`/team status\` - tech lead only`, add a sentence at the end of
the first paragraph:

```markdown
If a member has unfinished todos carried over from the last few days,
their line ends with `+N carried` (or reads `no new todo (N carried)`
when they haven't posted anything today).
```

In `## \`/team report\` - tech lead only`, add a sentence at the end:

```markdown
Any todos carried over from earlier days (still open, no Done report)
appear in a **Carried over** block at the end of each member's section,
with the latest status and whether it was updated today.
```

- [ ] **Step 2: Update `CLAUDE.md`**

In the config paragraph (the one starting "Config file: `$XDG_CONFIG_HOME/dispatchd/config.toml`"), the keys are documented in `config.example.toml`; no change needed there. Instead, in the **Project layout** section, update these bullets:

`config.rs` line — append:
```
; carryover_lookback_days ([carryover] lookback_days, default 7, 0
disables) - how far back /progress add + /team look for unfinished todos
```

`entries.rs` line — append after the `update` rows description:
```
Carry-over query helpers: carryover_todos / carryover_count /
carryover_report - `type='todo'` rows from the last N days with no
`done` update, for /progress add autocomplete, /team status ("+N
carried"), /team report ("Carried over" block) and /todo list. short_date
formats an origin date ("2026-08-27" -> "Aug 27").
```

`status.rs` line — append:
```
team_status / team_report take carryover_lookback_days; MemberStatus
gains carried_count, MemberReport gains carried (Vec<CarryoverDetail>);
matched_update_count is now scoped to today's todo ids so a carry-over
update doesn't inflate it.
```

`discord/progress.rs` line — append:
```
add's autocomplete unions today's open todos with carryover_todos
(merge_task_choices, deduped, cap 25).
```

`discord/todo.rs` line — append:
```
list appends a read-only "Carried over (still open)" section
(format_todo_list).
```

`discord/mod.rs` line — append:
```
Handler carries carryover_lookback_days, passed to team status/report,
progress autocomplete, and todo list.
```

- [ ] **Step 3: Verify the build is still clean**

Run: `cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test`
Expected: PASS, clean (docs don't affect the build; this is the final full-suite gate).

- [ ] **Step 4: Commit**

```bash
git add docs/user-guide.md CLAUDE.md
git commit -m "docs: document todo carry-over"
```

---

## Self-Review

**1. Spec coverage:**

| Spec section | Task |
|---|---|
| The carry-over rule (window, no-`done` eligibility, `0` disables) | 2, 3, 4 |
| `entries::carryover_todos` / `carryover_count` / `carryover_report` | 2, 3, 4 |
| Config `[carryover] lookback_days` (default 7, `0` disables) | 1 |
| Threading the value onto `Handler` + 4 handlers | 5 (Handler + run + status), 6 (report), 7 (progress autocomplete), 8 (todo list) |
| `/progress add` autocomplete union + `id:` value passthrough | 7 |
| `/team status` `matched_update_count` fix + `+N carried` / `no new todo` | 5 |
| `/team report` `Carried over:` block + empty-check | 6 |
| `/todo list` read-only carried block | 8 |
| Unchanged: `/todo edit`/`delete`, `/progress edit`, followups, ticker sync | (no task — untouched by design) |
| Tests: eligibility matrix, latest-status, `updated_today`, matched fix, render blocks, merge helper, config | 2–8 |
| Docs: user-guide, config.example.toml, CLAUDE.md | 1 (config.example.toml), 9 |

No gaps.

**2. Placeholder scan:** No "TBD"/"handle edge cases"/"similar to". Every code step is a full code block; every test step has real assertions.

**3. Type consistency:**
- `carryover_todos(conn, user, today, lookback_days: i64, partial)` — same signature in Task 2 (def), Task 7 (progress), Task 8 (todo). ✓
- `carryover_count(conn, user, today, lookback_days: i64)` — Task 3 def, Task 5 use. ✓
- `carryover_report(conn, user, today, lookback_days: i64)` — Task 4 def, Task 6 use. ✓
- `CarryoverTodo { id, task, sow_ref, origin_date }` — consistent across Tasks 2, 7, 8. ✓
- `CarryoverDetail { task, sow_ref, origin_date, latest_status, latest_progress, latest_blocker, updated_today }` — Task 4 def, Task 6 use (both `status.rs` field type `Vec<crate::entries::CarryoverDetail>` and the test literal). ✓
- `team_status(conn, date, carryover_lookback_days: i64)` / `team_report(conn, date, carryover_lookback_days: i64)` — Task 5/6 defs; `handle_status`/`handle_report` pass `carryover_lookback_days as i64` from a `u32`. ✓
- `Handler.carryover_lookback_days: u32` — added Task 5, read in Tasks 5–8 as `self.carryover_lookback_days`. ✓
- `short_date(&str) -> String` — Task 2 def; used in Tasks 6, 7, 8. ✓
- `merge_task_choices(Vec<(i64,String)>, Vec<entries::CarryoverTodo>) -> Vec<(String,String)>` — Task 7 only. ✓
- `format_todo_list(&[(i64,String,Option<String>)], &[entries::CarryoverTodo]) -> String` — Task 8 only; input matches `entries::list_todos` return `Vec<(i64, String, Option<String>)>`. ✓

Consistent.
