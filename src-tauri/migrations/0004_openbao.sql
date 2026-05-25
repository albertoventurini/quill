ALTER TABLE connections ADD COLUMN credential_source TEXT NOT NULL DEFAULT 'password';
ALTER TABLE connections ADD COLUMN bao_role_path TEXT;

CREATE TABLE settings (
    key   TEXT NOT NULL PRIMARY KEY,
    value TEXT NOT NULL
);
