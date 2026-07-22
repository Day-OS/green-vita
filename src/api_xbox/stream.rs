use crate::api_xbox::api::ApiClient;
use crate::api_xbox::auth::{EndpointCredentials, MsalAuth};
use anyhow::{Context, Result};
use reqwest::Method;
use rtc::peer_connection::transport::RTCIceCandidateInit;
use serde::Deserialize;
use serde_json::{Value, json};
use std::net::{Ipv4Addr, Ipv6Addr};
use tokio::time::{Duration, sleep};

/// UDP port a console listens on for streaming sessions.
const CONSOLE_STREAMING_PORT: u16 = 9002;
/// First hextet of the `2001:0::/32` Teredo prefix.
const TEREDO_PREFIX: u16 = 0x2001;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StartStreamResponse {
    pub session_path: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamState {
    New,
    Provisioning,
    WaitingForResources,
    ReadyToConnect,
    Provisioned,
    Error,
}

impl StreamState {
    fn from_api(value: &str) -> Self {
        match value {
            "Provisioning" => Self::Provisioning,
            "WaitingForResources" => Self::WaitingForResources,
            "ReadyToConnect" => Self::ReadyToConnect,
            "Provisioned" => Self::Provisioned,
            "Failed" | "Error" => Self::Error,
            _ => Self::New,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Stream {
    api_client: ApiClient,
    credentials: EndpointCredentials,
    pub session_id: String,
    session_path: String,
    pub state: StreamState,
}

#[derive(Deserialize)]
struct StateResponse {
    state: String,
}

impl Stream {
    pub fn new(
        api_client: ApiClient,
        credentials: EndpointCredentials,
        response: StartStreamResponse,
    ) -> Self {
        let session_id = response
            .session_path
            .rsplit('/')
            .next()
            .unwrap_or(&response.session_path)
            .to_owned();

        Self {
            api_client,
            credentials,
            session_id,
            session_path: response.session_path,
            state: StreamState::New,
        }
    }

    pub fn session_path(&self) -> String {
        format!("/{}", self.session_path.trim_start_matches('/'))
    }

    fn session_endpoint(&self, suffix: &str) -> String {
        format!("{}/{}", self.session_path(), suffix)
    }

    pub async fn refresh_state(&mut self) -> Result<StreamState> {
        let response: StateResponse = self
            .api_client
            .request_json(
                &self.credentials,
                Method::GET,
                &self.session_endpoint("state"),
                None,
            )
            .await?;
        self.state = StreamState::from_api(&response.state);
        Ok(self.state)
    }

    pub async fn poll_provisioning(&mut self, auth: &mut MsalAuth) -> Result<StreamState> {
        let state = self.refresh_state().await?;
        match state {
            StreamState::ReadyToConnect => {
                let passport_token = auth.get_passport_token().await?;
                self.send_msal_auth(&passport_token).await?;
            }
            StreamState::Error => anyhow::bail!("stream session entered an error state"),
            StreamState::New
            | StreamState::Provisioning
            | StreamState::WaitingForResources
            | StreamState::Provisioned => {}
        }
        Ok(state)
    }

    pub async fn send_sdp_offer(&self, sdp: &str) -> Result<String> {
        let body = json!({
            "messageType": "offer",
            "sdp": sdp,
            "requestId": "1",
            "configuration": {
                "chatConfiguration": {
                    "bytesPerSample": 2,
                    "expectedClipDurationMs": 20,
                    "format": {
                        "codec": "opus",
                        "container": "webm",
                    },
                    "numChannels": 1,
                    "sampleFrequencyHz": 24000,
                },
                "chat": { "minVersion": 1, "maxVersion": 1 },
                "control": { "minVersion": 1, "maxVersion": 3 },
                "input": { "minVersion": 1, "maxVersion": 9 },
                "message": { "minVersion": 1, "maxVersion": 1 },
                "reliableinput": { "minVersion": 9, "maxVersion": 9 },
                "unreliableinput": { "minVersion": 9, "maxVersion": 9 },
            },
        });
        let _: Value = self
            .api_client
            .request_json(
                &self.credentials,
                Method::POST,
                &self.session_endpoint("sdp"),
                Some(&body),
            )
            .await?;
        let response = self.wait_for_sdp_response().await?;
        check_exchange_error(&response)?;
        extract_answer_sdp(&response)
    }

    pub async fn wait_for_sdp_response(&self) -> Result<Value> {
        loop {
            let value: Value = self
                .api_client
                .request_json(
                    &self.credentials,
                    Method::GET,
                    &self.session_endpoint("sdp"),
                    None,
                )
                .await?;
            if value.get("status").and_then(Value::as_u64) != Some(204) {
                return Ok(value);
            }
            sleep(Duration::from_millis(500)).await;
        }
    }

    pub async fn post_ice_candidates(&self, candidates: Vec<RTCIceCandidateInit>) -> Result<()> {
        let body = json!({
            "candidates": candidates
                .iter()
                .map(serialize_local_candidate)
                .collect::<Vec<_>>()
        });
        let response: Value = self
            .api_client
            .request_json(
                &self.credentials,
                Method::POST,
                &self.session_endpoint("ice"),
                Some(&body),
            )
            .await?;
        check_exchange_error(&response)?;
        Ok(())
    }

    pub async fn poll_ice_candidates(&self) -> Result<Option<Vec<RTCIceCandidateInit>>> {
        let value: Value = self
            .api_client
            .request_json(
                &self.credentials,
                Method::GET,
                &self.session_endpoint("ice"),
                None,
            )
            .await?;
        if value.get("status").and_then(Value::as_u64) == Some(204) {
            return Ok(None);
        }
        check_exchange_error(&value)?;
        Ok(Some(extract_remote_candidates(&value)))
    }

    pub async fn send_keepalive(&self) -> Result<Value> {
        self.api_client
            .request_json(
                &self.credentials,
                Method::POST,
                &self.session_endpoint("keepalive"),
                None,
            )
            .await
    }

    pub async fn send_msal_auth(&self, user_token: &str) -> Result<Value> {
        self.api_client
            .request_json(
                &self.credentials,
                Method::POST,
                &self.session_endpoint("connect"),
                Some(&json!({ "userToken": user_token })),
            )
            .await
    }

    pub async fn stop(&self) -> Result<Value> {
        self.api_client
            .request_json(
                &self.credentials,
                Method::DELETE,
                &self.session_path(),
                None,
            )
            .await
    }
}

/// The streaming service always answers with an `errorDetails` envelope. xHome fills it with
/// `{"code":null,"message":null}` even on a successful exchange, so only a populated field marks
/// a real failure.
fn check_exchange_error(value: &Value) -> Result<()> {
    let Some(error_details) = value.get("errorDetails").filter(|v| !v.is_null()) else {
        return Ok(());
    };
    if error_details
        .as_object()
        .is_some_and(|fields| fields.values().all(Value::is_null))
    {
        return Ok(());
    }
    anyhow::bail!("xCloud exchange failed: {error_details}");
}

fn extract_answer_sdp(response: &Value) -> Result<String> {
    response
        .get("exchangeResponse")
        .and_then(Value::as_str)
        .and_then(|exchange| serde_json::from_str::<Value>(exchange).ok())
        .and_then(|parsed| parsed.get("sdp").and_then(Value::as_str).map(str::to_owned))
        .with_context(|| {
            format!("xCloud SDP answer response was missing an SDP payload: {response}")
        })
}

fn serialize_local_candidate(candidate: &RTCIceCandidateInit) -> String {
    let mut value = json!({
        "candidate": candidate.candidate,
        "sdpMid": candidate
            .sdp_mid
            .as_deref()
            .filter(|mid| !mid.is_empty())
            .unwrap_or("0"),
        "sdpMLineIndex": candidate.sdp_mline_index.unwrap_or(0),
    });

    if let Some(username_fragment) = &candidate.username_fragment {
        value["usernameFragment"] = Value::String(username_fragment.clone());
    }

    serde_json::to_string(&value).expect("ICE candidate JSON serialization should not fail")
}

fn extract_remote_candidates(response: &Value) -> Vec<RTCIceCandidateInit> {
    let Some(exchange) = response.get("exchangeResponse").and_then(Value::as_str) else {
        return Vec::new();
    };
    let Ok(parsed) = serde_json::from_str::<Value>(exchange) else {
        return Vec::new();
    };
    let candidates = parsed
        .as_array()
        .or_else(|| parsed.get("candidates").and_then(Value::as_array));
    let Some(candidates) = candidates else {
        return Vec::new();
    };

    let mut decoded = Vec::new();
    for candidate in candidates {
        let Some(candidate) = parse_remote_candidate(candidate) else {
            continue;
        };
        // A console publishes itself over Teredo, so its reachable IPv4 endpoint only exists
        // tunnelled inside the IPv6 candidate. Derive it before the address-family filter below
        // drops that candidate, otherwise a home session is left with nothing to connect to.
        decoded.extend(teredo_host_candidates(&candidate));
        if let Some(candidate) = normalize_decoded_candidate(candidate) {
            decoded.push(candidate);
        }
    }
    decoded
}

fn parse_remote_candidate(candidate: &Value) -> Option<RTCIceCandidateInit> {
    match candidate {
        Value::String(value) => serde_json::from_str(value).ok(),
        value => serde_json::from_value(value.clone()).ok(),
    }
}

fn normalize_decoded_candidate(mut candidate: RTCIceCandidateInit) -> Option<RTCIceCandidateInit> {
    candidate.candidate = normalize_remote_candidate(&candidate.candidate)?;
    if candidate.sdp_mid.as_deref() == Some("") {
        candidate.sdp_mid = Some("0".to_owned());
    }
    if candidate.sdp_mline_index.is_none() {
        candidate.sdp_mline_index = Some(0);
    }
    Some(candidate)
}

/// Rebuilds the host candidates a Teredo address tunnels, matching how Greenlight reaches a
/// console. Teredo (RFC 4380) stores the client's IPv4 address in the last 32 bits and its UDP
/// port in bits 80..96, both one's-complemented, under the `2001:0::/32` prefix.
fn teredo_host_candidates(candidate: &RTCIceCandidateInit) -> Vec<RTCIceCandidateInit> {
    let Some((client, port)) = candidate_address(&candidate.candidate).and_then(teredo_endpoint)
    else {
        return Vec::new();
    };

    let derive = |foundation: u8, port: u16| RTCIceCandidateInit {
        candidate: format!("candidate:{foundation} 1 UDP 1 {client} {port} typ host"),
        sdp_mid: Some("0".to_owned()),
        sdp_mline_index: Some(0),
        username_fragment: candidate.username_fragment.clone(),
        url: None,
    };

    let mut derived = vec![derive(10, CONSOLE_STREAMING_PORT)];
    if port != CONSOLE_STREAMING_PORT {
        derived.push(derive(11, port));
    }
    derived
}

fn candidate_address(candidate: &str) -> Option<&str> {
    let candidate = candidate.trim();
    let candidate = candidate.strip_prefix("a=").unwrap_or(candidate);
    if !candidate.starts_with("candidate:") {
        return None;
    }
    candidate.split_whitespace().nth(4)
}

fn teredo_endpoint(address: &str) -> Option<(Ipv4Addr, u16)> {
    let segments = address.parse::<Ipv6Addr>().ok()?.segments();
    if segments[0] != TEREDO_PREFIX || segments[1] != 0 {
        return None;
    }

    let port = segments[5] ^ u16::MAX;
    let client = (u32::from(segments[6]) << 16) | u32::from(segments[7]);
    Some((Ipv4Addr::from(client ^ u32::MAX), port))
}

fn normalize_remote_candidate(candidate: &str) -> Option<String> {
    let candidate = candidate.trim();
    if candidate.is_empty()
        || candidate.eq_ignore_ascii_case("a=end-of-candidates")
        || candidate.eq_ignore_ascii_case("end-of-candidates")
    {
        return None;
    }

    let candidate = candidate.strip_prefix("a=").unwrap_or(candidate);
    if !candidate.starts_with("candidate:") {
        return None;
    }
    if candidate
        .split_whitespace()
        .nth(4)
        .is_some_and(|address| address.contains(':'))
    {
        return None;
    }

    Some(candidate.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn serializes_local_ice_as_api_xbox_candidate_string() {
        let candidate = RTCIceCandidateInit {
            candidate: "candidate:1 1 udp 1 192.0.2.10 5000 typ host".to_owned(),
            sdp_mid: Some(String::new()),
            sdp_mline_index: Some(0),
            username_fragment: None,
            url: None,
        };

        let serialized = serialize_local_candidate(&candidate);
        let payload: Value =
            serde_json::from_str(&serialized).expect("serialized ICE candidate is valid JSON");

        assert_eq!(
            payload,
            json!({
                "candidate": "candidate:1 1 udp 1 192.0.2.10 5000 typ host",
                "sdpMid": "0",
                "sdpMLineIndex": 0,
            })
        );
    }

    #[test]
    fn extracts_remote_ice_from_array_or_wrapper_payload() {
        let remote = json!([
            {
                "candidate": "a=candidate:2 1 UDP 1 203.0.113.10 9002 typ host ",
                "sdpMid": "0",
                "sdpMLineIndex": 0
            },
            {
                "candidate": "a=end-of-candidates",
                "sdpMid": "0",
                "sdpMLineIndex": 0
            },
            {
                "candidate": "a=candidate:3 1 UDP 1 2603:1050:4:D3::ADB:C44F 9002 typ host ",
                "sdpMid": "0",
                "sdpMLineIndex": 0
            }
        ]);
        let array_response = json!({ "exchangeResponse": remote.to_string() });
        let wrapper_response =
            json!({ "exchangeResponse": json!({ "candidates": remote }).to_string() });

        let candidates = extract_remote_candidates(&array_response);
        assert_eq!(candidates.len(), 1);
        assert_eq!(
            candidates[0].candidate,
            "candidate:2 1 UDP 1 203.0.113.10 9002 typ host"
        );
        assert_eq!(extract_remote_candidates(&wrapper_response).len(), 1);
    }

    #[test]
    fn accepts_the_empty_error_envelope_a_home_exchange_returns() {
        let empty_envelope = json!({
            "exchangeResponse": "{}",
            "errorDetails": { "code": null, "message": null },
        });
        assert!(check_exchange_error(&empty_envelope).is_ok());
        assert!(check_exchange_error(&json!({ "errorDetails": null })).is_ok());
        assert!(check_exchange_error(&json!({})).is_ok());

        let reported_failure = json!({
            "errorDetails": { "code": 404, "message": "SessionNotFound" },
        });
        assert!(check_exchange_error(&reported_failure).is_err());
    }

    #[test]
    fn derives_the_tunnelled_ipv4_endpoint_of_a_teredo_console_candidate() {
        // 2001:0:4136:e378:8000:63bf:3fff:fdd2 tunnels 192.0.2.45 on UDP port 40000.
        let response = json!({
            "exchangeResponse": json!([{
                "candidate": "a=candidate:1 1 UDP 1 2001:0:4136:e378:8000:63bf:3fff:fdd2 9002 typ host ",
                "sdpMid": "0",
                "sdpMLineIndex": 0
            }])
            .to_string()
        });

        let candidates = extract_remote_candidates(&response);

        // The IPv6 candidate itself stays filtered out: the Vita has no IPv6 route.
        let addresses: Vec<&str> = candidates
            .iter()
            .map(|candidate| candidate.candidate.as_str())
            .collect();
        assert_eq!(
            addresses,
            vec![
                "candidate:10 1 UDP 1 192.0.2.45 9002 typ host",
                "candidate:11 1 UDP 1 192.0.2.45 40000 typ host",
            ]
        );
    }

    #[test]
    fn leaves_non_teredo_ipv6_candidates_without_a_derived_endpoint() {
        let candidate = RTCIceCandidateInit {
            candidate: "candidate:3 1 UDP 1 2603:1050:4:D3::ADB:C44F 9002 typ host".to_owned(),
            sdp_mid: Some("0".to_owned()),
            sdp_mline_index: Some(0),
            username_fragment: None,
            url: None,
        };

        assert!(teredo_host_candidates(&candidate).is_empty());
    }
}
