use anyhow::{Context, Result};
use interceptor::{NackGeneratorBuilder, NackGeneratorInterceptor, NoopInterceptor, Registry};
use rtc::data_channel::{RTCDataChannelId, RTCDataChannelInit};
use rtc::peer_connection::configuration::RTCConfigurationBuilder;
use rtc::peer_connection::configuration::media_engine::MediaEngine;
use rtc::peer_connection::configuration::setting_engine::SettingEngine;
use rtc::peer_connection::transport::RTCIceServer;
use rtc::peer_connection::RTCPeerConnectionBuilder;
use rtc::rtp_transceiver::rtp_sender::RtpCodecKind;
use std::time::Duration;

/// The video/audio codecs are already registered with `nack`/`nack pli` in their RTCP feedback
/// (see `register_vita_codecs`), which only advertises the capability in SDP - it doesn't make
/// this client act on it. This is the interceptor that actually does: it watches incoming video
/// RTP for gaps in the sequence numbers and sends a NACK asking Xbox Cloud Gaming's server to
/// retransmit just the missing packet, instead of the only recovery this client had before,
/// waiting for `damage_score` to cross its threshold and requesting an entire new keyframe.
/// There's no responder half of the pair (which would retransmit our own outgoing RTP if the
/// remote NACKed it) because this client never sends RTP media, only receives it - nothing we
/// send is NACK-able.
///
/// The concrete type has to be spelled out because `Interceptor` requires `Sized` - it isn't
/// object-safe, so it can't be erased behind `Box<dyn Interceptor>`. Every other module that
/// touches a peer connection imports the `RTCPeerConnection` alias below instead of the crate's
/// bare (and now mismatched) `rtc::peer_connection::RTCPeerConnection`.
type VitaInterceptor = NackGeneratorInterceptor<NoopInterceptor>;

pub(crate) type RTCPeerConnection = rtc::peer_connection::RTCPeerConnection<VitaInterceptor>;

pub(crate) struct RtcDataChannelConfig {
    pub label: &'static str,
    pub protocol: &'static str,
    pub ordered: bool,
    pub max_packet_life_time: Option<u16>,
    pub max_retransmits: Option<u16>,
}

/// Builds the provider-neutral WebRTC peer. Providers supply only their negotiated codecs,
/// ICE servers and data-channel descriptions.
pub(crate) fn create(
    media_engine: MediaEngine,
    ice_servers: Vec<RTCIceServer>,
    channels: &[RtcDataChannelConfig],
) -> Result<(RTCPeerConnection, Vec<RTCDataChannelId>)> {
    let configuration = RTCConfigurationBuilder::new()
        .with_ice_servers(ice_servers)
        .build();
    let mut setting_engine = SettingEngine::default();
    setting_engine.set_ice_connection_attempts(Some(Duration::from_millis(200)), Some(75));
    let interceptors = Registry::new().with(NackGeneratorBuilder::new().build());

    let mut peer = RTCPeerConnectionBuilder::new()
        .with_configuration(configuration)
        .with_setting_engine(setting_engine)
        .with_media_engine(media_engine)
        .with_interceptor_registry(interceptors)
        .build()
        .context("failed to create rtc peer connection")?;

    let mut channel_ids = Vec::with_capacity(channels.len());
    for channel in channels {
        let init = RTCDataChannelInit {
            ordered: channel.ordered,
            max_packet_life_time: channel.max_packet_life_time,
            max_retransmits: channel.max_retransmits,
            protocol: channel.protocol.to_owned(),
            ..Default::default()
        };
        let data_channel = peer
            .create_data_channel(channel.label, Some(init))
            .with_context(|| format!("failed to create {} data channel", channel.label))?;
        channel_ids.push(data_channel.id());
    }

    peer.add_transceiver_from_kind(RtpCodecKind::Video, None)
        .context("failed to add video transceiver")?;
    peer.add_transceiver_from_kind(RtpCodecKind::Audio, None)
        .context("failed to add audio transceiver")?;

    Ok((peer, channel_ids))
}
