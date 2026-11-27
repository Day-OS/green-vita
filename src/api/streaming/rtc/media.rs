use crate::api::streaming::rtc::rtp;
use crate::streaming::video::{self, DecodedFrame, DecoderConfig, DirectVideoOutput, VideoDecodeWorker, STREAM_HEIGHT, STREAM_WIDTH};
use anyhow::Result;
use bytes::Bytes;
use rtc::media_stream::MediaStreamTrackId;
use crate::api::streaming::rtc::peer::FeedbackPeer as RTCPeerConnection;
use rtc::rtp::Packet;
use rtc::rtp_transceiver::RTCRtpReceiverId;
use std::collections::HashMap;
use std::sync::{Arc, LazyLock, RwLock};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};
use crate::streaming::video::metrics::{drain_metrics, METRICS};
use crate::api_xbox::streaming::control::input::{SEND_RENDER_TIMING_METADATA, queue_video_metadata};

const STREAM_STATS_INTERVAL: Duration = Duration::from_secs(1);
const REMB_INTERVAL: Duration = Duration::from_millis(500);
const TWCC_INTERVAL: Duration = Duration::from_millis(60);
/// RR-first probe (31/10): Chrome sends RR ~1.2/s (pcap 20261030: median
/// 0.83 s inter-arrival) — match that family.
const RR_INTERVAL: Duration = Duration::from_millis(800);
/// One-variable gate (A2 method): RTCP Receiver Reports on/off. The passive
/// SR sensor inside send_rr is NOT gated — it observes, never transmits.
const SEND_RR: bool = true;
/// S5b master switch (A2 method): hand-rolled NACK send on/off.
const SEND_NACK: bool = true;

/// REMB feedback master switch — **FALSE (reverted 2026-11-01).** The A/B
/// arm (`2275aa8`, 288 s, cap 5M) answered: descent-only REMB is HARMFUL
/// on this link. Ladder fired on ordinary radio bursts (NACK ≈0.9/s) and
/// ratcheted the announced ceiling down with no re-growth → console
/// obeyed, bw fell thirds 2.45→2.13→2.0 Mbps (baseline 4.21→4.96→4.91)
/// while its pacer QUEUED the surplus: stream_lag thirds 11→131→90 ms
/// (max 1.0 s), queue_full 3.15 vs 0.42, presented 47 vs 52.5 — the
/// accumulating-latency class, from the LOW direction this time (15M-const
/// was the HIGH direction). User observation matches: pause/menu drains
/// latency to floor (console queue empties), freezes release in bursts
/// that catch up only to the accumulated point. LESSONS for any revival:
/// thresholds ≥1 NACK/s ratchet down on a link whose healthy baseline is
/// 0.5–1.5/s; passive/no-re-growth turns bursts into permanent collapse;
/// remb_effective_bps must be a CSV column BEFORE the next attempt.
/// Chrome-parity (TWCC-60 + RR + static b=AS cap) remains the best config.
const SEND_REMB: bool = false;
/// Safe-Ceiling REMB (2026-11-01, user+eng plan): the ceiling = the user's
/// bitrate-cap setting (the SAME field that drives the SDP b=AS line, read
/// fresh at session start); 0 (= "no cap" in the UI) falls back to this
/// default. Replaces the constant 15 Mbps announcement that advertised
/// headroom the 2.4 GHz hop does not have — the documented-intent fix that
/// never landed. Descent-only: the ladder below steps down from LIVE
/// pre-repair damage (our own NACK egress) and never grows back (passive).
const REMB_DEFAULT_CAP_BPS: f32 = 15_000_000.0;
/// Ladder floor — never descend below this, so the signal stays sane.
const REMB_FLOOR_BPS: f32 = 1_000_000.0;
/// NACK-egress/s ladder thresholds, calibrated from 2026-11-01 archives
/// (clean ≈0.18/s, congested 2–5.6/s): ≥GENTLE −10 %/tick, ≥DEEP −25 %/tick,
/// below GENTLE = hold at current value.
const REMB_NACK_GENTLE: f32 = 1.0;
const REMB_NACK_DEEP: f32 = 4.0;
const REMB_STEP_GENTLE: f32 = 0.90;
const REMB_STEP_DEEP: f32 = 0.75;
/// Egress-NACK feeder for the ladder. transport.rs increments per NACK
/// packet leaving the socket; the 1 Hz stats tick swaps it out — deliberately
/// separate from METRICS.rtcp_nack_sent so the controller and the CSV drain
/// never steal each other's counts.
pub(crate) static REMB_NACK_EGRESS: AtomicU64 = AtomicU64::new(0);

