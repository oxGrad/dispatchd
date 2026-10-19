//! Tests for `github::report`, kept in their own file - they need a lot of
//! fixture setup. The main fixture is the design doc's own example team
//! (Andi = designer, Budi / Sari / Rina = engineers, repos `api` / `web`),
//! so the expected output can be checked against the spec line for line.

use super::*;
use chrono_tz::Tz;

const DAY: &str = "2026-10-15";

fn utc() -> Tz {
    "UTC".parse().unwrap()
}

fn open_test_db() -> Connection {
    let dir = tempfile::tempdir().unwrap();
    crate::db::open(&dir.keep().join("d.sqlite3")).unwrap()
}

fn member(conn: &Connection, id: &str, name: &str, role: &str, github: Option<(&str, i64)>) {
    conn.execute(
        "INSERT INTO members (discord_user_id, name, role, is_lead, github_user, github_id)
         VALUES (?1, ?2, ?3, 0, ?4, ?5)",
        params![id, name, role, github.map(|g| g.0), github.map(|g| g.1)],
    )
    .unwrap();
}

#[allow(clippy::too_many_arguments)]
fn pr(
    conn: &Connection,
    node: &str,
    repo: &str,
    number: i64,
    title: &str,
    author: Option<i64>,
    state: &str,
    created: &str,
    merged: Option<&str>,
) {
    conn.execute(
        "INSERT INTO github_prs (node_id, repo, number, title, state, is_draft, author_id,
             author_login, created_at, merged_at, closed_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'x', ?8, ?9, ?9, COALESCE(?9, ?8))",
        params![
            node,
            repo,
            number,
            title,
            if state == "draft" { "open" } else { state },
            state == "draft",
            author,
            created,
            merged
        ],
    )
    .unwrap();
}

#[allow(clippy::too_many_arguments)]
fn issue(
    conn: &Connection,
    node: &str,
    number: i64,
    title: &str,
    author: Option<i64>,
    parent: Option<&str>,
    sow_ref: Option<&str>,
    created: &str,
    closed: Option<(&str, &str)>,
) {
    conn.execute(
        "INSERT INTO github_issues (node_id, repo, number, title, state, state_reason, author_id,
             author_login, created_at, closed_at, parent_node_id, sow_ref, updated_at)
         VALUES (?1, 'api', ?2, ?3, ?4, ?5, ?6, 'x', ?7, ?8, ?9, ?10, COALESCE(?8, ?7))",
        params![
            node,
            number,
            title,
            if closed.is_some() { "closed" } else { "open" },
            closed.map(|c| c.1),
            author,
            created,
            closed.map(|c| c.0),
            parent,
            sow_ref
        ],
    )
    .unwrap();
}

fn link(conn: &Connection, pr: &str, issue: &str) {
    conn.execute(
        "INSERT INTO github_pr_issues (pr_node_id, issue_node_id) VALUES (?1, ?2)",
        params![pr, issue],
    )
    .unwrap();
}

fn review(conn: &Connection, node: &str, pr: &str, author: i64, state: &str, at: &str) {
    conn.execute(
        "INSERT INTO github_reviews (node_id, pr_node_id, author_id, author_login, state,
             submitted_at)
         VALUES (?1, ?2, ?3, 'x', ?4, ?5)",
        params![node, pr, author, state, at],
    )
    .unwrap();
}

