use crate::streaming::control::channel::{
    CHAT_CHANNEL, CONTROL_CHANNEL, INPUT_CHANNEL, MESSAGE_CHANNEL,
};
use crate::streaming::rtc::STUN_SERVER;
use anyhow::{Context, Result};
use rtc::data_channel::{RTCDataChannelId, RTCDataChannelInit};
use rtc::peer_connection::RTCPeerConnection;
use rtc::peer_connection::RTCPeerConnectionBuilder;
use rtc::peer_connection::configuration::RTCConfigurationBuilder;
use rtc::peer_connection::configuration::media_engine::{
    MIME_TYPE_H264, MIME_TYPE_OPUS, MediaEngine,
};
use rtc::peer_connection::configuration::setting_engine::SettingEngine;
use rtc::peer_connection::sdp::RTCSessionDescription;
use rtc::peer_connection::transport::RTCIceServer;
use rtc::rtp_transceiver::rtp_sender::{
    RTCPFeedback, RTCRtpCodec, RTCRtpCodecParameters, RtpCodecKind,
};
use rtc::sansio::Protocol;
use std::time::Duration;

#[derive(Clone, Copy)]
pub struct ChannelIds {
    pub control: RTCDataChannelId,
    pub input: RTCDataChannelId,
    pub message: RTCDataChannelId,
}

pub struct RtcPeer {
    pub peer_connection: RTCPeerConnection,
    pub channel_ids: ChannelIds,
}

impl RtcPeer {
    pub fn new() -> Result<Self> {
        let mut media_engine = MediaEngine::default();
        register_vita_codecs(&mut media_engine).context("failed to register Vita codecs")?;

        let configuration = RTCConfigurationBuilder::new()
            .with_ice_servers(vec![RTCIceServer {
                urls: vec![format!("stun:{STUN_SERVER}")],
                username: String::new(),
                credential: String::new(),
            }])
            .build();
        let mut setting_engine = SettingEngine::default();
        setting_engine.set_ice_connection_attempts(Some(Duration::from_millis(200)), Some(75));

        let mut peer_connection = RTCPeerConnectionBuilder::new()
            .with_configuration(configuration)
            .with_setting_engine(setting_engine)
            .with_media_engine(media_engine)
            .build()
            .context("failed to create rtc peer connection")?;

        let mut ids = [None; 4];
        for (index, channel) in [
            CHAT_CHANNEL,
            CONTROL_CHANNEL,
            INPUT_CHANNEL,
            MESSAGE_CHANNEL,
        ]
        .into_iter()
        .enumerate()
        {
            let init = RTCDataChannelInit {
                ordered: channel.ordered,
                protocol: channel.protocol.to_owned(),
                ..Default::default()
            };
            let data_channel = peer_connection
                .create_data_channel(channel.label, Some(init))
                .with_context(|| format!("failed to create {} data channel", channel.label))?;
            ids[index] = Some(data_channel.id());
        }
        // `ids[0]` (chat) is created for protocol compatibility with the remote peer, but its id
        // is never needed locally.
        let channel_ids = ChannelIds {
            control: ids[1].expect("control channel created"),
            input: ids[2].expect("input channel created"),
            message: ids[3].expect("message channel created"),
        };

        peer_connection
            .add_transceiver_from_kind(RtpCodecKind::Video, None)
            .context("failed to add video transceiver")?;
        peer_connection
            .add_transceiver_from_kind(RtpCodecKind::Audio, None)
            .context("failed to add audio transceiver")?;

        Ok(Self {
            peer_connection,
            channel_ids,
        })
    }

    pub fn create_offer(&mut self) -> Result<RTCSessionDescription> {
        let offer = self
            .peer_connection
            .create_offer(None)
            .context("failed to create rtc offer")?;
        self.peer_connection
            .set_local_description(offer.clone())
            .context("failed to set local rtc description")?;
        Ok(offer)
    }

    pub fn set_remote_answer(&mut self, sdp: impl Into<String>) -> Result<()> {
        let answer = RTCSessionDescription::answer(sdp.into());
        self.peer_connection
            .set_remote_description(answer?)
            .context("failed to set remote rtc answer")
    }

    pub fn close(&mut self) -> Result<()> {
        self.peer_connection
            .close()
            .context("failed to close rtc peer connection")
    }
}

fn register_vita_codecs(media_engine: &mut MediaEngine) -> Result<()> {
    media_engine.register_codec(
        RTCRtpCodecParameters {
            rtp_codec: RTCRtpCodec {
                mime_type: MIME_TYPE_OPUS.to_owned(),
                clock_rate: 48_000,
                channels: 2,
                sdp_fmtp_line: "minptime=10;useinbandfec=1;stereo=1".to_owned(),
                rtcp_feedback: vec![],
            },
            payload_type: 111,
        },
        RtpCodecKind::Audio,
    )?;

    media_engine.register_codec(
        RTCRtpCodecParameters {
            rtp_codec: RTCRtpCodec {
                mime_type: MIME_TYPE_H264.to_owned(),
                clock_rate: 90_000,
                channels: 0,
                sdp_fmtp_line:
                    "level-asymmetry-allowed=0;packetization-mode=1;profile-level-id=42e01f;max-fs=3600;max-mbps=108000"
                        .to_owned(),
                rtcp_feedback: vec![
                    RTCPFeedback {
                        typ: "goog-remb".to_owned(),
                        parameter: "".to_owned(),
                    },
                    RTCPFeedback {
                        typ: "ccm".to_owned(),
                        parameter: "fir".to_owned(),
                    },
                    RTCPFeedback {
                        typ: "nack".to_owned(),
                        parameter: "".to_owned(),
                    },
                    RTCPFeedback {
                        typ: "nack".to_owned(),
                        parameter: "pli".to_owned(),
                    },
                ],
            },
            payload_type: 102,
        },
        RtpCodecKind::Video,
    )?;

    Ok(())
}
