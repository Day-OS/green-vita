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
use rtc::peer_connection::sdp::RTCSessionDescription;

use crate::api::streaming::rtc::peer::FeedbackPeer as RTCPeerConnection;
use crate::settings::{ColorProfile, H264Profile};

struct XboxRtcWorkerProvider {
    stream: Stream,
    video_fps: u32,
    video_bitrate_kbps: u32,
    video_h264_profile: H264Profile,
    color_profile: ColorProfile,
}

impl RtcWorkerProvider for XboxRtcWorkerProvider {
    type Protocol = XboxRtcProtocol;

    fn create_peer(&self) -> Result<(RTCPeerConnection, Self::Protocol)> {
        peer::create(self.video_fps, self.video_h264_profile)
    }

    fn session_config(&self) -> RtcSessionConfig {
        RtcSessionConfig {
            stun_server: STUN_SERVER,
            route_probe: ROUTE_PROBE,
            audio_sample_rate: AUDIO_SAMPLE_RATE as u32,
            audio_payload_type: AUDIO_PAYLOAD_TYPE,
            video_fps: self.video_fps,
            video_bitrate_kbps: self.video_bitrate_kbps,
            decoder: DecoderConfig {
                decode_width: STREAM_WIDTH,
                decode_height: STREAM_HEIGHT,
                output_width: HW_OUTPUT_WIDTH,
                output_height: HW_OUTPUT_HEIGHT,
                color_profile: self.color_profile,
            },
        }
    }

    async fn exchange_sdp(&self, offer: &RTCSessionDescription) -> Result<String> {
        let sdp = sdp::cap_video_bitrate(&offer.sdp, self.video_bitrate_kbps);
        // Thread the user's fps switch into the offer (2026-11-01 fix: this
        // call hardcoded 30 while `video_fps` sat unused on the provider).
        // The console reportedly ignores a=framerate (content-driven rate),
        // so this is signaling honesty, not a performance change.
        let sdp = sdp::request_video_fps(&sdp, self.video_fps);
        let answer = self.stream.send_sdp_offer(&sdp).await?;
        // 2a: inbound extension ids from the ANSWER (0 = still not
        // negotiated — pre-2a offers carried none and unwrap_or(0) hid that).
        let twcc_id = sdp::extract_extmap_id(&answer, super::peer::TWCC_URI).unwrap_or(0);
        let ast_id = sdp::extract_extmap_id(&answer, super::peer::ABS_SEND_TIME_URI).unwrap_or(0);
        let vt_id = sdp::extract_extmap_id(&answer, super::peer::VIDEO_TIMING_URI).unwrap_or(0);
        eprintln!("negotiated ext ids: twcc={twcc_id} abs-send-time={ast_id} video-timing={vt_id}");
        crate::api::streaming::rtc::rtp::set_negotiated_twcc_ext_id(twcc_id);
        crate::api::streaming::rtc::rtp::set_negotiated_ast_ext_id(ast_id);
        crate::api::streaming::rtc::rtp::set_negotiated_vt_ext_id(vt_id);
        // S5b: negotiated RTX PT map from the ANSWER (source of truth — the
        // console may renumber; ours echoes 112/apt=102 but parse, don't assume).
        let rtx_map = sdp::extract_negotiated_rtx_mapping(&answer);
        if !rtx_map.is_empty() {
            eprintln!("negotiated RTX map: {rtx_map:?}");
            crate::api::streaming::rtc::media::set_negotiated_rtx_map(rtx_map);
        }
        // S5b-3: FID-derived SSRC map (fork-trace fix — bind-time learning
        // never fires for us; the interceptor rewrites from THIS).
        let fid_map = sdp::extract_fid_rtx_ssrcs(&answer);
        if !fid_map.is_empty() {
            eprintln!("negotiated RTX FID map: {fid_map:?}");
            crate::api::streaming::rtc::media::set_negotiated_rtx_ssrc_map(fid_map);
        }
        Ok(answer)
    }
}

pub(crate) fn spawn(
    stream: Stream,
    request_60fps: bool,
    video_bitrate_kbps: u32,
    video_h264_profile: H264Profile,
    color_profile: ColorProfile,
) -> Result<RtcWorker> {
    let video_fps = if request_60fps {
        UNLOCKED_VIDEO_FPS
    } else {
        DEFAULT_VIDEO_FPS
    };
    RtcWorker::spawn(XboxRtcWorkerProvider {
        stream,
        video_fps,
        video_bitrate_kbps,
        video_h264_profile,
        color_profile,
    })
}
