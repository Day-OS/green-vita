use crate::streaming::control::channel::{self, HandshakeStage};
use crate::streaming::control::input::{GamepadFrame, InputQueue, PointerEvent, PointerFrame};
use crate::streaming::rtc::AUDIO_SAMPLE_RATE;
use crate::streaming::rtc::ice;
use crate::streaming::rtc::media::{AudioReceiver, VideoReceiver};
use crate::streaming::rtc::peer::RtcPeer;
use crate::streaming::video::{DecodedFrame, DecoderConfig};
use anyhow::{Context, Result};
use bytes::{Bytes, BytesMut};
use rtc::peer_connection::event::{RTCDataChannelEvent, RTCPeerConnectionEvent, RTCTrackEvent};
use rtc::peer_connection::message::RTCMessage;
use rtc::peer_connection::state::RTCPeerConnectionState;
use rtc::peer_connection::transport::RTCIceCandidateInit;
use rtc::rtp_transceiver::rtp_sender::RtpCodecKind;
use rtc::sansio::Protocol;
use rtc::shared::{TaggedBytesMut, TransportContext, TransportProtocol};
use std::net::SocketAddr;
use std::time::{Duration, Instant};
use tokio::net::UdpSocket;

const KEYFRAME_REQUEST_COOLDOWN: Duration = Duration::from_millis(300);

pub const STREAM_WIDTH: u32 = 1280;
pub const STREAM_HEIGHT: u32 = 720;
pub const HW_DECODE_WIDTH: u32 = 1280;
pub const HW_DECODE_HEIGHT: u32 = 720;
const HW_OUTPUT_WIDTH: u32 = 960;
const HW_OUTPUT_HEIGHT: u32 = 544;

pub struct RtcSession {
    pub peer: RtcPeer,
    socket: UdpSocket,
    local_addr: SocketAddr,
    recv_buf: Vec<u8>,
    handshake_stage: HandshakeStage,
    pub connection_state: RTCPeerConnectionState,
    video: VideoReceiver,
    audio: AudioReceiver,
    last_keyframe_request: Option<Instant>,
    input_queue: InputQueue,
    input_channel_ready: bool,
    pub status: String,
    pub server_video_size: Option<(u32, u32)>,
}

impl RtcSession {
    pub async fn new(mut peer: RtcPeer) -> Result<Self> {
        let socket = UdpSocket::bind("0.0.0.0:0")
            .await
            .context("failed to bind UDP socket for WebRTC transport")?;
        let socket_addr = socket
            .local_addr()
            .context("failed to read local UDP socket address")?;
        let video = VideoReceiver::new(DecoderConfig {
            decode_width: HW_DECODE_WIDTH,
            decode_height: HW_DECODE_HEIGHT,
            output_width: HW_OUTPUT_WIDTH,
            output_height: HW_OUTPUT_HEIGHT,
        })?;

        let host_addr = ice::add_host_candidate(&mut peer.peer_connection, socket_addr.port())
            .context("failed to add local host ICE candidate")?;

        match ice::discover_server_reflexive_candidate(&socket, host_addr).await {
            Ok(Some(public_addr)) => {
                if let Err(error) =
                    ice::add_srflx_candidate(&mut peer.peer_connection, public_addr, host_addr)
                {
                    eprintln!("Failed to add server-reflexive ICE candidate: {error:#}");
                }
            }
            Ok(None) => eprintln!("STUN request produced no usable response"),
            Err(error) => eprintln!("STUN discovery failed: {error:#}"),
        }

        ice::local_candidate(&mut peer.peer_connection, String::new(), None)
            .context("failed to signal end-of-candidates")?;

        Ok(Self {
            peer,
            socket,
            local_addr: host_addr,
            recv_buf: vec![0u8; 2048],
            handshake_stage: HandshakeStage::WaitingForChannels,
            connection_state: RTCPeerConnectionState::New,
            video,
            audio: AudioReceiver::new(AUDIO_SAMPLE_RATE as u32),
            last_keyframe_request: None,
            input_queue: InputQueue::default(),
            input_channel_ready: false,
            status: "Negotiating WebRTC connection".to_owned(),
            server_video_size: None,
        })
    }