/// Pure Safe-Ceiling descent (unit-tested): passive — never rises, floored
/// at REMB_FLOOR_BPS. Evaluated on the 1 Hz stats tick; send_remb (500 ms
/// cadence) only reads the current effective value.
fn remb_descent(effective_bps: f32, nack_rate_per_s: f32) -> f32 {
    let stepped = if nack_rate_per_s >= REMB_NACK_DEEP {
        effective_bps * REMB_STEP_DEEP
    } else if nack_rate_per_s >= REMB_NACK_GENTLE {
        effective_bps * REMB_STEP_GENTLE
    } else {
        effective_bps
    };
    stepped.clamp(REMB_FLOOR_BPS, effective_bps.max(REMB_FLOOR_BPS))
}
/// Arbitrary stable local RTCP sender SSRC (we transmit no RTP of our own).
/// 2c: also the sender_ssrc of the vendored TWCC feedback Recorder.
pub(crate) const LOCAL_RTCP_SENDER_SSRC: u32 = 1;

/// S5b: negotiated RTX PT -> primary PT map from the ANSWER (renegotiation-
/// safe overwrite — 9367138's fix over the OnceLock original). Read on the
/// RTP hot path (is_negotiated_rtx_pt) and by the rewrite interceptor.
static NEGOTIATED_RTX_MAP: LazyLock<RwLock<HashMap<u8, u8>>> =
    LazyLock::new(|| RwLock::new(HashMap::new()));

/// Process start reference for monotonic timestamps used by RTCP SR anchoring
/// and capture-to-arrival latency. Initialized once at first use.
pub(crate) static PROCESS_START: LazyLock<Instant> = LazyLock::new(Instant::now);

/// S5b-3 (fork-trace fix): RTX SSRC -> primary SSRC from the answer's
/// `a=ssrc-group:FID` lines. The fork's bind-time learning is dead for us
/// (deferred track codec => fuzzy-search None => bind skipped; the
/// first-packet path has "//TODO: what about RTX/FEC stream?"), so the
/// interceptor reads THIS map — set before any packet flows.
static NEGOTIATED_RTX_SSRC_MAP: LazyLock<RwLock<HashMap<u32, u32>>> =
    LazyLock::new(|| RwLock::new(HashMap::new()));

/// Sets the negotiated RTX mapping (called once per answer in worker.rs).
pub(crate) fn set_negotiated_rtx_map(map: HashMap<u8, u8>) {
    *NEGOTIATED_RTX_MAP.write().unwrap() = map;
}

/// Sets the FID-derived RTX SSRC map from the answer.
pub(crate) fn set_negotiated_rtx_ssrc_map(map: HashMap<u32, u32>) {
    *NEGOTIATED_RTX_SSRC_MAP.write().unwrap() = map;
}

/// Primary SSRC for a given RTX SSRC (answer FID group), if negotiated.
pub(crate) fn rtx_primary_for(ssrc: u32) -> Option<u32> {
    NEGOTIATED_RTX_SSRC_MAP.read().unwrap().get(&ssrc).copied()
}

/// Checks if a payload type is a negotiated RTX type (hot path — direct read).
pub(crate) fn is_negotiated_rtx_pt(payload_type: u8) -> bool {
    NEGOTIATED_RTX_MAP.read().unwrap().contains_key(&payload_type)
}

/// Primary PT for a negotiated RTX PT (`a=fmtp apt=`).
pub(crate) fn rtx_apt_for(payload_type: u8) -> Option<u8> {
    NEGOTIATED_RTX_MAP.read().unwrap().get(&payload_type).copied()
}

/// S5b: flatten hole ranges (wrapping-safe, capped) and group into
/// RFC 4585 pid + 16-bit loss-bitmask pairs. Pure — host-tested
/// (grouping math + marshal round-trip in `mod nack_tests`).
fn ranges_to_nack_pairs(
    ranges: &[(u16, u16)],
    max_seqs: usize,
) -> Vec<rtcp::transport_feedbacks::transport_layer_nack::NackPair> {
    use rtcp::transport_feedbacks::transport_layer_nack::NackPair;
    let mut seqs: Vec<u16> = Vec::new();
    'collect: for (start, end) in ranges {
        let count = u32::from(end.wrapping_sub(*start)).wrapping_add(1);
        for i in 0..count {
            if seqs.len() >= max_seqs {
                break 'collect;
            }
            seqs.push(start.wrapping_add(i as u16));
        }
    }
    let mut nacks: Vec<NackPair> = Vec::new();
    let mut i = 0;
    while i < seqs.len() {
        let pid = seqs[i];
        let mut mask: u16 = 0;
        let mut j = i + 1;
        while j < seqs.len()
            && (j - i) <= 16
            && seqs[j] == pid.wrapping_add((j - i) as u16)
        {
            mask |= 1u16 << ((j - i) - 1);
            j += 1;
        }
        nacks.push(NackPair {
            packet_id: pid,
            lost_packets: mask,
        });
        i = j;
    }
    nacks
}

#[cfg(test)]
mod nack_tests {
    use super::ranges_to_nack_pairs;
    use rtcp::packet::{marshal as rtcp_marshal, unmarshal as rtcp_unmarshal};
    use rtcp::transport_feedbacks::transport_layer_nack::{
        NackPair, TransportLayerNack,
    };

