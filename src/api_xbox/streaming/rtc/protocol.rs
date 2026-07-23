use crate::api::streaming::rtc::session::RtcSessionBackend;
use crate::api_xbox::streaming::control::channel::{self, HandshakeStage};
use crate::api_xbox::streaming::control::input::{InputQueue, PointerFrame};
use crate::streaming::input::{GamepadFrame, PointerEvent};
use bytes::BytesMut;
use rtc::data_channel::RTCDataChannelId;
use rtc::peer_connection::RTCPeerConnection;

#[derive(Clone, Copy)]
pub(in crate::api_xbox::streaming) struct ChannelIds {
    pub control: RTCDataChannelId,
    pub input: RTCDataChannelId,
    pub message: RTCDataChannelId,
}

pub(super) struct XboxRtcProtocol {
    handshake_stage: HandshakeStage,
    input_queue: InputQueue,
    input_channel_ready: bool,
    server_video_size: Option<(u32, u32)>,
    channel_ids: ChannelIds,
    video_fps: u32,
}

impl XboxRtcProtocol {
    pub(super) fn new(channel_ids: ChannelIds, video_fps: u32) -> Self {
        Self {
            handshake_stage: HandshakeStage::WaitingForChannels,
            input_queue: InputQueue::default(),
            input_channel_ready: false,
            server_video_size: None,
            channel_ids,
            video_fps,
        }
    }

    fn send_gamepad_frame_inner(
        &mut self,
        peer: &mut RTCPeerConnection,
        frame: GamepadFrame,
    ) -> bool {
        if !self.input_channel_ready {
            return false;
        }
        let Some(bytes) = self.input_queue.queue_gamepad_frames([frame], true) else {
            return false;
        };
        self.send_input_bytes(peer, &bytes)
    }

    fn send_pointer_event_inner(&mut self, peer: &mut RTCPeerConnection, event: PointerEvent) {
        if !self.input_channel_ready {
            return;
        }
        let Some(bytes) = self.input_queue.queue_pointer_frame(PointerFrame {
            events: vec![event],
        }) else {
            return;
        };
        let _ = self.send_input_bytes(peer, &bytes);
    }

    fn handle_channel_open_inner(
        &mut self,
        peer: &mut RTCPeerConnection,
        channel_id: RTCDataChannelId,
    ) {
        let channel_ids = self.channel_ids;
        if channel_id == channel_ids.message
            && self.handshake_stage == HandshakeStage::WaitingForChannels
            && let Some(mut message_channel) = peer.data_channel(channel_id)
        {
            let _ = message_channel.send_text(channel::message_handshake().to_string());
            self.handshake_stage = HandshakeStage::WaitingForHandshakeAck;
        }
        if channel_id == channel_ids.input
            && let Some(mut input_channel) = peer.data_channel(channel_id)
        {
            let client_metadata = self.input_queue.client_metadata_packet(0);
            let _ = input_channel.send(BytesMut::from(client_metadata.as_slice()));
            self.input_channel_ready = true;
        }
    }

    fn handle_channel_message_inner(
        &mut self,
        peer: &mut RTCPeerConnection,
        channel_id: RTCDataChannelId,
        data: &[u8],
    ) {
        let channel_ids = self.channel_ids;
        channel::handle_data_channel_message(
            peer,
            &channel_ids,
            &mut self.handshake_stage,
            channel_id,
            data,
            self.video_fps,
        );
        if let Some(size) = channel::parse_server_video_size(&channel_ids, channel_id, data) {
            eprintln!("xCloud reported server video size: {size:?}");
            self.server_video_size = Some(size);
        }
    }

    fn notify_keyframe_requested_inner(&mut self, peer: &mut RTCPeerConnection) {
        if let Some(mut control_channel) = peer.data_channel(self.channel_ids.control) {
            let _ = control_channel.send_text(channel::video_keyframe_requested(true).to_string());
        }
    }

    fn send_input_bytes(&self, peer: &mut RTCPeerConnection, bytes: &[u8]) -> bool {
        if let Some(mut input_channel) = peer.data_channel(self.channel_ids.input) {
            input_channel.send(BytesMut::from(bytes)).is_ok()
        } else {
            false
        }
    }
}

impl RtcSessionBackend for XboxRtcProtocol {
    fn handle_channel_open(&mut self, peer: &mut RTCPeerConnection, channel_id: RTCDataChannelId) {
        self.handle_channel_open_inner(peer, channel_id);
    }

    fn handle_channel_message(
        &mut self,
        peer: &mut RTCPeerConnection,
        channel_id: RTCDataChannelId,
        data: &[u8],
    ) {
        self.handle_channel_message_inner(peer, channel_id, data);
    }

    fn send_gamepad_frame(&mut self, peer: &mut RTCPeerConnection, frame: GamepadFrame) -> bool {
        self.send_gamepad_frame_inner(peer, frame)
    }

    fn send_pointer_event(&mut self, peer: &mut RTCPeerConnection, event: PointerEvent) {
        self.send_pointer_event_inner(peer, event);
    }

    fn notify_keyframe_requested(&mut self, peer: &mut RTCPeerConnection) {
        self.notify_keyframe_requested_inner(peer);
    }

    fn server_video_size(&self) -> Option<(u32, u32)> {
        self.server_video_size
    }
}
