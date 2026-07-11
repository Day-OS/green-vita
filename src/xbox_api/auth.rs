use anyhow::{Context, Result, bail};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};

const CLIENT_ID: &str = "1f907974-e22b-4810-a9de-d9647380c97e";
const OAUTH_SCOPE: &str = "xboxlive.signin openid profile offline_access";
const TOKEN_STORE_DIR: &str = "ux0:data/xcloud-rust";
const TOKEN_STORE_PATH: &str = "ux0:data/xcloud-rust/xcloud-tokens.json";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone)]
pub struct EndpointCredentials {
    pub host: String,
    pub token: String,
}

#[derive(Debug, Clone)]
pub struct StreamingCredentials {
    pub home: EndpointCredentials,
    pub cloud: EndpointCredentials,
    pub cloud_f2p: Option<EndpointCredentials>,
}

#[derive(Debug, Clone, Deserialize)]
struct DeviceCodeResponse {
    user_code: String,
    device_code: String,
    verification_uri: String,
    expires_in: u64,
    interval: u64,
    message: String,
}

#[derive(Debug, Clone)]
pub struct DeviceCodeAuth {
    pub user_code: String,
    pub verification_uri: String,
    pub message: String,
    device_code: String,
    pub poll_interval: Duration,
    deadline: Instant,
}

impl DeviceCodeAuth {
    pub fn is_expired(&self) -> bool {
        Instant::now() >= self.deadline
    }
}

#[derive(Debug, Clone, Deserialize)]
struct UserTokenResponse {
    access_token: String,
    refresh_token: String,
}

#[derive(Debug, Deserialize)]
struct DeviceCodeErrorResponse {
    error: String,
    error_description: Option<String>,
}

pub enum DeviceCodePoll {
    Pending(Duration),
    Authorized,
    Restart,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct TokenStoreData {
    refresh_token: String,
}

#[derive(Debug, Clone, Deserialize)]
struct XstsTokenResponse {
    #[serde(rename = "Token")]
    token: String,
    #[serde(rename = "DisplayClaims")]
    display_claims: XstsDisplayClaims,
}

#[derive(Debug, Clone, Deserialize)]
struct XstsDisplayClaims {
    xui: Vec<XstsUserClaim>,
}

#[derive(Debug, Clone, Deserialize)]
struct XstsUserClaim {
    uhs: String,
}

pub struct XboxProfile {
    pub gamertag: Option<String>,
    pub gamerscore: Option<String>,
    pub avatar_url: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct ProfileResponse {
    #[serde(rename = "profileUsers")]
    profile_users: Vec<ProfileUser>,
}

#[derive(Debug, Clone, Deserialize)]
struct ProfileUser {
    settings: Vec<ProfileSetting>,
}

#[derive(Debug, Clone, Deserialize)]
struct ProfileSetting {
    id: String,
    value: String,
}

#[derive(Debug, Clone, Deserialize)]
struct StreamingTokenResponse {
    #[serde(rename = "gsToken")]
    gs_token: String,
    #[serde(rename = "offeringSettings")]
    offering_settings: OfferingSettings,
}

#[derive(Debug, Clone, Deserialize)]
struct OfferingSettings {
    regions: Vec<StreamingRegion>,
}

#[derive(Debug, Clone, Deserialize)]
struct StreamingRegion {
    #[serde(rename = "baseUri")]
    base_uri: String,
    #[serde(rename = "isDefault")]
    is_default: bool,
}

impl StreamingTokenResponse {
    fn into_credentials(self) -> Result<EndpointCredentials> {
        let region = self
            .offering_settings
            .regions
            .into_iter()
            .find(|region| region.is_default)
            .context("streaming token response had no default region")?;

        Ok(EndpointCredentials {
            host: region.base_uri,
            token: self.gs_token,
        })
    }
}

#[derive(Clone)]
pub struct MsalAuth {
    client: Client,
    refresh_token: Option<String>,
}

impl MsalAuth {
    pub fn new() -> Self {
        let _ = ensure_token_store_dir();
        let refresh_token = std::fs::File::open(TOKEN_STORE_PATH)
            .ok()
            .and_then(|file| serde_json::from_reader::<_, TokenStoreData>(file).ok())
            .map(|data| data.refresh_token);

        Self {
            client: Client::builder()
                .timeout(REQUEST_TIMEOUT)
                .build()
                .unwrap_or_default(),
            refresh_token,
        }
    }

    pub fn has_saved_login(&self) -> bool {
        self.refresh_token.is_some()
    }

    pub fn logout(&mut self) {
        self.clear_saved_login();
    }

    fn save_refresh_token(&mut self, refresh_token: String) {
        self.refresh_token = Some(refresh_token.clone());

        let result = ensure_token_store_dir().and_then(|_| {
            crate::fs_utils::write_file_truncating(
                TOKEN_STORE_PATH,
                serde_json::to_string_pretty(&TokenStoreData { refresh_token })?,
            )
            .context("failed to persist xCloud login token")
        });
        if let Err(error) = result {
            eprintln!("Could not persist xCloud login: {error:#}");
        }
    }

