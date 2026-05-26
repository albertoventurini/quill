use std::sync::Mutex;
use std::time::Duration;

use secrecy::{ExposeSecret, SecretString};
use serde::Deserialize;
use sqlx::SqlitePool;
use thiserror::Error;

const KEYRING_SERVICE: &str = "quill";
const KEYRING_ACCOUNT: &str = "openbao_token";

/// Where the OpenBao token lives at runtime.
///
/// Preference order is OS keyring → in-memory. Any keyring failure (no Secret Service
/// provider, locked keychain, denied access) is treated the same: the token is kept in
/// memory for this session only and the caller is told it did not persist, so the UI can
/// warn that a re-login will be needed after restart. The keyring is only ever touched on
/// explicit user actions (login, connect, refresh, opening Settings) — never eagerly at
/// launch.
#[derive(Default)]
pub struct TokenStore {
    mem: Mutex<Option<SecretString>>,
    persisted: Mutex<bool>,
}

#[derive(Clone, Copy, Debug)]
pub enum StoreOutcome {
    Persisted,
    InMemoryOnly,
}

impl StoreOutcome {
    pub fn persisted(self) -> bool {
        matches!(self, StoreOutcome::Persisted)
    }
}

impl TokenStore {
    /// Store `token`, trying the OS keyring first. Always succeeds (falls back to memory);
    /// the returned outcome says whether it actually persisted.
    pub async fn store(&self, token: &str) -> StoreOutcome {
        let owned = token.to_string();
        let persisted = tokio::task::spawn_blocking(move || {
            keyring::Entry::new(KEYRING_SERVICE, KEYRING_ACCOUNT)
                .and_then(|e| e.set_password(&owned))
                .is_ok()
        })
        .await
        .unwrap_or(false);

        *self.mem.lock().unwrap() = Some(SecretString::from(token.to_string()));
        *self.persisted.lock().unwrap() = persisted;

        if persisted {
            StoreOutcome::Persisted
        } else {
            tracing::warn!(
                "OS keyring unavailable; keeping OpenBao token in memory for this session only"
            );
            StoreOutcome::InMemoryOnly
        }
    }

    /// Return the token, checking memory first and falling back to the keyring (which
    /// hydrates memory on a hit). `None` if neither holds one.
    pub async fn load(&self) -> Option<SecretString> {
        if let Some(tok) = self.mem.lock().unwrap().as_ref() {
            return Some(SecretString::from(tok.expose_secret().to_string()));
        }
        let fetched = tokio::task::spawn_blocking(|| {
            keyring::Entry::new(KEYRING_SERVICE, KEYRING_ACCOUNT)
                .and_then(|e| e.get_password())
                .ok()
        })
        .await
        .ok()
        .flatten()?;

        *self.mem.lock().unwrap() = Some(SecretString::from(fetched.clone()));
        *self.persisted.lock().unwrap() = true;
        Some(SecretString::from(fetched))
    }

    /// Forget the token: wipe memory and best-effort delete from the keyring.
    pub async fn clear(&self) {
        *self.mem.lock().unwrap() = None;
        *self.persisted.lock().unwrap() = false;
        let _ = tokio::task::spawn_blocking(|| {
            if let Ok(e) = keyring::Entry::new(KEYRING_SERVICE, KEYRING_ACCOUNT) {
                let _ = e.delete_credential();
            }
        })
        .await;
    }

    /// Seed the in-memory token without touching the keyring. Used by the one-time
    /// plaintext→keyring migration so the current session keeps working.
    pub fn seed_memory(&self, token: &str) {
        *self.mem.lock().unwrap() = Some(SecretString::from(token.to_string()));
        *self.persisted.lock().unwrap() = false;
    }

    /// Cheap, mem-only snapshot: `(present, persisted)`.
    pub fn status(&self) -> (bool, bool) {
        let present = self.mem.lock().unwrap().is_some();
        let persisted = *self.persisted.lock().unwrap();
        (present, persisted)
    }
}

