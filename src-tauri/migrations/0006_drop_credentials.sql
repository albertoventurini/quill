-- Quill no longer stores any credentials in SQLite.
--
-- `username` was persisted per-connection and sent on password-auth connects;
-- it is now prompted for on every connect alongside the password (and supplied
-- dynamically by OpenBao for vault-backed connections). `password_ref` was an
-- always-NULL keyring placeholder for an OS-keychain feature that was never
-- built. Drop both: the connections table holds no credential fields.
ALTER TABLE connections DROP COLUMN username;
ALTER TABLE connections DROP COLUMN password_ref;
