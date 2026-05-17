//! Real Postgres `Connector` implementation for the slot manager.
//!
//! See AGENTS.md principle 2: the slot manager *is* the connection pool.
//! This module deliberately uses a raw `PgConnection`, never `PgPool` —
//! a pool would defeat the budget by opening connections behind our back.

use async_trait::async_trait;
use secrecy::{ExposeSecret, SecretString};
use sqlx::Connection as _;
use sqlx::PgConnection;
use sqlx::postgres::{PgConnectOptions, PgSslMode};

use crate::slots::{Connector, ConnectorError};

/// Connection parameters for a single saved server.
///
/// The password is held as a `SecretString` so it never appears in `Debug`
/// output or in panic messages.  M6 will populate this from the OS keychain;
/// in M1 it comes from the user's in-process input via the `connect_server`
/// command (which lands in M1.5).
pub struct PgConnector {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub password: SecretString,
    pub ssl_mode: PgSslMode,
}

impl PgConnector {
    /// Map the textual `ssl_mode` stored in the SQLite `connections` table
    /// to the typed `PgSslMode`.  Accepts the same spellings as libpq.
    pub fn parse_ssl_mode(s: &str) -> Result<PgSslMode, ConnectorError> {
        match s {
            "disable" => Ok(PgSslMode::Disable),
            "allow" => Ok(PgSslMode::Allow),
            "prefer" => Ok(PgSslMode::Prefer),
            "require" => Ok(PgSslMode::Require),
            "verify-ca" => Ok(PgSslMode::VerifyCa),
            "verify-full" => Ok(PgSslMode::VerifyFull),
            other => Err(ConnectorError(format!("unknown ssl_mode: {other}"))),
        }
    }
}

#[async_trait]
impl Connector for PgConnector {
    type Conn = PgConnection;

    async fn connect(&self, database: &str) -> Result<Self::Conn, ConnectorError> {
        let opts = PgConnectOptions::new()
            .host(&self.host)
            .port(self.port)
            .database(database)
            .username(&self.username)
            .password(self.password.expose_secret())
            .ssl_mode(self.ssl_mode)
            .application_name("quill");

        PgConnection::connect_with(&opts)
            .await
            .map_err(|e| ConnectorError(e.to_string()))
    }

    async fn close(conn: Self::Conn) {
        let _ = conn.close().await;
    }
}

// TODO(M3): Cancellation plumbing…
