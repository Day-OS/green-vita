//! HTTP client for the xCloud/xHome streaming APIs: sessions, titles, consoles, wait times.

use crate::xbox_api::auth::EndpointCredentials;
use crate::xbox_api::stream::{StartStreamResponse, Stream};
use anyhow::{Context, Result};
use reqwest::{Client, Method};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::time::Duration;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

const DEVICE_INFO_JSON: &str = r#"{"appInfo":{"env":{"clientAppId":"www.xbox.com","clientAppType":"browser","clientAppVersion":"26.1.97","clientSdkVersion":"10.3.7","httpEnvironment":"prod","sdkInstallId":""}},"dev":{"hw":{"make":"Microsoft","model":"unknown","sdktype":"web"},"os":{"name":"android","ver":"22631.2715","platform":"desktop"},"displayInfo":{"dimensions":{"widthInPixels":1280,"heightInPixels":720},"pixelDensity":{"dpiX":1,"dpiY":1}},"browser":{"browserName":"chrome","browserVersion":"140.0.3485.54"}}}"#;

#[derive(Debug, Clone)]
pub struct ApiClientConfig {
    pub locale: String,
    pub home: EndpointCredentials,
    pub cloud: EndpointCredentials,
    pub cloud_f2p: Option<EndpointCredentials>,
}