/// The design doc's example day (`2026-10-15`, UTC):
/// - Budi merges api#210 (closes sub-issue api#201 of deliverable
///   D.P.1.2) and api#224 (no issue), and requests changes on api#219;
/// - Rina merges web#222, approves api#210, comments twice on api#221;
/// - Sari opens api#219 and issue api#230 (a sub-issue of D.P.1.2), and
///   approves api#210;
/// - Andi (designer) has no GitHub account; a viewer and dependabot exist
///   but must never show up.
fn example_team(conn: &Connection) {
    member(conn, "1", "Budi", "senior", Some(("budi-dev", 1)));
    member(conn, "2", "Sari", "senior", Some(("sari", 2)));
    member(conn, "3", "Rina", "medior", Some(("rina", 3)));
    member(conn, "4", "Andi", "designer", None);
    member(conn, "5", "Manager", "viewer", Some(("boss", 5)));

    issue(
        conn,
        "I_100",
        100,
        "D.P.1.2 Payment gateway integration",
        None,
        None,
        Some("D.P.1.2"),
        "2026-09-01T00:00:00Z",
        None,
    );
    issue(
        conn,
        "I_201",
        201,
        "Stripe webhook handler",
        Some(2),
        Some("I_100"),
        None,
        "2026-10-01T00:00:00Z",
        Some(("2026-10-15T08:00:00Z", "completed")),
    );
    issue(
        conn,
        "I_230",
        230,
        "Webhook signature check",
        Some(2),
        Some("I_100"),
        None,
        "2026-10-15T09:00:00Z",
        None,
    );

    pr(
        conn,
        "PR_210",
        "api",
        210,
        "Stripe webhook handler",
        Some(1),
        "merged",
        "2026-10-14T02:00:00Z",
        Some("2026-10-15T08:00:00Z"),
    );
    link(conn, "PR_210", "I_201");
    pr(
        conn,
        "PR_224",
        "api",
        224,
        "Bump serde to 1.0.215",
        Some(1),
        "merged",
        "2026-10-15T03:00:00Z",
        Some("2026-10-15T04:00:00Z"),
    );
    pr(
        conn,
        "PR_222",
        "web",
        222,
        "Fix flaky login test",
        Some(3),
        "merged",
        "2026-10-13T00:00:00Z",
        Some("2026-10-15T05:00:00Z"),
    );
    pr(
        conn,
        "PR_219",
        "api",
        219,
        "Refund flow",
        Some(2),
        "open",
        "2026-10-15T06:00:00Z",
        None,
    );
    pr(
        conn,
        "PR_221",
        "api",
        221,
        "Webhook retries",
        Some(2),
        "draft",
        "2026-10-10T00:00:00Z",
        None,
    );
    // Not the team's work: a bot, and the viewer.
    pr(
        conn,
        "PR_900",
        "api",
        900,
        "Bump tokio",
        None,
        "merged",
        "2026-10-15T01:00:00Z",
        Some("2026-10-15T01:30:00Z"),
    );
    pr(
        conn,
        "PR_901",
        "api",
        901,
        "Viewer's PR",
        Some(5),
        "merged",
        "2026-10-15T01:00:00Z",
        Some("2026-10-15T01:30:00Z"),
    );

    review(
        conn,
        "R_1",
        "PR_219",
        1,
        "changes_requested",
        "2026-10-15T10:00:00Z",
    );
    review(conn, "R_2", "PR_210", 3, "approved", "2026-10-15T07:00:00Z");
    review(
        conn,
        "R_3",
        "PR_221",
        3,
        "commented",
        "2026-10-15T07:30:00Z",
    );
    review(
        conn,
        "R_4",
        "PR_221",
        3,
        "commented",
        "2026-10-15T07:45:00Z",
    );
    review(conn, "R_5", "PR_210", 2, "approved", "2026-10-15T07:10:00Z");
}

fn day_window() -> Window {
    window(DAY, DAY, &utc()).unwrap()
}

#[test]
fn window_covers_local_days_in_utc() {
    assert_eq!(
        window(DAY, DAY, &utc()).unwrap(),
        Window {
            lo: "2026-10-15T00:00:00Z".into(),
            hi: "2026-10-16T00:00:00Z".into(),
        }
    );
    let jakarta: Tz = "Asia/Jakarta".parse().unwrap();
    assert_eq!(
        window("2026-10-01", DAY, &jakarta).unwrap(),
        Window {
            lo: "2026-09-30T17:00:00Z".into(),
            hi: "2026-10-15T17:00:00Z".into(),
        }
    );
}