    #[test]
    fn nack_pair_grouping_math() {
        let one = ranges_to_nack_pairs(&[(100, 100)], 32);
        assert_eq!(one.len(), 1);
        assert_eq!(one[0].packet_id, 100);
        assert_eq!(one[0].lost_packets, 0);

        // 18 consecutive (100..=117): pid=100 with bits for 101..=116, then pid=117.
        let run = ranges_to_nack_pairs(&[(100, 117)], 32);
        assert_eq!(run.len(), 2);
        assert_eq!(run[0].packet_id, 100);
        assert_eq!(run[0].lost_packets, 0xFFFF);
        assert_eq!(run[1].packet_id, 117);
        assert_eq!(run[1].lost_packets, 0);

        // Wrapping range 65534..=0 -> [65534, 65535, 0], one pair, mask 0b11.
        let wr = ranges_to_nack_pairs(&[(65534, 0)], 32);
        assert_eq!(wr.len(), 1);
        assert_eq!(wr[0].packet_id, 65534);
        assert_eq!(wr[0].lost_packets, 0b11);

        // Per-send cap: (0,1000) capped at 8 seqs -> [0..=7], mask 0x7F.
        let capped = ranges_to_nack_pairs(&[(0, 1000)], 8);
        assert_eq!(capped.len(), 1);
        assert_eq!(capped[0].packet_id, 0);
        assert_eq!(capped[0].lost_packets, 0x7F);
    }

    #[test]
    fn nack_bytes_roundtrip() {
        // The exact packet send_nack emits: marshal -> unmarshal -> field-equal.
        let original = TransportLayerNack {
            sender_ssrc: 1,
            media_ssrc: 0xDEAD_BEEF,
            nacks: vec![
                NackPair { packet_id: 100, lost_packets: 0b101 },
                NackPair { packet_id: 200, lost_packets: 0 },
            ],
        };
        let marshalled = rtcp_marshal(&[Box::new(original.clone())]).unwrap();
        let unmarshalled = rtcp_unmarshal(&mut marshalled.as_ref()).unwrap();
        assert_eq!(unmarshalled.len(), 1);
        let rt = unmarshalled[0]
            .as_any()
            .downcast_ref::<TransportLayerNack>()
            .expect("TransportLayerNack type");
        assert_eq!(rt.sender_ssrc, 1);
        assert_eq!(rt.media_ssrc, 0xDEAD_BEEF);
        assert_eq!(rt.nacks.len(), 2);
        assert_eq!(rt.nacks[0].packet_id, 100);
        assert_eq!(rt.nacks[0].lost_packets, 0b101);
        assert_eq!(rt.nacks[1].packet_id, 200);
        assert_eq!(rt.nacks[1].lost_packets, 0);
    }
}

#[derive(Default)]
struct VideoStats {
    dropped: u64,
    decode_errors: u64,
    last_sample_duration_us: Option<u64>,
    encoded_resolution: Option<(u32, u32)>,
    color: Option<u8>,
}

pub(crate) struct VideoReceiver {
    track_id: Option<MediaStreamTrackId>,
    receiver_id: Option<RTCRtpReceiverId>,
    ssrc: Option<u32>,
    rtp: rtp::VideoRtp,
    pub(crate) decoder: VideoDecodeWorker,
    pub(crate) latest_frame: Option<(u64, DecodedFrame)>,
    next_frame_id: u64,
    pub(crate) received_packet: bool,
    pub(crate) packets: Vec<Bytes>,
    last_stats_report: Instant,
    last_bw_bytes: u64,
    last_remb_at: Option<Instant>,
    last_twcc_at: Option<Instant>,
    last_rr_at: Option<Instant>,
    /// Safe-Ceiling REMB controller state (inert while SEND_REMB is false).
    remb_cap_bps: f32,
    remb_effective_bps: f32,
    remb_nack_window: [u32; 5],
    remb_window_idx: usize,
    stats: VideoStats,
    direct_output: Arc<DirectVideoOutput>,
}

impl VideoReceiver {
    pub(crate) fn new(
        config: DecoderConfig,
        direct_output: Arc<DirectVideoOutput>,
        video_fps: u32,
        video_bitrate_kbps: u32,
    ) -> Result<Self> {
        let decoder = VideoDecodeWorker::spawn(config, Arc::clone(&direct_output))?;
        Ok(Self::init(
            decoder,
            config,
            direct_output,
            video_fps,
            video_bitrate_kbps,
        ))
    }

