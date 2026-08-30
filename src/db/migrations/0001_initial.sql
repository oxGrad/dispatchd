-- Team roster — source of truth for who's on the team and permissions
CREATE TABLE members (
    discord_user_id TEXT PRIMARY KEY,
    name            TEXT NOT NULL,
    role            TEXT NOT NULL,   -- 'lead' | 'designer' | 'senior' | 'medior' | 'junior'
    is_lead         BOOLEAN NOT NULL DEFAULT FALSE
);

-- All todo and update submissions
CREATE TABLE entries (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    discord_user_id TEXT NOT NULL,
    date            TEXT NOT NULL,      -- 'YYYY-MM-DD'
    type            TEXT NOT NULL,      -- 'todo' | 'update'
    task            TEXT NOT NULL,      -- todo text, or resolved task text for an update
    todo_id         INTEGER,            -- FK to entries.id (a 'todo' row); NULL if ad-hoc/unplanned work
    notes           TEXT,               -- todo-only: optional context/plan
    status          TEXT,               -- update-only: 'done' | 'in_progress' | 'blocked'
    progress        TEXT,               -- update-only: what actually got done
    blocker         TEXT,               -- update-only: optional
    created_at      TEXT NOT NULL,
    FOREIGN KEY (todo_id) REFERENCES entries(id)
);

-- Tracks whether the 9am/3pm/4pm reminder already fired for a given date,
-- so a bot restart never causes a double-post
CREATE TABLE reminders_sent (
    date TEXT NOT NULL,
    type TEXT NOT NULL,   -- 'todo_reminder' | 'update_reminder' | 'meeting_reminder' | 'thread_creation'
    PRIMARY KEY (date, type)
);

-- Tracks whether the missing-todo/missing-update follow-up already fired
-- for a person/date, enforcing "send at most once"
CREATE TABLE followups_sent (
    date            TEXT NOT NULL,
    discord_user_id TEXT NOT NULL,
    type            TEXT NOT NULL,   -- 'todo_followup' | 'update_followup'
    PRIMARY KEY (date, discord_user_id, type)
);
