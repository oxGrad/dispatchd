# `/team` Command Group Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the standalone `/team-status` command with a `/team` group holding `status` (unchanged), `report` (full per-member todo+progress detail), and `remind` (tech lead sends a manual todo/progress nudge to one member).

**Architecture:** A new `src/discord/team.rs` module (renamed from `team_status.rs`) registers one `/team` command with three subcommands, dispatched from `mod.rs` the same way `/todo`'s subcommands already are. Report queries and formatting live in `src/status.rs` alongside the existing `team_status` code; the manual reminder reuses `reminders::thread_for` and posts into today's standup thread exactly like the automated follow-ups in `ticker.rs`. No database schema change.

**Tech Stack:** Rust, `serenity` 0.12.5 (`serenity::all::*`), `rusqlite`, `chrono` / `chrono-tz`, `tokio`. Tests use `tempfile` + a real on-disk SQLite file (never `:memory:`).

**Spec:** `docs/superpowers/specs/2026-09-02-team-command-group-design.md`

## Global Constraints

- `cargo fmt --check`, `cargo clippy --all-targets -D warnings`, `cargo test`, and `cargo build --release` must all be clean before a task is done. Note the CI pipeline's clippy is **not** `--all-targets`, so `#[cfg(test)]` warnings only fail locally — always run `--all-targets`.
- Tests that touch the DB open a real file via `tempfile::tempdir()`, never `:memory:` (WAL pragma is silently ignored in memory). Reuse the existing `open_test_db()` helper in each test module.
- Any test that mutates a `DISPATCHD_*` / `XDG_CONFIG_HOME` env var must hold `crate::test_support::ENV_LOCK`. None of the tests in this plan need env vars, so none need the lock.
- serenity types can't be constructed outside a live gateway — command/interaction handlers stay uncovered by unit tests, consistent with the rest of the codebase. Only pure functions and DB-layer functions get tests.
- Commit after every task with a conventional-commit message (`feat:`, `refactor:`, `docs:`). Do **not** add a `Co-Authored-By` trailer.
- Do not create a git worktree; work directly on the current branch (`main`).
- Discord message content cap is 2000 characters — the `/team report` chunker enforces this.
- `/team remind`'s `member` and `kind` options are both **required**; the reminder posts to today's standup thread only and never reads or writes `followups_sent`.

---

## File Structure

| File | Responsibility | Change |
| --- | --- | --- |
| `src/discord/team.rs` | The `/team` command: builder, subcommand routing, `handle_status` / `handle_report` / `handle_remind` / `handle_autocomplete`, `RemindKind`, `send_reminder` | Renamed from `team_status.rs`, heavily extended |
| `src/discord/mod.rs` | Register `/team`, dispatch its command + autocomplete interactions; own the shared `is_unknown_channel_*` helpers | Modified |
| `src/discord/ticker.rs` | Automated reminders/sync | Modified — `is_unknown_channel_*` helpers move out to `mod.rs` |
| `src/status.rs` | Team-wide read + format logic: existing `team_status` + new `team_report`, `format_report`, `split_into_messages` | Modified |
| `src/members.rs` | Roster seeding + lookups | Modified — add `roster`, `name_of` |
| `src/discord/help.rs` | `/help` text | Modified — `/team-status` line → three `/team …` lines |
| `README.md`, `docs/discord-setup.md`, `docs/user-guide.md`, `CLAUDE.md` | User + contributor docs | Modified |

---

## Task 1: `members::roster` and `members::name_of`

**Files:**
- Modify: `src/members.rs` (add two functions after `all_member_ids`, ~line 115; add tests in the existing `#[cfg(test)] mod tests`)

**Interfaces:**
- Consumes: nothing from other tasks. Uses the existing `members` table (`discord_user_id TEXT PRIMARY KEY, name TEXT NOT NULL, role TEXT, is_lead BOOLEAN`).
- Produces:
  - `pub fn roster(conn: &rusqlite::Connection) -> anyhow::Result<Vec<(String, String)>>` — `(discord_user_id, name)` for every member, ordered by `name`.
  - `pub fn name_of(conn: &rusqlite::Connection, discord_user_id: &str) -> anyhow::Result<Option<String>>` — the member's `name`, or `None` if the id isn't on the roster.

- [ ] **Step 1: Write the failing tests**

Add to `src/members.rs` inside `mod tests` (the module already imports `super::*`, has `open_test_db()`, and `seed_member(conn, id, name)` which inserts with `role = 'senior'`):

```rust
    #[test]
    fn roster_returns_every_member_ordered_by_name() {
        let conn = open_test_db();
        seed_member(&conn, "2", "Budi");
        seed_member(&conn, "1", "Alice");
        seed_member(&conn, "3", "Citra");

        let roster = roster(&conn).unwrap();
        assert_eq!(
            roster,
            vec![
                ("1".to_string(), "Alice".to_string()),
                ("2".to_string(), "Budi".to_string()),
                ("3".to_string(), "Citra".to_string()),
            ]
        );
    }

    #[test]
    fn name_of_finds_a_seeded_member_and_misses_an_unknown_id() {
        let conn = open_test_db();
        seed_member(&conn, "7", "Gita");

        assert_eq!(name_of(&conn, "7").unwrap().as_deref(), Some("Gita"));
        assert_eq!(name_of(&conn, "999").unwrap(), None);
    }
```

The `mod tests` in `members.rs` currently only has `open_test_db` defined? Check: it has `write_members`, `seed_from`, `row_count`. It does **not** have `open_test_db` or `seed_member`. Add these two helpers to `mod tests` (copying the established pattern from `followups.rs`):

```rust
    fn open_test_db() -> Connection {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.keep().join("d.sqlite3");
        crate::db::open(&path).unwrap()
    }

    fn seed_member(conn: &Connection, id: &str, name: &str) {
        conn.execute(
            "INSERT INTO members (discord_user_id, name, role, is_lead) VALUES (?1, ?2, 'senior', 0)",
            rusqlite::params![id, name],
        )
        .unwrap();
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib members::tests::roster_returns_every_member_ordered_by_name members::tests::name_of_finds_a_seeded_member_and_misses_an_unknown_id`
Expected: FAIL — `cannot find function \`roster\`` / `cannot find function \`name_of\`` (compile error).

- [ ] **Step 3: Implement the two functions**

Add to `src/members.rs` after `all_member_ids` (before `#[cfg(test)]`):

```rust
/// Every member as `(discord_user_id, name)`, ordered by name. Backs the
/// `/team remind` autocomplete.
pub fn roster(conn: &Connection) -> Result<Vec<(String, String)>> {
    let mut stmt = conn.prepare("SELECT discord_user_id, name FROM members ORDER BY name")?;
    let rows = stmt
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// A member's display name, or `None` if `discord_user_id` isn't on the roster.
pub fn name_of(conn: &Connection, discord_user_id: &str) -> Result<Option<String>> {
    Ok(conn
        .query_row(
            "SELECT name FROM members WHERE discord_user_id = ?1",
            [discord_user_id],
            |row| row.get(0),
        )
        .optional()?)
}
```

