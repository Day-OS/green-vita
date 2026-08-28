use crate::streaming::audio::{AudioPacket, RED_PAYLOAD_TYPE};
use crate::streaming::video::{STREAM_HEIGHT, STREAM_WIDTH, VideoDecodeWorker};
use h264_reader::annexb::AnnexBReader;
use h264_reader::nal::sps::SeqParameterSet;
use h264_reader::nal::{Nal, RefNal, UnitType};
use h264_reader::push::NalInterest;
use rtc::rtp::Packet;
use rtc::rtp::codec::h264::H264Packet;
use rtc::rtp::packetizer::Depacketizer;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

const MAX_H264_ACCESS_UNIT_BYTES: usize = 2 * 1024 * 1024;
const VIDEO_RTP_CLOCK_RATE: u32 = 90_000;
const LOW_FPS_DAMAGE_LIMIT: u8 = 8;
const HIGH_FPS_DAMAGE_LIMIT: u8 = 3;
/// Cut down from 2-12 ms after on-hardware testing showed real loss, not mere reordering,
/// dominates on this network: the wait was neither rescuing packets often enough to earn its
/// keep nor avoiding keyframe requests, while adding measurable input delay. Kept nonzero
/// (rather than removing the hold path outright) only to still catch packets that are already
/// sitting in the same batch of reads - anything genuinely late is back to being treated as lost
/// almost immediately, close to the pre-reorder-hold behavior.
const MIN_REORDER_GRACE: Duration = Duration::from_micros(100);
const MAX_REORDER_GRACE: Duration = Duration::from_micros(1_000);

pub(super) struct AudioRtp {
    payload_type: u8,
}

impl AudioRtp {
    pub(super) fn new(_sample_rate: u32, payload_type: u8) -> Self {
        Self { payload_type }
    }

    /// Hands the raw RTP payload straight to the jitter buffer's decode worker - Opus (and the
    /// RED envelope it might be wrapped in) is never fragmented across RTP packets, so there is
    /// nothing to reassemble here, only sequencing to do further down. Submitted directly from
    /// this RTP-receiving call, not batched for the shell's render loop to pick up later: the
    /// jitter buffer's reordering window is thin enough that render-frame-cadence delay was
    /// enough on its own to make merely-late packets look lost.
    pub(super) fn receive(&mut self, packet: Packet) {
        let payload_type = packet.header.payload_type;
        if payload_type != self.payload_type && payload_type != RED_PAYLOAD_TYPE {
            return;
        }
        crate::streaming::audio::submit_packet(AudioPacket {
            payload: packet.payload,
            sequence: packet.header.sequence_number,
            payload_type,
        });
    }
}

#[derive(Default)]
pub(super) struct VideoSampleStats {
    pub dropped: u32,
    pub source_frame_duration_us: Option<u64>,
    pub encoded_resolution: Option<(u32, u32)>,
    pub jitter_ms: f32,
    pub reorder_rescued: u32,
    pub reorder_expired: u32,
}

/// The subset of `VideoDecodeWorker` the RTP layer needs - kept as a trait so the reorder-hold
/// and grace-expiry logic below can be driven directly by tests instead of a real decoder.
pub(super) trait VideoAuSink {
    fn submit_access_unit(&self, data: Vec<u8>, source_frame_duration_us: Option<u64>) -> bool;
    fn begin_resync(&self);
}

impl VideoAuSink for VideoDecodeWorker {
    fn submit_access_unit(&self, data: Vec<u8>, source_frame_duration_us: Option<u64>) -> bool {
        VideoDecodeWorker::submit_access_unit(self, data, source_frame_duration_us)
    }

    fn begin_resync(&self) {
        VideoDecodeWorker::begin_resync(self);
    }
}

/// RFC 3550's jitter estimator: a running estimate of how much the gap between consecutive
/// packets' arrival times differs from the gap the RTP timestamps say it should be.
pub(super) struct JitterEstimator {
    jitter_us: f64,
    last_arrival: Option<Instant>,
    last_rtp_timestamp: Option<u32>,
}

impl Default for JitterEstimator {
    fn default() -> Self {
        Self {
            jitter_us: 0.0,
            last_arrival: None,
            last_rtp_timestamp: None,
        }
    }
}

