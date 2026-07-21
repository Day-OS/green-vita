use crate::Stream;
use crate::api::streaming::rtc::session::RtcSessionConfig;
use crate::api::streaming::rtc::worker::{RtcWorker, RtcWorkerProvider};
use crate::api_xbox::streaming::rtc::protocol::XboxRtcProtocol;
use crate::api_xbox::streaming::rtc::{AUDIO_PAYLOAD_TYPE, ROUTE_PROBE, STUN_SERVER, peer};
use crate::streaming::audio::AUDIO_SAMPLE_RATE;
use crate::streaming::video::{
    DecoderConfig, HW_DECODE_HEIGHT, HW_DECODE_WIDTH, HW_OUTPUT_HEIGHT, HW_OUTPUT_WIDTH,
};
use anyhow::Result;
use rtc::peer_connection::RTCPeerConnection;
use rtc::peer_connection::sdp::RTCSessionDescription;

struct XboxRtcWorkerProvider {
    stream: Stream,
}

impl RtcWorkerProvider for XboxRtcWorkerProvider {
    type Protocol = XboxRtcProtocol;

    fn create_peer(&self) -> Result<(RTCPeerConnection, Self::Protocol)> {
        peer::create()
    }

    fn session_config(&self) -> RtcSessionConfig {
        RtcSessionConfig {
            stun_server: STUN_SERVER,
            route_probe: ROUTE_PROBE,
            audio_sample_rate: AUDIO_SAMPLE_RATE as u32,
            audio_payload_type: AUDIO_PAYLOAD_TYPE,
            decoder: DecoderConfig {
                decode_width: HW_DECODE_WIDTH,
                decode_height: HW_DECODE_HEIGHT,
                output_width: HW_OUTPUT_WIDTH,
                output_height: HW_OUTPUT_HEIGHT,
            },
        }
    }

    async fn exchange_sdp(&self, offer: &RTCSessionDescription) -> Result<String> {
        self.stream.send_sdp_offer(&offer.sdp).await
    }
}

pub(crate) fn spawn(stream: Stream) -> Result<RtcWorker> {
    RtcWorker::spawn(XboxRtcWorkerProvider { stream })
}
