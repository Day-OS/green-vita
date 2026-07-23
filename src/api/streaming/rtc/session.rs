use crate::api::streaming::rtc::media::{AudioReceiver, VideoReceiver};
use crate::api::streaming::rtc::transport::RtcTransport;
use crate::streaming::input::{GamepadFrame, PointerEvent};
use crate::streaming::video::{DecoderConfig, DirectVideoOutput};
use anyhow::{Context, Result};
use rtc::peer_connection::RTCPeerConnection;
use rtc::peer_connection::event::{RTCDataChannelEvent, RTCPeerConnectionEvent, RTCTrackEvent};
use rtc::peer_connection::message::RTCMessage;
use rtc::peer_connection::sdp::RTCSessionDescription;
use rtc::peer_connection::state::RTCPeerConnectionState;
use rtc::peer_connection::transport::RTCIceCandidateInit;
use rtc::rtp_transceiver::rtp_sender::RtpCodecKind;
use rtc::sansio::Protocol;
use std::sync::Arc;
use std::time::{Duration, Instant};

const KEYFRAME_REQUEST_COOLDOWN: Duration = Duration::from_millis(300);
const INITIAL_VIDEO_GRACE: Duration = Duration::from_millis(500);
const INITIAL_VIDEO_KEYFRAME_INTERVAL: Duration = Duration::from_millis(500);

pub(crate) struct RtcSessionConfig {
    pub stun_server: &'static str,
    pub route_probe: &'static str,
    pub audio_sample_rate: u32,
    pub audio_payload_type: u8,
    pub video_fps: u32,
    pub decoder: DecoderConfig,
}

/// Provider-specific hooks invoked by the reusable RTC session.
pub(crate) trait RtcSessionBackend {
    fn handle_channel_open(
        &mut self,
        peer: &mut RTCPeerConnection,
        channel_id: rtc::data_channel::RTCDataChannelId,
    );
    fn handle_channel_message(
        &mut self,
        peer: &mut RTCPeerConnection,
        channel_id: rtc::data_channel::RTCDataChannelId,
        data: &[u8],
    );
    fn send_gamepad_frame(&mut self, peer: &mut RTCPeerConnection, frame: GamepadFrame) -> bool;
    fn send_pointer_event(&mut self, peer: &mut RTCPeerConnection, event: PointerEvent);
    fn notify_keyframe_requested(&mut self, peer: &mut RTCPeerConnection);
    fn server_video_size(&self) -> Option<(u32, u32)>;
}

pub(crate) struct RtcSession<B: RtcSessionBackend> {
    pub(crate) peer: RTCPeerConnection,
    pub(crate) transport: RtcTransport,
    pub(crate) backend: B,
    pub connection_state: RTCPeerConnectionState,
    pub(crate) video: VideoReceiver,
    pub(crate) audio: AudioReceiver,
    last_keyframe_request: Option<Instant>,
    initial_video_watchdog_started_at: Option<Instant>,
    last_initial_video_keyframe_request: Option<Instant>,
    pub status: String,
}

impl<B: RtcSessionBackend> RtcSession<B> {
    pub async fn new(
        mut peer: RTCPeerConnection,
        backend: B,
        config: RtcSessionConfig,
        direct_output: Arc<DirectVideoOutput>,
    ) -> Result<Self> {
        let transport =
            RtcTransport::bind(&mut peer, config.stun_server, config.route_probe).await?;
        let video = VideoReceiver::new(config.decoder, direct_output, config.video_fps)?;

        Ok(Self {
            peer,
            transport,
            backend,
            connection_state: RTCPeerConnectionState::New,
            video,
            audio: AudioReceiver::new(config.audio_sample_rate, config.audio_payload_type),
            last_keyframe_request: None,
            initial_video_watchdog_started_at: None,
            last_initial_video_keyframe_request: None,
            status: "Negotiating WebRTC connection".to_owned(),
        })
    }

    pub fn create_offer(&mut self) -> Result<RTCSessionDescription> {
        let offer = self
            .peer
            .create_offer(None)
            .context("failed to create rtc offer")?;
        self.peer
            .set_local_description(offer.clone())
            .context("failed to set local rtc description")?;
        Ok(offer)
    }