impl JitterEstimator {
    fn update(&mut self, arrival: Instant, rtp_timestamp: u32) -> f32 {
        if let (Some(last_arrival), Some(last_rtp)) = (self.last_arrival, self.last_rtp_timestamp)
        {
            let arrival_diff_us = arrival.saturating_duration_since(last_arrival).as_micros() as f64;
            let rtp_diff_us = (rtp_timestamp.wrapping_sub(last_rtp) as f64 * 1_000_000.0)
                / f64::from(VIDEO_RTP_CLOCK_RATE);
            let transit_diff_us = (arrival_diff_us - rtp_diff_us).abs();
            // RFC 3550 EWMA smoother: J = J + (|D| - J) / 16
            self.jitter_us += (transit_diff_us - self.jitter_us) / 16.0;
        }
        self.last_arrival = Some(arrival);
        self.last_rtp_timestamp = Some(rtp_timestamp);
        (self.jitter_us / 1000.0) as f32
    }

    fn current_jitter_ms(&self) -> f32 {
        (self.jitter_us / 1000.0) as f32
    }
}

/// Twice the measured jitter, clamped to a deliberately tiny range - see the comment on
/// `MIN_REORDER_GRACE`/`MAX_REORDER_GRACE` for why this is no longer sized to ride out ordinary
/// reordering.
pub(super) fn reorder_grace_from_jitter(jitter_ms: f32) -> Duration {
    let twice_jitter = Duration::from_micros((jitter_ms.max(0.0) * 2.0 * 1000.0) as u64);
    twice_jitter.clamp(MIN_REORDER_GRACE, MAX_REORDER_GRACE)
}

pub(super) struct VideoRtp {
    depacketizer: H264Packet,
    /// The frame currently being assembled from in-order packets.
    pending: Option<PendingVideoFrame>,
    /// An older, still-incomplete frame held back in case its missing packet is only late, not
    /// lost - released or given up on within `reorder_hold_grace` of `reorder_hold_started_at`.
    reorder_hold: Option<PendingVideoFrame>,
    reorder_hold_started_at: Instant,
    reorder_hold_grace: Duration,
    reorder_hold_expected_sequence: Option<u16>,
    /// A newer frame that finished assembling while `reorder_hold` was still pending - queued
    /// rather than submitted immediately so frames reach the decoder in timestamp order.
    parked_au: Option<AssembledVideoFrame>,
    assemble_order: Vec<usize>,
    assemble_buf: Vec<u8>,
    next_sequence: Option<u16>,
    last_frame_timestamp: Option<u32>,
    source_frame_duration_us: Option<u64>,
    damage_score: u8,
    stream_too_large: bool,
    waiting_for_keyframe: bool,
    jitter_estimator: JitterEstimator,
}

struct PendingVideoFrame {
    timestamp: u32,
    first_packet_at: Instant,
    packets: Vec<Packet>,
}

struct AssembledVideoFrame {
    data: Vec<u8>,
    timestamp: u32,
    marker_sequence: u16,
    first_packet_at: Instant,
}

enum FrameAssembly {
    Pending,
    Complete { data: Vec<u8>, marker_sequence: u16 },
    Invalid,
}

impl PendingVideoFrame {
    fn new(packet: Packet) -> Self {
        Self {
            timestamp: packet.header.timestamp,
            first_packet_at: Instant::now(),
            packets: vec![packet],
        }
    }

    fn insert(&mut self, packet: Packet) {
        if !self
            .packets
            .iter()
            .any(|existing| existing.header.sequence_number == packet.header.sequence_number)
        {
            self.packets.push(packet);
        }
    }

    fn marker_sequence(&self) -> Option<u16> {
        self.packets
            .iter()
            .find(|packet| packet.header.marker)
            .map(|packet| packet.header.sequence_number)
    }