    pub fn take_new_video_frame(
        &mut self,
        last_sent_frame: &mut Option<u64>,
    ) -> Option<(u64, DecodedFrame)> {
        self.video.take_new_frame(last_sent_frame)
    }

    pub fn add_remote_candidate(&mut self, candidate: RTCIceCandidateInit) -> Result<()> {
        self.peer
            .peer_connection
            .add_remote_candidate(candidate)
            .context("failed to add remote ICE candidate")
    }

    pub fn drain_audio_packets(&mut self) -> Vec<Bytes> {
        self.audio.drain()
    }

    pub fn send_gamepad_frame(&mut self, frame: GamepadFrame) {
        if !self.input_channel_ready {
            return;
        }
        let Some(bytes) = self.input_queue.queue_gamepad_frames([frame], true) else {
            return;
        };
        self.send_input_bytes(&bytes);
    }

    pub fn send_pointer_event(&mut self, event: PointerEvent) {
        if !self.input_channel_ready {
            return;
        }
        let Some(bytes) = self.input_queue.queue_pointer_frame(PointerFrame {
            events: vec![event],
        }) else {
            return;
        };
        self.send_input_bytes(&bytes);
    }

    fn send_input_bytes(&mut self, bytes: &[u8]) {
        let input_channel_id = self.peer.channel_ids.input;
        if let Some(mut input_channel) = self.peer.peer_connection.data_channel(input_channel_id) {
            let _ = input_channel.send(BytesMut::from(bytes));
        }
    }

    pub async fn pump(&mut self) -> Result<Vec<RTCIceCandidateInit>> {
        self.flush_udp().await;
        self.receive_udp();
        let gathered_candidates = self.handle_peer_events();
        let mut keyframe_requested = self.handle_peer_messages();
        self.video.drain_decoder(&mut keyframe_requested);

        let now = Instant::now();
        self.advance_peer_timeout(now);
        self.request_keyframe(keyframe_requested, now);
        if let Some(status) = self.video.status(now) {
            self.status = status;
            eprintln!("{}", self.status);
        }

        Ok(gathered_candidates)
    }

    async fn flush_udp(&mut self) {
        while let Some(outgoing) = self.peer.peer_connection.poll_write() {
            if let Err(error) = self
                .socket
                .send_to(&outgoing.message, outgoing.transport.peer_addr)
                .await
            {
                eprintln!(
                    "Failed to send WebRTC UDP packet to {}: {error}",
                    outgoing.transport.peer_addr
                );
            }
        }
    }

