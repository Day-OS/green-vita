use anyhow::{Context, Result, bail};
use reqwest::Client;
use serde::Deserialize;
use std::time::{Duration, Instant};

const CLIENT_ID: &str = "q61ddeJrVt7O90Nl-P-N7I36yctih4Ml6FyXLrb6j-U";
const DEFAULT_IDP_ID: &str = "PDiAhv2kJTFeQ7WOPqiQ2tRZ7lGhR2X11dXvM4TZSxg";
const USER_AGENT: &str = "Mozilla/5.0 (X11; Linux x86_64; Steam Deck) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/128.0.0.0 Safari/537.36";
const DEVICE_AUTHORIZE_URL: &str = "https://login.nvidia.com/device/authorize";
const TOKEN_URL: &str = "https://login.nvidia.com/token";

#[derive(Debug, Clone)]
pub struct DeviceCodeAuth {
    pub user_code: String,
    pub verification_uri: String,
    pub verification_uri_complete: String,
    device_code: String,
    pub poll_interval: Duration,
    deadline: Instant,
}

impl DeviceCodeAuth {
    pub fn is_expired(&self) -> bool {
        Instant::now() >= self.deadline
    }
}

#[derive(Debug, Deserialize)]
struct DeviceAuthorizationResponse {
    device_code: String,
    user_code: String,
    verification_uri: String,
    verification_uri_complete: String,
    expires_in: Option<u64>,
    interval: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: Option<String>,
    id_token: Option<String>,
    expires_in: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct TokenErrorResponse {
    error: String,
    error_description: Option<String>,
}

pub enum DeviceCodePoll {
    Pending(Duration),
    Authorized,
    Restart,
}

#[derive(Clone)]
pub struct NvidiaAuth {
    client: Client,
    access_token: Option<String>,
    refresh_token: Option<String>,
    id_token: Option<String>,
    expires_at: Option<Instant>,
}

impl NvidiaAuth {
    pub fn new() -> Self {
        Self {
            client: Client::builder()
                .timeout(Duration::from_secs(15))
                .build()
                .unwrap_or_default(),
            access_token: None,
            refresh_token: None,
            id_token: None,
            expires_at: None,
        }
    }

    pub fn logout(&mut self) {
        self.access_token = None;
        self.refresh_token = None;
        self.id_token = None;
        self.expires_at = None;
    }

    pub fn token(&self) -> Result<&str> {
        self.id_token
            .as_deref()
            .or(self.access_token.as_deref())
            .context("GeForce NOW is not signed in")
    }

    pub async fn request_device_code(&self) -> Result<DeviceCodeAuth> {
        let device_id = uuid::Uuid::new_v4().to_string();
        let body = format!(
            "client_id={CLIENT_ID}&scope=openid%20consent%20email%20tk_client%20age&device_id={device_id}&display_name=GreenVita&idp_id={DEFAULT_IDP_ID}"
        );
        let response = self
            .client
            .post(DEVICE_AUTHORIZE_URL)
            .headers(device_headers(&device_id)?)
            .body(body)
            .send()
            .await
            .context("GeForce NOW device authorization request failed")?;
        if !response.status().is_success() {
            bail!(
                "GeForce NOW device authorization failed ({}): {}",
                response.status(),
                response.text().await.unwrap_or_default()
            );
        }
        let response: DeviceAuthorizationResponse = response.json().await?;
        let expires_in = response.expires_in.unwrap_or(600);
        Ok(DeviceCodeAuth {
            user_code: response.user_code,
            verification_uri: response.verification_uri,
            verification_uri_complete: response.verification_uri_complete,
            device_code: response.device_code,
            poll_interval: Duration::from_secs(response.interval.unwrap_or(5).max(1)),
            deadline: Instant::now() + Duration::from_secs(expires_in),
        })
    }

    pub async fn poll_device_code(&mut self, auth: &DeviceCodeAuth) -> Result<DeviceCodePoll> {
        let body = format!(
            "grant_type=urn:ietf:params:oauth:grant-type:device_code&device_code={}&client_id={CLIENT_ID}",
            auth.device_code
        );
        let response = self
            .client
            .post(TOKEN_URL)
            .header(
                "Content-Type",
                "application/x-www-form-urlencoded; charset=UTF-8",
            )
            .header("Origin", "https://play.geforcenow.com")
            .header("Referer", "https://play.geforcenow.com/")
            .header("User-Agent", USER_AGENT)
            .body(body)
            .send()
            .await
            .context("GeForce NOW sign-in poll failed")?;

        if !response.status().is_success() {
            let error: TokenErrorResponse = response.json().await?;
            return match error.error.as_str() {
                "authorization_pending" => Ok(DeviceCodePoll::Pending(auth.poll_interval)),
                "slow_down" => Ok(DeviceCodePoll::Pending(
                    auth.poll_interval + Duration::from_secs(5),
                )),
                "expired_token" | "access_denied" => Ok(DeviceCodePoll::Restart),
                _ => bail!(
                    "GeForce NOW sign-in failed: {}",
                    error.error_description.unwrap_or(error.error)
                ),
            };
        }

        let tokens: TokenResponse = response.json().await?;
        self.access_token = Some(tokens.access_token);
        self.refresh_token = tokens.refresh_token;
        self.id_token = tokens.id_token;
        self.expires_at =
            Some(Instant::now() + Duration::from_secs(tokens.expires_in.unwrap_or(86400)));
        Ok(DeviceCodePoll::Authorized)
    }
}

fn device_headers(device_id: &str) -> Result<reqwest::header::HeaderMap> {
    use reqwest::header::{HeaderMap, HeaderValue};
    let mut headers = HeaderMap::new();
    for (name, value) in [
        ("accept", "application/json, text/plain, */*"),
        (
            "content-type",
            "application/x-www-form-urlencoded; charset=UTF-8",
        ),
        ("origin", "https://play.geforcenow.com"),
        ("referer", "https://play.geforcenow.com/"),
        ("user-agent", USER_AGENT),
        ("x-device-id", device_id),
        ("nv-client-id", CLIENT_ID),
        ("nv-client-streamer", "WEBRTC"),
        ("nv-client-type", "BROWSER"),
        ("nv-client-platform-name", "browser"),
        ("nv-browser-type", "CHROME"),
        ("nv-device-os", "STEAMOS"),
        ("nv-device-type", "CONSOLE"),
        ("nv-device-model", "STEAMDECK"),
        ("nv-device-make", "VALVE"),
    ] {
        headers.insert(
            reqwest::header::HeaderName::from_bytes(name.as_bytes())?,
            HeaderValue::from_str(value)?,
        );
    }
    Ok(headers)
}

pub(crate) const GFN_USER_AGENT: &str = USER_AGENT;
pub(crate) const GFN_CLIENT_VERSION: &str = "2.0.80.173";
pub(crate) const GFN_LCARS_CLIENT_ID: &str = "ec7e38d4-03af-4b58-b131-cfb0495903ab";