    fn assemble(
        &self,
        depacketizer: &mut H264Packet,
        expected_sequence: Option<u16>,
        order: &mut Vec<usize>,
        buf: &mut Vec<u8>,
    ) -> FrameAssembly {
        let Some(marker_sequence) = self.marker_sequence() else {
            return FrameAssembly::Pending;
        };

        order.clear();
        if self.packets.len() == 1 {
            order.push(0);
        } else {
            order.extend(0..self.packets.len());
            order.sort_unstable_by_key(|&index| {
                std::cmp::Reverse(
                    marker_sequence.wrapping_sub(self.packets[index].header.sequence_number),
                )
            });
        }

        let Some(&first_index) = order.first() else {
            return FrameAssembly::Pending;
        };
        let first = &self.packets[first_index];
        if expected_sequence.is_some_and(|expected| first.header.sequence_number != expected)
            || !depacketizer.is_partition_head(&first.payload)
        {
            return FrameAssembly::Pending;
        }
        if order.windows(2).any(|pair| {
            self.packets[pair[1]].header.sequence_number
                != self.packets[pair[0]]
                    .header
                    .sequence_number
                    .wrapping_add(1)
        }) {
            return FrameAssembly::Pending;
        }

        *depacketizer = H264Packet::default();
        let payload_bytes: usize = order
            .iter()
            .map(|&index| self.packets[index].payload.len())
            .sum();
        buf.clear();
        buf.reserve(payload_bytes);
        for &index in order.iter() {
            let Ok(nalu) = depacketizer.depacketize(&self.packets[index].payload) else {
                *depacketizer = H264Packet::default();
                buf.clear();
                return FrameAssembly::Invalid;
            };
            buf.extend_from_slice(&nalu);
            if buf.len() > MAX_H264_ACCESS_UNIT_BYTES {
                *depacketizer = H264Packet::default();
                buf.clear();
                return FrameAssembly::Invalid;
            }
        }
        *depacketizer = H264Packet::default();
        FrameAssembly::Complete {
            data: std::mem::take(buf),
            marker_sequence,
        }
    }
}

impl VideoRtp {
    pub(super) fn new() -> Self {
        Self {
            depacketizer: H264Packet::default(),
            pending: None,
            reorder_hold: None,
            reorder_hold_started_at: Instant::now(),
            reorder_hold_grace: MIN_REORDER_GRACE,
            reorder_hold_expected_sequence: None,
            parked_au: None,
            assemble_order: Vec::new(),
            assemble_buf: Vec::new(),
            next_sequence: None,
            last_frame_timestamp: None,
            source_frame_duration_us: None,
            damage_score: 0,
            stream_too_large: false,
            waiting_for_keyframe: false,
            jitter_estimator: JitterEstimator::default(),
        }
    }

    pub(super) fn waiting_for_keyframe(&self) -> bool {
        self.waiting_for_keyframe
    }

    pub(super) fn wait_for_keyframe(&mut self) {
        self.waiting_for_keyframe = true;
    }

    pub(super) fn current_jitter_ms(&self) -> f32 {
        self.jitter_estimator.current_jitter_ms()
    }

    /// When the reorder hold's grace period expires, if it hasn't already.
    pub(super) fn reorder_deadline(&self) -> Option<Instant> {
        self.reorder_hold
            .as_ref()
            .map(|_| self.reorder_hold_started_at + self.reorder_hold_grace)
    }

    pub(super) fn receive(
        &mut self,
        worker: &VideoDecodeWorker,
        packet: Packet,
        keyframe_requested: &mut bool,
        arrival: Instant,
    ) -> VideoSampleStats {
        self.receive_into(worker, packet, keyframe_requested, arrival)
    }