    fn receive_udp(&mut self) {
        loop {
            match self.socket.try_recv_from(&mut self.recv_buf) {
                Ok((n, peer_addr)) => {
                    if let Err(error) = self.peer.peer_connection.handle_read(TaggedBytesMut {
                        now: Instant::now(),
                        transport: TransportContext {
                            local_addr: self.local_addr,
                            peer_addr,
                            ecn: None,
                            transport_protocol: TransportProtocol::UDP,
                        },
                        message: BytesMut::from(&self.recv_buf[..n]),
                    }) {
                        eprintln!("Failed to handle WebRTC UDP packet from {peer_addr}: {error}");
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(error) => {
                    eprintln!("Failed to receive WebRTC UDP packet: {error}");
                    break;
                }
            }
        }
    }

    fn handle_peer_events(&mut self) -> Vec<RTCIceCandidateInit> {
        let mut gathered_candidates = Vec::new();
        while let Some(event) = self.peer.peer_connection.poll_event() {
            match event {
                RTCPeerConnectionEvent::OnConnectionStateChangeEvent(state) => {
                    self.connection_state = state;
                    self.status = format!("WebRTC connection: {state:?}");
                }
                RTCPeerConnectionEvent::OnIceCandidateEvent(ice_event) => {
                    if let Ok(candidate) = ice_event.candidate.to_json() {
                        gathered_candidates.push(candidate);
                    }
                }
                RTCPeerConnectionEvent::OnIceCandidateErrorEvent(error) => {
                    eprintln!(
                        "ICE candidate error from {} ({}:{}): {} {}",
                        error.url, error.address, error.port, error.error_code, error.error_text
                    );
                }
                RTCPeerConnectionEvent::OnTrack(RTCTrackEvent::OnOpen(init)) => {
                    let track_kind = self
                        .peer
                        .peer_connection
                        .rtp_receiver(init.receiver_id)
                        .map(|receiver| receiver.track().kind());
                    match track_kind {
                        Some(RtpCodecKind::Video) => {
                            self.video.open(init.track_id, init.receiver_id, init.ssrc);
                            self.status = "Receiving video track".to_owned();
                        }
                        Some(RtpCodecKind::Audio) => {
                            self.audio.open(init.track_id);
                            self.status = "Receiving audio track".to_owned();
                        }
                        _ => {}
                    }
                }
                RTCPeerConnectionEvent::OnDataChannel(RTCDataChannelEvent::OnOpen(channel_id)) => {
                    let channel_ids = self.peer.channel_ids;
                    if channel_id == channel_ids.message
                        && self.handshake_stage == HandshakeStage::WaitingForChannels
                        && let Some(mut message_channel) =
                            self.peer.peer_connection.data_channel(channel_id)
                    {
                        let _ = message_channel.send_text(channel::message_handshake().to_string());
                        self.handshake_stage = HandshakeStage::WaitingForHandshakeAck;
                    }
                    if channel_id == channel_ids.input
                        && let Some(mut input_channel) =
                            self.peer.peer_connection.data_channel(channel_id)
                    {
                        let client_metadata = self.input_queue.client_metadata_packet(0);
                        let _ = input_channel.send(BytesMut::from(client_metadata.as_slice()));
                        self.input_channel_ready = true;
                    }
                }
                _ => {}
            }
        }
        gathered_candidates
    }

    fn handle_peer_messages(&mut self) -> bool {
        let channel_ids = self.peer.channel_ids;
        let mut keyframe_requested = false;
        while let Some(message) = self.peer.peer_connection.poll_read() {
            match message {
                RTCMessage::RtpPacket(track_id, packet) => {
                    if self.video.handles(&track_id) {
                        self.video.receive(packet, &mut keyframe_requested);
                    } else if self.audio.handles(&track_id) {
                        self.audio.receive(packet);
                    }
                }
                RTCMessage::DataChannelMessage(channel_id, data_message) => {
                    channel::handle_data_channel_message(
                        &mut self.peer.peer_connection,
                        &channel_ids,
                        &mut self.handshake_stage,
                        channel_id,
                        &data_message.data,
                    );
                    if let Some(size) = channel::parse_server_video_size(
                        &channel_ids,
                        channel_id,
                        &data_message.data,
                    ) {
                        eprintln!("xCloud reported server video size: {size:?}");
                        self.server_video_size = Some(size);
                    }
                }
                _ => {}
            }
        }
        keyframe_requested
    }

    fn advance_peer_timeout(&mut self, now: Instant) {
        if let Some(deadline) = self.peer.peer_connection.poll_timeout()
            && now >= deadline
        {
            let _ = self.peer.peer_connection.handle_timeout(now);
        }
    }

    fn request_keyframe(&mut self, requested: bool, now: Instant) {
        let cooldown_elapsed = self.last_keyframe_request.is_none_or(|requested_at| {
            now.duration_since(requested_at) >= KEYFRAME_REQUEST_COOLDOWN
        });
        if !requested || !cooldown_elapsed {
            return;
        }

        self.last_keyframe_request = Some(now);
        let channel_ids = self.peer.channel_ids;
        if let Some(mut control_channel) =
            self.peer.peer_connection.data_channel(channel_ids.control)
        {
            let _ = control_channel.send_text(channel::video_keyframe_requested(true).to_string());
        }
        if let Some((receiver_id, ssrc)) = self.video.rtcp_target()
            && let Some(mut receiver) = self.peer.peer_connection.rtp_receiver(receiver_id)
        {
            let pli = rtcp::payload_feedbacks::picture_loss_indication::PictureLossIndication {
                sender_ssrc: 0,
                media_ssrc: ssrc,
            };
            let _ = receiver.write_rtcp(vec![Box::new(pli)]);
        }
    }
}
