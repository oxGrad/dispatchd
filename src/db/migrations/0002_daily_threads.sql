-- Maps a date to the Discord thread created for that day's standup, so
-- the 3pm/4pm reminders can post into the same thread the 9am one
-- created - including across a bot restart in between, since nothing
-- here can rely on in-memory state surviving.
CREATE TABLE daily_threads (
    date      TEXT PRIMARY KEY,
    thread_id TEXT NOT NULL
);
