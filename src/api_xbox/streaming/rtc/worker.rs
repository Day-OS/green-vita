use crate::Stream;
use crate::api::streaming::rtc::session::RtcSessionConfig;
use crate::api::streaming::rtc::worker::{RtcWorker, RtcWorkerProvider};
use crate::api_xbox::streaming::rtc::protocol::XboxRtcProtocol;
use crate::api_xbox::streaming::rtc::{AUDIO_PAYLOAD_TYPE, ROUTE_PROBE, STUN_SERVER, peer, sdp};
use crate::streaming::audio::AUDIO_SAMPLE_RATE;
use crate::streaming::video::{
    DEFAULT_VIDEO_FPS, DecoderConfig, HW_OUTPUT_HEIGHT, HW_OUTPUT_WIDTH, STREAM_HEIGHT,
    STREAM_WIDTH, UNLOCKED_VIDEO_FPS,
};
use anyhow::Result;
use rtc::peer_connection::RTCPeerConnection;
use rtc::peer_connection::sdp::RTCSessionDescription;

struct XboxRtcWorkerProvider {
    stream: Stream,
    video_fps: u32,
}

impl RtcWorkerProvider for XboxRtcWorkerProvider {
    type Protocol = XboxRtcProtocol;

    fn create_peer(&self) -> Result<(RTCPeerConnection, Self::Protocol)> {
        peer::create(self.video_fps)
    }

    fn session_config(&self) -> RtcSessionConfig {
        RtcSessionConfig {
            stun_server: STUN_SERVER,
            route_probe: ROUTE_PROBE,
            audio_sample_rate: AUDIO_SAMPLE_RATE as u32,
            audio_payload_type: AUDIO_PAYLOAD_TYPE,
            video_fps: self.video_fps,
            decoder: DecoderConfig {
                decode_width: STREAM_WIDTH,
                decode_height: STREAM_HEIGHT,
                output_width: HW_OUTPUT_WIDTH,
                output_height: HW_OUTPUT_HEIGHT,
            },
        }
    }

    async fn exchange_sdp(&self, offer: &RTCSessionDescription) -> Result<String> {
        self.stream
            .send_sdp_offer(&sdp::request_video_fps(&offer.sdp, self.video_fps))
            .await
    }
}

pub(crate) fn spawn(stream: Stream, unlock_video_fps: bool) -> Result<RtcWorker> {
    let video_fps = if unlock_video_fps {
        UNLOCKED_VIDEO_FPS
    } else {
        DEFAULT_VIDEO_FPS
    };
    RtcWorker::spawn(XboxRtcWorkerProvider { stream, video_fps })
}
