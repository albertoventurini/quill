use std::time::Duration;

use secrecy::{ExposeSecret, SecretString};
use serde::Deserialize;
use sqlx::SqlitePool;
use thiserror::Error;

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
}

#[derive(Debug, Deserialize)]
struct PgCredsData {
    username: String,
    password: String,
    #[serde(default)]
    lease_duration: u64,
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

pub async fn remove_setting(pool: &SqlitePool, key: &str) -> Result<(), OpenBaoError> {
    sqlx::query("DELETE FROM settings WHERE key = ?")
        .bind(key)
        .execute(pool)
        .await?;
    Ok(())
}

impl OpenBaoClient {
    pub async fn from_store(pool: &SqlitePool) -> Result<Option<Self>, OpenBaoError> {
        let addr = match get_setting(pool, "openbao_addr").await {
            Some(a) => a,
            None => return Ok(None),
        };
        let token = match get_setting(pool, "openbao_token").await {
            Some(t) => t,
            None => return Ok(None),
        };
        Ok(Some(Self {
            addr,
            token: SecretString::from(token),
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

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(OpenBaoError::Request(format!(
                "OpenBao returned {}: {}",
                status,
                body,
            )));
        }

        let creds: PgCredsResponse = resp
            .json()
            .await
            .map_err(|e| OpenBaoError::BadResponse(format!("failed to parse credentials: {e}")))?;

        Ok(PgCredentials {
            username: creds.data.username,
            password: SecretString::from(creds.data.password),
            lease_duration_secs: if creds.data.lease_duration > 0 {
                creds.data.lease_duration
            } else {
                3600
            },
        })
    }
}

pub async fn start_oidc_login(
    bao_addr: &str,
    app_handle: &tauri::AppHandle,
) -> Result<String, OpenBaoError> {
    use tauri_plugin_opener::OpenerExt;

    let client = reqwest::Client::new();

    let listener = std::net::TcpListener::bind("127.0.0.1:0")
        .map_err(|e| OpenBaoError::LoginCallback(format!("bind failed: {e}")))?;
    let port = listener.local_addr().unwrap().port();
    let redirect_uri = format!("http://localhost:{port}/callback");

    let auth_url_resp: OidcAuthUrlResponse = client
        .post(format!(
            "{}/v1/auth/oidc/auth_url",
            bao_addr.trim_end_matches('/')
        ))
        .json(&serde_json::json!({
            "role": "default",
            "redirect_uri": &redirect_uri,
        }))
        .send()
        .await?
        .json()
        .await
        .map_err(|e| OpenBaoError::BadResponse(format!("auth_url parse: {e}")))?;

    app_handle
        .opener()
        .open_url(&auth_url_resp.data.auth_url, None::<&str>)
        .map_err(|e| OpenBaoError::LoginCallback(format!("browser open failed: {e}")))?;

    let (code, state) = accept_callback(listener).await?;

    let cb_resp: OidcCallbackResponse = client
        .post(format!(
            "{}/v1/auth/oidc/callback",
            bao_addr.trim_end_matches('/')
        ))
        .json(&serde_json::json!({
            "state": state,
            "code": code,
        }))
        .send()
        .await?
        .json()
        .await
        .map_err(|e| OpenBaoError::BadResponse(format!("callback parse: {e}")))?;

    Ok(cb_resp.auth.client_token)
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
