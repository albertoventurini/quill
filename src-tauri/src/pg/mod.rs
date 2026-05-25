//! Real Postgres `Connector` implementation for the slot manager.
//!
//! M3.1 migration: switched from sqlx-postgres to
//! `tokio_postgres::Client`.  The driving reason is M3.2/M3.3 cancellation —
//! tokio-postgres exposes `Client::cancel_token()` (backend PID + secret key
//! captured during the protocol startup handshake), which sqlx 0.8 hides
//! behind crate-private fields.  See `MILESTONES.md` §M3.
//!
//! AGENTS.md principle 2: the slot manager *is* the pool.  This module
//! deliberately uses a raw `Client`, never `tokio_postgres::Pool` — there is
//! no built-in pool in tokio-postgres anyway.

use std::sync::Arc;

use async_trait::async_trait;
use secrecy::{ExposeSecret, SecretString};
use tokio_postgres::{CancelToken, Client, Config, NoTls, config::SslMode};
use tokio_postgres_rustls::MakeRustlsConnect;

use crate::slots::{Connector, ConnectorError};

/// Logical SSL policy parsed from the textual `ssl_mode` stored on
/// `connections.ssl_mode`.  Mirrors libpq.
#[derive(Debug, Clone, Copy)]
pub enum SslPolicy {
    Disable,
    Allow,
    Prefer,
    Require,
    VerifyCa,
    VerifyFull,
}

impl SslPolicy {
    /// Map to tokio-postgres' [`SslMode`].  `VerifyCa` / `VerifyFull` both
    /// degrade to `Require` because v1 does not ship custom root certs;
    /// the spec writer should re-examine when M6 polish lands.
    fn as_tokio(self) -> SslMode {
        match self {
            SslPolicy::Disable => SslMode::Disable,
            SslPolicy::Allow | SslPolicy::Prefer => SslMode::Prefer,
            SslPolicy::Require | SslPolicy::VerifyCa | SslPolicy::VerifyFull => SslMode::Require,
        }
    }

    /// Whether we need to build a TLS connector at all for this policy.
    /// `Disable` skips TLS entirely; `Prefer` *may* upgrade if the server
    /// supports it, so we still pass a connector when the user asked for it.
    fn wants_tls(self) -> bool {
        !matches!(self, SslPolicy::Disable)
    }
}

pub struct PgConnector {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub password: SecretString,
    pub ssl_mode: SslPolicy,
}

impl PgConnector {
    /// Map the textual `ssl_mode` stored in the SQLite `connections` table
    /// to the typed [`SslPolicy`].  Accepts the same spellings as libpq.
    pub fn parse_ssl_mode(s: &str) -> Result<SslPolicy, ConnectorError> {
        Ok(match s {
            "disable" => SslPolicy::Disable,
            "allow" => SslPolicy::Allow,
            "prefer" => SslPolicy::Prefer,
            "require" => SslPolicy::Require,
            "verify-ca" => SslPolicy::VerifyCa,
            "verify-full" => SslPolicy::VerifyFull,
            other => return Err(ConnectorError(format!("unknown ssl_mode: {other}"))),
        })
    }

    fn build_config(&self, database: &str) -> Config {
        let mut config = Config::new();
        config
            .host(&self.host)
            .port(self.port)
            .dbname(database)
            .user(&self.username)
            .password(self.password.expose_secret())
            .application_name("quill")
            .ssl_mode(self.ssl_mode.as_tokio());
        config
    }
}

#[async_trait]
impl Connector for PgConnector {
    type Conn = Client;
    type Cancel = PgCancelHandle;

    async fn connect(&self, database: &str) -> Result<(Self::Conn, Self::Cancel), ConnectorError> {
        let config = self.build_config(database);

        let (client, cancel_token) = if self.ssl_mode.wants_tls() {
            let tls =
                make_rustls().map_err(|e| ConnectorError(format!("rustls setup failed: {e}")))?;
            let (client, connection) = config
                .connect(tls)
                .await
                .map_err(|e| ConnectorError(e.to_string()))?;
            let cancel_token = client.cancel_token();
            spawn_connection_driver(connection);
            (client, cancel_token)
        } else {
            let (client, connection) = config
                .connect(NoTls)
                .await
                .map_err(|e| ConnectorError(e.to_string()))?;
            let cancel_token = client.cancel_token();
            spawn_connection_driver(connection);
            (client, cancel_token)
        };

        Ok((client, PgCancelHandle::new(cancel_token, self.ssl_mode)))
    }

