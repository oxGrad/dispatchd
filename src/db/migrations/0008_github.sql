-- GitHub activity, synced from the org by the GitHub App (see
-- docs/superpowers/specs/2026-10-15-github-activity-design.md). Every
-- github_* table is retained forever, like entries and missed_submissions:
-- maintenance.rs never prunes them, since the recap reads their history.
-- Timestamps are UTC RFC3339 strings exactly as GitHub sends them.

-- The member's GitHub username from members.toml, and the numeric user id
-- the sync resolves it to (activity rows are matched on the id, so a
-- username rename doesn't orphan history). Both are rewritten by every
-- reseed; the id is re-resolved by the next sync.
ALTER TABLE members ADD COLUMN github_user TEXT;
ALTER TABLE members ADD COLUMN github_id INTEGER;

-- todo-only, like sow_ref/notes: the issue a /todo was created from.
-- Set by `/todo add issue:` (design phase 4); a GraphQL node id.
ALTER TABLE entries ADD COLUMN github_issue_id TEXT;
-- update-only: the PR a /progress report was filed from (phase 4).
ALTER TABLE entries ADD COLUMN github_pr_id TEXT;

CREATE TABLE github_issues (
    node_id        TEXT PRIMARY KEY,
    repo           TEXT NOT NULL,
    number         INTEGER NOT NULL,
    title          TEXT NOT NULL,
    state          TEXT NOT NULL,      -- 'open' | 'closed'
    state_reason   TEXT,               -- 'completed' | 'not_planned' | 'reopened' | 'duplicate'
    author_id      INTEGER,            -- NULL for bots / deleted accounts
    author_login   TEXT,
    created_at     TEXT NOT NULL,
    closed_at      TEXT,
    parent_node_id TEXT,               -- sub-issue -> parent issue
    sow_ref        TEXT,               -- normalised (sow.rs), set on parents only
    updated_at     TEXT NOT NULL
);
CREATE INDEX github_issues_parent ON github_issues (parent_node_id);

CREATE TABLE github_issue_assignees (
    issue_node_id TEXT NOT NULL,
    github_id     INTEGER NOT NULL,
    PRIMARY KEY (issue_node_id, github_id)
);

CREATE TABLE github_prs (
    node_id      TEXT PRIMARY KEY,
    repo         TEXT NOT NULL,
    number       INTEGER NOT NULL,
    title        TEXT NOT NULL,
    state        TEXT NOT NULL,        -- 'open' | 'merged' | 'closed'
    is_draft     INTEGER NOT NULL,
    author_id    INTEGER,
    author_login TEXT,
    created_at   TEXT NOT NULL,
    merged_at    TEXT,
    closed_at    TEXT,
    updated_at   TEXT NOT NULL
);
CREATE INDEX github_prs_author ON github_prs (author_id);

-- A PR's closingIssuesReferences: issues linked from the PR sidebar's
-- "Development" panel (or a closing keyword).
CREATE TABLE github_pr_issues (
    pr_node_id    TEXT NOT NULL,
    issue_node_id TEXT NOT NULL,
    PRIMARY KEY (pr_node_id, issue_node_id)
);
CREATE INDEX github_pr_issues_issue ON github_pr_issues (issue_node_id);

CREATE TABLE github_reviews (
    node_id      TEXT PRIMARY KEY,
    pr_node_id   TEXT NOT NULL,
    author_id    INTEGER,
    author_login TEXT,
    state        TEXT NOT NULL,        -- 'approved' | 'changes_requested' | 'commented' | 'dismissed'
    submitted_at TEXT NOT NULL
);
CREATE INDEX github_reviews_author ON github_reviews (author_id, submitted_at);

-- Sync bookkeeping: 'pr_high_water' / 'issue_high_water' (the newest
-- updatedAt seen), 'last_sync_at', 'last_sync_error'.
CREATE TABLE github_sync_state (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