    fn init(
        decoder: VideoDecodeWorker,
        _config: DecoderConfig,
        direct_output: Arc<DirectVideoOutput>,
        _video_fps: u32,
        video_bitrate_kbps: u32,
    ) -> Self {
        // Safe-Ceiling REMB: ceiling = the user's bitrate cap (0 = no cap →
        // default). Effective starts AT the cap; only the descent ladder
        // moves it, and only down.
        let remb_cap_bps = if video_bitrate_kbps == 0 {
            REMB_DEFAULT_CAP_BPS
        } else {
            video_bitrate_kbps as f32 * 1000.0
        };
        Self {
            track_id: None,
            receiver_id: None,
            ssrc: None,
            rtp: rtp::VideoRtp::new(),
            decoder,
            latest_frame: None,
            next_frame_id: 0,
            received_packet: false,
            packets: Vec::new(),
            last_stats_report: Instant::now(),
            last_bw_bytes: 0,
            last_remb_at: None,
            last_twcc_at: None,
            last_rr_at: None,
            remb_cap_bps,
            remb_effective_bps: remb_cap_bps,
            remb_nack_window: [0; 5],
            remb_window_idx: 0,
            stats: VideoStats::default(),
            direct_output,
        }
    }

    pub(crate) fn open(
        &mut self,
        track_id: MediaStreamTrackId,
        receiver_id: RTCRtpReceiverId,
        ssrc: u32,
    ) {
        self.track_id = Some(track_id);
        self.receiver_id = Some(receiver_id);
        self.ssrc = Some(ssrc);
    }

    pub(crate) fn handles(&self, track_id: &MediaStreamTrackId) -> bool {
        self.track_id.as_ref() == Some(track_id)
    }

    pub(crate) fn receive(
        &mut self,
        _peer: &mut RTCPeerConnection,
        packet: Packet,
        keyframe_requested: &mut bool,
    ) {
        self.received_packet = true;
        let sample_stats = self.rtp.receive(&self.decoder, packet, keyframe_requested);
        if !sample_stats.nack_requests.is_empty() {
            crate::streaming::video::metrics::METRICS
                .rtp_gaps
                .fetch_add(sample_stats.nack_requests.len() as u64, Ordering::Relaxed);
            // Gaps are typically the console skipping AUs under load, not network loss.
            // NACKing creates a feedback loop (throttled egress → deeper queue → more skips).
            // Count for diagnostics; do not send NACKs.
        }
        self.stats.dropped = self
            .stats
            .dropped
            .saturating_add(sample_stats.dropped as u64);
        if sample_stats.source_frame_duration_us.is_some() {
            self.stats.last_sample_duration_us = sample_stats.source_frame_duration_us;
        }
        if sample_stats.encoded_resolution.is_some() {
            self.stats.encoded_resolution = sample_stats.encoded_resolution;
        }
        if sample_stats.color.is_some() {
            self.stats.color = sample_stats.color;
        }
    }

    pub(crate) fn drain_decoder(&mut self, keyframe_requested: &mut bool) {
        // Expire grace-held packets on the pump tick so a burst followed by silence
        // (static screen) cannot strand them waiting for a newer packet to arrive.
        {
            let mut expired_stats = rtp_stats_scratch();
            self.rtp
                .flush_expired(&self.decoder, keyframe_requested, &mut expired_stats);
            self.stats.dropped = self
                .stats
                .dropped
                .saturating_add(expired_stats.dropped as u64);
        }
        let mut decode_errors = 0u64;
        while let Some(result) = self
            .decoder
            .latest_result
            .lock()
            .ok()
            .and_then(|mut latest| latest.take())
        {
            match result {
                Ok(frame) => {
                    self.next_frame_id = self.next_frame_id.wrapping_add(1);
                    self.latest_frame = Some((self.next_frame_id, frame));
                }
                Err(error) => {
                    eprintln!("Failed to decode H264 video frame: {error}");
                    decode_errors = decode_errors.saturating_add(1);
                    *keyframe_requested = true;
                }
            }
        }

        if decode_errors > 0 {
            self.stats.decode_errors = self.stats.decode_errors.saturating_add(decode_errors);
            self.decoder.reset_decoder();
            self.rtp.wait_for_keyframe();
        }
    }

    /// Re-baseline the glass-lag measurement (e.g. after a backlog flush) without
    /// touching decoder or keyframe state.
    pub(crate) fn reset_lag_anchor(&mut self) {
        self.rtp.reset_lag_anchor();
    }

    pub(crate) fn request_keyframe(&self, peer: &mut RTCPeerConnection) {        // A PLI needs both identifiers recorded when the remote video track was opened.
        if let (Some(receiver_id), Some(ssrc)) = (self.receiver_id, self.ssrc)
            && let Some(mut receiver) = peer.rtp_receiver(receiver_id)
        {
            let pli = rtcp::payload_feedbacks::picture_loss_indication::PictureLossIndication {
                sender_ssrc: 0,
                media_ssrc: ssrc,
            };
            let _ = receiver.write_rtcp(vec![Box::new(pli)]);
        }
    }