    fn receive_into<S: VideoAuSink>(
        &mut self,
        sink: &S,
        packet: Packet,
        keyframe_requested: &mut bool,
        arrival: Instant,
    ) -> VideoSampleStats {
        let mut stats = VideoSampleStats::default();
        stats.jitter_ms = self.jitter_estimator.update(arrival, packet.header.timestamp);
        self.expire_reorder_grace(sink, keyframe_requested, arrival, &mut stats);

        if packet.payload.is_empty() {
            if self.next_sequence == Some(packet.header.sequence_number) {
                self.next_sequence = Some(packet.header.sequence_number.wrapping_add(1));
            }
            return stats;
        }

        let packet_timestamp = packet.header.timestamp;

        // The reordered/late packet the hold is waiting on: insert and try to complete it.
        if self
            .reorder_hold
            .as_ref()
            .is_some_and(|hold| hold.timestamp == packet_timestamp)
        {
            if let Some(hold) = &mut self.reorder_hold {
                hold.insert(packet);
            }
            self.try_complete_reorder_hold(sink, keyframe_requested, &mut stats);
            return stats;
        }

        // A packet for some other, newer frame arrived while a hold (or its parked successor) is
        // outstanding - the hold isn't getting rescued by this packet, so let it run its grace
        // period out now rather than wait for the timer.
        let belongs_to_pending = self
            .pending
            .as_ref()
            .is_some_and(|pending| pending.timestamp == packet_timestamp);
        if !belongs_to_pending && (self.reorder_hold.is_some() || self.parked_au.is_some()) {
            let reference_ts = self
                .pending
                .as_ref()
                .map(|pending| pending.timestamp)
                .or_else(|| self.parked_au.as_ref().map(|ready| ready.timestamp))
                .or_else(|| self.reorder_hold.as_ref().map(|hold| hold.timestamp));
            if reference_ts.is_some_and(|ts| !timestamp_is_newer(packet_timestamp, ts)) {
                return stats;
            }
            if self.reorder_hold.is_some() {
                self.reorder_hold_started_at =
                    arrival.checked_sub(self.reorder_hold_grace).unwrap_or(arrival);
                self.expire_reorder_grace(sink, keyframe_requested, arrival, &mut stats);
            } else if let Some(ready) = self.parked_au.take() {
                self.process_assembled(ready, sink, keyframe_requested, &mut stats, false);
            }
        }

        if let Some(pending) = &self.pending
            && pending.timestamp != packet_timestamp
        {
            if !timestamp_is_newer(packet_timestamp, pending.timestamp) {
                return stats;
            }
            if let Some(incomplete) = self.pending.take() {
                let grace = reorder_grace_from_jitter(self.jitter_estimator.current_jitter_ms());
                self.reorder_hold_expected_sequence = self.next_sequence;
                self.reorder_hold_grace = grace;
                self.reorder_hold_started_at = arrival;
                self.next_sequence = Some(packet.header.sequence_number);
                self.reorder_hold = Some(incomplete);
            }
        }

        if self.pending.is_none() {
            if self
                .last_frame_timestamp
                .is_some_and(|last| !timestamp_is_newer(packet_timestamp, last))
            {
                return stats;
            }
            self.pending = Some(PendingVideoFrame::new(packet));
        } else if let Some(pending) = &mut self.pending {
            pending.insert(packet);
        }

        let Some(pending) = self.pending.take() else {
            return stats;
        };
        let assembly = pending.assemble(
            &mut self.depacketizer,
            self.next_sequence,
            &mut self.assemble_order,
            &mut self.assemble_buf,
        );
        let (data, marker_sequence) = match assembly {
            FrameAssembly::Pending => {
                self.pending = Some(pending);
                return stats;
            }
            FrameAssembly::Invalid => {
                self.next_sequence = None;
                *keyframe_requested = true;
                self.record_damage(sink);
                stats.dropped = stats.dropped.saturating_add(1);
                return stats;
            }
            FrameAssembly::Complete {
                data,
                marker_sequence,
            } => (data, marker_sequence),
        };

        if self.reorder_hold.is_some() {
            // Don't let this newer frame overtake the one still waiting on its hold - park it
            // until the hold resolves, one way or another.
            self.parked_au = Some(AssembledVideoFrame {
                data,
                timestamp: pending.timestamp,
                marker_sequence,
                first_packet_at: pending.first_packet_at,
            });
            return stats;
        }

        self.process_assembled(
            AssembledVideoFrame {
                data,
                timestamp: pending.timestamp,
                marker_sequence,
                first_packet_at: pending.first_packet_at,
            },
            sink,
            keyframe_requested,
            &mut stats,
            false,
        );
        stats
    }

    /// Checked once per RTC pump tick (not just on packet arrival), since the grace period has to
    /// expire even if no more packets ever show up.
    pub(super) fn expire_reorder_grace_if_due<S: VideoAuSink>(
        &mut self,
        sink: &S,
        keyframe_requested: &mut bool,
        now: Instant,
    ) -> VideoSampleStats {
        let mut stats = VideoSampleStats::default();
        self.expire_reorder_grace(sink, keyframe_requested, now, &mut stats);
        stats
    }

    fn expire_reorder_grace<S: VideoAuSink>(
        &mut self,
        sink: &S,
        keyframe_requested: &mut bool,
        now: Instant,
        stats: &mut VideoSampleStats,
    ) {
        if self.reorder_hold.is_none()
            || now.saturating_duration_since(self.reorder_hold_started_at) < self.reorder_hold_grace
        {
            return;
        }
        let Some(_hold) = self.reorder_hold.take() else {
            return;
        };
        stats.reorder_expired = stats.reorder_expired.saturating_add(1);
        stats.dropped = stats.dropped.saturating_add(1);
        *keyframe_requested = true;
        self.record_damage(sink);
        self.reorder_hold_expected_sequence = None;
        self.depacketizer = H264Packet::default();
        if let Some(ready) = self.parked_au.take() {
            // The drop just recorded above must not be immediately canceled out by the decrement
            // a successfully-parked frame would otherwise earn - see `process_assembled`'s
            // `just_damaged` parameter. Without this, sustained real loss (each drop paired with
            // its already-assembled successor flushing right behind it) never accumulates enough
            // damage to trigger a full keyframe wait, since every +1 is immediately undone by a -1.
            self.process_assembled(ready, sink, keyframe_requested, stats, true);
        }
    }