    fn clear_saved_login(&mut self) {
        self.refresh_token = None;
        let _ = std::fs::remove_file(TOKEN_STORE_PATH);
    }

    async fn post_form(
        &self,
        url: &str,
        body: String,
        context_label: &str,
    ) -> Result<reqwest::Response> {
        self.client
            .post(url)
            .header("Content-Type", "application/x-www-form-urlencoded")
            .body(body)
            .send()
            .await
            .with_context(|| format!("{context_label} failed"))
    }

    pub async fn request_device_code(&self) -> Result<DeviceCodeAuth> {
        let response: DeviceCodeResponse = self
            .post_form(
                "https://login.microsoftonline.com/consumers/oauth2/v2.0/devicecode",
                format!("client_id={CLIENT_ID}&scope={}", urlencode(OAUTH_SCOPE)),
                "device code request",
            )
            .await?
            .error_for_status()
            .context("device code request rejected")?
            .json()
            .await
            .context("failed to decode device code response")?;

        Ok(DeviceCodeAuth {
            user_code: response.user_code,
            verification_uri: response.verification_uri,
            message: response.message,
            device_code: response.device_code,
            poll_interval: Duration::from_secs(response.interval.max(1)),
            deadline: Instant::now() + Duration::from_secs(response.expires_in),
        })
    }

    pub async fn poll_device_code(&mut self, auth: &DeviceCodeAuth) -> Result<DeviceCodePoll> {
        let response = self
            .post_form(
                "https://login.microsoftonline.com/consumers/oauth2/v2.0/token",
                format!(
                    "grant_type=urn:ietf:params:oauth:grant-type:device_code&client_id={CLIENT_ID}&device_code={}",
                    auth.device_code
                ),
                "device code poll request",
            )
            .await?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response
                .text()
                .await
                .context("failed to read device code poll response")?;
            let error: DeviceCodeErrorResponse = serde_json::from_str(&body)
                .with_context(|| format!("device code poll rejected with {status}: {body}"))?;

            return match error.error.as_str() {
                "authorization_pending" => Ok(DeviceCodePoll::Pending(auth.poll_interval)),
                "slow_down" => Ok(DeviceCodePoll::Pending(
                    auth.poll_interval + Duration::from_secs(5),
                )),
                "expired_token" | "bad_verification_code" => Ok(DeviceCodePoll::Restart),
                _ => bail!(
                    "device code poll rejected: {}",
                    error.error_description.unwrap_or(error.error)
                ),
            };
        }

        let token: UserTokenResponse = response
            .json()
            .await
            .context("failed to decode device code token response")?;
        self.save_refresh_token(token.refresh_token);
        Ok(DeviceCodePoll::Authorized)
    }

    async fn refresh_user_token(&mut self) -> Result<String> {
        let Some(refresh_token) = self.refresh_token.clone() else {
            bail!("no saved xCloud login to refresh");
        };

        let response = self
            .post_form(
                "https://login.microsoftonline.com/consumers/oauth2/v2.0/token",
                format!(
                "client_id={CLIENT_ID}&grant_type=refresh_token&refresh_token={refresh_token}&scope={}",
                urlencode(OAUTH_SCOPE)
            ),
                "token refresh request",
            )
            .await?;

        if response.status().as_u16() == 400 {
            self.clear_saved_login();
            bail!("saved xCloud login expired; please sign in again");
        }

        let token: UserTokenResponse = response
            .error_for_status()
            .context("token refresh rejected")?
            .json()
            .await
            .context("failed to decode token refresh response")?;

        self.save_refresh_token(token.refresh_token.clone());
        Ok(token.access_token)
    }

    pub async fn get_passport_token(&mut self) -> Result<String> {
        self.refresh_user_token().await?;
        let Some(refresh_token) = self.refresh_token.clone() else {
            bail!("no saved xCloud login to derive a passport token from");
        };

        let response = self
            .post_form(
                "https://login.live.com/oauth20_token.srf",
                format!(
                    "client_id={CLIENT_ID}&scope=service::http://Passport.NET/purpose::PURPOSE_XBOX_CLOUD_CONSOLE_TRANSFER_TOKEN&grant_type=refresh_token&refresh_token={refresh_token}"
                ),
                "passport token request",
            )
            .await?
            .error_for_status()
            .context("passport token request rejected")?;

        let token: UserTokenResponse = response
            .json()
            .await
            .context("failed to decode passport token response")?;

        Ok(token.access_token)
    }

    /// Shared shape of every xCloud/Xbox Live token exchange below.
    async fn post_json<T: serde::de::DeserializeOwned>(
        &self,
        url: impl reqwest::IntoUrl,
        headers: &[(&str, &str)],
        body: &serde_json::Value,
        context_label: &str,
    ) -> Result<T> {
        let mut request = self.client.post(url).json(body);
        for (key, value) in headers {
            request = request.header(*key, *value);
        }
        request
            .send()
            .await
            .with_context(|| format!("{context_label} request failed"))?
            .error_for_status()
            .with_context(|| format!("{context_label} rejected"))?
            .json()
            .await
            .with_context(|| format!("failed to decode {context_label} response"))
    }