    /// Sends Receiver Estimated Max Bitrate — Safe-Ceiling (2026-11-01): the
    /// payload carries the ladder-fed effective value (starts at the user's
    /// bitrate cap, descends on NACK egress, never rises — see remb_descent).
    /// Still gated OFF (SEND_REMB) until the device A/B; historic note: the old
    /// constant-15M announcement caused the accumulating-latency bug and Chrome
    /// sends ZERO REMB (pcap-verified 2026-10-30). Called every pump (session.rs),
    /// paced internally to REMB_INTERVAL.
    pub(crate) fn send_remb(&mut self, peer: &mut RTCPeerConnection, now: Instant) {
        if !SEND_REMB {
            return;
        }
        let (Some(receiver_id), Some(ssrc)) = (self.receiver_id, self.ssrc) else {
            return;
        };
        if self
            .last_remb_at
            .is_some_and(|sent_at| now.duration_since(sent_at) < REMB_INTERVAL)
        {
            return;
        }
        self.last_remb_at = Some(now);
        if let Some(mut receiver) = peer.rtp_receiver(receiver_id) {
            let remb =
                rtcp::payload_feedbacks::receiver_estimated_maximum_bitrate::ReceiverEstimatedMaximumBitrate {
                    sender_ssrc: LOCAL_RTCP_SENDER_SSRC,
                    // Safe-Ceiling: the ladder-fed effective value (starts at
                    // the user cap, descends on NACK egress, never rises).
                    bitrate: self.remb_effective_bps,
                    ssrcs: vec![ssrc],
                };
            let _ = receiver.write_rtcp(vec![Box::new(remb)]);
        }
    }

    /// 2c (2026-10-30): MANUAL transport-cc feedback — the vendored Recorder
    /// (twcc/, fed from VideoRtp::receive()) is drained here every
    /// TWCC_INTERVAL (60 ms since 2026-11-01; was 100 ms unchanged from the
    /// interceptor era, when Safari's ~17/s ≈ 59 ms was noted but not adopted —
    /// the Chrome pcap cadence adopted for the bad-link A/B).
    /// Built packets carry sender_ssrc/media_ssrc/fb_pkt_count internally
    /// (recorder.rs). Proof-of-send = twcc_sent + rtcp_rtpfb_any at the
    /// socket flush; the console annotates 100% of packets when negotiated
    /// (session 154412: rtp_ext_twcc = 202,571). Flag-gated per A2 — flip
    /// MANUAL_TWCC (rtp.rs) for the on/off A/B. History: the interceptor-era
    /// send_twcc never sent a packet (take_twcc_report -> None, and pre-2a
    /// the offer carried no extmaps at all).
    /// 2026-11-01: 60 ms = Chrome pcap cadence (chrome-dumps/README), was 100 ms.
    pub(crate) fn send_twcc(&mut self, peer: &mut RTCPeerConnection, now: Instant) {
        if !rtp::MANUAL_TWCC.load(Ordering::Relaxed) {
            return;
        }
        let (Some(receiver_id), Some(_ssrc)) = (self.receiver_id, self.ssrc) else {
            return;
        };
        if self
            .last_twcc_at
            .is_some_and(|sent_at| now.duration_since(sent_at) < TWCC_INTERVAL)
        {
            return;
        }
        let packets = self.rtp.take_twcc_feedback();
        if packets.is_empty() {
            return;
        }
        self.last_twcc_at = Some(now);
        if let Some(mut receiver) = peer.rtp_receiver(receiver_id) {
            if let Err(error) = receiver.write_rtcp(packets) {
                eprintln!("twcc write_rtcp failed: {error}");
            }
        }
    }