    fn try_complete_reorder_hold<S: VideoAuSink>(
        &mut self,
        sink: &S,
        keyframe_requested: &mut bool,
        stats: &mut VideoSampleStats,
    ) {
        let Some(hold) = self.reorder_hold.take() else {
            return;
        };
        let assembly = hold.assemble(
            &mut self.depacketizer,
            self.reorder_hold_expected_sequence,
            &mut self.assemble_order,
            &mut self.assemble_buf,
        );
        let FrameAssembly::Complete {
            data,
            marker_sequence,
        } = assembly
        else {
            self.reorder_hold = Some(hold);
            return;
        };
        self.reorder_hold_expected_sequence = None;
        self.depacketizer = H264Packet::default();
        stats.reorder_rescued = stats.reorder_rescued.saturating_add(1);
        self.process_assembled(
            AssembledVideoFrame {
                data,
                timestamp: hold.timestamp,
                marker_sequence,
                first_packet_at: hold.first_packet_at,
            },
            sink,
            keyframe_requested,
            stats,
            false,
        );
        if let Some(ready) = self.parked_au.take() {
            self.process_assembled(ready, sink, keyframe_requested, stats, false);
        }
    }

    /// `just_damaged` is true only when this frame is being flushed immediately after
    /// `record_damage` was called for a drop in the very same event (see `expire_reorder_grace`):
    /// without it, that drop's increment would be undone by this call's own decrement before
    /// `damage_score` could ever accumulate across repeated real loss.
    fn process_assembled<S: VideoAuSink>(
        &mut self,
        assembled: AssembledVideoFrame,
        sink: &S,
        keyframe_requested: &mut bool,
        stats: &mut VideoSampleStats,
        just_damaged: bool,
    ) {
        let AssembledVideoFrame {
            data,
            timestamp,
            marker_sequence,
            first_packet_at,
        } = assembled;
        self.next_sequence = Some(marker_sequence.wrapping_add(1));

        // Record both average and worst-case RTP assembly time for the stream HUD.
        let assembly_us = first_packet_at.elapsed().as_micros() as u64;
        crate::streaming::video::metrics::METRICS
            .rtp_assembly_sum_us
            .fetch_add(assembly_us, Ordering::Relaxed);
        crate::streaming::video::metrics::METRICS
            .rtp_assembly_count
            .fetch_add(1, Ordering::Relaxed);
        crate::streaming::video::metrics::METRICS
            .rtp_assembly_max_us
            .fetch_max(assembly_us, Ordering::Relaxed);

        stats.source_frame_duration_us = self.last_frame_timestamp.map(|previous| {
            u64::from(timestamp.wrapping_sub(previous)) * 1_000_000 / u64::from(VIDEO_RTP_CLOCK_RATE)
        });
        if let Some(duration) = stats.source_frame_duration_us {
            self.source_frame_duration_us = Some(
                self.source_frame_duration_us
                    .map(|average| (average * 7 + duration) / 8)
                    .unwrap_or(duration),
            );
        }
        self.last_frame_timestamp = Some(timestamp);

        let unit = inspect_h264_access_unit(&data);
        stats.encoded_resolution = unit.resolution;
        let sample_too_large = unit
            .resolution
            .is_some_and(|(width, height)| width > STREAM_WIDTH || height > STREAM_HEIGHT);
        if sample_too_large {
            eprintln!(
                "Dropping H264 access unit larger than decoder: {:?} > {}x{}",
                unit.resolution, STREAM_WIDTH, STREAM_HEIGHT
            );
            self.stream_too_large = true;
            // Flush queued decoder work once, then wait for a compatible IDR instead of feeding
            // frames that the Vita hardware cannot decode.
            *keyframe_requested = true;
            if !self.waiting_for_keyframe {
                sink.begin_resync();
            }
            self.waiting_for_keyframe = true;
            stats.dropped = stats.dropped.saturating_add(1);
            return;
        }
        if self.stream_too_large {
            if unit.resolution.is_none() || !unit.has_idr {
                *keyframe_requested = true;
                self.waiting_for_keyframe = true;
                stats.dropped = stats.dropped.saturating_add(1);
                return;
            }
            self.stream_too_large = false;
        }
        if self.waiting_for_keyframe {
            // Later keyframes may contain only IDR; AVCDEC retains SPS/PPS across resyncs.
            if !unit.has_idr {
                *keyframe_requested = true;
                stats.dropped = stats.dropped.saturating_add(1);
                return;
            }
            self.waiting_for_keyframe = false;
            self.damage_score = 0;
        } else if !just_damaged {
            self.damage_score = self.damage_score.saturating_sub(1);
        }

        if !sink.submit_access_unit(data, self.source_frame_duration_us) {
            eprintln!("Video decoder queue is full; continuing while requesting a keyframe");
            *keyframe_requested = true;
            stats.dropped = stats.dropped.saturating_add(1);
        }
    }