impl Default for ApiClientConfig {
    fn default() -> Self {
        Self {
            locale: "en-US".to_owned(),
            home: EndpointCredentials {
                host: String::new(),
                token: String::new(),
            },
            cloud: EndpointCredentials {
                host: String::new(),
                token: String::new(),
            },
            cloud_f2p: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum StreamKind {
    Home,
    Cloud,
}

impl StreamKind {
    pub fn as_path(self) -> &'static str {
        match self {
            Self::Home => "home",
            Self::Cloud => "cloud",
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConsolesResponse {
    pub results: Vec<Console>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WaitTimeResponse {
    pub estimated_total_wait_time_in_seconds: u64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Console {
    pub device_name: String,
    pub server_id: String,
    pub power_state: String,
    pub console_type: String,
}

#[derive(Debug, Clone)]
pub struct ApiClient {
    client: Client,
    pub config: ApiClientConfig,
}

impl ApiClient {
    pub fn new(config: ApiClientConfig) -> Self {
        Self {
            client: Client::builder()
                .timeout(REQUEST_TIMEOUT)
                .build()
                .unwrap_or_default(),
            config,
        }
    }

    fn credentials_for(&self, kind: StreamKind) -> &EndpointCredentials {
        match kind {
            StreamKind::Home => &self.config.home,
            StreamKind::Cloud => &self.config.cloud,
        }
    }

    pub async fn get_consoles(&self) -> Result<ConsolesResponse> {
        self.get_json(StreamKind::Home, "/v6/servers/home").await
    }

    pub async fn get_titles(&self) -> Result<Value> {
        self.get_json(StreamKind::Cloud, "/v2/titles").await
    }

    pub async fn get_wait_time(
        &self,
        kind: StreamKind,
        target_id: &str,
    ) -> Result<WaitTimeResponse> {
        self.get_json(kind, &format!("/v1/waittime/{target_id}"))
            .await
    }

    pub async fn get_active_session_paths(&self, kind: StreamKind) -> Result<Vec<String>> {
        let response: Value = self
            .get_json(kind, &format!("/v5/sessions/{}/active", kind.as_path()))
            .await?;
        let mut paths = Vec::new();
        collect_session_paths(&response, &mut paths);
        Ok(paths)
    }

    pub async fn stop_session(&self, kind: StreamKind, session_path: &str) -> Result<Value> {
        let path = format!("/{}", session_path.trim_start_matches('/'));
        self.delete_json(kind, &path).await
    }

    /// Creates a streaming session. Cloud titles missing from the primary offering are retried
    /// against the free-to-play endpoint when one is available.
    pub async fn start_stream(&self, kind: StreamKind, title_or_server_id: &str) -> Result<Stream> {
        let body = json!({
            "clientSessionId": "",
            "titleId": if kind == StreamKind::Cloud { title_or_server_id } else { "" },
            "systemUpdateGroup": "",
            "settings": {
                "nanoVersion": "V3;WebrtcTransport.dll",
                "enableOptionalDataCollection": false,
                "enableTextToSpeech": false,
                "highContrast": 0,
                "locale": self.config.locale,
                "useIceConnection": false,
                "timezoneOffsetMinutes": 120,
                "sdkType": "web",
                "osName": "android",
            },
            "serverId": if kind == StreamKind::Home { title_or_server_id } else { "" },
            "fallbackRegionNames": [],
        });
        let path = format!("/v5/sessions/{}/play", kind.as_path());

        if kind == StreamKind::Home {
            return self
                .start_stream_with_credentials(self.config.home.clone(), &path, &body)
                .await;
        }

        match self
            .start_stream_with_credentials(self.config.cloud.clone(), &path, &body)
            .await
        {
            Ok(stream) => Ok(stream),
            Err(error) if error.to_string().contains("OfferingDoesNotContainTitle") => {
                let Some(fallback) = self.config.cloud_f2p.clone() else {
                    return Err(error);
                };
                self.start_stream_with_credentials(fallback, &path, &body)
                    .await
            }
            Err(error) => Err(error),
        }
    }

    async fn start_stream_with_credentials(
        &self,
        credentials: EndpointCredentials,
        path: &str,
        body: &Value,
    ) -> Result<Stream> {
        let response: StartStreamResponse = self
            .request_json(&credentials, Method::POST, path, Some(body))
            .await?;
        Ok(Stream::new(self.clone(), credentials, response))
    }

    pub async fn get_json<T>(&self, kind: StreamKind, path: &str) -> Result<T>
    where
        T: for<'de> Deserialize<'de>,
    {
        self.request_json(self.credentials_for(kind), Method::GET, path, None)
            .await
    }

    pub async fn delete_json<T>(&self, kind: StreamKind, path: &str) -> Result<T>
    where
        T: for<'de> Deserialize<'de>,
    {
        self.request_json(self.credentials_for(kind), Method::DELETE, path, None)
            .await
    }

    /// Shared HTTP+JSON path for both `kind`-routed requests above and `Stream`'s per-session
    /// credential requests in `xbox_api::stream`.
    pub(super) async fn request_json<T>(
        &self,
        credentials: &EndpointCredentials,
        method: Method,
        path: &str,
        body: Option<&Value>,
    ) -> Result<T>
    where
        T: for<'de> Deserialize<'de>,
    {
        let url = format!("{}{}", credentials.host, path);
        let mut request = self
            .client
            .request(method, url)
            .header("Accept", "application/json")
            .header("Content-Type", "application/json")
            .header("X-Gssv-Client", "XboxComBrowser")
            .header("X-MS-Device-Info", DEVICE_INFO_JSON);

        if !credentials.token.is_empty() {
            request = request.bearer_auth(&credentials.token);
        }
        if let Some(body) = body {
            request = request.json(body);
        }

        let response = request.send().await.context("xCloud HTTP request failed")?;
        let status = response.status();
        let text = response
            .text()
            .await
            .context("failed to read xCloud response body")?;
        if !status.is_success() {
            anyhow::bail!("xCloud request failed with {status}: {text}");
        }
        if text.trim().is_empty() {
            let retry = json!({ "status": status.as_u16() });
            return serde_json::from_value(retry)
                .context("failed to decode empty-body status marker");
        }
        serde_json::from_str::<T>(&text)
            .with_context(|| format!("failed to decode xCloud JSON response: {text}"))
    }
}

fn collect_session_paths(value: &Value, output: &mut Vec<String>) {
    match value {
        Value::Array(items) => {
            for item in items {
                collect_session_paths(item, output);
            }
        }
        Value::Object(map) => {
            for (key, val) in map {
                if (key == "sessionPath" || key == "path")
                    && let Some(path) = val.as_str()
                {
                    output.push(path.to_owned());
                }
                collect_session_paths(val, output);
            }
        }
        _ => {}
    }
}