    pub fn set_remote_answer(&mut self, sdp: impl Into<String>) -> Result<()> {
        let answer = RTCSessionDescription::answer(sdp.into());
        self.peer
            .set_remote_description(answer?)
            .context("failed to set remote rtc answer")
    }

    pub fn close(&mut self) -> Result<()> {
        self.peer
            .close()
            .context("failed to close rtc peer connection")
    }

    pub fn add_remote_candidate(&mut self, candidate: RTCIceCandidateInit) -> Result<()> {
        self.peer
            .add_remote_candidate(candidate)
            .context("failed to add remote ICE candidate")
    }

    pub async fn pump(&mut self) -> Result<Vec<RTCIceCandidateInit>> {
        self.transport.flush(&mut self.peer).await;
        self.transport.receive(&mut self.peer);
        let gathered_candidates = self.handle_peer_events();
        let mut keyframe_requested = self.handle_peer_messages();
        self.video.drain_decoder(&mut keyframe_requested);

        let now = Instant::now();
        // rtc-rs is sans-I/O, so its expired internal timer must be advanced by our pump.
        if let Some(deadline) = self.peer.poll_timeout()
            && now >= deadline
        {
            let _ = self.peer.handle_timeout(now);
        }
        if self.initial_video_keyframe_due(now) {
            eprintln!("No initial video RTP after WebRTC connected; requesting a keyframe");
            keyframe_requested = true;
        }
        self.request_keyframe(keyframe_requested, now);
        if let Some(status) = self.video.status(now) {
            self.status = if let Some((width, height)) = self.backend.server_video_size() {
                format!("srv:{width}x{height} {status}")
            } else {
                format!("srv:? {status}")
            };
            eprintln!("{}", self.status);
        }

        Ok(gathered_candidates)
    }

    fn initial_video_keyframe_due(&mut self, now: Instant) -> bool {
        if self.video.received_packet {
            self.initial_video_watchdog_started_at = None;
            self.last_initial_video_keyframe_request = None;
            return false;
        }
        if self.connection_state != RTCPeerConnectionState::Connected {
            self.initial_video_watchdog_started_at = None;
            self.last_initial_video_keyframe_request = None;
            return false;
        }

        let connected_at = *self.initial_video_watchdog_started_at.get_or_insert(now);
        if now.duration_since(connected_at) < INITIAL_VIDEO_GRACE {
            return false;
        }
        if self
            .last_initial_video_keyframe_request
            .is_some_and(|requested_at| {
                now.duration_since(requested_at) < INITIAL_VIDEO_KEYFRAME_INTERVAL
            })
        {
            return false;
        }

        self.last_initial_video_keyframe_request = Some(now);
        true
    }

    fn handle_peer_events(&mut self) -> Vec<RTCIceCandidateInit> {
        let mut gathered_candidates = Vec::new();
        while let Some(event) = self.peer.poll_event() {
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
                    self.backend.handle_channel_open(&mut self.peer, channel_id);
                }
                _ => {}
            }
        }
        gathered_candidates
    }

    fn handle_peer_messages(&mut self) -> bool {
        let mut keyframe_requested = false;
        while let Some(message) = self.peer.poll_read() {
            match message {
                RTCMessage::RtpPacket(track_id, packet) => {
                    if self.video.handles(&track_id) {
                        self.video.receive(packet, &mut keyframe_requested);
                    } else if self.audio.handles(&track_id) {
                        self.audio.receive(packet);
                    }
                }
                RTCMessage::DataChannelMessage(channel_id, data_message) => {
                    self.backend.handle_channel_message(
                        &mut self.peer,
                        channel_id,
                        &data_message.data,
                    );
                }
                _ => {}
            }
        }
        keyframe_requested
    }

    fn request_keyframe(&mut self, requested: bool, now: Instant) {
        let cooldown_elapsed = self.last_keyframe_request.is_none_or(|requested_at| {
            now.duration_since(requested_at) >= KEYFRAME_REQUEST_COOLDOWN
        });
        if !requested || !cooldown_elapsed {
            return;
        }

        self.last_keyframe_request = Some(now);
        self.backend.notify_keyframe_requested(&mut self.peer);
        self.video.request_keyframe(&mut self.peer);
    }
}