`Result`, `Connection`, `OptionalExtension` (for `.optional()`) are already imported at the top of `members.rs` (`use rusqlite::{Connection, OptionalExtension};`, `use anyhow::{Context, Result, bail};`).

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib members::tests`
Expected: PASS (all members tests, old and new).

- [ ] **Step 5: Full check**

Run: `cargo fmt && cargo clippy --all-targets -- -D warnings && cargo test`
Expected: clean.

- [ ] **Step 6: Commit**

```bash
git add src/members.rs
git commit -m "feat: add members::roster and members::name_of"
```

---

## Task 2: `status::team_report` — query + structs

**Files:**
- Modify: `src/status.rs` (add structs + `team_report` after `team_status`, ~line 64; tests in the existing `mod tests`)

**Interfaces:**
- Consumes: the existing `entries` table. Test helpers already in `status.rs::tests`: `open_test_db()`, `seed_member(conn, id, name, role)`, `entries::insert_todo(conn, uid, date, task, notes: Option<&str>, sow_ref: Option<&str>) -> i64`, `entries::insert_update(conn, uid, date, task, todo_id: Option<i64>, status, progress, blocker: Option<&str>) -> i64`.
- Produces:
  ```rust
  #[derive(Debug, PartialEq, Eq)]
  pub struct UpdateDetail {
      pub task: String,
      pub status: String,
      pub progress: String,
      pub blocker: Option<String>,
  }

  #[derive(Debug, PartialEq, Eq)]
  pub struct TodoDetail {
      pub task: String,
      pub notes: Option<String>,
      pub sow_ref: Option<String>,
      pub updates: Vec<UpdateDetail>,
  }

  #[derive(Debug, PartialEq, Eq)]
  pub struct MemberReport {
      pub name: String,
      pub todos: Vec<TodoDetail>,
      pub ad_hoc: Vec<UpdateDetail>,
  }

  pub fn team_report(conn: &rusqlite::Connection, date: &str) -> anyhow::Result<Vec<MemberReport>>;
  ```
  One `MemberReport` per row in `members`, ordered by name. `todos` are that member's `type='todo'` rows for `date` ordered by id, each with its `type='update'` rows (matched by `todo_id`) ordered by id. `ad_hoc` are that member's `type='update'` rows for `date` with `todo_id IS NULL`, ordered by id.

- [ ] **Step 1: Write the failing tests**

Add to `src/status.rs` inside `mod tests`:

```rust
    #[test]
    fn team_report_nests_updates_under_their_todo_in_id_order() {
        let conn = open_test_db();
        seed_member(&conn, "1", "Alice", "lead");
        let todo = entries::insert_todo(&conn, "1", DATE, "Refactor auth", Some("split it"), Some("M1D2"))
            .unwrap();
        entries::insert_update(&conn, "1", DATE, "Refactor auth", Some(todo), "in_progress", "started", None)
            .unwrap();
        entries::insert_update(&conn, "1", DATE, "Refactor auth", Some(todo), "done", "finished", None)
            .unwrap();

        let reports = team_report(&conn, DATE).unwrap();
        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0].name, "Alice");
        assert_eq!(reports[0].todos.len(), 1);
        let t = &reports[0].todos[0];
        assert_eq!(t.task, "Refactor auth");
        assert_eq!(t.notes.as_deref(), Some("split it"));
        assert_eq!(t.sow_ref.as_deref(), Some("M1D2"));
        assert_eq!(
            t.updates,
            vec![
                UpdateDetail { task: "Refactor auth".into(), status: "in_progress".into(), progress: "started".into(), blocker: None },
                UpdateDetail { task: "Refactor auth".into(), status: "done".into(), progress: "finished".into(), blocker: None },
            ]
        );
        assert!(reports[0].ad_hoc.is_empty());
    }

    #[test]
    fn team_report_puts_unmatched_updates_in_ad_hoc() {
        let conn = open_test_db();
        seed_member(&conn, "2", "Budi", "designer");
        entries::insert_update(&conn, "2", DATE, "Hotfix prod", None, "done", "added an index", Some("was waiting on DBA"))
            .unwrap();

        let reports = team_report(&conn, DATE).unwrap();
        assert!(reports[0].todos.is_empty());
        assert_eq!(
            reports[0].ad_hoc,
            vec![UpdateDetail {
                task: "Hotfix prod".into(),
                status: "done".into(),
                progress: "added an index".into(),
                blocker: Some("was waiting on DBA".into()),
            }]
        );
    }

    #[test]
    fn team_report_includes_members_with_nothing_ordered_by_name() {
        let conn = open_test_db();
        seed_member(&conn, "9", "Zoya", "junior");
        seed_member(&conn, "1", "Alice", "lead");

        let reports = team_report(&conn, DATE).unwrap();
        let names: Vec<&str> = reports.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, vec!["Alice", "Zoya"]);
        assert!(reports[0].todos.is_empty() && reports[0].ad_hoc.is_empty());
    }

    #[test]
    fn team_report_todo_with_no_update_has_empty_updates() {
        let conn = open_test_db();
        seed_member(&conn, "3", "Citra", "senior");
        entries::insert_todo(&conn, "3", DATE, "Design audit", None, None).unwrap();

        let reports = team_report(&conn, DATE).unwrap();
        assert_eq!(reports[0].todos.len(), 1);
        assert!(reports[0].todos[0].updates.is_empty());
        assert!(reports[0].todos[0].notes.is_none());
        assert!(reports[0].todos[0].sow_ref.is_none());
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib status::tests::team_report`
Expected: FAIL — `cannot find type \`UpdateDetail\`` / `cannot find function \`team_report\`` (compile error).

- [ ] **Step 3: Implement the structs + query**

Add to `src/status.rs` after `team_status` (which ends ~line 64), before `format_status_line`:

```rust
/// Full per-member detail for `/team report`: every todo with its notes,
/// SOW ref and matching progress report(s), plus any unplanned progress.
#[derive(Debug, PartialEq, Eq)]
pub struct UpdateDetail {
    pub task: String,
    pub status: String,
    pub progress: String,
    pub blocker: Option<String>,
}

#[derive(Debug, PartialEq, Eq)]
pub struct TodoDetail {
    pub task: String,
    pub notes: Option<String>,
    pub sow_ref: Option<String>,
    pub updates: Vec<UpdateDetail>,
}

#[derive(Debug, PartialEq, Eq)]
pub struct MemberReport {
    pub name: String,
    pub todos: Vec<TodoDetail>,
    pub ad_hoc: Vec<UpdateDetail>,
}

/// One `MemberReport` per roster row, ordered by name. Per-member queries
/// rather than one big JOIN - same rationale as `team_status`: a 6-person
/// team's worth of rows, easier to read and verify.
pub fn team_report(conn: &Connection, date: &str) -> Result<Vec<MemberReport>> {
    let mut members_stmt =
        conn.prepare("SELECT discord_user_id, name FROM members ORDER BY name")?;
    let members = members_stmt
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    let mut reports = Vec::with_capacity(members.len());
    for (discord_user_id, name) in members {
        let mut todo_stmt = conn.prepare(
            "SELECT id, task, notes, sow_ref FROM entries
             WHERE type = 'todo' AND date = ?1 AND discord_user_id = ?2
             ORDER BY id",
        )?;
        let todo_rows = todo_stmt
            .query_map(params![date, discord_user_id], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<String>>(3)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;

        let mut todos = Vec::with_capacity(todo_rows.len());
        for (todo_id, task, notes, sow_ref) in todo_rows {
            let mut upd_stmt = conn.prepare(
                "SELECT task, status, progress, blocker FROM entries
                 WHERE type = 'update' AND todo_id = ?1
                 ORDER BY id",
            )?;
            let updates = upd_stmt
                .query_map([todo_id], row_to_update_detail)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            todos.push(TodoDetail { task, notes, sow_ref, updates });
        }

        let mut adhoc_stmt = conn.prepare(
            "SELECT task, status, progress, blocker FROM entries
             WHERE type = 'update' AND date = ?1 AND discord_user_id = ?2 AND todo_id IS NULL
             ORDER BY id",
        )?;
        let ad_hoc = adhoc_stmt
            .query_map(params![date, discord_user_id], row_to_update_detail)?
            .collect::<rusqlite::Result<Vec<_>>>()?;

        reports.push(MemberReport { name, todos, ad_hoc });
    }
    Ok(reports)
}

fn row_to_update_detail(row: &rusqlite::Row<'_>) -> rusqlite::Result<UpdateDetail> {
    Ok(UpdateDetail {
        task: row.get(0)?,
        status: row.get(1)?,
        progress: row.get(2)?,
        blocker: row.get(3)?,
    })
}
```

`Connection`, `Result`, `params` are already imported at the top of `status.rs` (`use anyhow::Result;`, `use rusqlite::{Connection, params};`).

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib status::tests`
Expected: PASS (existing `team_status` tests + the four new ones).

- [ ] **Step 5: Full check**

Run: `cargo fmt && cargo clippy --all-targets -- -D warnings && cargo test`
Expected: clean.

- [ ] **Step 6: Commit**

```bash
git add src/status.rs
git commit -m "feat: add status::team_report query for the full team report"
```

---

## Task 3: `status::format_report`

**Files:**
- Modify: `src/status.rs` (add `format_report` + a private `status_glyph_label` helper after `format_status_line`; tests in `mod tests`)

**Interfaces:**
- Consumes: `MemberReport`, `TodoDetail`, `UpdateDetail` from Task 2.
- Produces: `pub fn format_report(report: &MemberReport) -> String` — the Discord-markdown block for one member, **no trailing newline**. Callers join blocks with `"\n\n"`.

Output rules:
- Member with no todos and no ad-hoc updates → exactly `**{name}** — nothing posted today`
- Otherwise, first line `**{name}**`, then:
  - per todo: `• {task}` with ` [{sow_ref}]` appended when `sow_ref` is `Some`; then
    - if `notes` is `Some`: a line `  notes: {notes}`
    - if `updates` is empty: a line `  ❌ no progress report yet`
    - else one line per update: `  {glyph} {label} — {progress}` with ` (blocker: {blocker})` appended when `blocker` is `Some`
  - per ad-hoc update: `• unplanned: {task}` then the same update line as above (indented two spaces)
- `status_glyph_label`: `"done"` → `("✅", "done")`, `"in_progress"` → `("⏳", "in progress")`, `"blocked"` → `("⛔", "blocked")`, anything else → `("•", <the status string verbatim>)`

- [ ] **Step 1: Write the failing tests**

Add to `src/status.rs` `mod tests`:

```rust
    fn sample_report() -> MemberReport {
        MemberReport {
            name: "Alice".into(),
            todos: vec![
                TodoDetail {
                    task: "Refactor auth".into(),
                    notes: Some("split into service + handler".into()),
                    sow_ref: Some("M1D2".into()),
                    updates: vec![UpdateDetail {
                        task: "Refactor auth".into(),
                        status: "done".into(),
                        progress: "extracted AuthService".into(),
                        blocker: None,
                    }],
                },
                TodoDetail {
                    task: "Write migration".into(),
                    notes: None,
                    sow_ref: None,
                    updates: vec![UpdateDetail {
                        task: "Write migration".into(),
                        status: "blocked".into(),
                        progress: "schema drafted".into(),
                        blocker: Some("waiting on DBA".into()),
                    }],
                },
                TodoDetail {
                    task: "Docs pass".into(),
                    notes: None,
                    sow_ref: None,
                    updates: vec![],
                },
            ],
            ad_hoc: vec![UpdateDetail {
                task: "Hotfix prod 500".into(),
                status: "done".into(),
                progress: "bad index, added it".into(),
                blocker: None,
            }],
        }
    }

    #[test]
    fn format_report_renders_todos_notes_updates_and_ad_hoc() {
        let out = format_report(&sample_report());
        assert_eq!(
            out,
            "**Alice**\n\
             • Refactor auth [M1D2]\n\
             \u{20}\u{20}notes: split into service + handler\n\
             \u{20}\u{20}✅ done — extracted AuthService\n\
             • Write migration\n\
             \u{20}\u{20}⛔ blocked — schema drafted (blocker: waiting on DBA)\n\
             • Docs pass\n\
             \u{20}\u{20}❌ no progress report yet\n\
             • unplanned: Hotfix prod 500\n\
             \u{20}\u{20}✅ done — bad index, added it"
        );
    }

    #[test]
    fn format_report_empty_member_is_a_single_line() {
        let report = MemberReport { name: "Budi".into(), todos: vec![], ad_hoc: vec![] };
        assert_eq!(format_report(&report), "**Budi** — nothing posted today");
    }

    #[test]
    fn format_report_unknown_status_falls_back_to_verbatim() {
        let report = MemberReport {
            name: "Citra".into(),
            todos: vec![TodoDetail {
                task: "Thing".into(),
                notes: None,
                sow_ref: None,
                updates: vec![UpdateDetail {
                    task: "Thing".into(),
                    status: "weird".into(),
                    progress: "hmm".into(),
                    blocker: None,
                }],
            }],
            ad_hoc: vec![],
        };
        assert_eq!(
            format_report(&report),
            "**Citra**\n• Thing\n\u{20}\u{20}• weird — hmm"
        );
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib status::tests::format_report`
Expected: FAIL — `cannot find function \`format_report\``.

- [ ] **Step 3: Implement**

Add to `src/status.rs` after `format_status_line`:

```rust
/// `(glyph, label)` for a progress-report status value. Unknown values
/// (shouldn't happen - the command only writes done/in_progress/blocked)
/// fall back to a neutral bullet and the raw string.
fn status_glyph_label(status: &str) -> (&'static str, String) {
    match status {
        "done" => ("✅", "done".to_string()),
        "in_progress" => ("⏳", "in progress".to_string()),
        "blocked" => ("⛔", "blocked".to_string()),
        other => ("•", other.to_string()),
    }
}

fn push_update_line(out: &mut String, update: &UpdateDetail) {
    let (glyph, label) = status_glyph_label(&update.status);
    out.push_str(&format!("  {glyph} {label} — {}", update.progress));
    if let Some(blocker) = &update.blocker {
        out.push_str(&format!(" (blocker: {blocker})"));
    }
}

/// One member's block for `/team report`, in Discord markdown, with no
/// trailing newline. Callers join member blocks with "\n\n".
pub fn format_report(report: &MemberReport) -> String {
    if report.todos.is_empty() && report.ad_hoc.is_empty() {
        return format!("**{}** — nothing posted today", report.name);
    }

    let mut out = format!("**{}**", report.name);
    for todo in &report.todos {
        out.push('\n');
        match &todo.sow_ref {
            Some(sow_ref) => out.push_str(&format!("• {} [{sow_ref}]", todo.task)),
            None => out.push_str(&format!("• {}", todo.task)),
        }
        if let Some(notes) = &todo.notes {
            out.push_str(&format!("\n  notes: {notes}"));
        }
        if todo.updates.is_empty() {
            out.push_str("\n  ❌ no progress report yet");
        } else {
            for update in &todo.updates {
                out.push('\n');
                push_update_line(&mut out, update);
            }
        }
    }
    for update in &report.ad_hoc {
        out.push_str(&format!("\n• unplanned: {}\n", update.task));
        push_update_line(&mut out, update);
    }
    out
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib status::tests`
Expected: PASS.

- [ ] **Step 5: Full check**

Run: `cargo fmt && cargo clippy --all-targets -- -D warnings && cargo test`
Expected: clean.

- [ ] **Step 6: Commit**

```bash
git add src/status.rs
git commit -m "feat: add status::format_report for the full team report"
```

---

## Task 4: `status::split_into_messages`

**Files:**
- Modify: `src/status.rs` (add `split_into_messages` after `format_report`; tests in `mod tests`)

**Interfaces:**
- Consumes: nothing.
- Produces: `pub fn split_into_messages(full: &str, limit: usize) -> Vec<String>` — splits `full` on `"\n\n"` (member) boundaries, greedily packing blocks into chunks of at most `limit` **characters**. A single block longer than `limit` is hard-wrapped at `limit`-char boundaries. `""` → `vec![]`.

- [ ] **Step 1: Write the failing tests**

Add to `src/status.rs` `mod tests`:

```rust
    #[test]
    fn split_into_messages_empty_input_is_empty() {
        assert_eq!(split_into_messages("", 2000), Vec::<String>::new());
    }

    #[test]
    fn split_into_messages_keeps_a_small_report_as_one_chunk() {
        let full = "**Alice**\n• a\n  ✅ done — x\n\n**Budi** — nothing posted today";
        assert_eq!(split_into_messages(full, 2000), vec![full.to_string()]);
    }

    #[test]
    fn split_into_messages_breaks_on_member_boundaries() {
        let a = format!("**Alice**\n{}", "x".repeat(1200));
        let b = format!("**Budi**\n{}", "y".repeat(1200));
        let c = format!("**Citra**\n{}", "z".repeat(1200));
        let full = format!("{a}\n\n{b}\n\n{c}");

        let chunks = split_into_messages(&full, 2000);
        assert_eq!(chunks, vec![a, b, c]);
        for chunk in &split_into_messages(&full, 2000) {
            assert!(chunk.chars().count() <= 2000);
        }
    }

    #[test]
    fn split_into_messages_hard_wraps_an_oversized_block() {
        let full = format!("**Alice**\n{}", "x".repeat(4100));
        let chunks = split_into_messages(&full, 2000);
        assert_eq!(chunks.len(), 3);
        assert_eq!(chunks[0].chars().count(), 2000);
        assert_eq!(chunks[1].chars().count(), 2000);
        assert!(chunks[2].chars().count() <= 2000);
        assert_eq!(chunks.concat().chars().count(), full.chars().count());
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib status::tests::split_into_messages`
Expected: FAIL — `cannot find function \`split_into_messages\``.

- [ ] **Step 3: Implement**

Add to `src/status.rs` after `format_report`:

```rust
/// Packs member blocks (separated by "\n\n" in `full`) into chunks of at
/// most `limit` characters, so `/team report` fits Discord's 2000-char
/// message cap. A single block over `limit` is hard-wrapped at char
/// boundaries. Empty input yields no chunks.
pub fn split_into_messages(full: &str, limit: usize) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut current = String::new();

    for block in full.split("\n\n") {
        if block.is_empty() {
            continue;
        }
        let sep_len = if current.is_empty() { 0 } else { 2 };
        let fits = current.chars().count() + sep_len + block.chars().count() <= limit;
        if fits {
            if !current.is_empty() {
                current.push_str("\n\n");
            }
            current.push_str(block);
            continue;
        }

        if !current.is_empty() {
            out.push(std::mem::take(&mut current));
        }
        if block.chars().count() > limit {
            let mut chars = block.chars();
            loop {
                let chunk: String = chars.by_ref().take(limit).collect();
                if chunk.is_empty() {
                    break;
                }
                out.push(chunk);
            }
        } else {
            current = block.to_string();
        }
    }

    if !current.is_empty() {
        out.push(current);
    }
    out
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib status::tests`
Expected: PASS.

- [ ] **Step 5: Full check**

Run: `cargo fmt && cargo clippy --all-targets -- -D warnings && cargo test`
Expected: clean.

- [ ] **Step 6: Commit**

```bash
git add src/status.rs
git commit -m "feat: add status::split_into_messages to chunk the team report"
```

---

## Task 5: Move `is_unknown_channel_*` helpers to `discord/mod.rs`

**Files:**
- Modify: `src/discord/ticker.rs` (remove `UNKNOWN_CHANNEL_ERROR_CODE`, `is_unknown_channel_code`, `is_unknown_channel_error`, and the `is_unknown_channel_code` test; update the one call site; trim now-unused imports)
- Modify: `src/discord/mod.rs` (add the three items as `pub(crate)`, plus the relocated test)

**Interfaces:**
- Consumes: nothing.
- Produces (in `crate::discord`, i.e. `mod.rs`):
  - `pub(crate) fn is_unknown_channel_error(err: &serenity::all::Error) -> bool`
  - (private) `const UNKNOWN_CHANNEL_ERROR_CODE: isize`, `fn is_unknown_channel_code(code: isize) -> bool`
  Both `ticker.rs` and (Task 8) `team.rs` call `super::is_unknown_channel_error`.

- [ ] **Step 1: Add the helpers to `mod.rs`**

In `src/discord/mod.rs`, extend the `serenity::all` import to include `Error as SerenityError` and `HttpError` (add them to the existing `use serenity::all::{...}` list, keeping it sorted-ish):

```rust
use serenity::all::{
    ActionRowComponent, ChannelId, Client, CommandDataOption, CommandDataOptionValue,
    Context as SerenityContext, CreateCommand, CreateInteractionResponse,
    CreateInteractionResponseMessage, Error as SerenityError, EventHandler, GatewayIntents, GuildId,
    HttpError, Interaction, ModalInteraction, Ready,
};
```

Then add, just above `pub(crate) fn modal_value` (~line 106):

```rust
/// Discord's JSON error code for "Unknown Channel" - returned when a
/// request targets a channel or thread that no longer exists (e.g. a
/// deleted standup thread).
const UNKNOWN_CHANNEL_ERROR_CODE: isize = 10003;

fn is_unknown_channel_code(code: isize) -> bool {
    code == UNKNOWN_CHANNEL_ERROR_CODE
}

/// True when `err` is Discord reporting that the channel/thread a request
/// targeted no longer exists, as opposed to a transient failure. Not
/// unit-tested directly - building a real `serenity::Error` needs a
/// `reqwest::Method`, which isn't a direct dependency of this crate.
pub(crate) fn is_unknown_channel_error(err: &SerenityError) -> bool {
    matches!(
        err,
        SerenityError::Http(HttpError::UnsuccessfulRequest(response))
            if is_unknown_channel_code(response.error.code)
    )
}
```

Add the relocated test to `mod.rs`. If `mod.rs` has no `#[cfg(test)] mod tests`, add one at the end of the file:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_channel_code_matches_discords_10003() {
        assert!(is_unknown_channel_code(UNKNOWN_CHANNEL_ERROR_CODE));
        assert!(is_unknown_channel_code(10003));
        assert!(!is_unknown_channel_code(10004));
    }
}
```

- [ ] **Step 2: Remove them from `ticker.rs` and update the call site**

In `src/discord/ticker.rs`:
- Delete the `UNKNOWN_CHANNEL_ERROR_CODE` const, `is_unknown_channel_code`, `is_unknown_channel_error` (lines ~24-46) and their doc comments.
- Delete the `is_unknown_channel_code` test in `ticker.rs`'s `mod tests` (search for `fn ` mentioning `is_unknown_channel` / `10003`).
- The two call sites (`if is_unknown_channel_error(&e)` around lines 476 and any other) become `if super::is_unknown_channel_error(&e)`.
- Trim the `serenity::all` import in `ticker.rs`: remove `Error as SerenityError` and `HttpError` if nothing else in the file uses them (they were only used by the moved fn). Keep `ChannelId, ChannelType, CreateMessage, CreateThread, Http`.

- [ ] **Step 3: Build and test**

Run: `cargo build && cargo test --lib`
Expected: PASS. If clippy later flags an unused import, remove it.

- [ ] **Step 4: Full check**

Run: `cargo fmt && cargo clippy --all-targets -- -D warnings && cargo test`
Expected: clean. (Watch for "unused import" in `ticker.rs` — fix by trimming.)

- [ ] **Step 5: Commit**

```bash
git add src/discord/mod.rs src/discord/ticker.rs
git commit -m "refactor: hoist is_unknown_channel_error to discord::mod for reuse"
```

---

## Task 6: Rename `team_status.rs` → `team.rs`, wire up `/team status`

**Files:**
- Rename: `src/discord/team_status.rs` → `src/discord/team.rs` (via `git mv`)
- Modify: `src/discord/team.rs` (new command builder + `subcommand` helper + `handle_status`)
- Modify: `src/discord/mod.rs` (`mod team;`, registration, dispatch)
- Modify: `src/discord/help.rs` (help text + test needles)

**Interfaces:**
- Consumes: `status::team_status`, `status::format_status_line`, `members::is_lead` (all unchanged).
- Produces (in `crate::discord::team`):
  - `pub fn command() -> serenity::all::CreateCommand` — a `"team"` command with a `status` subcommand (`report` / `remind` added in Tasks 7 & 9).
  - `pub fn subcommand(options: &[CommandDataOption]) -> Option<(&str, &[CommandDataOption])>`
  - `pub async fn handle_status(ctx: &SerenityContext, command: &CommandInteraction, db: &Arc<Mutex<Connection>>, timezone: &Tz)`

- [ ] **Step 1: Rename the file**

```bash
git mv src/discord/team_status.rs src/discord/team.rs
```

- [ ] **Step 2: Rewrite `team.rs`'s command surface**

Replace the top of `src/discord/team.rs` (imports + `command` + `handle_command`) so it reads:

```rust
use std::sync::{Arc, Mutex};

use chrono_tz::Tz;
use rusqlite::Connection;
use serenity::all::{
    CommandDataOption, CommandDataOptionValue, CommandInteraction, CommandOptionType,
    Context as SerenityContext, CreateCommand, CreateCommandOption, CreateInteractionResponse,
    CreateInteractionResponseMessage, Permissions,
};

use crate::{entries, members, status};

pub fn command() -> CreateCommand {
    CreateCommand::new("team")
        .description("Tech-lead tools: status summary, full report, manual reminders")
        .default_member_permissions(Permissions::MANAGE_GUILD)
        .add_option(CreateCommandOption::new(
            CommandOptionType::SubCommand,
            "status",
            "One-line-per-member update summary for today",
        ))
}

/// Discord nests a subcommand's own options one level under an entry named
/// after the subcommand - this pulls out `(subcommand_name, its_options)`.
/// (Same shape as `todo::subcommand`.)
pub fn subcommand(options: &[CommandDataOption]) -> Option<(&str, &[CommandDataOption])> {
    let top = options.first()?;
    match &top.value {
        CommandDataOptionValue::SubCommand(nested) => Some((top.name.as_str(), nested)),
        _ => None,
    }
}

pub async fn handle_status(
    ctx: &SerenityContext,
    command: &CommandInteraction,
    db: &Arc<Mutex<Connection>>,
    timezone: &Tz,
) {
    let discord_user_id = command.user.id.to_string();
    let date = entries::today_in(timezone);

    let reply_text = {
        let conn = db.lock().expect("db mutex poisoned");
        match members::is_lead(&conn, &discord_user_id) {
            Ok(false) => "⛔ This command is restricted to the tech lead.".to_string(),
            Ok(true) => match status::team_status(&conn, &date) {
                Ok(rows) if rows.is_empty() => {
                    "No team members configured yet - see members.toml.".to_string()
                }
                Ok(rows) => rows
                    .iter()
                    .map(status::format_status_line)
                    .collect::<Vec<_>>()
                    .join("\n"),
                Err(e) => {
                    eprintln!("failed to fetch team status: {e}");
                    "⚠️ Something went wrong fetching team status.".to_string()
                }
            },
            Err(e) => {
                eprintln!("failed to check is_lead: {e}");
                "⚠️ Something went wrong checking permissions.".to_string()
            }
        }
    };

    let reply = CreateInteractionResponseMessage::new()
        .content(reply_text)
        .ephemeral(true);
    if let Err(e) = command
        .create_response(&ctx.http, CreateInteractionResponse::Message(reply))
        .await
    {
        eprintln!("failed to respond to /team status: {e}");
    }
}
```

(The body of `handle_status` is the old `handle_command` verbatim except the final `eprintln!` string. `use crate::{entries, members, status};` replaces the old `use crate::{entries, members, status};` — `entries` is now needed for `today_in`, which the old code already used.)

- [ ] **Step 3: Update `mod.rs`**

In `src/discord/mod.rs`:
- Line 3: `mod team_status;` → `mod team;` (keep the `mod` list sorted: `help, progress, team, ticker, todo`).
- In the `ready` handler's `commands` vec (~line 37): `team_status::command(),` → `team::command(),`.
- In the `Interaction::Command` match (~line 73), replace:

  ```rust
  "team-status" => {
      team_status::handle_command(&ctx, &command, &self.db, &self.timezone).await
  }
  ```

  with:

  ```rust
  "team" => match team::subcommand(&command.data.options) {
      Some(("status", _)) => {
          team::handle_status(&ctx, &command, &self.db, &self.timezone).await
      }
      _ => {}
  },
  ```

- [ ] **Step 4: Update `help.rs`**

In `src/discord/help.rs`, replace the `/team-status` line in `HELP_TEXT`:

```rust
`/team status` - (tech lead only) one line per member: who's updated today
```

And in the test `help_text_mentions_every_command`, change the `"/team-status"` needle to `"/team status"`.

- [ ] **Step 5: Build, test, clippy**

Run: `cargo build && cargo test && cargo clippy --all-targets -- -D warnings && cargo fmt --check`
Expected: all clean. `/team-status` no longer exists anywhere (grep to confirm: `git grep -n "team-status\|team_status::" -- 'src/*'` returns nothing except possibly `status::team_status` which is the DB fn and stays).

- [ ] **Step 6: Commit**

```bash
git add -A src/discord/ src/discord/help.rs
git commit -m "feat: replace /team-status with a /team group (status subcommand)"
```

---

## Task 7: `/team report` handler

**Files:**
- Modify: `src/discord/team.rs` (add `report` subcommand + `handle_report`)
- Modify: `src/discord/mod.rs` (dispatch `report`)
- Modify: `src/discord/help.rs` (help line + needle)

**Interfaces:**
- Consumes: `status::team_report`, `status::format_report`, `status::split_into_messages` (Tasks 2-4); `members::is_lead`; `team::subcommand` (Task 6).
- Produces: `pub async fn handle_report(ctx: &SerenityContext, command: &CommandInteraction, db: &Arc<Mutex<Connection>>, timezone: &Tz)`.

- [ ] **Step 1: Add the subcommand to `command()`**

In `src/discord/team.rs`, chain onto the `CreateCommand` in `command()` after the `status` option:

```rust
        .add_option(CreateCommandOption::new(
            CommandOptionType::SubCommand,
            "report",
            "Full detail: everyone's todos and progress for today",
        ))
```

- [ ] **Step 2: Add `handle_report`**

Extend `team.rs`'s `serenity::all` import with `CreateInteractionResponseFollowup`. Add after `handle_status`:

```rust
pub async fn handle_report(
    ctx: &SerenityContext,
    command: &CommandInteraction,
    db: &Arc<Mutex<Connection>>,
    timezone: &Tz,
) {
    let discord_user_id = command.user.id.to_string();
    let date = entries::today_in(timezone);

    let body = {
        let conn = db.lock().expect("db mutex poisoned");
        match members::is_lead(&conn, &discord_user_id) {
            Ok(false) => Err("⛔ This command is restricted to the tech lead.".to_string()),
            Ok(true) => match status::team_report(&conn, &date) {
                Ok(reports) if reports.is_empty() => {
                    Err("No team members configured yet - see members.toml.".to_string())
                }
                Ok(reports) => {
                    let full = reports
                        .iter()
                        .map(status::format_report)
                        .collect::<Vec<_>>()
                        .join("\n\n");
                    Ok(status::split_into_messages(&full, 2000))
                }
                Err(e) => {
                    eprintln!("failed to fetch team report: {e}");
                    Err("⚠️ Something went wrong fetching the team report.".to_string())
                }
            },
            Err(e) => {
                eprintln!("failed to check is_lead: {e}");
                Err("⚠️ Something went wrong checking permissions.".to_string())
            }
        }
    };

    let mut chunks = match body {
        Ok(chunks) => chunks.into_iter(),
        Err(message) => vec![message].into_iter(),
    };
    let first = chunks.next().unwrap_or_else(|| "No activity today.".to_string());

    let reply = CreateInteractionResponseMessage::new()
        .content(first)
        .ephemeral(true);
    if let Err(e) = command
        .create_response(&ctx.http, CreateInteractionResponse::Message(reply))
        .await
    {
        eprintln!("failed to respond to /team report: {e}");
        return;
    }

    for chunk in chunks {
        if let Err(e) = command
            .create_followup(
                &ctx.http,
                CreateInteractionResponseFollowup::new()
                    .content(chunk)
                    .ephemeral(true),
            )
            .await
        {
            eprintln!("failed to send /team report follow-up: {e}");
        }
    }
}
```

- [ ] **Step 3: Dispatch it in `mod.rs`**

In the `"team" => match team::subcommand(...)` block, add an arm:

```rust
      Some(("report", _)) => {
          team::handle_report(&ctx, &command, &self.db, &self.timezone).await
      }
```

- [ ] **Step 4: Update `help.rs`**

Add a line to `HELP_TEXT` under the `/team status` line:

```rust
`/team report` - (tech lead only) full detail of everyone's todos + progress today
```

Add `"/team report"` to the test needles.

- [ ] **Step 5: Build, test, clippy, fmt**

Run: `cargo build && cargo test && cargo clippy --all-targets -- -D warnings && cargo fmt --check`
Expected: clean.

- [ ] **Step 6: Commit**

```bash
git add src/discord/team.rs src/discord/mod.rs src/discord/help.rs
git commit -m "feat: add /team report - full per-member todo and progress detail"
```

---

## Task 8: `RemindKind` + `send_reminder` in `team.rs`

**Files:**
- Modify: `src/discord/team.rs` (add `RemindKind`, `SendOutcome`, `send_reminder`; tests in a `#[cfg(test)] mod tests` — `team.rs` has none yet)

**Interfaces:**
- Consumes: `reminders::thread_for`, `super::is_unknown_channel_error` (Task 5), `entries::today_in`.
- Produces (in `crate::discord::team`):
  ```rust
  pub enum RemindKind { Todo, Progress }
  impl RemindKind {
      pub fn parse(s: &str) -> Option<RemindKind>;   // "todo" | "progress"
      pub fn reminder_text(&self, user_id: &str) -> String;
  }
  pub enum SendOutcome { Sent, NoThread, ThreadGone, Failed }
  pub async fn send_reminder(
      http: &Arc<serenity::all::Http>,
      db: &Arc<Mutex<Connection>>,
      timezone: &Tz,
      member_id: &str,
      kind: RemindKind,
  ) -> SendOutcome;
  ```

- [ ] **Step 1: Write the failing tests**

Add at the bottom of `src/discord/team.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remind_kind_parses_known_values_only() {
        assert!(matches!(RemindKind::parse("todo"), Some(RemindKind::Todo)));
        assert!(matches!(RemindKind::parse("progress"), Some(RemindKind::Progress)));
        assert!(RemindKind::parse("").is_none());
        assert!(RemindKind::parse("TODO").is_none());
        assert!(RemindKind::parse("nope").is_none());
    }

    #[test]
    fn reminder_text_mentions_the_user_and_the_right_command() {
        let todo = RemindKind::Todo.reminder_text("123");
        assert!(todo.contains("<@123>"));
        assert!(todo.contains("/todo"));

        let progress = RemindKind::Progress.reminder_text("456");
        assert!(progress.contains("<@456>"));
        assert!(progress.contains("/progress"));
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib discord::team::tests`
Expected: FAIL — `cannot find type \`RemindKind\``.

- [ ] **Step 3: Implement**

Extend `team.rs`'s `serenity::all` import with `ChannelId`, `CreateMessage`, `Http`. Add after `subcommand`:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemindKind {
    Todo,
    Progress,
}

