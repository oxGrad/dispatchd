-- Historical record of days a member submitted no /todo, or no
-- /progress update, at all. Written once per day by the ticker
-- (alongside the day_summary/day_summary_missing posts, at
-- day_summary_time) so /missed can report on any date range later.
-- Unlike reminders_sent/followups_sent, never pruned by
-- `maintenance run` - retained like entries, since it's the history
-- /missed reports against.
CREATE TABLE missed_submissions (
    date            TEXT NOT NULL,
    discord_user_id TEXT NOT NULL,
    kind            TEXT NOT NULL,   -- 'todo' | 'update'
    PRIMARY KEY (date, discord_user_id, kind)
);
