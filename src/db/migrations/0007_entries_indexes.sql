-- entries is never pruned, and nearly every query filters it by day (and
-- usually member) or by the todo an update reports against - without
-- these, each of those (and every FK check on deleting a todo) is a full
-- table scan that grows with the team's whole history.
CREATE INDEX idx_entries_date_user ON entries (date, discord_user_id);
CREATE INDEX idx_entries_todo_id ON entries (todo_id);