impl RemindKind {
    pub fn parse(s: &str) -> Option<RemindKind> {
        match s {
            "todo" => Some(RemindKind::Todo),
            "progress" => Some(RemindKind::Progress),
            _ => None,
        }
    }

    pub fn reminder_text(&self, user_id: &str) -> String {
        match self {
            RemindKind::Todo => format!(
                "👋 <@{user_id}> — reminder from the tech lead: please submit your `/todo` for today."
            ),
            RemindKind::Progress => format!(
                "👋 <@{user_id}> — reminder from the tech lead: please post a `/progress` update for today's todo(s)."
            ),
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum SendOutcome {
    Sent,
    NoThread,
    ThreadGone,
    Failed,
}

/// Posts a manual reminder for `member_id` into today's standup thread.
/// Independent of the ticker's automated follow-ups - never reads or
/// writes `followups_sent`.
pub async fn send_reminder(
    http: &Arc<Http>,
    db: &Arc<Mutex<Connection>>,
    timezone: &Tz,
    member_id: &str,
    kind: RemindKind,
) -> SendOutcome {
    let date = entries::today_in(timezone);

    let thread_id = {
        let conn = db.lock().expect("db mutex poisoned");
        crate::reminders::thread_for(&conn, &date)
    };
    let thread_id = match thread_id {
        Ok(Some(id)) => id,
        Ok(None) => return SendOutcome::NoThread,
        Err(e) => {
            eprintln!("failed to look up standup thread for /team remind: {e}");
            return SendOutcome::Failed;
        }
    };
    let Ok(raw_id) = thread_id.parse::<u64>() else {
        eprintln!("invalid stored thread_id {thread_id:?} for {date}");
        return SendOutcome::Failed;
    };

    match ChannelId::new(raw_id)
        .send_message(http, CreateMessage::new().content(kind.reminder_text(member_id)))
        .await
    {
        Ok(_) => SendOutcome::Sent,
        Err(e) if super::is_unknown_channel_error(&e) => SendOutcome::ThreadGone,
        Err(e) => {
            eprintln!("failed to post /team remind message: {e}");
            SendOutcome::Failed
        }
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib discord::team::tests`
Expected: PASS.

- [ ] **Step 5: Full check**

Run: `cargo fmt && cargo clippy --all-targets -- -D warnings && cargo test`
Expected: clean. (`send_reminder` is `pub` but not yet called — clippy's `dead_code` only fires for private items, and `pub` items in a bin crate's module tree are exempt while a caller lands in Task 9. If clippy *does* warn, proceed to Task 9 in the same session before the final check.)

- [ ] **Step 6: Commit**

```bash
git add src/discord/team.rs
git commit -m "feat: add RemindKind and send_reminder for /team remind"
```

---

## Task 9: `/team remind` — command option, autocomplete, handler

**Files:**
- Modify: `src/discord/team.rs` (`remind` subcommand in `command()`, `handle_autocomplete`, `handle_remind`)
- Modify: `src/discord/mod.rs` (dispatch `remind` command + `"team"` autocomplete)
- Modify: `src/discord/help.rs` (help line + needle)

**Interfaces:**
- Consumes: `RemindKind`, `SendOutcome`, `send_reminder` (Task 8); `members::roster`, `members::name_of` (Task 1); `members::is_lead`; `super::get_option_string`; `team::subcommand`.
- Produces:
  - `pub async fn handle_autocomplete(ctx: &SerenityContext, autocomplete: &CommandInteraction, db: &Arc<Mutex<Connection>>)`
  - `pub async fn handle_remind(ctx: &SerenityContext, command: &CommandInteraction, options: &[CommandDataOption], db: &Arc<Mutex<Connection>>, timezone: &Tz)`

- [ ] **Step 1: Add the `remind` subcommand to `command()`**

In `team.rs`, chain onto `command()` after `report`:

```rust
        .add_option(
            CreateCommandOption::new(
                CommandOptionType::SubCommand,
                "remind",
                "Remind a team member to submit a todo or progress update",
            )
            .add_sub_option(
                CreateCommandOption::new(CommandOptionType::String, "member", "Who to remind")
                    .required(true)
                    .set_autocomplete(true),
            )
            .add_sub_option(
                CreateCommandOption::new(
                    CommandOptionType::String,
                    "kind",
                    "What to remind them about",
                )
                .required(true)
                .add_string_choice("Submit a todo", "todo")
                .add_string_choice("Post a progress update", "progress"),
            ),
        )
```

- [ ] **Step 2: Add `handle_autocomplete`**

Extend `team.rs`'s `serenity::all` import with `AutocompleteChoice`, `CreateAutocompleteResponse`. Add `use super::{get_option_string};` (there's currently no `use super::...` in `team.rs` — add it after `use crate::{...};`). Then:

```rust
pub async fn handle_autocomplete(
    ctx: &SerenityContext,
    autocomplete: &CommandInteraction,
    db: &Arc<Mutex<Connection>>,
) {
    let partial = subcommand(&autocomplete.data.options)
        .and_then(|(_, opts)| get_option_string(opts, "member"))
        .unwrap_or_default()
        .to_lowercase();

    let roster = {
        let conn = db.lock().expect("db mutex poisoned");
        members::roster(&conn)
    };

    let response = match roster {
        Ok(rows) => {
            let choices = rows
                .into_iter()
                .filter(|(_, name)| name.to_lowercase().contains(&partial))
                .take(25)
                .map(|(id, name)| AutocompleteChoice::new(name, id))
                .collect();
            CreateAutocompleteResponse::new().set_choices(choices)
        }
        Err(e) => {
            eprintln!("failed to load roster for /team remind autocomplete: {e}");
            CreateAutocompleteResponse::new()
        }
    };

    if let Err(e) = autocomplete
        .create_response(&ctx.http, CreateInteractionResponse::Autocomplete(response))
        .await
    {
        eprintln!("failed to respond to /team remind autocomplete: {e}");
    }
}
```

Note `CreateInteractionResponse::Autocomplete` needs `CreateInteractionResponse` (already imported) — also add `CreateAutocompleteResponse` and `AutocompleteChoice` to the import list.

- [ ] **Step 3: Add `handle_remind`**

```rust
pub async fn handle_remind(
    ctx: &SerenityContext,
    command: &CommandInteraction,
    options: &[CommandDataOption],
    db: &Arc<Mutex<Connection>>,
    timezone: &Tz,
) {
    let discord_user_id = command.user.id.to_string();
    let member = get_option_string(options, "member");
    let kind = get_option_string(options, "kind");

    let reply_text = 'reply: {
        let (member, kind) = match (member, kind) {
            (Some(m), Some(k)) => (m, k),
            _ => break 'reply "⚠️ Provide both a member and a kind.".to_string(),
        };
        let Some(kind) = RemindKind::parse(&kind) else {
            break 'reply "⚠️ Unknown reminder kind.".to_string();
        };

        let name = {
            let conn = db.lock().expect("db mutex poisoned");
            match members::is_lead(&conn, &discord_user_id) {
                Ok(false) => {
                    break 'reply "⛔ This command is restricted to the tech lead.".to_string()
                }
                Ok(true) => {}
                Err(e) => {
                    eprintln!("failed to check is_lead: {e}");
                    break 'reply "⚠️ Something went wrong checking permissions.".to_string();
                }
            }
            match members::name_of(&conn, &member) {
                Ok(Some(name)) => name,
                Ok(None) => break 'reply "⚠️ That user isn't on the team roster.".to_string(),
                Err(e) => {
                    eprintln!("failed to look up member name: {e}");
                    break 'reply "⚠️ Something went wrong looking up that member.".to_string();
                }
            }
        };

        match send_reminder(&ctx.http, db, timezone, &member, kind).await {
            SendOutcome::Sent => format!("✅ Reminder sent to {name} in today's standup thread."),
            SendOutcome::NoThread => {
                "⚠️ Today's standup thread hasn't been created yet — try again once it's posted."
                    .to_string()
            }
            SendOutcome::ThreadGone => {
                "⚠️ Today's standup thread appears to have been deleted.".to_string()
            }
            SendOutcome::Failed => "⚠️ Something went wrong sending the reminder.".to_string(),
        }
    };

    let reply = CreateInteractionResponseMessage::new()
        .content(reply_text)
        .ephemeral(true);
    if let Err(e) = command
        .create_response(&ctx.http, CreateInteractionResponse::Message(reply))
        .await
    {
        eprintln!("failed to respond to /team remind: {e}");
    }
}
```

(`'reply: { ... break 'reply <value> }` is a labelled-block early-exit — stable Rust, already the kind of control flow used elsewhere. If the reviewer prefers, refactor to a helper returning `String`; behaviour must match.)

- [ ] **Step 4: Dispatch in `mod.rs`**

In the `"team" => match team::subcommand(...)` block add:

```rust
      Some(("remind", opts)) => {
          team::handle_remind(&ctx, &command, opts, &self.db, &self.timezone).await
      }
```

In the `Interaction::Autocomplete` match (~line 78), add a `"team"` arm alongside `"todo"` / `"progress"`:

```rust
                "team" => team::handle_autocomplete(&ctx, &autocomplete, &self.db).await,
```

- [ ] **Step 5: Update `help.rs`**

Add under the `/team report` line:

```rust
`/team remind member:<name> kind:<todo|progress>` - (tech lead only) nudge a member in today's thread
```

Add `"/team remind"` to the test needles.

- [ ] **Step 6: Build, test, clippy, fmt**

Run: `cargo build && cargo test && cargo clippy --all-targets -- -D warnings && cargo fmt --check`
Expected: clean.

- [ ] **Step 7: Commit**

```bash
git add src/discord/team.rs src/discord/mod.rs src/discord/help.rs
git commit -m "feat: add /team remind - manual tech-lead reminder to one member"
```

---

## Task 10: Documentation

**Files:**
- Modify: `README.md`, `docs/discord-setup.md`, `docs/user-guide.md`, `CLAUDE.md`

No code, no tests. Prose only.

- [ ] **Step 1: `README.md`**

In the "In Discord" command table (~line 55-58), replace the `/team-status` row with:

```markdown
| `/team status` | tech lead | one line per member: how many of today's todos have a progress report |
| `/team report` | tech lead | full detail — everyone's todos, notes, SOW refs, and progress reports for today |
| `/team remind` | tech lead | post a reminder to one member in today's thread to submit a todo / progress update |
```

- [ ] **Step 2: `docs/discord-setup.md` §5**

Where it lists `/ping`, `/todo`, `/progress`, and `/team-status` showing up (~line 149), change `/team-status` to `/team` (and note it has `status` / `report` / `remind` subcommands).

- [ ] **Step 3: `docs/discord-setup.md` §6**

Retitle `## 6. \`/team-status\` permissions` → `## 6. \`/team\` permissions`. Update the body: the `MANAGE_GUILD` default permission and the bot-side `is_lead` check apply to **all three** `/team` subcommands. The per-subcommand override path is **Server Settings → Integrations → dispatchd → team**, where each of `status` / `report` / `remind` can be restricted to specific roles.

- [ ] **Step 4: `docs/user-guide.md`**

Rename `## /team-status - tech lead only` → `## /team status - tech lead only` (keep the existing body about the one-liner + SOW-ref tags). Add two sections after it:

```markdown
## `/team report` - tech lead only

The full picture in one message: every member's todos for today, each with
its notes, SOW ref, and the progress report(s) filed against it, plus any
unplanned work. Long reports are split across follow-up messages (Discord
caps a message at 2000 characters). Ephemeral - only you see it.

## `/team remind` - tech lead only

`/team remind member:<name> kind:<todo|progress>` posts a reminder that
mentions the chosen member in today's standup thread, asking them to
submit a `/todo` or a `/progress` update. It's separate from the
automated follow-up nags - sending one by hand doesn't stop the scheduled
one, and vice versa. If today's thread hasn't been created yet, the bot
tells you so and posts nothing.
```

- [ ] **Step 5: `CLAUDE.md`**

- The three `(\`/todo\`, \`/progress\`, \`/team-status\`)` style mentions (search `team-status`): change `/team-status` → `/team status` and, where there's room, note `/team report` and `/team remind` exist.
- The "live Discord testing" paragraph lists the Discord-facing features — add `/team report` / `/team remind` to that list (still un-exercisable without a live gateway).
- Project-layout block: the `status.rs` line — add that it now also holds `team_report` + report formatting/chunking. The `team_status.rs` entry → rename to `team.rs` and describe it as "the `/team` command group: `status` (the old `/team-status`), `report` (full per-member detail), `remind` (manual tech-lead nudge into the standup thread)".
- If `mod.rs`'s layout line mentions the shared helpers, note `is_unknown_channel_error` now lives there (moved from `ticker.rs`).

- [ ] **Step 6: Sanity check the docs build**

Run: `git diff --stat` and re-read each hunk. No command names left as `/team-status` anywhere in docs: `git grep -n "team-status"` should return nothing.

- [ ] **Step 7: Commit**

```bash
git add README.md docs/discord-setup.md docs/user-guide.md CLAUDE.md
git commit -m "docs: document the /team command group"
```

---

## Task 11: Final verification

**Files:** none (verification only).

- [ ] **Step 1: Full local gate**

Run each and confirm clean output:

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
cargo build --release
```

- [ ] **Step 2: Grep for leftovers**

```bash
git grep -n "team-status"                    # expect: no matches
git grep -n "team_status::"                   # expect: no matches (status::team_status the DB fn is fine)
git grep -n "is_unknown_channel" -- src/discord/ticker.rs   # expect: only "super::is_unknown_channel_error" call sites
```

- [ ] **Step 3: Confirm the command tree by eye**

Open `src/discord/team.rs::command()` and check it builds one `"team"` command with exactly three subcommands (`status`, `report`, `remind`), `remind` having required `member` (autocomplete) + `kind` (two string choices), and `.default_member_permissions(Permissions::MANAGE_GUILD)`.

- [ ] **Step 4: Confirm dispatch coverage**

In `src/discord/mod.rs`: the `Interaction::Command` `"team"` arm routes `status` / `report` / `remind`; the `Interaction::Autocomplete` match has a `"team"` arm. No `"team-status"` string remains.

- [ ] **Step 5: Commit any formatting-only drift**

```bash
git status
# if cargo fmt changed anything:
git add -A && git commit -m "style: cargo fmt"
```

---

## Self-Review

**1. Spec coverage**

| Spec section | Task |
| --- | --- |
| §1 module rename + `command()` (3 subcommands, `MANAGE_GUILD`) | 6 (rename + status), 7 (report opt), 9 (remind opt) |
| §1 `team::subcommand` | 6 |
| §2 `mod.rs` routing (Command + Autocomplete, no Component) | 6, 7, 9 |
| §3 `/team status` verbatim move | 6 |
| §4a `team_report` query + structs | 2 |
| §4b `format_report` | 3 |
| §4c `split_into_messages` | 4 |
| §4d `handle_report` (is_lead, empty roster, first chunk + follow-ups) | 7 |
| §5a `handle_autocomplete` (roster filter, ≤25) | 9 |
| §5b `handle_remind` (is_lead, both-required fallback, RemindKind parse, roster validate, outcome replies) | 9 |
| §6 `RemindKind` / `SendOutcome` / `send_reminder` (thread_for, unknown-channel → ThreadGone, no `followups_sent`) | 8 |
| §6 move `is_unknown_channel_error` to `mod.rs` + relocate test | 5 |
| §7 `members::roster` / `members::name_of` | 1 |
| §8 help + README + discord-setup + user-guide + CLAUDE.md | 6/7/9 (help incrementally), 10 (rest) |
| §9 not-in-scope (no schema change, no picker, single-target, no audit log) | respected — no migration task, no component task |
| Testing section | tests in Tasks 1-4, 8; help needles in 6/7/9 |
| Rollout (guild commands overwritten on `ready`) | inherent — no code needed; noted in Task 10 docs |

No gaps.

**2. Placeholder scan**

No "TBD"/"handle appropriately"/"similar to Task N"/bare-prose code steps. Every code step carries the real code. The one soft spot — Task 9's labelled block — has an explicit "refactor to a helper if preferred, behaviour must match" note, and the code is complete as written.

**3. Type consistency**

- `roster` returns `Vec<(String, String)>` = `(id, name)` — consumed in Task 9 as `.map(|(id, name)| AutocompleteChoice::new(name, id))` and `.filter(|(_, name)| ...)`. ✓
- `name_of` returns `Result<Option<String>>` — matched in Task 9 as `Ok(Some(name)) | Ok(None) | Err(e)`. ✓
- `team_report` → `Vec<MemberReport>`; `format_report(&MemberReport) -> String`; `split_into_messages(&str, usize) -> Vec<String>` — Task 7 does `reports.iter().map(status::format_report).collect::<Vec<_>>().join("\n\n")` then `split_into_messages(&full, 2000)`. ✓ (`format_report` takes `&MemberReport`, and `iter()` yields `&MemberReport`, so `.map(status::format_report)` type-checks.)
- `RemindKind::parse(&str) -> Option<RemindKind>`, `reminder_text(&self, &str) -> String`, `send_reminder(&Arc<Http>, &Arc<Mutex<Connection>>, &Tz, &str, RemindKind) -> SendOutcome` — Task 9 calls `send_reminder(&ctx.http, db, timezone, &member, kind)`. Note `ctx.http` is `Arc<Http>` in serenity 0.12 (`&ctx.http` = `&Arc<Http>`). ✓
- `SendOutcome` variants `Sent | NoThread | ThreadGone | Failed` — matched exhaustively in Task 9. ✓
- `is_unknown_channel_error(&SerenityError) -> bool` in `mod.rs`, called `super::is_unknown_channel_error(&e)` from both `ticker.rs` (Task 5) and `team.rs` (Task 8). ✓
- `subcommand(&[CommandDataOption]) -> Option<(&str, &[CommandDataOption])>` — same signature as `todo::subcommand`; Task 9 autocomplete uses `.and_then(|(_, opts)| get_option_string(opts, "member"))`. ✓

Consistent.
