-- 'admin' role: a strict superset of the tech lead. Seeded rows with
-- role='admin' get is_admin=1 AND is_lead=1, so every existing is_lead
-- permission check admits admins with no code change.
ALTER TABLE members ADD COLUMN is_admin BOOLEAN NOT NULL DEFAULT FALSE;