    /// RR-first probe (31/10): send an RTCP Receiver Report every RR_INTERVAL
    /// (Chrome parity — pcap: ~1.2/s), as an RFC 3550 §6.1 COMPOUND
    /// [RR + SDES CNAME] — probe iteration 2: the bare-RR v1 was non-
    /// compliant and may have been discarded (contaminating the first
    /// SR reading). Report block = RFC A.3 stats from VideoRtp;
    /// LSR/DLSR = 0 while no SR has been received (valid per RFC). Passive sensor: while rtc's stats show zero
    /// inbound SRs, ask get_stats whether an SR arrived (RemoteOutbound
    /// reports_sent is fed ONLY by on_rtcp_sr_received — the crate's SR read
    /// path) and store the count to CSV column `rtcp_sr_count`. Answers the
    /// gating question: does the console SR us once we RR?
    pub(crate) fn send_rr(&mut self, peer: &mut RTCPeerConnection, now: Instant) {
        if !SEND_RR {
            return;
        }
        let (Some(receiver_id), Some(_ssrc)) = (self.receiver_id, self.ssrc) else {
            return;
        };
        if self
            .last_rr_at
            .is_some_and(|sent_at| now.duration_since(sent_at) < RR_INTERVAL)
        {
            return;
        }
        let Some(report) = self.rtp.take_reception_report() else {
            return;
        };
        self.last_rr_at = Some(now);
        if let Some(mut receiver) = peer.rtp_receiver(receiver_id) {
            use rtcp::receiver_report::ReceiverReport;
            use rtcp::source_description::{
                SourceDescription, SourceDescriptionChunk, SourceDescriptionItem, SdesType,
            };
            let rr = ReceiverReport {
                ssrc: LOCAL_RTCP_SENDER_SSRC,
                reports: vec![report],
                ..Default::default()
            };
            // RFC 3550 §6.1: RTCP MUST be compound (SR/RR first, SDES CNAME
            // required). v1 sent a bare RR — non-compliant, may be discarded;
            // iteration 2 sends [RR, SDES].
            let sdes = SourceDescription {
                chunks: vec![SourceDescriptionChunk {
                    source: LOCAL_RTCP_SENDER_SSRC,
                    items: vec![SourceDescriptionItem {
                        sdes_type: SdesType::SdesCname,
                        text: Bytes::from_static(b"green-vita"),
                    }],
                }],
            };
            if let Err(error) = receiver.write_rtcp(vec![Box::new(rr), Box::new(sdes)]) {
                eprintln!("rr write_rtcp failed: {error}");
            }
        }
        // Passive SR sensor (ungated): one get_stats probe per interval until
        // the first SR; after each window reset, one probe refreshes the count.
        if METRICS.rtcp_sr_count.load(Ordering::Relaxed) == 0 {
            use rtc::rtp_transceiver::rtp_sender::RtpCodecKind;
            use rtc::statistics::report::RTCStatsReportEntry;
            use rtc::statistics::stats::RTCStatsType;
            use rtc::statistics::StatsSelector;
            let stats = peer.get_stats(now, StatsSelector::None);
            let mut sr_count = 0u64;
            for entry in stats.iter_by_type(RTCStatsType::RemoteOutboundRTP) {
                if let RTCStatsReportEntry::RemoteOutboundRtp(s) = entry {
                    sr_count = sr_count.max(s.reports_sent);
                }
            }
            if sr_count > 0 {
                METRICS.rtcp_sr_count.store(sr_count, Ordering::Relaxed);
            }
            // S5b-2 discriminator (same get_stats call): did the CONSOLE
            // retransmit? The crate counts inbound RTX before endpoint drops
            // (0.2.5's note). CSV rtx_crate_seen vs rtx_packets_received:
            //   >0 & unwrap=0 => RTX dies INSIDE the fork (bind/rewrite)
            //   both 0        => console never answered our NACKs
            let mut rtx_seen = 0u64;
            for stream in stats.inbound_rtp_streams() {
                if stream.received_rtp_stream_stats.rtp_stream_stats.kind
                    == RtpCodecKind::Video
                {
                    rtx_seen = rtx_seen.max(stream.retransmitted_packets_received);
                }
            }
            if rtx_seen > 0 {
                METRICS.rtx_crate_seen.store(rtx_seen, Ordering::Relaxed);
            }
        }
    }

    /// S5b (A4): hand-rolled NACK — flush the assembler's proven-loss hole
    /// ranges as TransportLayerNack packets while the queue has data. Caps:
    /// ≤64 pending seqs (rtp.rs) + ≤32 seqs/packet; single-shot per AU
    /// (give-up keeps the existing damage/PLI behaviour). NOT the crate's
    /// NackGenerator: it NACKs unclassified gaps, including console AU-skips
    /// (media.rs:127 warning / A4 amendment). Socket truth for the CSV =
    /// transport.rs counts PT=205 FMT=1 on egress (rtcp_nack_sent).
    pub(crate) fn send_nack(&mut self, peer: &mut RTCPeerConnection, _now: Instant) {
        if !SEND_NACK {
            let _ = self.rtp.take_pending_nacks();
            return;
        }
        let (Some(receiver_id), Some(media_ssrc)) = (self.receiver_id, self.ssrc) else {
            return;
        };
        let ranges = self.rtp.take_pending_nacks();
        if ranges.is_empty() {
            return;
        }
        use rtcp::source_description::{
            SourceDescription, SourceDescriptionChunk, SourceDescriptionItem, SdesType,
        };
        use rtcp::transport_feedbacks::transport_layer_nack::TransportLayerNack;
        let nacks = ranges_to_nack_pairs(&ranges, 32);
        if nacks.is_empty() {
            return;
        }
        let nack = TransportLayerNack {
            sender_ssrc: LOCAL_RTCP_SENDER_SSRC,
            media_ssrc,
            nacks,
        };
        // S5b-2: compound [NACK + SDES CNAME] per RFC 3550 §6.1 — the first
        // session sent bare NACKs (2235 out, zero replies). rtcp-rsize in the
        // answer may permit reduced-size, but the RR fix went compound and
        // parity costs one SDES — removes the doubt at the packet level.
        let sdes = SourceDescription {
            chunks: vec![SourceDescriptionChunk {
                source: LOCAL_RTCP_SENDER_SSRC,
                items: vec![SourceDescriptionItem {
                    sdes_type: SdesType::SdesCname,
                    text: Bytes::from_static(b"green-vita"),
                }],
            }],
        };
        if let Some(mut receiver) = peer.rtp_receiver(receiver_id) {
            if let Err(error) = receiver.write_rtcp(vec![Box::new(nack), Box::new(sdes)]) {
                eprintln!("nack write_rtcp failed: {error}");
            }
        }
    }

    pub(crate) fn total_drops(&self) -> u64 {
        self.stats.dropped
    }