    fn record_damage<S: VideoAuSink>(&mut self, sink: &S) {
        if self.waiting_for_keyframe {
            return;
        }

        self.damage_score = self.damage_score.saturating_add(1);
        let source_fps = self
            .source_frame_duration_us
            .filter(|duration| *duration > 0)
            .map(|duration| 1_000_000 / duration)
            .unwrap_or(30);
        let damage_limit = if source_fps <= 30 {
            LOW_FPS_DAMAGE_LIMIT
        } else if source_fps >= 60 {
            HIGH_FPS_DAMAGE_LIMIT
        } else {
            LOW_FPS_DAMAGE_LIMIT
                - (((source_fps - 30) * u64::from(LOW_FPS_DAMAGE_LIMIT - HIGH_FPS_DAMAGE_LIMIT)
                    + 29)
                    / 30) as u8
        };
        if self.damage_score < damage_limit {
            return;
        }

        sink.begin_resync();
        self.waiting_for_keyframe = true;
        self.damage_score = 0;
    }
}

fn timestamp_is_newer(candidate: u32, reference: u32) -> bool {
    let distance = candidate.wrapping_sub(reference);
    distance != 0 && distance < (1 << 31)
}

struct AccessUnitInfo {
    has_idr: bool,
    resolution: Option<(u32, u32)>,
}