    async fn xsts_user_authenticate(&self, access_token: &str) -> Result<String> {
        let body = serde_json::json!({
            "Properties": {
                "AuthMethod": "RPS",
                "RpsTicket": format!("d={access_token}"),
                "SiteName": "user.auth.xboxlive.com",
            },
            "RelyingParty": "http://auth.xboxlive.com",
            "TokenType": "JWT",
        });

        let response: XstsTokenResponse = self
            .post_json(
                "https://user.auth.xboxlive.com/user/authenticate",
                &[("x-xbl-contract-version", "1")],
                &body,
                "XSTS user authentication",
            )
            .await?;

        Ok(response.token)
    }

    async fn xsts_authorize(
        &self,
        user_token: &str,
        relying_party: &str,
    ) -> Result<XstsTokenResponse> {
        let body = serde_json::json!({
            "Properties": {
                "SandboxId": "RETAIL",
                "UserTokens": [user_token],
            },
            "RelyingParty": relying_party,
            "TokenType": "JWT",
        });

        self.post_json(
            "https://xsts.auth.xboxlive.com/xsts/authorize",
            &[("x-xbl-contract-version", "1")],
            &body,
            "XSTS authorize",
        )
        .await
    }

    async fn streaming_token(
        &self,
        gssv_token: &str,
        offering: &str,
    ) -> Result<EndpointCredentials> {
        let body = serde_json::json!({
            "token": gssv_token,
            "offeringId": offering,
        });

        let response: StreamingTokenResponse = self
            .post_json(
                format!("https://{offering}.gssv-play-prod.xboxlive.com/v2/login/user"),
                &[("x-gssv-client", "XboxComBrowser")],
                &body,
                &format!("streaming token (offering {offering})"),
            )
            .await?;

        response.into_credentials()
    }

    pub async fn fetch_streaming_credentials(&mut self) -> Result<StreamingCredentials> {
        let access_token = self.refresh_user_token().await?;
        let web_token = self.xsts_user_authenticate(&access_token).await?;
        let gssv_token = self
            .xsts_authorize(&web_token, "http://gssv.xboxlive.com/")
            .await?
            .token;

        let home = self.streaming_token(&gssv_token, "xhome").await?;

        let primary = self.streaming_token(&gssv_token, "xgpuweb").await;
        let f2p = self.streaming_token(&gssv_token, "xgpuwebf2p").await;

        let (cloud, cloud_f2p) = match (primary, f2p) {
            (Ok(cloud), f2p) => (cloud, f2p.ok()),
            (Err(_), Ok(f2p)) => (f2p, None),
            (Err(error), Err(_)) => return Err(error),
        };

        Ok(StreamingCredentials {
            home,
            cloud,
            cloud_f2p,
        })
    }

    /// Gamertag + avatar URL, via a separate XSTS authorization for Xbox Live's own profile API.
    pub async fn fetch_xbox_profile(&mut self) -> Result<XboxProfile> {
        let access_token = self.refresh_user_token().await?;
        let web_token = self.xsts_user_authenticate(&access_token).await?;
        let xbl = self
            .xsts_authorize(&web_token, "http://xboxlive.com")
            .await?;
        // The "user hash" half of an `XBL3.0 x=<uhs>;<token>` Authorization header.
        let uhs = xbl
            .display_claims
            .xui
            .first()
            .map(|xui| xui.uhs.as_str())
            .context("XSTS response had no user hash (uhs)")?;

        let response: ProfileResponse = self
            .client
            .get(
                "https://profile.xboxlive.com/users/me/profile/settings?settings=GameDisplayPicRaw,Gamertag,Gamerscore",
            )
            .header("x-xbl-contract-version", "3")
            .header("Authorization", format!("XBL3.0 x={uhs};{}", xbl.token))
            .send()
            .await
            .context("Xbox profile request failed")?
            .error_for_status()
            .context("Xbox profile request rejected")?
            .json()
            .await
            .context("failed to decode Xbox profile response")?;

        let settings = response
            .profile_users
            .into_iter()
            .next()
            .map(|user| user.settings)
            .unwrap_or_default();
        let setting = |id: &str| {
            settings
                .iter()
                .find(|setting| setting.id == id)
                .map(|setting| setting.value.clone())
        };

        Ok(XboxProfile {
            gamertag: setting("Gamertag"),
            gamerscore: setting("Gamerscore"),
            avatar_url: setting("GameDisplayPicRaw"),
        })
    }
}

fn ensure_token_store_dir() -> Result<()> {
    std::fs::create_dir_all(TOKEN_STORE_DIR).context("failed to create xCloud token directory")
}

fn urlencode(value: &str) -> String {
    value.replace(' ', "%20")
}