    pub(crate) fn status(&mut self, now: Instant) -> Option<String> {
        if now.duration_since(self.last_stats_report) < STREAM_STATS_INTERVAL {
            return None;
        }
        let stats_window = now.duration_since(self.last_stats_report);
        self.last_stats_report = now;

        // Read current metrics for HUD display first (without draining).
        // The actual draining happens later for CSV logging.
        let performance = crate::streaming::video::video_performance_summary();

        // Drain all per-second metrics into a snapshot for CSV logging.
        // Since video_performance_summary() uses load() (not swap), the metrics
        // are still intact and will be properly captured by drain_metrics().
        // Compute q_slope_mpm from the queue-delay estimator before draining.
        let q_slope_mpm = self.rtp.q_estimator.trend_us_per_s() * 60.0 / 1000.0;
        METRICS.q_slope_mpm.store(q_slope_mpm.round() as i64, Ordering::Relaxed);
        // 7b: publish the live Q estimate so HUD/CSV q_us is fed (the drain
        // below swaps it into the snapshot; it was never stored before ⇒ 0).
        METRICS.q_us
            .store(self.rtp.q_estimator.q_us(), Ordering::Relaxed);
        let snapshot = drain_metrics();

        // If render timing metadata is enabled, queue a metadata frame.
        if SEND_RENDER_TIMING_METADATA {
            queue_video_metadata(
                0,
                Instant::now(),
            );
        }

        // Compute source fps and negotiated resolution before building extras.
        let source_fps = self
            .stats
            .last_sample_duration_us
            .filter(|duration| *duration > 0)
            .map(|duration| 1_000_000 / duration)
            .unwrap_or(0);
        let (enc_width, enc_height) = self
            .stats
            .encoded_resolution
            .unwrap_or((STREAM_WIDTH, STREAM_HEIGHT));

        // Build receiver-level extras for the CSV row.
        let extras = crate::streaming::video::metrics_log::RowExtras {
            src_fps: source_fps,
            enc_width,
            enc_height,
            waiting_keyframe: self.rtp.waiting_for_keyframe(),
            drops: self.stats.dropped,
            decode_errors: self.stats.decode_errors,
        };

        // Write the row to the RAM-buffered CSV log (no file I/O during the session).
        if let Ok(mut guard) = crate::streaming::video::metrics::METRICS_CSV_LOG.lock() {
            if let Some(log) = guard.as_mut() {
                let wrote = log.write_row(&snapshot, &extras);
                let _ = wrote;
            }
        }



        // Compute audio queue depth in ms from pending packets.
// RTP-queue proxy: this tracks the inbound RTP packet queue depth in milliseconds,
// derived from pending packet sizes at 48 kHz 16-bit stereo (192000 bytes/sec).
        // 48 kHz, 16-bit stereo: 1s = 192000 bytes.
        let pending_bytes: usize = self.packets.iter().map(|p| p.len()).sum();
        let queue_ms = (pending_bytes.saturating_mul(1000) / 192000) as u64;
        METRICS.audio_queue_ms.store(queue_ms, Ordering::Relaxed);
        // Publish TWCC stride forensics (median sequence gap between annotated
        // packets) into the summary line rendered below.
        crate::streaming::video::metrics::METRICS
            .twcc_stride
            .store(self.rtp.twcc_median_stride() as u64, Ordering::Relaxed);

        // Publish inbound video bandwidth (kbps over the elapsed window) for the
        // Vita-link-headroom investigation.
        {
            let total = self.rtp.total_bytes();
            let kbps = if stats_window.as_secs_f64() > 0.0 {
                ((total.saturating_sub(self.last_bw_bytes)) as f64 * 8.0
                    / stats_window.as_secs_f64()
                    / 1000.0) as u64
            } else {
                0
            };
            crate::streaming::video::metrics::METRICS
                .video_bandwidth_kbps
                .store(kbps, Ordering::Relaxed);
            self.last_bw_bytes = total;
        }

        // Publish TWCC per-interval delay summary (median and max us) once per second.
        // 0 unless the manual-TWCC switch is on: we are the RECEIVER, so we
        // only ever SEND TWCC feedback, and the recorder is fed per packet
        // only behind MANUAL_TWCC (default off). The console sends us no
        // transport-wide feedback. Do not read these columns as network
        // jitter — rx_gap_max_us / rtp_assembly_* / capture_to_arrival_us
        // cover that.
        let (twcc_delay_med_us, twcc_delay_max_us) =
            self.rtp.twcc_recorder.take_delay_summary_us();
        METRICS.twcc_delay_med_us.store(twcc_delay_med_us, Ordering::Relaxed);
        METRICS.twcc_delay_max_us.store(twcc_delay_max_us, Ordering::Relaxed);

        // Safe-Ceiling REMB ladder tick (1 Hz): swap our own egress-NACK
        // accumulator, slide the 5-tick window, descend. Runs even while
        // SEND_REMB is false so flipping the gate measures the real policy
        // (effective starts at cap; controller has no re-growth path).
        {
            let nacks = REMB_NACK_EGRESS.swap(0, Ordering::Relaxed) as u32;
            self.remb_nack_window[self.remb_window_idx] = nacks;
            self.remb_window_idx = (self.remb_window_idx + 1) % self.remb_nack_window.len();
            let rate = self.remb_nack_window.iter().sum::<u32>() as f32
                / self.remb_nack_window.len() as f32;
            self.remb_effective_bps = remb_descent(self.remb_effective_bps, rate);
        }

        // SR-anchored absolute latency is NOT measurable in this fork: the
        // console SRTCP-encrypts the sender-report body (see the census
        // comment in transport.rs), so the NTP / RTP-timestamp anchor inside
        // an SR cannot be read without the crate-internal SRTCP key.
        // `sr_clock_offset_ms` and `sr_anchor_ntp` / `sr_anchor_rtp` therefore
        // stay 0 by construction. What IS locally measurable is how long ago
        // the last SR arrived: a stalled SR flow (age climbing past a few
        // seconds) invalidates any console-clock reasoning and is visible in
        // the CSV. AU-level one-way delay (no clock sync needed) is computed
        // in rtp.rs and lands in capture_to_arrival_us.
        {
            use crate::streaming::video::metrics::METRICS;
            use std::sync::atomic::Ordering;

            // No SR anchor timestamps are available in this fork: SRTCP encrypts
            // the sender-report body, so NTP/RTP-timestamp anchors inside an SR
            // cannot be read without the crate-internal SRTCP key. Only local elapsed
            // time since the last SR arrival is measured via PROCESS_START.
        }

        let encoded_resolution = self
            .stats
            .encoded_resolution
            .map(|(width, height)| format!("{width}x{height}"))
            .unwrap_or_else(|| "?".to_owned());
        let cs_suffix = self.stats.color
            .map(|code| format!(" cs:{}", crate::api::streaming::rtc::rtp::color_matrix_label(code)))
            .unwrap_or_default();
        Some(format!(
            "enc:{encoded_resolution} srcfps:{source_fps} {performance} wait:{} drop:{} err:{}{cs_suffix}",
            u8::from(self.rtp.waiting_for_keyframe()),
            self.stats.dropped,
            self.stats.decode_errors,
        ))
    }
}