/// One-time migration: lift any legacy plaintext token out of the SQLite `settings` table.
/// Seeds it into memory for the current session, then deletes the plaintext row so no secret
/// is left behind on disk. The next login persists it to the keyring properly.
pub async fn migrate_plaintext_token(pool: &SqlitePool, tokens: &TokenStore) {
    if let Some(token) = get_setting(pool, "openbao_token").await {
        tokens.seed_memory(&token);
        match remove_setting(pool, "openbao_token").await {
            Ok(()) => tracing::info!("migrated OpenBao token out of plaintext settings storage"),
            Err(e) => tracing::warn!("failed to delete legacy plaintext OpenBao token: {e}"),
        }
    }
}

#[derive(Debug, Error)]
pub enum OpenBaoError {
    #[error("OpenBao not configured — set the server address in Settings")]
    NotConfigured,
    #[error("no OpenBao token — login in Settings")]
    NoToken,
    #[error("OpenBao request failed: {0}")]
    Request(String),
    #[error("OpenBao returned unexpected response: {0}")]
    BadResponse(String),
    #[error("OIDC login timed out")]
    LoginTimeout,
    #[error("OIDC callback failed: {0}")]
    LoginCallback(String),
    #[error("settings error: {0}")]
    Store(String),
}

impl From<reqwest::Error> for OpenBaoError {
    fn from(e: reqwest::Error) -> Self {
        Self::Request(e.to_string())
    }
}

impl From<sqlx::Error> for OpenBaoError {
    fn from(e: sqlx::Error) -> Self {
        Self::Store(e.to_string())
    }
}

#[derive(Debug, Deserialize)]
struct OidcAuthUrlResponse {
    data: OidcAuthUrlData,
}

#[derive(Debug, Deserialize)]
struct OidcAuthUrlData {
    auth_url: String,
}

#[derive(Debug, Deserialize)]
struct OidcCallbackResponse {
    auth: OidcCallbackAuth,
}

#[derive(Debug, Deserialize)]
struct OidcCallbackAuth {
    client_token: String,
}

#[derive(Debug, Deserialize)]
struct PgCredsResponse {
    data: PgCredsData,
    // The database secrets engine reports the lease TTL at the top level of the response,
    // not inside `data`.
    #[serde(default)]
    lease_duration: u64,
}

#[derive(Debug, Deserialize)]
struct PgCredsData {
    username: String,
    password: String,
}

pub struct PgCredentials {
    pub username: String,
    pub password: SecretString,
    pub lease_duration_secs: u64,
}

pub struct OpenBaoClient {
    pub addr: String,
    token: SecretString,
    client: reqwest::Client,
}

