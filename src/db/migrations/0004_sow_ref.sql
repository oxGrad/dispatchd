-- Purely informational cross-reference to a scope-of-work deliverable
-- (e.g. 'M1D2' for milestone 1, deliverable 2, or just 'M1' for the
-- milestone alone) - free text, never validated against any format or
-- external list. todo-only, like `notes`.
ALTER TABLE entries ADD COLUMN sow_ref TEXT;