fn rtp_stats_scratch() -> crate::api::streaming::rtc::rtp::VideoSampleStats {
    crate::api::streaming::rtc::rtp::VideoSampleStats::default()
}

pub(crate) struct AudioReceiver {
    track_id: Option<MediaStreamTrackId>,
    rtp: rtp::AudioRtp,
    pub(crate) packets: Vec<Bytes>,
}

impl AudioReceiver {
    pub(crate) fn new(sample_rate: u32, payload_type: u8) -> Self {
        Self {
            track_id: None,
            rtp: rtp::AudioRtp::new(sample_rate, payload_type),
            packets: Vec::new(),
        }
    }

    pub(crate) fn open(&mut self, track_id: MediaStreamTrackId) {
        self.track_id = Some(track_id);
    }

    pub(crate) fn handles(&self, track_id: &MediaStreamTrackId) -> bool {
        self.track_id.as_ref() == Some(track_id)
    }

    pub(crate) fn receive(&mut self, packet: Packet) {
        self.rtp.receive(packet, &mut self.packets);
    }
}

#[cfg(test)]
mod remb_ladder_tests {
    use super::{REMB_FLOOR_BPS, remb_descent};

    #[test]
    fn clean_link_holds_at_current_value() {
        // Measured clean-link NACK egress ≈ 0.18/s — well under GENTLE.
        assert_eq!(remb_descent(5_000_000.0, 0.18), 5_000_000.0);
        assert_eq!(remb_descent(5_000_000.0, 0.0), 5_000_000.0);
    }

    #[test]
    fn gentle_band_steps_down_ten_percent() {
        let out = remb_descent(5_000_000.0, 1.5);
        assert!((out - 4_500_000.0).abs() < 1.0);
    }

    #[test]
    fn deep_band_steps_down_twenty_five_percent() {
        // Measured congested runs: 2–5.6/s — 5.0/s is deep territory.
        let out = remb_descent(5_000_000.0, 5.0);
        assert!((out - 3_750_000.0).abs() < 1.0);
    }

    #[test]
    fn descent_never_rises_and_never_passes_the_floor() {
        // Already-floored value stays floored regardless of rate.
        assert_eq!(remb_descent(REMB_FLOOR_BPS, 9.0), REMB_FLOOR_BPS);
        // A deep rate from just above the floor clamps AT the floor.
        assert_eq!(remb_descent(1_200_000.0, 9.0), REMB_FLOOR_BPS);
        // Monotonic: output never exceeds the input at any rate.
        for rate in [0.0, 0.5, 1.0, 2.0, 4.0, 8.0, 100.0] {
            assert!(remb_descent(5_000_000.0, rate) <= 5_000_000.0);
        }
    }
}