    /// tokio-postgres has no explicit `close` — dropping the [`Client`]
    /// causes the spawned connection task to exit on its next poll.  We just
    /// consume the value so callers can stop holding it.
    async fn close(_conn: Self::Conn) {
        // intentionally empty
    }
}

/// Spawn the driver future returned alongside the `Client`.  The driver is
/// what actually pumps bytes between the socket and the client; without it
/// queries hang forever.  Errors are logged to stderr — there is no UI path
/// for "connection silently went away" in v1.
fn spawn_connection_driver<S, T>(connection: tokio_postgres::Connection<S, T>)
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
    T: tokio_postgres::tls::TlsStream + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        if let Err(e) = connection.await {
            tracing::error!("postgres connection task ended with error: {e}");
        }
    });
}

/// Build a rustls-based TLS connector.  Uses webpki-roots — no custom CA
/// support in v1.
fn make_rustls() -> Result<MakeRustlsConnect, Box<dyn std::error::Error>> {
    use rustls::ClientConfig;

    // Install the default crypto provider once per process.  Calling this
    // twice is harmless (it returns Err that we ignore); calling it never
    // is fatal at handshake time.
    let _ = rustls::crypto::ring::default_provider().install_default();

    let mut root_store = rustls::RootCertStore::empty();
    root_store.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());

    let config = ClientConfig::builder()
        .with_root_certificates(root_store)
        .with_no_client_auth();

    Ok(MakeRustlsConnect::new(config))
}

// ═══════════════════════════════════════════════════════════════════════════
// Cancel handle
// ═══════════════════════════════════════════════════════════════════════════

/// Cancel handle for a single Postgres backend.
///
/// Carries the [`CancelToken`] (which embeds PID + secret key from the
/// `BackendKeyData` startup message) and the [`SslPolicy`] used by the
/// originating connection — `cancel_query` opens its own TCP socket and
/// must match the SSL setup of the original handshake.
///
/// Wrapped in [`Arc`] so the slot manager can clone it cheaply for hand-off
/// to the `cancel_query` Tauri command.
#[derive(Clone)]
pub struct PgCancelHandle {
    inner: Arc<PgCancelInner>,
}

struct PgCancelInner {
    token: CancelToken,
    ssl_policy: SslPolicy,
}

impl PgCancelHandle {
    fn new(token: CancelToken, ssl_policy: SslPolicy) -> Self {
        Self {
            inner: Arc::new(PgCancelInner { token, ssl_policy }),
        }
    }

    /// Send a `CancelRequest` over a fresh TCP connection.  Returns the
    /// underlying tokio-postgres error message on failure; the cancel is
    /// best-effort — Postgres returns no acknowledgement.
    pub async fn cancel(&self) -> Result<(), String> {
        let inner = &self.inner;
        if inner.ssl_policy.wants_tls() {
            let tls = make_rustls().map_err(|e| format!("rustls setup failed: {e}"))?;
            inner
                .token
                .cancel_query(tls)
                .await
                .map_err(|e| e.to_string())
        } else {
            inner
                .token
                .cancel_query(NoTls)
                .await
                .map_err(|e| e.to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_ssl_mode_round_trips_known_values() {
        for (input, expected) in [
            ("disable", SslPolicy::Disable),
            ("allow", SslPolicy::Allow),
            ("prefer", SslPolicy::Prefer),
            ("require", SslPolicy::Require),
            ("verify-ca", SslPolicy::VerifyCa),
            ("verify-full", SslPolicy::VerifyFull),
        ] {
            let parsed = PgConnector::parse_ssl_mode(input).expect("valid ssl_mode");
            assert!(
                std::mem::discriminant(&parsed) == std::mem::discriminant(&expected),
                "parse_ssl_mode({input:?}) returned wrong variant: {parsed:?}"
            );
        }
    }

    #[test]
    fn parse_ssl_mode_rejects_garbage() {
        assert!(PgConnector::parse_ssl_mode("nope").is_err());
        assert!(PgConnector::parse_ssl_mode("").is_err());
        assert!(PgConnector::parse_ssl_mode("DISABLE").is_err());
    }
}
