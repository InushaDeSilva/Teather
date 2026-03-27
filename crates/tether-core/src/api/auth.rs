//! OAuth 2.0 PKCE authentication flow for Autodesk Platform Services (APS).

use anyhow::{Context, Result};
use oauth2::basic::BasicClient;
use oauth2::{
    AuthUrl, ClientId, CsrfToken, PkceCodeChallenge, RedirectUrl, Scope, TokenUrl,
};
use reqwest;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tracing::info;

use crate::api::models::TokenResponse;
use crate::config::secure_storage;

const APS_AUTH_URL: &str = "https://developer.api.autodesk.com/authentication/v2/authorize";
const APS_TOKEN_URL: &str = "https://developer.api.autodesk.com/authentication/v2/token";

const TOKEN_ACCESS_KEY: &str = "access_token";
const TOKEN_REFRESH_KEY: &str = "refresh_token";

/// APS authentication client using OAuth 2.0 with PKCE.
#[derive(Clone)]
pub struct ApsAuthClient {
    client_id: String,
    redirect_uri: String,
    http: reqwest::Client,
}

impl ApsAuthClient {
    pub fn new(client_id: String, redirect_uri: String) -> Self {
        Self {
            client_id,
            redirect_uri,
            http: reqwest::Client::new(),
        }
    }

    /// Build the authorization URL that should be opened in the system browser.
    /// Returns (url, csrf_token, pkce_verifier).
    pub fn build_auth_url(&self) -> (String, String, String) {
        let (pkce_challenge, pkce_verifier) = PkceCodeChallenge::new_random_sha256();

        let oauth_client = BasicClient::new(ClientId::new(self.client_id.clone()))
            .set_auth_uri(AuthUrl::new(APS_AUTH_URL.into()).unwrap())
            .set_token_uri(TokenUrl::new(APS_TOKEN_URL.into()).unwrap())
            .set_redirect_uri(RedirectUrl::new(self.redirect_uri.clone()).unwrap());

        let (auth_url, csrf_token) = oauth_client
            .authorize_url(CsrfToken::new_random)
            .add_scope(Scope::new("data:read".into()))
            .add_scope(Scope::new("data:write".into()))
            .add_scope(Scope::new("data:create".into()))
            .set_pkce_challenge(pkce_challenge)
            .url();

        (
            auth_url.to_string(),
            csrf_token.secret().clone(),
            pkce_verifier.secret().clone(),
        )
    }

    /// Exchange an authorization code for access/refresh tokens.
    pub async fn exchange_code(
        &self,
        code: &str,
        pkce_verifier: &str,
    ) -> Result<TokenResponse> {
        let params = [
            ("client_id", self.client_id.as_str()),
            ("code", code),
            ("code_verifier", pkce_verifier),
            ("grant_type", "authorization_code"),
            ("redirect_uri", self.redirect_uri.as_str()),
        ];

        let resp = self
            .http
            .post(APS_TOKEN_URL)
            .form(&params)
            .send()
            .await
            .context("Failed to exchange auth code")?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("Token exchange failed ({}): {}", status, body);
        }

        let token: TokenResponse = resp.json().await?;
        self.store_tokens(&token)?;
        info!("Successfully obtained access token");
        Ok(token)
    }

    /// Refresh the access token using a stored refresh token.
    pub async fn refresh_token(&self) -> Result<TokenResponse> {
        let refresh_token = secure_storage::get_credential(TOKEN_REFRESH_KEY)
            .context("No refresh token stored")?;

        let params = [
            ("client_id", self.client_id.as_str()),
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token.as_str()),
            ("scope", "data:read data:write data:create"),
        ];

        let resp = self
            .http
            .post(APS_TOKEN_URL)
            .form(&params)
            .send()
            .await
            .context("Failed to refresh token")?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("Token refresh failed ({}): {}", status, body);
        }

        let token: TokenResponse = resp.json().await?;
        self.store_tokens(&token)?;
        info!("Successfully refreshed access token");
        Ok(token)
    }

    /// Get the current access token from secure storage.
    pub fn get_access_token(&self) -> Result<String> {
        secure_storage::get_credential(TOKEN_ACCESS_KEY)
    }

    /// Persist tokens to the OS credential manager.
    fn store_tokens(&self, token: &TokenResponse) -> Result<()> {
        secure_storage::store_credential(TOKEN_ACCESS_KEY, &token.access_token)?;
        if let Some(ref rt) = token.refresh_token {
            secure_storage::store_credential(TOKEN_REFRESH_KEY, rt)?;
        }
        Ok(())
    }

    /// Clear stored tokens (sign out).
    pub fn clear_tokens(&self) -> Result<()> {
        let _ = secure_storage::delete_credential(TOKEN_ACCESS_KEY);
        let _ = secure_storage::delete_credential(TOKEN_REFRESH_KEY);
        info!("Cleared stored tokens");
        Ok(())
    }

    /// Spin up a local TCP server on port 8765 to listen for the OAuth callback.
    /// Returns the authorization code if the state matches.
    pub async fn listen_for_callback(&self, expected_state: &str) -> Result<String> {
        let listener = TcpListener::bind("127.0.0.1:8765").await?;
        info!("Listening for OAuth callback on http://127.0.0.1:8765/callback");

        loop {
            let (mut socket, _) = listener.accept().await?;
            let mut buf = [0; 4096];
            let n = socket.read(&mut buf).await?;
            if n == 0 {
                continue;
            }

            let request_str = String::from_utf8_lossy(&buf[..n]);
            if request_str.starts_with("GET /callback?") {
                let first_line = request_str.lines().next().unwrap_or("");
                let mut parts = first_line.split_whitespace();
                parts.next(); // GET
                let path = parts.next().unwrap_or(""); // /callback?...
                
                let full_url = format!("http://localhost{}", path);
                let Ok(url) = reqwest::Url::parse(&full_url) else {
                    continue;
                };

                let mut code = None;
                let mut state = None;
                for (k, v) in url.query_pairs() {
                    if k == "code" { code = Some(v.to_string()); }
                    else if k == "state" { state = Some(v.to_string()); }
                }

                if state.as_deref() != Some(expected_state) {
                    let response = "HTTP/1.1 400 Bad Request\r\n\r\nCSRF state mismatch. Please close this window and try again.";
                    let _ = socket.write_all(response.as_bytes()).await;
                    anyhow::bail!("OAuth CSRF state mismatch");
                }

                if let Some(c) = code {
                    let response = "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\n\r\n<html><head><title>Success</title><style>body{font-family:sans-serif;background:#0f0f14;color:#e8e8f0;display:flex;align-items:center;justify-content:center;height:100vh;}div{text-align:center;background:#1e1e2a;padding:40px;border-radius:12px;}</style></head><body><div><h2>Authentication Successful 🎉</h2><p>You can close this window and return to Tether.</p></div></body></html>";
                    let _ = socket.write_all(response.as_bytes()).await;
                    return Ok(c);
                } else {
                    let response = "HTTP/1.1 400 Bad Request\r\n\r\nNo auth code found.";
                    let _ = socket.write_all(response.as_bytes()).await;
                    anyhow::bail!("OAuth callback missing code parameter");
                }
            } else {
                let response = "HTTP/1.1 404 Not Found\r\n\r\nNot Found";
                let _ = socket.write_all(response.as_bytes()).await;
            }
        }
    }
}
