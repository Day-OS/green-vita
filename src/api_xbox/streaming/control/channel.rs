use rtc::data_channel::RTCDataChannelId;
use rtc::peer_connection::RTCPeerConnection;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::api::streaming::rtc::peer::RtcDataChannelConfig;
use crate::api_xbox::streaming::rtc::protocol::ChannelIds;
use crate::streaming::video::{STREAM_HEIGHT, STREAM_WIDTH};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum HandshakeStage {
    WaitingForChannels,
    WaitingForHandshakeAck,
    Ready,
}

pub(in crate::api_xbox::streaming) fn handle_data_channel_message(
    pc: &mut RTCPeerConnection,
    ids: &ChannelIds,
    handshake_stage: &mut HandshakeStage,
    channel_id: RTCDataChannelId,
    data: &[u8],
    video_fps: u32,
) {
    if channel_id != ids.message || *handshake_stage != HandshakeStage::WaitingForHandshakeAck {
        return;
    }
    let Ok(payload) = serde_json::from_slice::<Value>(data) else {
        return;
    };
    if payload.get("type").and_then(Value::as_str) != Some("HandshakeAck") {
        return;
    }

    if let Some(mut control_channel) = pc.data_channel(ids.control) {
        let _ = control_channel.send_text(authorization_request().to_string());
        let _ = control_channel.send_text(gamepad_changed(0, true).to_string());
    }
    if let Some(mut message_channel) = pc.data_channel(ids.message) {
        for message in startup_messages(video_fps) {
            let _ = message_channel.send_text(message.to_string());
        }
    }
    *handshake_stage = HandshakeStage::Ready;
}

pub(in crate::api_xbox::streaming) fn parse_server_video_size(
    ids: &ChannelIds,
    channel_id: RTCDataChannelId,
    data: &[u8],
) -> Option<(u32, u32)> {
    use crate::api_xbox::streaming::control::input::ReportType;

    if channel_id != ids.input || data.len() < 10 {
        return None;
    }
    let report_type = u16::from_le_bytes([data[0], data[1]]);
    if report_type != ReportType::ServerMetadata as u16 {
        return None;
    }
    let height = u32::from_le_bytes(data[2..6].try_into().ok()?);
    let width = u32::from_le_bytes(data[6..10].try_into().ok()?);
    Some((width, height))
}

pub const CHAT_CHANNEL: RtcDataChannelConfig = RtcDataChannelConfig {
    label: "chat",
    protocol: "chatV1",
    ordered: true,
    max_packet_life_time: None,
    max_retransmits: None,
};
pub const CONTROL_CHANNEL: RtcDataChannelConfig = RtcDataChannelConfig {
    label: "control",
    protocol: "controlV1",
    ordered: true,
    max_packet_life_time: None,
    max_retransmits: None,
};
pub const INPUT_CHANNEL: RtcDataChannelConfig = RtcDataChannelConfig {
    label: "input",
    protocol: "1.0",
    ordered: false,
    max_packet_life_time: None,
    max_retransmits: Some(0),
};
pub const MESSAGE_CHANNEL: RtcDataChannelConfig = RtcDataChannelConfig {
    label: "message",
    protocol: "messageV1",
    ordered: true,
    max_packet_life_time: None,
    max_retransmits: None,
};

pub fn authorization_request() -> Value {
    json!({
        "message": "authorizationRequest",
        "accessKey": "4BDB3609-C1F1-4195-9B37-FEFF45DA8B8E",
    })
}

pub fn gamepad_changed(gamepad_index: u8, was_added: bool) -> Value {
    json!({
        "message": "gamepadChanged",
        "gamepadIndex": gamepad_index,
        "wasAdded": was_added,
    })
}

pub fn video_keyframe_requested(ifr_requested: bool) -> Value {
    json!({
        "message": "videoKeyframeRequested",
        "ifrRequested": ifr_requested,
    })
}

pub fn message_handshake() -> Value {
    json!({
        "type": "Handshake",
        "version": "messageV1",
        "id": "be0bfc6d-1e83-4c8a-90ed-fa8601c5a179",
        "cv": "0",
    })
}

pub fn generate_message(path: &str, data: Value) -> Value {
    json!({
        "type": "Message",
        "content": data.to_string(),
        "id": Uuid::new_v4(),
        "target": path,
        "cv": "",
    })
}

pub fn startup_messages(video_fps: u32) -> Vec<Value> {
    let width = STREAM_WIDTH;
    let height = STREAM_HEIGHT;

    vec![
        generate_message(
            "/streaming/systemUi/configuration",
            json!({
                "version": [0, 2, 0],
                "systemUis": [],
            }),
        ),
        generate_message(
            "/streaming/properties/clientappinstallidchanged",
            json!({ "clientAppInstallId": "c97d7ee0-73b2-4239-bf1d-9d805a338429" }),
        ),
        generate_message(
            "/streaming/characteristics/orientationchanged",
            json!({ "orientation": 0 }),
        ),
        generate_message(
            "/streaming/characteristics/touchinputenabledchanged",
            json!({ "touchInputEnabled": false }),
        ),
        generate_message(
            "/streaming/characteristics/clientdevicecapabilities",
            json!({
                "supportsCustomResolution": true,
                "supportsHevc": false,
                "supportsHdr": false,
                "supportsFps": video_fps,
                "maxWidth": width,
                "maxHeight": height,
                "maxBitrateKbps": 2000,
                "video": {
                    "width": width,
                    "height": height,
                    "maxWidth": width,
                    "maxHeight": height,
                    "maxBitrateKbps": 2000,
                },
            }),
        ),
        generate_message(
            "/streaming/characteristics/dimensionschanged",
            json!({
                "horizontal": width,
                "vertical": height,
                "preferredWidth": width,
                "preferredHeight": height,
                "safeAreaLeft": 0,
                "safeAreaTop": 0,
                "safeAreaRight": width,
                "safeAreaBottom": height,
                "supportsCustomResolution": true,
            }),
        ),
    ]
}