#[test]
fn team_github_matches_the_design_example_for_one_day() {
    let conn = open_test_db();
    example_team(&conn);
    assert_eq!(
        team_github(&conn, DAY, DAY, &utc()).unwrap(),
        "🐙 **GitHub — 2026-10-15**\n\
         Team: 3 PRs merged · 1 sub-issue closed (D.P.1.2) · 4 reviews\n\
         \n\
         **Andi** — no GitHub account linked\n\
         **Budi** — 2 PRs merged (api#210, api#224) · 1 sub-issue closed (api#201, D.P.1.2) · 1 review\n\
         **Rina** — 1 PR merged (web#222) · 2 reviews\n\
         **Sari** — 1 PR opened (api#219) · 1 issue opened (api#230) · 1 review"
    );
}

#[test]
fn team_github_over_a_range_counts_without_listing() {
    let conn = open_test_db();
    example_team(&conn);
    let out = team_github(&conn, "2026-10-01", "2026-10-15", &utc()).unwrap();
    assert!(
        out.starts_with("🐙 **GitHub — 2026-10-01 to 2026-10-15**\n"),
        "{out}"
    );
    assert!(
        out.contains("**Budi** — 2 PRs merged · 1 sub-issue closed (D.P.1.2) · 1 review"),
        "{out}"
    );
    // api#221 was opened on Oct 10 and is still open: an "opened" PR now;
    // api#201 (Oct 1) joins api#230 as an issue she opened.
    assert!(
        out.contains("**Sari** — 2 PRs opened · 2 issues opened · 1 review"),
        "{out}"
    );
}

#[test]
fn day_boundaries_follow_the_configured_timezone() {
    let conn = open_test_db();
    member(&conn, "1", "Budi", "senior", Some(("budi-dev", 1)));
    // 18:00 UTC on Oct 15 is already Oct 16 in Jakarta (UTC+7).
    pr(
        &conn,
        "PR_1",
        "api",
        1,
        "Late merge",
        Some(1),
        "merged",
        "2026-10-15T17:00:00Z",
        Some("2026-10-15T18:00:00Z"),
    );
    let jakarta: Tz = "Asia/Jakarta".parse().unwrap();
    assert!(
        team_github(&conn, DAY, DAY, &jakarta)
            .unwrap()
            .contains("Team: no GitHub activity")
    );
    assert!(
        team_github(&conn, "2026-10-16", "2026-10-16", &jakarta)
            .unwrap()
            .contains("Team: 1 PR merged")
    );
}

#[test]
fn a_sub_issue_closed_without_a_pr_credits_its_assignees_and_not_planned_is_ignored() {
    let conn = open_test_db();
    member(&conn, "1", "Budi", "senior", Some(("budi-dev", 1)));
    member(&conn, "2", "Sari", "senior", Some(("sari", 2)));
    issue(
        &conn,
        "I_100",
        100,
        "Parent",
        None,
        None,
        Some("D.P.2.1"),
        "2026-09-01T00:00:00Z",
        None,
    );
    issue(
        &conn,
        "I_1",
        1,
        "Done by hand",
        Some(1),
        Some("I_100"),
        None,
        "2026-10-01T00:00:00Z",
        Some(("2026-10-15T08:00:00Z", "completed")),
    );
    issue(
        &conn,
        "I_2",
        2,
        "Dropped",
        Some(1),
        Some("I_100"),
        None,
        "2026-10-01T00:00:00Z",
        Some(("2026-10-15T08:00:00Z", "not_planned")),
    );
    conn.execute(
        "INSERT INTO github_issue_assignees (issue_node_id, github_id) VALUES ('I_1', 2)",
        [],
    )
    .unwrap();

    let a = team_activity(&conn, &day_window()).unwrap();
    assert_eq!(a.totals.sub_issue_sows, vec![Some("D.P.2.1".to_string())]);
    let sari = a.members.iter().find(|m| m.name == "Sari").unwrap();
    assert_eq!(sari.sub_issues_closed.len(), 1);
    let budi = a.members.iter().find(|m| m.name == "Budi").unwrap();
    assert!(budi.sub_issues_closed.is_empty());
}