pub async fn set_setting(pool: &SqlitePool, key: &str, value: &str) -> Result<(), OpenBaoError> {
    sqlx::query(
        "INSERT INTO settings(key, value) VALUES (?, ?) \
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
    )
    .bind(key)
    .bind(value)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn get_setting(pool: &SqlitePool, key: &str) -> Option<String> {
    sqlx::query_scalar::<_, String>("SELECT value FROM settings WHERE key = ?")
        .bind(key)
        .fetch_optional(pool)
        .await
        .ok()
        .flatten()
}

pub async fn get_all_settings(
    pool: &SqlitePool,
) -> Result<std::collections::HashMap<String, String>, OpenBaoError> {
    let rows = sqlx::query_as::<_, (String, String)>("SELECT key, value FROM settings")
        .fetch_all(pool)
        .await?;
    Ok(rows.into_iter().collect())
}

pub async fn remove_setting(pool: &SqlitePool, key: &str) -> Result<(), OpenBaoError> {
    sqlx::query("DELETE FROM settings WHERE key = ?")
        .bind(key)
        .execute(pool)
        .await?;
    Ok(())
}

impl OpenBaoClient {
    pub async fn from_store(
        pool: &SqlitePool,
        tokens: &TokenStore,
    ) -> Result<Option<Self>, OpenBaoError> {
        let addr = match get_setting(pool, "openbao_addr").await {
            Some(a) => a,
            None => return Ok(None),
        };
        let token = match tokens.load().await {
            Some(t) => t,
            None => return Ok(None),
        };
        Ok(Some(Self {
            addr,
            token,
            client: reqwest::Client::new(),
        }))
    }

    pub async fn fetch_pg_creds(&self, role_path: &str) -> Result<PgCredentials, OpenBaoError> {
        let url = format!("{}/v1/{}", self.addr.trim_end_matches('/'), role_path);

        let resp = self
            .client
            .get(&url)
            .header("X-Vault-Token", self.token.expose_secret())
            .send()
            .await?;

        let status = resp.status();
        let body_text = resp.text().await.unwrap_or_default();

        if !status.is_success() {
            tracing::error!("OpenBao ({url}) returned {status}: {body_text}",);
            return Err(OpenBaoError::Request(format!(
                "OpenBao returned {status}: {}",
                truncate_body(&body_text),
            )));
        }

        let creds: PgCredsResponse = serde_json::from_str(&body_text).map_err(|e| {
            tracing::error!(
                "OpenBao creds parse failed (status {status}): body={body_text} error={e}",
            );
            OpenBaoError::BadResponse(format!(
                "failed to parse credentials: {e}. Body: {}",
                truncate_body(&body_text),
            ))
        })?;

        Ok(PgCredentials {
            username: creds.data.username,
            password: SecretString::from(creds.data.password),
            lease_duration_secs: if creds.lease_duration > 0 {
                creds.lease_duration
            } else {
                3600
            },
        })
    }
}

pub async fn start_oidc_login(
    bao_addr: &str,
    role: &str,
    app_handle: &tauri::AppHandle,
) -> Result<String, OpenBaoError> {
    use tauri_plugin_opener::OpenerExt;

    let client = reqwest::Client::new();

    // The OIDC role pins an exact redirect_uri in its allowed_redirect_uris. Our org (like the
    // Vault/OpenBao CLI default) registers http://localhost:8250/oidc/callback, so we bind that
    // exact port and path — a random port is silently rejected (auth_url comes back empty).
    let redirect_uri = "http://localhost:8250/oidc/callback";
    let listener = std::net::TcpListener::bind("127.0.0.1:8250").map_err(|e| {
        OpenBaoError::LoginCallback(format!(
            "could not bind 127.0.0.1:8250 for the OIDC callback \
             (another login in progress, or port already in use?): {e}"
        ))
    })?;

    let auth_url = format!(
        "{}/v1/auth/oidc/oidc/auth_url",
        bao_addr.trim_end_matches('/')
    );

    // An empty role tells OpenBao to use the mount's configured default_role — this is what the
    // Vault UI's "Default" means. Only send `role` when the user configured an explicit one.
    let mut body = serde_json::json!({ "redirect_uri": redirect_uri });
    if !role.is_empty() {
        body["role"] = serde_json::Value::String(role.to_string());
    }

    let resp = client.post(&auth_url).json(&body).send().await?;

    let status = resp.status();
    let body_text = resp.text().await.unwrap_or_default();

    if !status.is_success() {
        tracing::error!(
            "OpenBao auth_url ({}) returned {status}: {body_text}",
            auth_url,
        );
        return Err(OpenBaoError::BadResponse(format!(
            "auth_url returned {status}: {}",
            truncate_body(&body_text),
        )));
    }

    let auth_url_resp: OidcAuthUrlResponse = serde_json::from_str(&body_text).map_err(|e| {
        tracing::error!(
            "OpenBao auth_url parse failed (status {status}): body={body_text} error={e}",
        );
        OpenBaoError::BadResponse(format!(
            "auth_url returned {status}, could not parse response: {e}. Body: {}",
            truncate_body(&body_text),
        ))
    })?;

    if auth_url_resp.data.auth_url.is_empty() {
        return Err(OpenBaoError::BadResponse(
            "OpenBao returned an empty auth_url. The redirect_uri \
             (http://localhost:8250/oidc/callback) is not in the OIDC role's \
             allowed_redirect_uris, or the mount has no default role configured."
                .into(),
        ));
    }

    app_handle
        .opener()
        .open_url(&auth_url_resp.data.auth_url, None::<&str>)
        .map_err(|e| OpenBaoError::LoginCallback(format!("browser open failed: {e}")))?;

    let (code, state) = accept_callback(listener).await?;

    let cb_url = format!(
        "{}/v1/auth/oidc/oidc/callback",
        bao_addr.trim_end_matches('/')
    );

    // OpenBao's OIDC callback is a GET with state/code as query params (not a POST body) —
    // posting returns 405 Method Not Allowed.
    let resp = client
        .get(&cb_url)
        .query(&[("state", state.as_str()), ("code", code.as_str())])
        .send()
        .await?;

    let status = resp.status();
    let body_text = resp.text().await.unwrap_or_default();

    if !status.is_success() {
        tracing::error!(
            "OpenBao callback ({}) returned {status}: {body_text}",
            cb_url,
        );
        return Err(OpenBaoError::BadResponse(format!(
            "callback returned {status}: {}",
            truncate_body(&body_text),
        )));
    }

    let cb_resp: OidcCallbackResponse = serde_json::from_str(&body_text).map_err(|e| {
        tracing::error!(
            "OpenBao callback parse failed (status {status}): body={body_text} error={e}",
        );
        OpenBaoError::BadResponse(format!(
            "callback returned {status}, could not parse response: {e}. Body: {}",
            truncate_body(&body_text),
        ))
    })?;

    Ok(cb_resp.auth.client_token)
}

fn truncate_body(body: &str) -> &str {
    if body.len() > 300 { &body[..300] } else { body }
}

async fn accept_callback(
    listener: std::net::TcpListener,
) -> Result<(String, String), OpenBaoError> {
    use tokio::io::AsyncBufReadExt;
    use tokio::io::AsyncWriteExt;

    listener
        .set_nonblocking(true)
        .map_err(|e| OpenBaoError::LoginCallback(e.to_string()))?;
    let tokio_listener = tokio::net::TcpListener::from_std(listener)
        .map_err(|e| OpenBaoError::LoginCallback(e.to_string()))?;

    let (mut stream, _) = tokio::time::timeout(Duration::from_secs(300), tokio_listener.accept())
        .await
        .map_err(|_| OpenBaoError::LoginTimeout)?
        .map_err(|e| OpenBaoError::LoginCallback(e.to_string()))?;

    let (reader, mut writer) = stream.split();
    let mut buf_reader = tokio::io::BufReader::new(reader);
    let mut request_line = String::new();
    buf_reader
        .read_line(&mut request_line)
        .await
        .map_err(|e| OpenBaoError::LoginCallback(e.to_string()))?;

    let parts: Vec<&str> = request_line.split_whitespace().collect();
    if parts.len() < 2 {
        return Err(OpenBaoError::LoginCallback("malformed request".into()));
    }
    let path = parts[1];

    let parsed = url::Url::parse(&format!("http://localhost{path}"))
        .map_err(|_| OpenBaoError::LoginCallback("malformed URL".into()))?;

    let code = parsed
        .query_pairs()
        .find(|(k, _)| k == "code")
        .map(|(_, v)| v.into_owned())
        .ok_or_else(|| OpenBaoError::LoginCallback("missing 'code' parameter".into()))?;

    let state = parsed
        .query_pairs()
        .find(|(k, _)| k == "state")
        .map(|(_, v)| v.into_owned())
        .ok_or_else(|| OpenBaoError::LoginCallback("missing 'state' parameter".into()))?;

    let response = "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\n\r\n\
                    <html><body><h1>Quill</h1><p>Login successful. \
                    You can close this tab.</p></body></html>";
    writer
        .write_all(response.as_bytes())
        .await
        .map_err(|e| OpenBaoError::LoginCallback(e.to_string()))?;

    Ok((code, state))
}