fn inspect_h264_access_unit(data: &[u8]) -> AccessUnitInfo {
    let mut info = AccessUnitInfo {
        has_idr: false,
        resolution: None,
    };
    let mut reader = AnnexBReader::accumulate(|nal: RefNal<'_>| {
        let Ok(header) = nal.header() else {
            return NalInterest::Ignore;
        };
        match header.nal_unit_type() {
            UnitType::SliceLayerWithoutPartitioningIdr => {
                info.has_idr = true;
                NalInterest::Ignore
            }
            UnitType::SeqParameterSet => {
                if nal.is_complete() {
                    info.resolution = SeqParameterSet::from_bits(nal.rbsp_bits())
                        .and_then(|sps| sps.pixel_dimensions())
                        .ok();
                }
                NalInterest::Buffer
            }
            _ => NalInterest::Ignore,
        }
    });
    reader.push(data);
    reader.reset();
    info
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use std::cell::RefCell;
    use std::sync::atomic::AtomicU64;

    struct FakeSink {
        submitted: RefCell<Vec<Vec<u8>>>,
        resyncs: AtomicU64,
        accept: RefCell<bool>,
    }

    impl FakeSink {
        fn new() -> Self {
            Self {
                submitted: RefCell::new(Vec::new()),
                resyncs: AtomicU64::new(0),
                accept: RefCell::new(true),
            }
        }
    }

    impl VideoAuSink for FakeSink {
        fn submit_access_unit(&self, data: Vec<u8>, _source_frame_duration_us: Option<u64>) -> bool {
            if !*self.accept.borrow() {
                return false;
            }
            self.submitted.borrow_mut().push(data);
            true
        }

        fn begin_resync(&self) {
            self.resyncs.fetch_add(1, Ordering::Relaxed);
        }
    }

    fn single_nal_payload() -> Bytes {
        Bytes::from_static(&[0x41, 0x9a, 0x00])
    }

    fn pkt(seq: u16, ts: u32, marker: bool) -> Packet {
        Packet {
            header: rtc::rtp::Header {
                version: 2,
                padding: false,
                extension: false,
                marker,
                payload_type: 96,
                sequence_number: seq,
                timestamp: ts,
                ssrc: 1,
                csrc: Vec::new(),
                extension_profile: 0,
                extensions: Vec::new(),
                extensions_padding: 0,
            },
            payload: single_nal_payload(),
        }
    }

    #[test]
    fn reorder_grace_clamps_to_a_tiny_range() {
        assert_eq!(reorder_grace_from_jitter(0.0), MIN_REORDER_GRACE);
        assert_eq!(reorder_grace_from_jitter(0.02), MIN_REORDER_GRACE);
        assert_eq!(reorder_grace_from_jitter(0.2), Duration::from_micros(400));
        assert_eq!(reorder_grace_from_jitter(100.0), MAX_REORDER_GRACE);
    }

    #[test]
    fn reorder_hold_rescues_late_packet_before_deadline() {
        let sink = FakeSink::new();
        let mut rtp = VideoRtp::new();
        let mut pli = false;
        let base = Instant::now();

        rtp.receive_into(&sink, pkt(1, 90000, false), &mut pli, base);
        rtp.receive_into(
            &sink,
            pkt(3, 180000, true),
            &mut pli,
            base + Duration::from_micros(500),
        );
        assert!(rtp.reorder_hold.is_some());
        assert!(rtp.reorder_deadline().is_some());
        assert_eq!(sink.submitted.borrow().len(), 0);

        let stats = rtp.receive_into(
            &sink,
            pkt(2, 90000, true),
            &mut pli,
            base + Duration::from_micros(1_000),
        );
        assert_eq!(stats.reorder_rescued, 1);
        assert!(rtp.reorder_hold.is_none());
        assert_eq!(sink.submitted.borrow().len(), 2);
    }

    #[test]
    fn reorder_hold_expires_after_grace_and_flushes_ready() {
        let sink = FakeSink::new();
        let mut rtp = VideoRtp::new();
        let mut pli = false;
        let base = Instant::now();

        rtp.receive_into(&sink, pkt(1, 90000, false), &mut pli, base);
        rtp.receive_into(
            &sink,
            pkt(3, 180000, true),
            &mut pli,
            base + Duration::from_micros(200),
        );
        assert!(rtp.parked_au.is_some());

        let deadline = rtp.reorder_deadline().expect("reorder hold active");
        let stats = rtp.expire_reorder_grace_if_due(&sink, &mut pli, deadline);
        assert_eq!(stats.reorder_expired, 1);
        assert!(rtp.reorder_hold.is_none());
        assert_eq!(sink.submitted.borrow().len(), 1);
        assert!(pli);
    }

    #[test]
    fn third_timestamp_force_expires_before_parked_overwrite() {
        let sink = FakeSink::new();
        let mut rtp = VideoRtp::new();
        let mut pli = false;
        let base = Instant::now();

        rtp.receive_into(&sink, pkt(1, 90000, false), &mut pli, base);
        rtp.receive_into(
            &sink,
            pkt(3, 180000, true),
            &mut pli,
            base + Duration::from_micros(200),
        );
        assert!(rtp.reorder_hold.is_some());
        assert!(rtp.parked_au.is_some());
        assert!(rtp.pending.is_none());
        let parked_ts_before = rtp.parked_au.as_ref().unwrap().timestamp;
        assert_eq!(parked_ts_before, 180000);

        rtp.receive_into(
            &sink,
            pkt(5, 270000, false),
            &mut pli,
            base + Duration::from_micros(400),
        );
        assert!(
            sink.submitted.borrow().iter().any(|au| !au.is_empty()),
            "parked AU B must be flushed on third-TS force expire"
        );
        assert!(
            rtp.parked_au
                .as_ref()
                .is_none_or(|parked| parked.timestamp != parked_ts_before),
            "parked B must not remain after third timestamp"
        );
    }

    #[test]
    fn queue_full_does_not_request_keyframe() {
        let sink = FakeSink::new();
        *sink.accept.borrow_mut() = false;
        let mut rtp = VideoRtp::new();
        let mut pli = false;
        let stats = rtp.receive_into(&sink, pkt(1, 90000, true), &mut pli, Instant::now());
        assert_eq!(stats.dropped, 1);
        assert!(!pli);
    }
}