#[test]
fn an_unresolved_github_user_says_so() {
    let conn = open_test_db();
    member(&conn, "1", "Budi", "senior", None);
    conn.execute("UPDATE members SET github_user = 'budi-dev'", [])
        .unwrap();
    assert!(
        team_github(&conn, DAY, DAY, &utc())
            .unwrap()
            .contains("**Budi** — GitHub account not resolved yet (next sync)")
    );
}

#[test]
fn headline_is_the_team_line_or_nothing() {
    let conn = open_test_db();
    example_team(&conn);
    let a = team_activity(&conn, &day_window()).unwrap();
    assert_eq!(
        headline(&a.totals).as_deref(),
        Some("GitHub: 3 PRs merged · 1 sub-issue closed (D.P.1.2) · 4 reviews")
    );
    assert_eq!(headline(&TeamTotals::default()), None);
}

#[test]
fn sow_summary_orders_numerically_and_counts_repeats() {
    let refs: Vec<String> = ["D.P.1.10", "D.P.1.2", "D.P.1.2", "D.P.2.1"]
        .iter()
        .map(ToString::to_string)
        .collect();
    assert_eq!(
        sow_summary(&refs).as_deref(),
        Some("D.P.1.2 ×2, D.P.1.10, D.P.2.1")
    );
}

