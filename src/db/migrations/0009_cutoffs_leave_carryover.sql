-- One row per member per leave/sick day (ritual days only). Never pruned.
CREATE TABLE member_leave (
    date            TEXT NOT NULL,
    discord_user_id TEXT NOT NULL,
    reason          TEXT NOT NULL,   -- 'leave' | 'sick'
    PRIMARY KEY (date, discord_user_id)
);
-- One row per process start; last_seen bumped every ticker tick. Never
-- pruned - the cut-off waiver for any recorded day is derived from it.
CREATE TABLE uptime_spans (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    started_at TEXT NOT NULL,
    last_seen  TEXT NOT NULL
);
-- Small durable key/value facts, e.g. 'cutoff_rules_since'.
CREATE TABLE app_meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
-- The earlier todo this one continues (explicit carry-over).
ALTER TABLE entries ADD COLUMN carried_from INTEGER REFERENCES entries(id);
CREATE UNIQUE INDEX idx_entries_carried_from ON entries (carried_from)
    WHERE carried_from IS NOT NULL;