#[test]
fn day_github_lists_unplanned_work_and_reviews_by_name() {
    let conn = open_test_db();
    example_team(&conn);
    let d = day_github(&conn, DAY, &day_window()).unwrap();

    let unplanned: Vec<(&str, &str, Option<&str>, &str)> = d
        .unplanned
        .iter()
        .map(|u| {
            (
                u.author.as_str(),
                u.item.as_str(),
                u.sow.as_deref(),
                u.today,
            )
        })
        .collect();
    assert_eq!(
        unplanned,
        vec![
            (
                "Budi",
                "PR api#210 Stripe webhook handler",
                Some("D.P.1.2"),
                "merged"
            ),
            ("Budi", "PR api#224 Bump serde to 1.0.215", None, "merged"),
            ("Rina", "PR web#222 Fix flaky login test", None, "merged"),
            // Sari authored api#201; Budi's PR closing it gets the sub-issue
            // credit, but the issue itself still shows as her item.
            (
                "Sari",
                "Issue api#201 Stripe webhook handler",
                Some("D.P.1.2"),
                "closed"
            ),
            (
                "Sari",
                "Issue api#230 Webhook signature check",
                Some("D.P.1.2"),
                "opened"
            ),
            ("Sari", "PR api#219 Refund flow", None, "opened"),
        ]
    );

    let reviews: Vec<String> = d
        .reviews
        .iter()
        .map(|r| {
            format!(
                "{}: {}",
                r.reviewer,
                r.reviewed
                    .iter()
                    .map(|(i, s)| format!("{i} {s}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        })
        .collect();
    assert_eq!(
        reviews,
        vec![
            "Budi: api#219 changes_requested",
            "Rina: api#210 approved, api#221 commented",
            "Sari: api#210 approved",
        ]
    );
}

#[test]
fn work_already_on_a_todo_is_not_listed_as_unplanned() {
    let conn = open_test_db();
    example_team(&conn);
    conn.execute(
        "INSERT INTO entries (discord_user_id, date, type, task, github_issue_id, created_at)
         VALUES ('1', '2026-10-14', 'todo', 'Stripe webhook handler', 'I_201', 'x')",
        [],
    )
    .unwrap();
    let d = day_github(&conn, DAY, &day_window()).unwrap();
    assert!(
        !d.unplanned.iter().any(|u| u.item.contains("api#210")),
        "{:?}",
        d.unplanned
    );
}

#[test]
fn format_day_sections_renders_both_tables_and_omits_empty_ones() {
    let d = DayGithub {
        unplanned: vec![UnplannedItem {
            author: "Budi".into(),
            item: "PR api#224 Bump | serde".into(),
            sow: None,
            today: "merged",
        }],
        reviews: vec![ReviewLine {
            reviewer: "Rina".into(),
            reviewed: vec![(
                ItemRef {
                    repo: "api".into(),
                    number: 210,
                },
                "changes_requested".into(),
            )],
        }],
    };
    let out = format_day_sections(&d, 2);
    assert!(
        out.starts_with("## GitHub work not on a todo\n\n| Author | Item"),
        "{out}"
    );
    assert!(out.contains("PR api#224 Bump / serde"), "{out}");
    assert!(
        out.contains("## Code reviews\n\n| Reviewer | Reviewed"),
        "{out}"
    );
    assert!(out.contains("api#210 (changes requested)"), "{out}");
    assert_eq!(format_day_sections(&DayGithub::default(), 2), "");
}

#[test]
fn github_cells_show_a_todos_issue_and_its_prs() {
    let conn = open_test_db();
    example_team(&conn);
    let rows = vec![
        RecapRow {
            github_issue_id: Some("I_201".into()),
            ..RecapRow::default()
        },
        RecapRow {
            github_pr_id: Some("PR_221".into()),
            ..RecapRow::default()
        },
        RecapRow::default(),
    ];
    assert_eq!(
        github_cells(&conn, &rows).unwrap(),
        vec!["api#201 · PR api#210 merged", "PR api#221 draft", "—"]
    );
}

#[test]
fn day_summary_has_a_headline_and_a_standup_first_file() {
    let conn = open_test_db();
    example_team(&conn);
    let (headline, file) = day_summary(&conn, DAY, &utc(), None).unwrap();
    assert!(headline.unwrap().starts_with("GitHub: 3 PRs merged"));
    let file = file.unwrap();
    assert!(
        file.starts_with(
            "# 2026-10-15\n\n## Standup\n\nNo `/todo` or `/progress` submissions.\n\n\
             ## GitHub work not on a todo"
        ),
        "{file}"
    );
    assert!(file.contains("## Code reviews"), "{file}");
}

#[test]
fn day_summary_with_nothing_at_all_has_no_file() {
    let conn = open_test_db();
    member(&conn, "1", "Budi", "senior", Some(("budi-dev", 1)));
    assert_eq!(day_summary(&conn, DAY, &utc(), None).unwrap(), (None, None));
}

/// A deliverable over Oct 1-15: D.P.1.2 with sub-issues closed in range
/// (201, 202), closed before it (198), blocked on a linked todo (205), in
/// progress via a draft PR (206), untouched (207), and dropped as not
/// planned (209); plus todos tagged with the legacy `M1D2`, a free-text
/// ref, and none; plus an unlinked PR.
fn example_deliverable(conn: &Connection) {
    member(conn, "1", "Budi", "senior", Some(("budi-dev", 1)));
    issue(
        conn,
        "I_100",
        100,
        "D.P.1.2 Payment gateway integration",
        None,
        None,
        Some("D.P.1.2"),
        "2026-09-01T00:00:00Z",
        None,
    );
    issue(
        conn,
        "I_201",
        201,
        "Stripe webhook handler",
        None,
        Some("I_100"),
        None,
        "2026-09-02T00:00:00Z",
        Some(("2026-10-06T08:00:00Z", "completed")),
    );
    issue(
        conn,
        "I_202",
        202,
        "Payment intent API",
        None,
        Some("I_100"),
        None,
        "2026-09-02T00:00:00Z",
        Some(("2026-10-08T08:00:00Z", "completed")),
    );
    issue(
        conn,
        "I_198",
        198,
        "Sandbox keys",
        None,
        Some("I_100"),
        None,
        "2026-09-02T00:00:00Z",
        Some(("2026-09-20T08:00:00Z", "completed")),
    );
    issue(
        conn,
        "I_205",
        205,
        "Refund flow",
        None,
        Some("I_100"),
        None,
        "2026-09-02T00:00:00Z",
        None,
    );
    issue(
        conn,
        "I_206",
        206,
        "Webhook retries",
        None,
        Some("I_100"),
        None,
        "2026-09-02T00:00:00Z",
        None,
    );
    issue(
        conn,
        "I_207",
        207,
        "Invoice PDF",
        None,
        Some("I_100"),
        None,
        "2026-09-02T00:00:00Z",
        None,
    );
    issue(
        conn,
        "I_209",
        209,
        "Crypto payments",
        None,
        Some("I_100"),
        None,
        "2026-09-02T00:00:00Z",
        Some(("2026-10-03T08:00:00Z", "not_planned")),
    );
    pr(
        conn,
        "PR_210",
        "api",
        210,
        "Stripe webhook",
        Some(1),
        "merged",
        "2026-10-05T00:00:00Z",
        Some("2026-10-06T08:00:00Z"),
    );
    link(conn, "PR_210", "I_201");
    pr(
        conn,
        "PR_221",
        "api",
        221,
        "Retries",
        Some(1),
        "draft",
        "2026-10-14T00:00:00Z",
        None,
    );
    link(conn, "PR_221", "I_206");
    pr(
        conn,
        "PR_224",
        "api",
        224,
        "Bump serde",
        Some(1),
        "merged",
        "2026-10-10T00:00:00Z",
        Some("2026-10-10T01:00:00Z"),
    );
    // A deliverable with no activity in range: left out.
    issue(
        conn,
        "I_300",
        300,
        "D.P.9.9 Someday",
        None,
        None,
        Some("D.P.9.9"),
        "2026-01-01T00:00:00Z",
        None,
    );

    let todo = |task: &str, sow: Option<&str>, issue: Option<&str>| -> i64 {
        conn.execute(
            "INSERT INTO entries (discord_user_id, date, type, task, sow_ref, github_issue_id,
                 created_at)
             VALUES ('1', '2026-10-14', 'todo', ?1, ?2, ?3, 'x')",
            params![task, sow, issue],
        )
        .unwrap();
        conn.last_insert_rowid()
    };
    let update = |todo_id: i64, status: &str, blocker: Option<&str>| {
        conn.execute(
            "INSERT INTO entries (discord_user_id, date, type, task, todo_id, status, progress,
                 blocker, created_at)
             VALUES ('1', '2026-10-14', 'update', 't', ?1, ?2, 'p', ?3, 'x')",
            params![todo_id, status, blocker],
        )
        .unwrap();
    };
    let a = todo("Refund flow", Some("M1D2"), Some("I_205"));
    update(a, "blocked", Some("waiting on vendor"));
    let b = todo("Payment intent", Some("D.P.1.2"), None);
    update(b, "done", None);
    let c = todo("Stripe", Some("m1d2"), None);
    update(c, "done", None);
    todo("Workshop", Some("Payment stuff"), None);
    let e = todo("Hotfix", None, None);
    update(e, "done", None);
}

fn range_deliverables(conn: &Connection) -> Vec<Deliverable> {
    let w = window("2026-10-01", DAY, &utc()).unwrap();
    deliverables(conn, "2026-10-01", DAY, &w, &utc()).unwrap()
}

#[test]
fn deliverables_count_sub_issues_prs_and_todos_per_sow_ref() {
    let conn = open_test_db();
    example_deliverable(&conn);
    let ds = range_deliverables(&conn);
    type Summary<'a> = (
        String,
        &'a str,
        Option<(usize, usize, usize)>,
        Option<usize>,
        (usize, usize),
    );
    let summary: Vec<Summary> = ds
        .iter()
        .map(|d| {
            (
                key_label(&d.key),
                d.title.as_str(),
                d.sub_issues,
                d.prs_merged,
                d.todos,
            )
        })
        .collect();
    assert_eq!(
        summary,
        vec![
            // 201, 202, 198 closed by the end (+201, 202 in range) out of six
            // (209 was dropped as not planned); api#210 merged in range;
            // M1D2 + D.P.1.2 + m1d2 todos, two done.
            (
                "D.P.1.2".into(),
                "Payment gateway integration",
                Some((3, 6, 2)),
                Some(1),
                (2, 3)
            ),
            ("Payment stuff".into(), "Payment stuff", None, None, (0, 1)),
            (
                "—".into(),
                "Not linked to a deliverable",
                None,
                Some(1),
                (1, 1)
            ),
        ]
    );
}

#[test]
fn deliverable_breakdown_orders_by_state_and_collapses_earlier_closes() {
    let conn = open_test_db();
    example_deliverable(&conn);
    let ds = range_deliverables(&conn);
    let rows: Vec<(String, SubIssueState, String, Option<String>)> = ds[0]
        .breakdown
        .iter()
        .map(|r| {
            (
                r.item.to_string(),
                r.state,
                r.prs.join(", "),
                r.last_activity.clone(),
            )
        })
        .collect();
    assert_eq!(
        rows,
        vec![
            (
                "api#205".into(),
                SubIssueState::Blocked,
                "".into(),
                Some("Oct 14 \"waiting on vendor\"".into())
            ),
            ("api#207".into(), SubIssueState::NotStarted, "".into(), None),
            (
                "api#206".into(),
                SubIssueState::InProgress,
                "api#221 draft".into(),
                Some("Oct 14 PR updated".into())
            ),
            (
                "api#201".into(),
                SubIssueState::Closed,
                "api#210 merged".into(),
                Some("Oct 6 closed".into())
            ),
            (
                "api#202".into(),
                SubIssueState::Closed,
                "".into(),
                Some("Oct 8 closed".into())
            ),
        ]
    );
    assert_eq!(
        ds[0]
            .closed_before
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>(),
        vec!["api#198"]
    );
}

#[test]
fn recap_file_is_person_free_unless_member_detail_is_asked_for() {
    let conn = open_test_db();
    example_deliverable(&conn);
    let plain = recap_file(&conn, "2026-10-01", DAY, &utc(), false)
        .unwrap()
        .unwrap();
    assert!(
        plain.starts_with("# Recap 2026-10-01 → 2026-10-15\n\n## Deliverables\n\n"),
        "{plain}"
    );
    assert!(
        plain.contains("### D.P.1.2 — Payment gateway integration (3/6 closed)"),
        "{plain}"
    );
    assert!(
        plain.contains("Closed before this period: api#198"),
        "{plain}"
    );
    assert!(plain.contains("3/6 closed (+2)"), "{plain}");
    assert!(
        !plain.contains("Budi"),
        "names leaked into the stakeholder file:\n{plain}"
    );
    assert!(!plain.contains("Member detail"), "{plain}");

    let detailed = recap_file(&conn, "2026-10-01", DAY, &utc(), true)
        .unwrap()
        .unwrap();
    assert!(
        detailed.contains("\n\n## Member detail by day\n\n### 2026-10-05\n\n#### Standup"),
        "{detailed}"
    );
    assert!(
        detailed.contains("### 2026-10-14\n\n#### Standup\n\n| Member"),
        "{detailed}"
    );
    assert!(detailed.contains("Budi"), "{detailed}");
}

#[test]
fn recap_file_is_none_when_nothing_happened() {
    let conn = open_test_db();
    member(&conn, "1", "Budi", "senior", Some(("budi-dev", 1)));
    assert_eq!(
        recap_file(&conn, "2026-10-01", DAY, &utc(), true).unwrap(),
        None
    );
}
