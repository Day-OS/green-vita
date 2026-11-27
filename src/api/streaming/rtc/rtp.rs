use crate::streaming::video::{self, STREAM_HEIGHT, STREAM_WIDTH, VideoDecodeWorker, metrics::METRICS};
use crate::api::streaming::rtc::media::LOCAL_RTCP_SENDER_SSRC;
use crate::api::streaming::rtc::reception_stats::ReceptionStats;
use crate::api::streaming::rtc::twcc::recorder::Recorder;
use crate::pipeline::clock::queue_delay::QueueDelayEstimator;
use crate::pipeline::clock::send_time::SendTimeUnwrapper;
use crate::pipeline::media::assembler::{AccessUnitAssembler, AssemblyOutcome};
use std::collections::VecDeque;
use std::sync::LazyLock;
use std::sync::Mutex;
use bytes::Bytes;
use h264_reader::annexb::AnnexBReader;
use h264_reader::nal::sps::SeqParameterSet;
use h264_reader::nal::{Nal, RefNal, UnitType};
use h264_reader::push::NalInterest;
use rtc::rtp::Packet;
// 8b (§6.3): test-only — the differential-fuzz oracle's depacketizer.
#[cfg(test)]
use rtc::rtp::codec::h264::H264Packet;
use rtc::rtp::codec::opus::OpusPacket;
// 8b (§6.3): test-only — `depacketize()`'s trait scope for the cfg(test)
// legacy oracle's assemble(); no production caller remains.
#[cfg(test)]
use rtc::rtp::packetizer::Depacketizer;
use rtc_media::io::sample_builder::SampleBuilder;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU8, AtomicU64, Ordering};
use std::time::Instant;
#[cfg(test)]
use std::time::Duration;

const MAX_PENDING_AUDIO_PACKETS: usize = 32;
// 8b (§6.3): only the cfg(test) legacy oracle's assemble() reads this now —
// the production size gate lives in the assembler (MAX_AU_BYTES, same 2 MiB).
#[cfg(test)]
const MAX_H264_ACCESS_UNIT_BYTES: usize = 2 * 1024 * 1024;
const VIDEO_RTP_CLOCK_RATE: u32 = 90_000;
const AUDIO_MAX_LATE_PACKETS: u16 = 32;
const LOW_FPS_DAMAGE_LIMIT: u8 = 8;
const HIGH_FPS_DAMAGE_LIMIT: u8 = 3;

/// 2c (2026-10-30): manual TWCC feedback master switch (A2 method — one
/// lever, flag-gated). Gates BOTH the per-packet record in receive() and
/// the 60 ms send in media::send_twcc; false = 8385cbc behaviour (silent
/// send, offer untouched — the console keeps annotating either way).
/// Shelved 2026-10-31 after the A/B (943227e regressed hard: 17 resyncs /
/// bw med 1195 vs 8385cbc's 1 / 5094; encoding proven valid 2544980 — the
/// console's delay control clamps this 2.4 GHz link). Revisit only with
/// Chrome's cadence numbers (pcap: ~60 ms — matched 2026-11-01) and a reason.
/// 2026-11-01: interval 100→60 ms to match Chrome's measured 14.4 TWCC/s cadence (chrome-dumps/README); bad-link A/B (metrics-b4f27d5-twcc60-*): 3.2× less frame damage, but regressions on good links — default off.
/// Runtime-switchable (settings manual_twcc_60ms → set_manual_twcc): read
/// per packet with Relaxed, applies live mid-stream without reconnecting.
pub(crate) static MANUAL_TWCC: AtomicBool = AtomicBool::new(false);

/// Hidden/compile-time switch (no settings row): store the manual-TWCC master
/// switch. Read from `Settings::manual_twcc_60ms` at stream start.
pub(crate) fn set_manual_twcc(enabled: bool) {
    MANUAL_TWCC.store(enabled, Ordering::Relaxed);
}

#[derive(Default)]
pub(crate) struct VideoSampleStats {
    pub dropped: u32,
    pub source_frame_duration_us: Option<u64>,
    pub encoded_resolution: Option<(u32, u32)>,
    pub color: Option<u8>,
    pub nack_requests: Vec<(u16, u16)>,
}

pub(super) struct AudioRtp {
    samples: SampleBuilder<OpusPacket>,
    payload_type: u8,
}

impl AudioRtp {
    pub(super) fn new(sample_rate: u32, payload_type: u8) -> Self {
        Self {
            samples: SampleBuilder::new(AUDIO_MAX_LATE_PACKETS, OpusPacket, sample_rate)
                .with_max_time_delay(std::time::Duration::from_millis(80)),
            payload_type,
        }
    }

    pub(super) fn receive(&mut self, packet: Packet, audio_packets: &mut Vec<Bytes>) {
        if packet.header.payload_type != self.payload_type {
            return;
        }

        self.samples.push(packet);
        while audio_packets.len() < MAX_PENDING_AUDIO_PACKETS {
            let Some(sample) = self.samples.pop() else {
                break;
            };
            audio_packets.push(sample.data);
        }
    }
}

pub(crate) struct VideoRtp {
    // 8b (§6.3): `depacketizer: H264Packet` + `pending: Option<PendingVideoFrame>`
    // deleted — the legacy collector is out of production (test-only oracle).
    next_sequence: Option<u16>,
    last_frame_timestamp: Option<u32>,
    source_frame_duration_us: Option<u64>,
    damage_score: u8,
    /// 2026-11-26: last time a clean (non-damaged) AU was submitted. When
    /// damage persists >= SUSTAINED_DAMAGE_MS the artefacts never clear on
    /// their own (the console's next IDR can be ~2 s away), so request one.
    last_clean_at: Option<Instant>,
    stream_too_large: bool,
    waiting_for_keyframe: bool,
    pli_idr_pending_at: Option<std::time::Instant>,
    stream_clock_anchor: Option<(Instant, u32)>,
    /// Drift instrumentation: (arrival, RTP timestamp) of the newest video
    /// packet seen; used by stream_slip() for the accumulation trend.
    latest_rtp: Option<(Instant, u32)>,
    /// newest_received_ts: RTP timestamp of the newest video packet received;
    /// used by the per-second SR-anchored latency metric computation in media.rs.
    pub(crate) newest_received_ts: Option<u32>,
    /// Drift instrumentation: per-window max of each video-timing (extmap id 7)
    /// stage delta (ms: encode-start, encode-finish, packetization, pacer-exit,
    /// network, network2) plus the sample count.
    vt_max: [u64; 6],
    vt_exit_count: u64,
    slip_slope_base: Option<(Instant, i64)>,
    total_bytes: u64,
    /// Queue-delay estimator Q from §4.3 (signed µs, never clamped).
    pub(crate) q_estimator: QueueDelayEstimator,
    /// 2b: abs-send-time unwrapping + anchor for a jump-free source switch
    /// (rate from ast, offset from the RTP-ts gradient at first ast sample).
    ast_unwrapper: SendTimeUnwrapper,
    ast_active: bool,
    ast_anchor_us: u64,
    grad_anchor_us: u64,
    /// 2c: manual TWCC recorder (vendored from rtc-interceptor 0.20.0-rc.2,
    /// MIT/Apache-2.0) — fed per annotated packet in receive() behind
    /// MANUAL_TWCC, drained by take_twcc_feedback() on send_twcc's gate.
    pub(crate) twcc_recorder: Recorder,
    /// RR-first probe (31/10): RFC A.3 receiver stats feeding RTCP Receiver
    /// Reports — fed every video packet; console ssrc bound on first packet.
    rr: ReceptionStats,
    /// S5b/A4: NACK side-channel ranges drained from the assembler each
    /// push and flushed by media::send_nack (capped; give-up falls back to
    /// the existing damage/PLI path).
    pending_nacks: Vec<(u16, u16)>,
    /// 8a shadow (§3): the new AccessUnitAssembler fed every packet in
    /// parallel with the legacy path. Never authoritative.
    shadow: AccessUnitAssembler,
    /// 8a shadow: pairs legacy and new outcomes by RTP timestamp and counts
    /// matches/mismatches. Never touches legacy state.
    shadow_cmp: ShadowComparator,
    /// Arrival-order packet digests for the mismatch-evidence capture
    /// (observe-only; fixed-capacity, no hot-path allocation).
    arrival: mismatch_log::ArrivalRing,
}

/// 8b (STAGE8-REDO-SPEC §6.2/§6.3): decode-authority selector — consumed at
/// ONE dispatch call site (`handle_shadow_outcome`).
/// `false` = 8a state: legacy `PendingVideoFrame`/`H264Packet` owns decode
/// submission; the new assembler feeds the comparator only (behaviour-neutral).
/// `true` = 8b state: new-assembler outcomes drive `finish_au` and the
/// mirrored statement arms; the legacy collector is out of production
/// (§6.3: kept `#[cfg(test)]` only as the differential-fuzz oracle).
const NEW_AUTHORITY: bool = true;

/// 8b (§6.3): the legacy collector. DELETED from production — `VideoRtp` no
/// longer holds one; it survives `#[cfg(test)]` solely as the oracle the
/// randomized differential fuzz compares the new authority against.
#[cfg(test)]
struct PendingVideoFrame {
    timestamp: u32,
    // 8b (§6.3): `first_packet_at` removed — only the deleted legacy
    // finish_au call site read it; the oracle never does.
    packets: Vec<Packet>,
}

/// 8b (STAGE8-REDO-SPEC §6.1): a completed AU handed to `finish_au`.
struct ResolvedAu {
    data: Bytes,
    marker_sequence: u16,
    timestamp: u32,
    first_packet_at: Instant,
}

/// 8b (§6.3): legacy assembly result — test-only alongside its collector.
#[cfg(test)]
enum FrameAssembly {
    Pending,
    Complete { data: Bytes, marker_sequence: u16 },
    Invalid,
}

#[cfg(test)]
impl PendingVideoFrame {
    fn new(packet: Packet) -> Self {
        Self {
            timestamp: packet.header.timestamp,
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
    ) -> FrameAssembly {
        let Some(marker_sequence) = self.marker_sequence() else {
            return FrameAssembly::Pending;
        };
        let mut packets = self.packets.iter().collect::<Vec<_>>();
        packets.sort_unstable_by_key(|packet| {
            std::cmp::Reverse(marker_sequence.wrapping_sub(packet.header.sequence_number))
        });
        let Some(first) = packets.first() else {
            return FrameAssembly::Pending;
        };
        if expected_sequence.is_some_and(|expected| first.header.sequence_number != expected)
            || !depacketizer.is_partition_head(&first.payload)
        {
            return FrameAssembly::Pending;
        }
        if packets.windows(2).any(|pair| {
            pair[1].header.sequence_number != pair[0].header.sequence_number.wrapping_add(1)
        }) {
            return FrameAssembly::Pending;
        }

        *depacketizer = H264Packet::default();
        let mut data = Vec::new();
        for packet in packets {
            let Ok(nalu) = depacketizer.depacketize(&packet.payload) else {
                *depacketizer = H264Packet::default();
                return FrameAssembly::Invalid;
            };
            data.extend_from_slice(&nalu);
            if data.len() > MAX_H264_ACCESS_UNIT_BYTES {
                *depacketizer = H264Packet::default();
                return FrameAssembly::Invalid;
            }
        }
        *depacketizer = H264Packet::default();
        FrameAssembly::Complete {
            data: Bytes::from(data),
            marker_sequence,
        }
    }
}

impl VideoRtp {

    pub(crate) fn new() -> Self {
        Self {
            // 8b (§6.3): legacy `depacketizer`/`pending` fields deleted.
            next_sequence: None,
            last_frame_timestamp: None,
            source_frame_duration_us: None,
            damage_score: 0,
            last_clean_at: None,
            stream_too_large: false,
            waiting_for_keyframe: false,
            pli_idr_pending_at: None,
            stream_clock_anchor: None,
            newest_received_ts: None,
            latest_rtp: None,
            slip_slope_base: None,
            vt_max: [0; 6],
            vt_exit_count: 0,
            total_bytes: 0,
            q_estimator: QueueDelayEstimator::new(),
            ast_unwrapper: SendTimeUnwrapper::new(),
            ast_active: false,
            ast_anchor_us: 0,
            grad_anchor_us: 0,
            twcc_recorder: Recorder::new(LOCAL_RTCP_SENDER_SSRC),
            rr: ReceptionStats::new(),
            pending_nacks: Vec::new(),
            shadow: AccessUnitAssembler::new(),
            shadow_cmp: ShadowComparator::new(),
            arrival: mismatch_log::ArrivalRing::new(),
        }
    }

    pub(super) fn waiting_for_keyframe(&self) -> bool {
        self.waiting_for_keyframe
    }

    pub(super) fn wait_for_keyframe(&mut self) {
        self.waiting_for_keyframe = true;
    }

    pub(crate) fn receive(
        &mut self,
        worker: &VideoDecodeWorker,
        packet: Packet,
        keyframe_requested: &mut bool,
    ) -> VideoSampleStats {
        let mut stats = VideoSampleStats::default();
        let mut frame_was_damaged = false;
        if packet.payload.is_empty() {
            if self.next_sequence == Some(packet.header.sequence_number) {
                self.next_sequence = Some(packet.header.sequence_number.wrapping_add(1));
            }
            return stats;
        }
        // S5b: RTX (RFC 4588) — negotiated PT, separate SSRC already rewritten
        // to primary by RtxSsrcRewrite. Unwrap the 2-byte original seq and feed
        // ONLY the authority assembler: no census/RR/dump/ring/Q bookkeeping
        // (duplicates must not disturb stream stats — 0.2.5 proven semantics).
        if crate::api::streaming::rtc::media::is_negotiated_rtx_pt(packet.header.payload_type)
        {
            return self.receive_rtx(worker, packet, keyframe_requested);
        }
        self.total_bytes += 12 + packet.payload.len() as u64;

        // 2a observability (2026-10-30): count packets carrying at least one
        // PARSED inbound header extension. The rtp_ext_any column previously
        // had no writer at all — a write-never placeholder like twcc_sent.
        // With 690f0c7 negotiating the extmaps this discriminates "console
        // annotates nothing" from "our parse map is broken" (vt_exit_count
        // alone can't: it also requires video-timing payload == 13 bytes).
        if !packet.header.extensions.is_empty() {
            METRICS.rtp_ext_any.fetch_add(1, Ordering::Relaxed);
        }
        // 2c diagnostic: does the console annotate the TWCC extension itself
        // (answer-negotiated id)? rtp_ext_any can't discriminate WHICH ext
        // arrived — this splits "console skips twcc" from "interceptor bug".
        let twcc_seen_id = NEG_TWCC_EXT_ID.load(Ordering::Relaxed);
        if twcc_seen_id != 0
            && let Some(ext) = packet
                .header
                .extensions
                .iter()
                .find(|ext| ext.id == twcc_seen_id)
        {
            METRICS.rtp_ext_twcc.fetch_add(1, Ordering::Relaxed);
            // 2c manual path: parse the transport-wide seq (2-byte BE,
            // verified against rtc-rtp's TransportCcExtension::unmarshal)
            // and feed the vendored Recorder — flag-gated so record+send
            // flip together (false = 8385cbc: annotation observed, nothing
            // sent). Runtime-switchable: per-packet Relaxed read, applies
            // live mid-stream. Arrival on the SHADOW_CLOCK timeline the CSV uses.
            if MANUAL_TWCC.load(Ordering::Relaxed)
                && let Some(twcc_seq) = parse_transport_wide_seq(&ext.payload[..])
            {
                // Stamp with TRUE socket arrival (seq-tagged FIFO fed by the
                // transport from CapturedDatagram.at); SHADOW_CLOCK fallback
                // covers SRTP drops/reorder so stamps stay monotonic-ish.
                self.twcc_recorder.record(
                    packet.header.ssrc,
                    twcc_seq,
                    take_twcc_arrival(packet.header.sequence_number),
                );
            }
        }

        let packet_timestamp = packet.header.timestamp;
        // RR-first probe: RFC A.3 reception stats for send_rr (every packet).
        self.rr.on_packet(
            packet.header.ssrc,
            packet.header.sequence_number,
            packet_timestamp,
            SHADOW_CLOCK.elapsed().as_micros() as i64,
        );
        // Track the newest received RTP timestamp for SR-anchored
        // capture-to-arrival latency metrics in media.rs.
        // Advance only to newer timestamps (mirrors the logic in the
        // later match block on self.latest_rtp, but unconditional).
        let prev_newest = self.newest_received_ts;
        self.newest_received_ts = Some(match prev_newest {
            Some(cur) if timestamp_is_newer(packet_timestamp, cur) => packet_timestamp,
            Some(cur) => cur,
            None => packet_timestamp,
        });

        crate::streaming::video::rtp_dump_log::record(
            SHADOW_CLOCK.elapsed().as_micros() as u64,
            packet.header.sequence_number,
            packet_timestamp,
            packet.header.marker,
            &packet.payload,
        );

        // Mismatch-evidence arrival ring (observe-only): per-packet
        // (seq, len, payload hash) in arrival order for the frame in
        // flight; fixed-capacity arrays, zero heap traffic.
        self.arrival.push(
            packet_timestamp,
            packet.header.sequence_number,
            &packet.payload,
        );

        // 8a shadow (STAGE8-REDO-SPEC §3): feed every non-empty video packet
        // to the new assembler alongside the legacy path. §6.2 (8b): every
        // outcome now flows through handle_shadow_outcome — comparator-only
        // while NEW_AUTHORITY is false; finish_au + mirrored abandon/invalid
        // statement arms once flipped true (§6.3). Queue-drain-before-direct
        // order mirrors legacy: an older AU forced out by this packet's newer
        // timestamp resolves logically BEFORE the AU this packet completes.
        let shadow_now = SHADOW_CLOCK.elapsed().as_micros() as u64;
        let late_before = self.shadow.late_after_resolve();
        let direct = self.shadow.push(
            packet.header.sequence_number,
            packet_timestamp,
            packet.header.marker,
            &packet.payload,
            shadow_now,
        );
        while let Some(outcome) = self.shadow.poll_outcome() {
            self.handle_shadow_outcome(
                outcome, false, worker, keyframe_requested, &mut stats,
                &mut frame_was_damaged,
            );
        }
        if let Some(outcome) = direct {
            self.handle_shadow_outcome(
                outcome, false, worker, keyframe_requested, &mut stats,
                &mut frame_was_damaged,
            );
        }
        if let Some(outcome) = self.shadow.flush_due(shadow_now) {
            self.handle_shadow_outcome(
                outcome, true, worker, keyframe_requested, &mut stats,
                &mut frame_was_damaged,
            );
        }
        while let Some(outcome) = self.shadow.poll_outcome() {
            self.handle_shadow_outcome(
                outcome, false, worker, keyframe_requested, &mut stats,
                &mut frame_was_damaged,
            );
        }
        // S5b: drain any hole report emitted by this push (A4 side-channel).
        let nack_ranges = self.shadow.take_nack_ranges();
        if !nack_ranges.is_empty() {
            self.queue_nacks(nack_ranges);
        }
        // Rule-4 phantom-guard drops (§11) mirrored into METRICS for the
        // 8a-v2 gate report.
        let late_delta = self.shadow.late_after_resolve() - late_before;
        if late_delta > 0 {
            METRICS
                .late_after_resolve
                .fetch_add(late_delta, Ordering::Relaxed);
        }

        // Drift instrumentation (log only, no behaviour change): establish the
        // session media-clock anchor on the first video packet and track the
        // newest-timestamp arrival so stream_slip() can trend arrival-vs-media
        // drift for the latency-accumulation investigation.
        if self.stream_clock_anchor.is_none() {
            self.stream_clock_anchor = Some((Instant::now(), packet_timestamp));
        }
        match self.latest_rtp {
            Some((_, last_ts)) if !timestamp_is_newer(packet_timestamp, last_ts) => {}
            _ => {
                self.latest_rtp = Some((Instant::now(), packet_timestamp));
                // Push queue-delay estimator input per video RTP packet.
                // 2b (2026-10-30): send_us prefers abs-send-time (negotiated
                // since 690f0c7, answer-stored id) — the console's own send
                // clock, the A3 wiring the old comment deferred. Until the
                // first ast sample (or if the console never annotates it)
                // the RTP-ts gradient stays active; at switch-over the ast
                // stream is ANCHORED to the gradient value so Q's delay
                // axis never jumps (rate from ast, offset preserved).
                let grad_us = (packet_timestamp as u64).wrapping_mul(100) / 9;
                let ast_id = NEG_AST_EXT_ID.load(Ordering::Relaxed);
                let send_us = if ast_id != 0
                    && let Some(ext) = packet
                        .header
                        .extensions
                        .iter()
                        .find(|ext| ext.id == ast_id)
                    && ext.payload.len() == 3
                {
                    let raw24 =
                        u32::from_be_bytes([0, ext.payload[0], ext.payload[1], ext.payload[2]]);
                    let unwrapped = self.ast_unwrapper.push(raw24);
                    if !self.ast_active {
                        self.ast_active = true;
                        self.ast_anchor_us = unwrapped;
                        self.grad_anchor_us = grad_us;
                    }
                    self.grad_anchor_us + (unwrapped - self.ast_anchor_us)
                } else {
                    grad_us
                };
                // arrival_us = same local-µs timeline as slip: wall time since
                // the session anchor (the slip computation uses
                // arrival_at.duration_since(anchor_at)).
                let arrival_us = self.stream_clock_anchor
                    .map_or(0, |(anchor_at, _)| {
                        Instant::now().duration_since(anchor_at).as_micros() as u64
                    });
                self.q_estimator.push(crate::pipeline::clock::queue_delay::PacketInput {
                    send_us,
                    arrival_us,
                });
                // 7b: per-second Q peak maintained alongside the push (mirrors
                // stream_slip_max_us below; clamp at 0 so a negative Q cannot
                // wrap the u64 accumulator). drain_metrics() swaps it to 0.
                METRICS.q_us_max.fetch_max(self.q_estimator.q_us().max(0) as u64, Ordering::Relaxed);
            }
        }

        // 8b (§6.3): legacy force-out abandon (pending.ts guard + abandon
        // arm) deleted — the new assembler owns stale-ts rejection (rule 4 +
        // the in-flight parity drop) and Damaged outcomes mirror the arm's
        // statements in dispatch_new_authority.
        // Video-timing census: parse six 16-bit stage deltas. Since 2a the
        // id is the ANSWER-negotiated value (rtc assigns from registration
        // order; chrome's sessions happened to use 7 — the old hardcoded 7
        // could never match an offer with zero extmaps: vt_exit_count was 0
        // in every pre-2a gate session, i.e. dead telemetry). id 0 = not
        // negotiated yet keeps the census safely off until signaling lands.
        let vt_ext_id = NEG_VT_EXT_ID.load(Ordering::Relaxed);
        if vt_ext_id != 0
            && let Some(ext) = packet.header.extensions.iter().find(|ext| ext.id == vt_ext_id)
            && ext.payload.len() == 13
        {
            for (i, slot) in self.vt_max.iter_mut().enumerate() {
                let d = u16::from_be_bytes([ext.payload[1 + i * 2], ext.payload[2 + i * 2]]) as u64;
                *slot = (*slot).max(d);
            }
            self.vt_exit_count += 1;
        }

        // 8b (§6.3): the legacy insert → assemble → observe → finish_au
        // chain is deleted. Outcomes for this packet were already dispatched
        // through handle_shadow_outcome at the top of receive(); the new
        // authority owns collection, resolution and submission.
        stats
    }

    /// 8b (STAGE8-REDO-SPEC §6.1): the legacy post-assembly body, moved
    /// verbatim. Executes once per Complete AU, in checklist order:
    /// assembly metrics -> slip -> next_sequence -> frame duration +
    /// last_frame_timestamp -> inspect_h264_access_unit -> size gates ->
    /// waiting_for_keyframe gate (+D0b) -> damage decay -> submit_access_unit
    /// (+queue_full handling).
    fn finish_au(
        &mut self,
        au: ResolvedAu,
        worker: &VideoDecodeWorker,
        keyframe_requested: &mut bool,
        mut stats: VideoSampleStats,
        frame_was_damaged: bool,
    ) -> VideoSampleStats {
        let ResolvedAu {
            data,
            marker_sequence,
            ..
        } = au;
        self.update_stream_lag(au.timestamp, au.first_packet_at);
        // AU-level one-way delay on the media clock, relative to the session's
        // first AU (perf plan 2026-11-26): delay = first-packet arrival µs −
        // RTP timestamp/90 kHz. Both timelines tick in real time, so the
        // constant console capture offset cancels; GROWTH in this column is
        // console-encode + network + receive-queue latency accumulation,
        // measured with no clock sync and independent of the decoder (that is
        // `pipeline_age_us`). Absolute capture-to-arrival would need the SR NTP
        // anchor, which the console SRTCP-encrypts (see transport.rs). One-off
        // step after ~13.2 h when the 32-bit RTP timestamp wraps.
        {
            let arrival_us = au
                .first_packet_at
                .duration_since(*SHADOW_CLOCK)
                .as_micros() as i64;
            let media_us = i64::from(au.timestamp) * 100 / 9;
            let delay_raw = arrival_us - media_us;
            let base = match DELAY_BASE_US.compare_exchange(
                i64::MIN,
                delay_raw,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => delay_raw,
                Err(existing) => existing,
            };
            let relative_us = delay_raw - base;
            METRICS
                .capture_to_arrival_us
                .store(relative_us, Ordering::Relaxed);
            METRICS
                .capture_to_arrival_max_us
                .fetch_max(relative_us, Ordering::Relaxed);
        }
        // Record both average and worst-case RTP assembly time for the stream HUD.
        let assembly_us = au.first_packet_at.elapsed().as_micros() as u64;
        crate::streaming::video::metrics::METRICS
            .rtp_assembly_sum_us
            .fetch_add(assembly_us, Ordering::Relaxed);
        crate::streaming::video::metrics::METRICS
            .rtp_assembly_count
            .fetch_add(1, Ordering::Relaxed);
        crate::streaming::video::metrics::METRICS
            .rtp_assembly_max_us
            .fetch_max(assembly_us, Ordering::Relaxed);

        // Drift instrumentation: accumulate slip metric from the latest video packet.
        if let Some(slip) = self.stream_slip() {
            let (slip_us, _wall_us, _media_us) = slip;
            // Current slip (updated every packet; HUD reads this value).
            // Store signed slip directly — no clamp to positive, since signed
            // slip conveys direction (wall vs media time). Cite
            // RENDERING-PIPELINE-ARCHITECTURE-PROPOSITION.md §4.4.
            METRICS.stream_slip_us.store(slip_us, Ordering::Relaxed);
            // Maximum slip observed during the session (max absolute magnitude).
            METRICS.stream_slip_max_us.fetch_max(slip_us.unsigned_abs() as u64, Ordering::Relaxed);
            // One-shot base slope: capture once when wall-time since anchor ≥ 60 s.
            let anchor_age_secs = self.stream_clock_anchor
                .map_or(0.0, |(anchor_at, _)| anchor_at.elapsed().as_secs_f64());
            if METRICS.stream_slip_slope_mpm.load(Ordering::Relaxed) == 0
                && anchor_age_secs >= 60.0
            {
                // Session-fixed base: never re-based for the duration of this session.
                METRICS.stream_slip_slope_mpm.store(slip_us, Ordering::Relaxed);
            } else if METRICS.stream_slip_slope_mpm.load(Ordering::Relaxed) != 0
            {
                // Compute running slope relative to the base captured at ~60 s.
                // slip is µs, slope is reported in ms/min:
                // (delta_us / 1000) ms per elapsed_s seconds, × 60 for a minute.
                let base_slip = METRICS.stream_slip_slope_mpm.load(Ordering::Relaxed);
                let elapsed_s = anchor_age_secs - 60.0; // seconds after the 60s base point
                if elapsed_s > 1.0 {
                    let delta_us = slip_us - base_slip;
                    let slope_mpm = (delta_us as f64 * 60.0) / (1000.0 * elapsed_s);
                    METRICS.stream_slip_slope_mpm.store(slope_mpm.round() as i64, Ordering::Relaxed);
                }
            }
        }

        self.next_sequence = Some(marker_sequence.wrapping_add(1));
        stats.source_frame_duration_us = self.last_frame_timestamp.map(|previous| {
            u64::from(au.timestamp.wrapping_sub(previous)) * 1_000_000
                / u64::from(VIDEO_RTP_CLOCK_RATE)
        });
        if let Some(duration) = stats.source_frame_duration_us {
            self.source_frame_duration_us = Some(
                self.source_frame_duration_us
                    .map(|average| (average * 7 + duration) / 8)
                    .unwrap_or(duration),
            );
        }
        self.last_frame_timestamp = Some(au.timestamp);

        let unit = inspect_h264_access_unit(&data);
        if self.pli_idr_pending_at.is_some() && unit.has_idr {
            let elapsed_ms = self.pli_idr_pending_at.unwrap().elapsed().as_millis() as u64;
            METRICS.pli_idr_ms_last.store(elapsed_ms, Ordering::Relaxed);
            METRICS.pli_idr_ms_max.fetch_max(elapsed_ms, Ordering::Relaxed);
            self.pli_idr_pending_at = None;
        }
        stats.encoded_resolution = unit.resolution;
        stats.color = unit.color;
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
                worker.begin_resync();
            }
            self.waiting_for_keyframe = true;
            stats.dropped = stats.dropped.saturating_add(1);
            return stats;
        }
        if self.stream_too_large {
            if unit.resolution.is_none() || !unit.has_idr {
                *keyframe_requested = true;
                self.waiting_for_keyframe = true;
                stats.dropped = stats.dropped.saturating_add(1);
                return stats;
            }
            self.stream_too_large = false;
        }
        if self.waiting_for_keyframe {
            // Later keyframes may contain only IDR; AVCDEC retains SPS/PPS across resyncs.
            if !unit.has_idr {
                *keyframe_requested = true;
                stats.dropped = stats.dropped.saturating_add(1);
                return stats;
            }
            self.waiting_for_keyframe = false;
            self.damage_score = 0;
            self.last_clean_at = Some(Instant::now());
        } else if !frame_was_damaged {
            self.damage_score = self.damage_score.saturating_sub(1);
            self.last_clean_at = Some(Instant::now());
        }

        // 8a telemetry (§3): submitted-AU counters that would have caught the
        // stage-8 black screen on day one.
        METRICS
            .au_submitted
            .fetch_add(1, Ordering::Relaxed);
        if !(data.starts_with(&[0, 0, 0, 1]) || data.starts_with(&[0, 0, 1])) {
            METRICS.au_no_startcode.fetch_add(1, Ordering::Relaxed);
        }
        if unit.has_idr {
            METRICS.au_idr_submitted.fetch_add(1, Ordering::Relaxed);
            // Perf-plan stage 0 probe: keyframe share of the stream's bytes.
            METRICS
                .idr_bytes
                .fetch_add(data.len() as u64, Ordering::Relaxed);
        }
        if !worker.submit_access_unit(data.to_vec(), self.source_frame_duration_us) {
            // Perf-plan stage 3: a decode-queue rejection is our own
            // back-pressure, not stream damage — requesting a keyframe on top
            // of the dropped frame made the console send its largest frames
            // at the worst moment. Stage 3 drains bursts by retiring stale
            // decoded pictures, so this path should be rare; if it fires,
            // the following frames conceal until the next natural keyframe.
            eprintln!("Video decoder queue is full; dropping the frame without a keyframe request");
            stats.dropped = stats.dropped.saturating_add(1);
        }
        stats
    }

    /// 8a shadow (§3): record one new-path outcome for the comparator.
    /// Never touches legacy state.
    fn observe_shadow(&mut self, outcome: AssemblyOutcome, via_hold: bool) {
        if via_hold && matches!(outcome, AssemblyOutcome::Complete(_)) {
            METRICS.sh_hold_saved.fetch_add(1, Ordering::Relaxed);
        }
        // Mismatch-evidence digests are only needed for Complete outcomes:
        // copy the arrival-ring list here so the comparator owns it until
        // pairing. Observe-only — no pending/submit/timing contact.
        let digests = if let AssemblyOutcome::Complete(desc) = &outcome {
            self.arrival.digests_for(desc.rtp_ts)
        } else {
            Box::new([])
        };
        self.shadow_cmp.observe_new(outcome, digests, &self.arrival);
    }

    /// §6.2 (8b): THE single dispatch point for new-assembler outcomes.
    /// `NEW_AUTHORITY == false` delegates to the 8a comparator sink (device
    /// behaviour identical to the shadow build); `true` runs the authority
    /// body. Switching = flipping the const — no call-site restructure.
    fn handle_shadow_outcome(
        &mut self,
        outcome: AssemblyOutcome,
        via_hold: bool,
        worker: &VideoDecodeWorker,
        keyframe_requested: &mut bool,
        stats: &mut VideoSampleStats,
        frame_was_damaged: &mut bool,
    ) {
        if NEW_AUTHORITY {
            self.dispatch_new_authority(
                outcome, via_hold, worker, keyframe_requested, stats,
                frame_was_damaged,
            );
        } else {
            self.observe_shadow(outcome, via_hold);
        }
    }

    /// §6.2 (8b): the NEW authority's per-outcome body.
    /// - `Complete` → `finish_au` (§6.1 checklist order unchanged). The AU's
    ///   first-packet Instant is reconstructed from the descriptor's
    ///   `first_arrival_us` on the same SHADOW_CLOCK timeline `arrival_us`
    ///   was recorded on, so `rtp_assembly_*` keeps its meaning.
    /// - `Damaged` → mirrors the legacy ABANDON arm statements:
    ///   `next_sequence = marker_seq + 1` iff a marker was seen (legacy:
    ///   `marker_sequence().map(+1)` — the flag packet is the AU's highest
    ///   seq, RTP marker semantics), `keyframe_requested`, `record_damage`,
    ///   `frame_was_damaged = true`, `stats.dropped`.
    /// - `Discarded` → mirrors the legacy INVALID arm: `next_sequence = None`,
    ///   `keyframe_requested`, `record_damage`, `stats.dropped` — and
    ///   deliberately does NOT set `frame_was_damaged` (the legacy asymmetry).
    /// The legacy `depacketizer` reset has no counterpart by design: the
    /// collector and its H264Packet state are deleted in §6.3, so no
    /// cross-frame depacketize state exists to reset.
    fn dispatch_new_authority(
        &mut self,
        outcome: AssemblyOutcome,
        via_hold: bool,
        worker: &VideoDecodeWorker,
        keyframe_requested: &mut bool,
        stats: &mut VideoSampleStats,
        frame_was_damaged: &mut bool,
    ) {
        if via_hold && matches!(outcome, AssemblyOutcome::Complete(_)) {
            METRICS.sh_hold_saved.fetch_add(1, Ordering::Relaxed);
        }
        match outcome {
            AssemblyOutcome::Complete(desc) => {
                let au = ResolvedAu {
                    data: bytes::Bytes::from(desc.data),
                    marker_sequence: desc.last_seq,
                    timestamp: desc.rtp_ts,
                    first_packet_at: *SHADOW_CLOCK
                        + std::time::Duration::from_micros(desc.first_arrival_us),
                };
                let taken = std::mem::take(stats);
                *stats = self.finish_au(
                    au,
                    worker,
                    keyframe_requested,
                    taken,
                    *frame_was_damaged,
                );
            }
            AssemblyOutcome::Damaged(desc) => {
                // Legacy abandon arm statements (observe_legacy record and
                // depacketizer reset excepted — see fn doc).
                self.next_sequence = desc
                    .marker_seen
                    .then(|| desc.last_seq.wrapping_add(1));
                // Step 6 (2026-11-26): the immediate keyframe request is a
                // property of the SHED policies, not of damage itself. The
                // LowLatency default is documented as "decodes through: brief
                // artefacts, no stall", but this line requested a keyframe on
                // EVERY damaged AU regardless of policy -- 128 damaged AUs
                // became 112 PLIs and 199 IDRs (14.5 MB) in 736 s. Under
                // LowLatency the gentler `record_damage` threshold (3 events at
                // 60 fps) is the trigger, and the resync path requests the
                // keyframe from `finish_au` while `waiting_for_keyframe` is set,
                // so the chain still recovers.
                if DAMAGE_POLICY.load(Ordering::Relaxed) != 0 {
                    *keyframe_requested = true;
                }
                // 2026-11-26: prolonged-artefact reaction. A single first-slice
                // loss can stay blocky until the console's next IDR (~2 s). If
                // no clean AU has been submitted for >= 1 s, request one now
                // instead of waiting out the damage_score threshold.
                if self
                    .last_clean_at
                    .is_some_and(|last| last.elapsed() >= std::time::Duration::from_millis(SUSTAINED_DAMAGE_MS))
                {
                    *keyframe_requested = true;
                }
                self.record_damage(worker);
                *frame_was_damaged = true;
                stats.dropped = stats.dropped.saturating_add(1);
                // §11.4 re-gate counters: Damaged clause splits (multi-clause
                // outcomes count each clause, mirroring the bucket-1 histogram).
                if !desc.marker_seen {
                    METRICS.dam_markerless.fetch_add(1, Ordering::Relaxed);
                }
                if !desc.had_first_slice {
                    METRICS.dam_first_slice.fetch_add(1, Ordering::Relaxed);
                }
                if desc.holes > 0 {
                    METRICS.dam_holes.fetch_add(1, Ordering::Relaxed);
                }
                // Step 6 damage policy (settings): Balanced sheds severe
                // damage (leading slice lost / multi-hole — the worst
                // decoder-concealment cases); Quality sheds EVERY damaged
                // AU — artefact-free at the cost of a PLI→IDR micro-freeze
                // per event. LowLatency (default) decodes through: the
                // dropped AU makes the decoder conceal on the next Complete
                // frames until the PLI/IDR resets the chain.
                let severe = !desc.had_first_slice || desc.holes > 1;
                let shed = match DAMAGE_POLICY.load(Ordering::Relaxed) {
                    2 => true,
                    1 => severe,
                    _ => false,
                };
                if shed && !self.waiting_for_keyframe {
                    self.waiting_for_keyframe = true;
                    self.pli_idr_pending_at = Some(std::time::Instant::now());
                    worker.begin_resync();
                    METRICS.damaged_dropped.fetch_add(1, Ordering::Relaxed);
                }
            }
            AssemblyOutcome::Discarded { .. } => {
                // Legacy Invalid arm statements; frame_was_damaged untouched.
                self.next_sequence = None;
                *keyframe_requested = true;
                self.record_damage(worker);
                stats.dropped = stats.dropped.saturating_add(1);
            }
        }
    }

    fn record_damage(&mut self, worker: &VideoDecodeWorker) {
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

        worker.begin_resync();
        self.waiting_for_keyframe = true;
        self.damage_score = 0;
    }
    
    /// 8b (§6.4): the idle-time flush hook — `flush_due` goes LIVE only here
    /// (§6.4). receive() flushes on packet arrival; this covers pump ticks
    /// where no fresh video arrives: a markerless AU past the ~40 ms
    /// backstop resolves as Damaged → the abandon-arm mirror runs
    /// (next_sequence/keyframe/record_damage), and queued outcomes drain
    /// through the same authority dispatch. `frame_was_damaged` is local —
    /// no Complete follows within this hook, so the damage-decay flag has
    /// no consumer here (legacy's force-out body was already a no-op stub).
    pub(crate) fn flush_expired(
        &mut self,
        worker: &VideoDecodeWorker,
        keyframe_requested: &mut bool,
        stats: &mut VideoSampleStats,
    ) {
        let now = SHADOW_CLOCK.elapsed().as_micros() as u64;
        let mut frame_was_damaged = false;
        if let Some(outcome) = self.shadow.flush_due(now) {
            self.handle_shadow_outcome(
                outcome, true, worker, keyframe_requested, stats,
                &mut frame_was_damaged,
            );
        }
        while let Some(outcome) = self.shadow.poll_outcome() {
            self.handle_shadow_outcome(
                outcome, false, worker, keyframe_requested, stats,
                &mut frame_was_damaged,
            );
        }
    }
    pub(crate) fn reset_lag_anchor(&mut self) { self.stream_clock_anchor = None; }
    /// Update the stream lag metric by computing the slip between the RTP timestamp
    /// schedule and actual arrival time, relative to the stream clock anchor.
    fn update_stream_lag(&mut self, au_timestamp: u32, au_first_packet_at: Instant) {
        let (anchor_at, anchor_timestamp) = *self
            .stream_clock_anchor
            .get_or_insert((au_first_packet_at, au_timestamp));
        let lag_us = schedule_slip_us(
            anchor_at,
            anchor_timestamp,
            au_first_packet_at,
            au_timestamp,
        );
        crate::streaming::video::metrics::METRICS
            .stream_lag_us
            .store(lag_us, Ordering::Relaxed);
        crate::streaming::video::metrics::METRICS
            .stream_lag_max_us
            .fetch_max(lag_us, Ordering::Relaxed);
    }
    /// Drift instrumentation: (slip_us, wall_us, media_us).  slip = wall time
    /// since the session anchor minus media time (RTP clock at 90 kHz) advanced
    /// since the anchor.  A flat slope means arrivals keep pace with the media
    /// timeline; a growing slope means they increasingly fall behind.
    /// Units are microseconds throughout — the CSV columns are `*_us`.
    pub(crate) fn stream_slip(&self) -> Option<(i64, i64, i64)> {
        let (anchor_at, anchor_ts) = self.stream_clock_anchor?;
        let (arrival_at, arrival_ts) = self.latest_rtp?;
        let wall_us = arrival_at.duration_since(anchor_at).as_micros() as i64;
        // 90 kHz ticks -> µs is ticks/90 ms -> ticks * 1000/90000 µs = *100/9.
        let media_us = (arrival_ts.wrapping_sub(anchor_ts) as i64) * 100 / 9;
        Some((wall_us - media_us, wall_us, media_us))
    }
    pub(crate) fn total_bytes(&self) -> u64 { self.total_bytes }
    /// 2026-10-30: stub 0 feeding the twcc_stride CSV — real value needs the
    /// 2c tracker (median gap between observed transport-wide sequence numbers).
    pub(crate) fn twcc_median_stride(&self) -> u16 { 0 }
    /// 2c (2026-10-30): drain the manual TWCC recorder — returns feedback for
    /// packets recorded since the previous call (the vendored Recorder
    /// advances its walk cursor, keeps the arrival map; empty vec = nothing
    /// new, safe at any cadence). sender_ssrc/media_ssrc/fb_pkt_count are
    /// set inside the vendored build (recorder.rs Feedback::new).
    pub(crate) fn take_twcc_feedback(&mut self) -> Vec<Box<dyn rtcp::Packet>> {
        self.twcc_recorder.build_feedback_packet()
    }

    /// RR-first probe (31/10): this interval's reception-report block —
    /// None until the first video packet has been seen.
    pub(crate) fn take_reception_report(
        &mut self,
    ) -> Option<rtcp::reception_report::ReceptionReport> {
        self.rr.build_report()
    }

    /// S5b: unwrap an RTX retransmission (RFC 4588) and feed the authority
    /// assembler ONLY. KEEP THE BLOCK IN SYNC with `receive()`'s authority
    /// section: RTX must not touch census/RR/dump/ring/Q/slip (a duplicate
    /// must not disturb stream stats). Outcome handling is identical so a
    /// hole-filling RTX completes the AU through the normal rule-2/flush
    /// paths (the marker hold window is up to 12 ms — LAN RTT fits).
    fn receive_rtx(
        &mut self,
        worker: &VideoDecodeWorker,
        packet: Packet,
        keyframe_requested: &mut bool,
    ) -> VideoSampleStats {
        let mut stats = VideoSampleStats::default();
        let mut frame_was_damaged = false;
        METRICS.rtx_packets_received.fetch_add(1, Ordering::Relaxed);
        if packet.payload.len() < 3 {
            // RTX must carry the 2-byte OSN + at least 1 payload byte.
            METRICS.rtp_invalid.fetch_add(1, Ordering::Relaxed);
            return stats;
        }
        let original_seq = u16::from_be_bytes([packet.payload[0], packet.payload[1]]);
        let mut unwrapped = packet;
        unwrapped.header.sequence_number = original_seq;
        unwrapped.payload = Bytes::copy_from_slice(&unwrapped.payload[2..]);
        let packet_timestamp = unwrapped.header.timestamp;
        let shadow_now = SHADOW_CLOCK.elapsed().as_micros() as u64;
        // --- authority block: keep in sync with receive() ---
        let direct = self.shadow.push(
            unwrapped.header.sequence_number,
            packet_timestamp,
            unwrapped.header.marker,
            &unwrapped.payload,
            shadow_now,
        );
        while let Some(outcome) = self.shadow.poll_outcome() {
            self.handle_shadow_outcome(
                outcome, false, worker, keyframe_requested, &mut stats,
                &mut frame_was_damaged,
            );
        }
        if let Some(outcome) = direct {
            self.handle_shadow_outcome(
                outcome, false, worker, keyframe_requested, &mut stats,
                &mut frame_was_damaged,
            );
        }
        if let Some(outcome) = self.shadow.flush_due(shadow_now) {
            self.handle_shadow_outcome(
                outcome, true, worker, keyframe_requested, &mut stats,
                &mut frame_was_damaged,
            );
        }
        while let Some(outcome) = self.shadow.poll_outcome() {
            self.handle_shadow_outcome(
                outcome, false, worker, keyframe_requested, &mut stats,
                &mut frame_was_damaged,
            );
        }
        // --- end authority block ---
        // Pull any hole report surfaced during this RTX push (late_after_resolve
        // deltas stay primary-attributed — see receive()'s snapshot windows).
        let ranges = self.shadow.take_nack_ranges();
        if !ranges.is_empty() {
            self.queue_nacks(ranges);
        }
        stats
    }

    /// S5b/A4 budget: cap pending NACK seqs (drop oldest ranges on overflow —
    /// give-up falls back to the existing damage/PLI path).
    fn queue_nacks(&mut self, ranges: Vec<(u16, u16)>) {
        for range in ranges {
            self.pending_nacks.push(range);
        }
        loop {
            let total: u64 = self
                .pending_nacks
                .iter()
                .map(|(s, e)| u32::from(e.wrapping_sub(*s)).wrapping_add(1) as u64)
                .sum();
            if total <= 64 || self.pending_nacks.is_empty() {
                break;
            }
            self.pending_nacks.remove(0);
        }
    }

    /// S5b: drain for media::send_nack.
    pub(crate) fn take_pending_nacks(&mut self) -> Vec<(u16, u16)> {
        std::mem::take(&mut self.pending_nacks)
    }
}

pub(crate) fn color_matrix_label(code: u8) -> String {
    match code {
        0 => "GBR".to_owned(),
        1 => "BT.709".to_owned(),
        2 => "Unspec".to_owned(),
        5 => "BT.470BG".to_owned(),
        6 => "BT.601".to_owned(),
        7 => "BT.601".to_owned(),
        9 => "BT.2020".to_owned(),
        10 => "BT.2020".to_owned(),
        _ => format!("m{}", code),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_twcc_transport_wide_seq_parse() {
        // 2c: 2-byte big-endian per draft-holmer + rtc-rtp reference parser
        // (TRANSPORT_CC_EXTENSION_SIZE = 2, seq = (b0 << 8) | b1).
        assert_eq!(parse_transport_wide_seq(&[0x00, 0x01]), Some(1));
        assert_eq!(parse_transport_wide_seq(&[0x12, 0x34]), Some(0x1234));
        assert_eq!(parse_transport_wide_seq(&[0xFF, 0xFE]), Some(65534));
        // Padded element: only the first two bytes are the seq.
        assert_eq!(parse_transport_wide_seq(&[0x12, 0x34, 0x00, 0x00]), Some(0x1234));
        // Sub-two-byte payloads are not a valid twcc element.
        assert_eq!(parse_transport_wide_seq(&[0x12]), None);
        assert_eq!(parse_transport_wide_seq(&[]), None);
    }

    #[test]
    fn test_color_matrix_label() {
        assert_eq!(color_matrix_label(0), "GBR");
        assert_eq!(color_matrix_label(1), "BT.709");
        assert_eq!(color_matrix_label(2), "Unspec");
        assert_eq!(color_matrix_label(5), "BT.470BG");
        assert_eq!(color_matrix_label(6), "BT.601");
        assert_eq!(color_matrix_label(7), "BT.601");
        assert_eq!(color_matrix_label(9), "BT.2020");
        assert_eq!(color_matrix_label(10), "BT.2020");
        // Anything else gets "m{code}" format
        assert_eq!(color_matrix_label(3), "m3");
        assert_eq!(color_matrix_label(255), "m255");
    }
}

// ---------- schedule_slip_us unit tests (from 0.2.5) ----------
#[test]
fn zero_slip_when_arrival_matches_schedule() {
    let anchor = Instant::now();
    // 30 frames at 90 kHz == 1 second of RTP time, arriving 1s later.
    assert_eq!(
        schedule_slip_us(anchor, 0, anchor + Duration::from_secs(1), 90_000),
        0
    );
}

#[test]
fn reports_positive_slip_when_late() {
    let anchor = Instant::now();
    // RTP says 0.5s elapsed, but 0.75s really elapsed -> 250ms slip.
    assert_eq!(
        schedule_slip_us(anchor, 0, anchor + Duration::from_millis(750), 45_000),
        250_000
    );
}

#[test]
fn clamps_early_or_reordered_arrival_to_zero() {
    let anchor = Instant::now();
    assert_eq!(schedule_slip_us(anchor, 0, anchor, 45_000), 0);
}

#[test]
fn wraps_across_the_rtp_timestamp_space() {
    let anchor = Instant::now();
    let anchor_ts = u32::MAX - 45_000 + 1;
    // next RTP timestamp wraps past u32::MAX by 45_000 (0.5s)
    assert_eq!(
        schedule_slip_us(anchor, anchor_ts, anchor + Duration::from_millis(500), anchor_ts.wrapping_add(45_000)),
        0
    );
}

// ---------------------------------------------------------------------------
// RANDOMIZED DIFFERENTIAL TEST
// §5.4: run identical packet streams through both the LEGACY and NEW
// assembly paths with a seeded PRNG, asserting byte-identical AU output for
// all Complete-vs-Complete pairs.  Designed to surface the rare legacy-vs-new
// byte disagreements observed on device (1 in 57,757 AUs).
// ---------------------------------------------------------------------------

/// Very small inline xorshift64* PRNG — no external crates.
/// State is a u64; next_u64() advances and returns the new state; below(n)
/// returns a uniform [0, n) u64.
#[cfg(test)]
struct XorShift64 {
    state: u64,
}

#[cfg(test)]
impl XorShift64 {
    const fn from_seed(seed: u64) -> Self {
        // Seed of 0 is disallowed (xorshift requires odd/non-zero).
        let state = seed | 1;
        Self { state }
    }

    fn next_u64(&mut self) -> u64 {
        // Xorshift64* : xor with left-shift, then multiply by odd constant.
        // Constant 0x254A_5555_5555_5551 works well for 64-bit.
        let mut z = self.state;
        z ^= z << 13;
        z ^= z >> 7;
        z ^= z << 17;
        self.state = z.wrapping_mul(0x254A_5555_5555_5551u64);
        self.state
    }

    fn below(&mut self, n: u64) -> u64 {
        // Uniform [0, n). Uses rejection sampling to avoid modulo bias.
        if n == 0 { return 0; }
        if n == 1 { return 0; }
        // Compute the minimum value that would cause modulo bias.
        // We want the largest multiple of n that is <= u64::MAX.
        // limit = ((u64::MAX + 1) / n) * n, but we compute it without overflow.
        // u64::MAX + 1 wraps to 0, so: limit = u64::MAX - (u64::MAX % n) + 1
        // but this can overflow when n=1 (already handled) or when the result wraps.
        // Safe approach: reject any r >= n * (u64::MAX / n)
        let quota = u64::MAX / n;     // number of complete groups of n
        let bound = quota * n;        // last valid value (always < u64::MAX since quota < n when n > 1)
        loop {
            let r = self.next_u64();
            if r < bound { return r % n; }
        }
    }
}

// Helper: create a Packet with the given fields.
#[cfg(test)]
fn make_pkt(seq: u16, ts: u32, marker: bool, payload: &[u8]) -> rtc::rtp::Packet {
    rtc::rtp::Packet {
        header: rtc::rtp::Header {
            sequence_number: seq,
            timestamp: ts,
            marker,
            ..Default::default()
        },
        payload: bytes::Bytes::copy_from_slice(payload),
    }
}

// Helper: build a STAP-A packet containing SPS and PPS NALs.
#[cfg(test)]
fn make_stap_a(sps: &[u8], pps: &[u8]) -> Vec<u8> {
    let mut payload = vec![0x78]; // nri=3, type=24 (STAP-A)
    payload.extend_from_slice(&(sps.len() as u16).to_be_bytes());
    payload.extend_from_slice(sps);
    payload.extend_from_slice(&(pps.len() as u16).to_be_bytes());
    payload.extend_from_slice(pps);
    payload
}

// Helper: build FU-A fragments for a NAL body, starting at the given seq.
// Inlined to avoid needing pub(crate) visibility on diff_fixtures helpers.
#[cfg(test)]
fn make_fu_fragments_inline(nal_body: &[u8], first_seq: u16, frag_size: usize, ts: u32, marker: bool) -> Vec<rtc::rtp::Packet> {
    let indicator = (nal_body[0] & 0x60) | 28; // preserve nri, type = FU-A
    let body = &nal_body[1..]; // drop NAL type byte
    let chunks: Vec<&[u8]> = body.chunks(frag_size).collect();
    let total = chunks.len();
    let mut seq = first_seq;
    let mut out = Vec::new();
    for (k, chunk) in chunks.iter().enumerate() {
        let mut fu_header = nal_body[0] & 0x1F;
        if k == 0 { fu_header |= 0x80; } // start bit
        if k + 1 == total { fu_header |= 0x40; } // end bit
        let mut payload = vec![indicator, fu_header];
        payload.extend_from_slice(chunk);
        out.push(rtc::rtp::Packet {
            header: rtc::rtp::Header {
                sequence_number: seq,
                timestamp: ts,
                marker,
                ..Default::default()
            },
            payload: bytes::Bytes::copy_from_slice(&payload),
        });
        seq = seq.wrapping_add(1);
    }
    out
}

// Core randomized differential test: run identical packet streams through
// both the LEGACY and NEW assembly paths with a seeded PRNG, asserting
// byte-identical AU output for all Complete-vs-Complete pairs.
#[test]
fn randomized_differential_matches() {
    // Seeded PRNG — deterministic across runs.
    let mut rng = XorShift64::from_seed(0x5DEECE66D_1234u64);

    const TOTAL_ITERATIONS: u64 = 8000; // tuned to fit < 60s host runtime
    const TS0: u32 = 90_000;

    let mut failures = 0;

    for iter in 0..TOTAL_ITERATIONS {
        // ---- Build a synthetic "session" of RTP video packets -----------------
        // Each session has: SPS/PPS STAP-A, then 2-3 frames (IDR FU-A + P-frames + SEI)
        // with distinct 30fps timestamps so rule-4 stragglers don't silently drop.

        // --- Per-iteration parameters (randomized) ---
        let frame_count = rng.below(3) + 2; // 2 or 3 frames per session
        let mut packets = Vec::new();

        // --- SPS/PPS STAP-A at the start ---
        let sps: Vec<u8> = {
            // Random-ish SPS bytes (3+ NAL bytes, valid Annex-B)
            let mut s = Vec::new();
            s.push(0x67); // forbidden_zero=0, nal_unit_type=7 (SPS)
            s.push(rng.below(256) as u8);
            s.push(rng.below(256) as u8);
            s.push(rng.below(256) as u8);
            // A few SPS parameters... keep it minimal but valid
            s.extend_from_slice(&[0x42, 0x00, 0x1E]); // typical SPS constraints
            s.push(rng.below(256) as u8);
            s.push(rng.below(256) as u8);
            s.push(rng.below(256) as u8);
            s.push(rng.below(256) as u8);
            s.push(rng.below(256) as u8);
            s.push(rng.below(256) as u8);
            s
        };
        let pps: Vec<u8> = {
            let mut p = Vec::new();
            p.push(0x68); // nal_unit_type=8 (PPS)
            p.push(rng.below(256) as u8);
            p.push(rng.below(256) as u8);
            p.extend_from_slice(&[0xCE, 0x06]); // typical PPS
            p.push(rng.below(256) as u8);
            p
        };
        // Helper to make a Packet (local to avoid crossing module boundaries)
        fn make_pkt_local(seq: u16, ts: u32, marker: bool, payload: &[u8]) -> rtc::rtp::Packet {
            rtc::rtp::Packet {
                header: rtc::rtp::Header {
                    sequence_number: seq,
                    timestamp: ts,
                    marker,
                    ..Default::default()
                },
                payload: bytes::Bytes::copy_from_slice(payload),
            }
        }
        // Helper to make STAP-A (local)
        fn make_stap_a_local(sps: &[u8], pps: &[u8]) -> Vec<u8> {
            let mut payload = vec![0x78]; // nri=3, type=24 (STAP-A)
            payload.extend_from_slice(&(sps.len() as u16).to_be_bytes());
            payload.extend_from_slice(sps);
            payload.extend_from_slice(&(pps.len() as u16).to_be_bytes());
            payload.extend_from_slice(pps);
            payload
        }
        // Use push instead of extend_from_slice to add single packet
        packets.push(make_pkt_local(0x0000, TS0, true, &make_stap_a_local(&sps, &pps)));

        // --- Per-frame construction ---
        let mut frame_ts = TS0;
        // We use a wide seq space so the session can wrap near 0xFFF0 as allowed.
        let base_seq: u16 = {
            let tmp = rng.below(2);
            if tmp == 0 { 0xFFF0 } else { tmp as u16 }
        };

        for frame_idx in 0..frame_count {
            let is_idr = rng.below(2) == 0; // ~50% IDR frames
            let payload_size = (rng.below(1200) + 1) as usize; // 1..1200 payload bytes

            if is_idr {
                // IDR frame: start with 0x65 (IDR type), followed by payload bytes
                let mut nal_body = vec![0x65]; // IDR NAL type
                // Fill with random-ish bytes
                for _ in 0..(payload_size.saturating_sub(1)) {
                    nal_body.push(rng.below(256) as u8);
                }
                nal_body.push(0x00); // ensure at least 3 bytes

                // Fragments: vary frag_size; some frames may have single fragment,
                // others many fragments to exercise the FU-A path.
                let frag_size = if rng.below(3) == 0 { 1 } else { (rng.below(120) + 1) as usize }; // 1..121
                // Set marker on the last fragment
                let frag_ts = frame_ts;

                // Build fragments using inline implementation
                let mut frag_packets = make_fu_fragments_inline(&nal_body, base_seq.wrapping_add(frame_idx as u16), frag_size, frag_ts, false);

                // Randomly set marker on one of the fragments (not necessarily the last)
                if rng.below(4) == 0 && frag_packets.len() > 1 {
                    let marker_idx = frag_packets.len() / 2;
                    frag_packets[marker_idx].header.marker = true;
                }

                // occasionally drop the marker entirely (markerless frame)
                if rng.below(10) == 0 {
                    // clear marker from all fragments
                    for pk in &mut frag_packets {
                        pk.header.marker = false;
                    }
                }

                packets.extend_from_slice(&frag_packets);
            } else {
                // P-frame: single NAL unit (no FU-A)
                let mut nal_body = vec![0x21]; // P type = 1, nal_ref_idc=1
                for _ in 0..(payload_size.saturating_sub(1)) {
                    nal_body.push(rng.below(256) as u8);
                }
                nal_body.push(0x00); // ensure at least 3 bytes

                // P-frame with marker on the last packet of the frame
                let p_ts = frame_ts;
                packets.push(make_pkt_local(
                    base_seq.wrapping_add(frame_idx as u16),
                    p_ts,
                    true, // marker
                    &nal_body,
                ));
            }

            // Advance frame timestamp by ~30fps (30000/90000 = 33333 us step)
            // but randomize slightly so timestamps aren't perfectly contiguous
            let step = 33333u32 + (rng.below(3000) as u32); // jitter ±3ms
            frame_ts = frame_ts.wrapping_add(step);
        }

        // --- Now run both paths ---
        // Legacy path
        let legacy_events = crate::api::streaming::rtc::rtp::diff_fixtures::legacy_run(&packets);

        // New path: feed packets with increasing now_us (1ms inter-arrival)
        // respecting MARKER_FILL_WINDOW_US=12ms and MARKERLESS_BACKSTOP_US=40ms
        let mut assembler = crate::pipeline::media::assembler::AccessUnitAssembler::new_without_grace();
        let mut new_events: Vec<(usize, crate::pipeline::media::assembler::AssemblyOutcome)> = Vec::new();
        for (i, packet) in packets.iter().enumerate() {
            let arrival_us = 1_000 + i as u64 * 1_000; // 1 ms inter-arrival
            if let Some(outcome) = assembler.push(
                packet.header.sequence_number,
                packet.header.timestamp,
                packet.header.marker,
                &packet.payload,
                arrival_us,
            ) {
                new_events.push((i, outcome));
            }
            while let Some(outcome) = assembler.poll_outcome() {
                new_events.push((i, outcome));
            }
        }
        // Drain any remaining outcomes via flush_due
        let mut now_us = 1_000 + packets.len() as u64 * 1_000;
        // The assembler tracks internal state; we pump enough to flush everything.
        for _ in 0..(packets.len() + 10) {
            if let Some(outcome) = assembler.flush_due(now_us) {
                new_events.push((new_events.len(), outcome));
            }
            now_us += 1_000;
            while let Some(outcome) = assembler.poll_outcome() {
                new_events.push((new_events.len(), outcome));
            }
            if now_us > 10_000_000 { break; } // safety cap
        }

        // --- Collect completes from both paths (inlined, not using private functions) ---
        // Legacy completes: filter LegacyEvent::Complete
        let legacy_complete: Vec<(usize, Vec<u8>)> = legacy_events
            .iter()
            .filter_map(|event| match event {
                crate::api::streaming::rtc::rtp::diff_fixtures::LegacyEvent::Complete { at, data, .. } => Some((*at, data.clone())),
                _ => None,
            })
            .collect();

        // New completes: filter AssemblyOutcome::Complete
        let new_complete: Vec<(usize, Vec<u8>)> = new_events
            .iter()
            .filter_map(|(at, outcome)| match outcome {
                crate::pipeline::media::assembler::AssemblyOutcome::Complete(desc) => Some((*at, desc.data.clone())),
                _ => None,
            })
            .collect();

        // --- Core assertion: where both sides complete, bytes must match ---
        // Build a map of packet index -> legacy data for frames that completed
        let mut legacy_by_idx: std::collections::HashMap<usize, Vec<u8>> = std::collections::HashMap::new();
        for (at, data) in &legacy_complete {
            legacy_by_idx.insert(*at, data.clone());
        }

        // Check each new complete against legacy
        let mut iteration_failed = false;
        for (at, data) in &new_complete {
            // Find matching legacy complete by packet index
            if let Some(ld_data) = legacy_by_idx.get(&at) {
                // Both sides completed a frame with the same packet index — bytes must match
                if data != ld_data {
                    failures += 1;
                    if failures <= 1 {
                        // Compute first differing byte offset
                        let min_len = data.len().min(ld_data.len());
                        let mut first_diff_idx = 0;
                        for j in 0..min_len {
                            if data[j] != ld_data[j] {
                                first_diff_idx = j;
                                break;
                            }
                        }
                        let window = 16usize;
                        let data_start = first_diff_idx.saturating_sub(window);
                        let data_end = std::cmp::min(data.len(), first_diff_idx + window);
                        let ld_start = ld_data.len().saturating_sub(window);
                        let ld_end = std::cmp::min(ld_data.len(), first_diff_idx + window);

                        eprintln!("\n=== RANDOMIZED DIFFERENTIAL MISMATCH ===");
                        eprintln!("SEED: 0x{:016X}", 0x5DEECE66D_1234u64);
                        eprintln!("ITERATION: {}", iter);
                        eprintln!("FIRST DIFFERING BYTE OFFSET: {}", first_diff_idx);
                        eprintln!("LEGACY bytes [{:02?}..{:02?}]: {:?}", 
                            if data_start < data.len() { &data[data_start..std::cmp::min(data.len(), data_start + window)] } else { &[] },
                            if data_start < data.len() { data_start } else { 0 },
                            &data[data_start..std::cmp::min(data.len(), data_start + window)]
                        );
                        eprintln!("NEW    bytes [{:02?}..{:02?}]: {:?}", 
                            if ld_start < ld_data.len() { &ld_data[ld_start..std::cmp::min(ld_data.len(), ld_start + window)] } else { &[] },
                            if ld_start < ld_data.len() { ld_start } else { 0 },
                            &ld_data[ld_start..std::cmp::min(ld_data.len(), ld_start + window)]
                        );

                        eprintln!("LEGACY full data len: {} bytes, first 32: {:?}", ld_data.len(), &ld_data[..std::cmp::min(32usize, ld_data.len())]);
                        eprintln!("NEW    full data len: {} bytes, first 32: {:?}", data.len(), &data[..std::cmp::min(32usize, data.len())]);
                    }
                    iteration_failed = true;
                    break; // stop processing this iteration's completes
                }
            }
        }

        if iteration_failed {
            panic!(
                "randomized_differential_matches: iteration {} FAILED with byte mismatch (seed=0x{:016X})",
                iter, 0x5DEECE66D_1234u64
            );
        }
    }

    // If we reach here, all iterations passed (no panics)
    eprintln!("randomized_differential_matches: {} iterations, 0 failures", TOTAL_ITERATIONS);
}

// 2c: TwccReport (the never-built manual-TWCC carrier struct) and
// take_twcc_report's always-None stub were deleted when the vendored
// Recorder path landed — take_twcc_feedback() hands back ready-to-send
// rtcp::Packet boxes instead of field bundles.

/// Negotiated inbound extension ids from the ANSWER (2a). 0 = not negotiated
/// — our pre-2a offer carried no extmaps at all, and extract_extmap_id's
/// unwrap_or(0) used to mask exactly that as "id 0". Set once per session in
/// worker::exchange_sdp after signaling.
static NEG_TWCC_EXT_ID: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);
static NEG_AST_EXT_ID: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

/// Damage policy (settings → stream start): 0 LowLatency (decode through —
/// the decoder conceals on following frames), 1 Balanced (shed-to-IDR on
/// severe damage), 2 Quality (shed-to-IDR on every damaged AU). Step 6;
/// written by the app at stream start, read in the Damaged arm.
pub(crate) static DAMAGE_POLICY: AtomicU8 = AtomicU8::new(0);

/// 2026-11-26: if no clean AU has been submitted for this long while damage
/// is still occurring, request a keyframe (prolonged-artefact reaction).
const SUSTAINED_DAMAGE_MS: u64 = 1000;
static NEG_VT_EXT_ID: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

/// 2a: store the answer-negotiated TWCC extension id (this was an empty stub
/// that discarded the value — the "tw:0 despite negotiated extmap" mystery
/// was really "never negotiated, then masked"). Consumed by 2c's tracker;
/// rtc's TwccReceiver derives its own copy from StreamInfo.
pub(crate) fn set_negotiated_twcc_ext_id(id: u8) {
    NEG_TWCC_EXT_ID.store(id, Ordering::Relaxed);
}

/// 2a: abs-send-time id — consumed by 2b (Q estimator send-time source).
pub(crate) fn set_negotiated_ast_ext_id(id: u8) {
    NEG_AST_EXT_ID.store(id, Ordering::Relaxed);
}

/// 2a: video-timing id — consumed by the vt census in `receive()`.
pub(crate) fn set_negotiated_vt_ext_id(id: u8) {
    NEG_VT_EXT_ID.store(id, Ordering::Relaxed);
}

/// 2c: transport-wide sequence number from a one-byte-header extension
/// payload — 2 bytes big-endian (draft-holmer-rmcat-transport-wide-cc-01).
/// Verified against rtc-rtp's reference parser: transport_cc_extension/mod.rs
/// has TRANSPORT_CC_EXTENSION_SIZE = 2 and unmarshals `seq = (b0 << 8) | b1`,
/// accepting any buffer with remaining >= 2 (trailing zero padding ignored).
fn parse_transport_wide_seq(payload: &[u8]) -> Option<u16> {
    let [b0, b1, ..] = payload else {
        return None;
    };
    Some(u16::from_be_bytes([*b0, *b1]))
}


fn timestamp_is_newer(candidate: u32, reference: u32) -> bool {
    let distance = candidate.wrapping_sub(reference);
    distance != 0 && distance < (1 << 31)
}

/// Expected-vs-actual arrival slip for one access unit, in microseconds.
/// `expected` is how long the RTP timestamps say should have elapsed since the
/// anchor; `actual` is how long really elapsed.  Never negative.
fn schedule_slip_us(
    anchor_at: Instant,
    anchor_timestamp: u32,
    au_at: Instant,
    au_timestamp: u32,
) -> u64 {
    let expected_us = u64::from(au_timestamp.wrapping_sub(anchor_timestamp))
        .saturating_mul(1_000_000)
        / u64::from(VIDEO_RTP_CLOCK_RATE);
    let actual_us = au_at.saturating_duration_since(anchor_at).as_micros() as u64;
    actual_us.saturating_sub(expected_us)
}

struct AccessUnitInfo {
    has_idr: bool,
    resolution: Option<(u32, u32)>,
    color: Option<u8>,
}

fn inspect_h264_access_unit(data: &[u8]) -> AccessUnitInfo {
    let mut info = AccessUnitInfo {
        has_idr: false,
        resolution: None,
        color: None,
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
                    if let Ok(sps) = SeqParameterSet::from_bits(nal.rbsp_bits()) {
                        info.resolution = sps.pixel_dimensions().ok();
                        // Per H.264: if SPS parsed, extract color from VUI;
                        // if VUI/video_signal_type/colour_description absent,
                        // matrix_coefficients defaults to Unspecified (2).
                        let color = sps.vui_parameters
                            .as_ref()
                            .and_then(|v| v.video_signal_type.as_ref())
                            .and_then(|v| v.colour_description.as_ref())
                            .map(|c| c.matrix_coefficients)
                            .unwrap_or(2);
                        info.color = Some(color);
                    }
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

// ---------------------------------------------------------------------------
// 8a shadow (STAGE8-REDO-SPEC §3): outcome pairing between the legacy authority
// and the new AccessUnitAssembler. Records are keyed by RTP timestamp and
// paired whenever both sides have produced one; verdicts feed METRICS counters.
// The comparator never touches pending, worker, keyframe_requested or stats.
// ---------------------------------------------------------------------------

/// Monotonic µs clock for the shadow assembler's 12 ms hold window.
static SHADOW_CLOCK: LazyLock<Instant> = LazyLock::new(Instant::now);

/// Baseline for `capture_to_arrival_us` (AU-level one-way delay): the
/// (arrival − media-time) value of the session's first completed AU, in µs on
/// SHADOW_CLOCK. `i64::MIN` = not yet set. Relative values make growth
/// readable without clock sync (see `finish_au`).
static DELAY_BASE_US: AtomicI64 = AtomicI64::new(i64::MIN);

// ---------------------------------------------------------------------------
// TWCC socket-arrival stamping (A arm of the 2026-11-01 stamp A/B). The
// 2026-11-01 diag build measured what we REPORT: median delay 42 µs but a
// ~36 ms per-second max tail (264 ms spikes). Suspicion: our stamps used
// processing time (SHADOW_CLOCK at VideoRtp::receive, after the 2048-deep
// capture channel + pump scheduling), so the tail may be OUR jitter, not
// radio. The transport now tags video-RTP datagrams with their true wire
// arrival (CapturedDatagram.at) and the recorder is fed from that.
// ---------------------------------------------------------------------------

/// Anchor for the socket-arrival µs domain. FORCED by the RX thread before
/// its recv loop starts, so capture instants can never precede it.
pub(crate) static RX_CLOCK: LazyLock<Instant> = LazyLock::new(Instant::now);

/// (seq, arrival µs on RX_CLOCK) for video-RTP datagrams, capture order.
/// Pushed by the transport's receive drain, popped by the TWCC record site.
static TWCC_ARRIVALS: LazyLock<Mutex<VecDeque<(u16, u64)>>> =
    LazyLock::new(|| Mutex::new(VecDeque::with_capacity(512)));

/// Transport side: staple a video-RTP datagram's capture instant to its seq.
pub(crate) fn offer_twcc_arrival(seq: u16, at_us: u64) {
    if let Ok(mut fifo) = TWCC_ARRIVALS.lock() {
        if fifo.len() >= 512 {
            fifo.pop_front();
        }
        fifo.push_back((seq, at_us));
    }
}

/// TWCC record side: the arrival stamp for `seq`. Pops stale heads (the fork
/// may drop a datagram post-SRTP); falls back to SHADOW_CLOCK when nothing
/// matches, so fidelity degrades gracefully instead of mis-stamping.
pub(crate) fn take_twcc_arrival(seq: u16) -> i64 {
    let shadow = SHADOW_CLOCK.elapsed().as_micros() as i64;
    let Ok(mut fifo) = TWCC_ARRIVALS.lock() else {
        return shadow;
    };
    while let Some(&(head_seq, at_us)) = fifo.front() {
        if head_seq == seq {
            fifo.pop_front();
            return at_us as i64;
        }
        let ahead = seq.wrapping_sub(head_seq);
        if ahead > 0 && ahead <= 128 {
            // Head is older and will never be asked for again — evict.
            fifo.pop_front();
            continue;
        }
        break;
    }
    shadow
}

/// FNV-1a — deterministic byte hash for legacy↔new AU parity comparison.
fn hash_bytes(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
// 8b (§6.3): `Abandoned`/`Invalid` are legacy classes — their only production
// constructors were the deleted legacy arms; comparator tests still build them.
#[allow(dead_code)]
enum ShadowClass {
    /// Legacy `FrameAssembly::Complete` / new `AssemblyOutcome::Complete`.
    Complete,
    /// New `AssemblyOutcome::Damaged` (legacy has no equivalent class).
    Damaged,
    /// New `AssemblyOutcome::Discarded` (oversize/malformed).
    Discarded,
    /// Legacy force-out of an incomplete frame on a newer timestamp.
    Abandoned,
    /// Legacy `FrameAssembly::Invalid` (depacketize error / oversize).
    Invalid,
}

#[derive(Debug, Clone)]
struct ShadowRecord {
    ts: u32,
    class: ShadowClass,
    first_seq: u16,
    last_seq: u16,
    packet_count: u16,
    bytes_hash: Option<u64>,
    /// Mismatch-evidence (observe-only): arrival-order (seq, len, hash)
    /// digests of the packets this path kept for the AU. Populated on
    /// Complete records; empty for reject-class records.
    digests: Box<[mismatch_log::PacketDigest]>,
    /// Mismatch-evidence: the assembled output bytes (refcount clone on the
    /// legacy side, zero-copy adoption on the new side). Empty for rejects.
    data: bytes::Bytes,
}

impl ShadowRecord {
    /// Build a shadow-side record from a new-path outcome. `digests` is the
    /// arrival-ring evidence copy for Complete outcomes (empty otherwise).
    fn from_new(
        outcome: AssemblyOutcome,
        digests: Box<[mismatch_log::PacketDigest]>,
    ) -> Self {
        match outcome {
            AssemblyOutcome::Complete(desc) => {
                let data = bytes::Bytes::from(desc.data);
                ShadowRecord {
                    ts: desc.rtp_ts,
                    class: ShadowClass::Complete,
                    first_seq: desc.first_seq,
                    last_seq: desc.last_seq,
                    packet_count: desc.packet_count,
                    bytes_hash: Some(hash_bytes(&data)),
                    digests,
                    data,
                }
            }
            AssemblyOutcome::Damaged(desc) => ShadowRecord {
                ts: desc.rtp_ts,
                class: ShadowClass::Damaged,
                first_seq: desc.first_seq,
                last_seq: desc.last_seq,
                packet_count: desc.packet_count,
                bytes_hash: None,
                digests: Box::new([]),
                data: bytes::Bytes::new(),
            },
            AssemblyOutcome::Discarded { reason: _, rtp_ts } => ShadowRecord {
                ts: rtp_ts,
                class: ShadowClass::Discarded,
                first_seq: 0,
                last_seq: 0,
                packet_count: 0, // Discarded carries no packet bounds
                bytes_hash: None,
                digests: Box::new([]),
                data: bytes::Bytes::new(),
            },
        }
    }
}

/// Verdict for one paired (legacy, new) outcome tuple.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PairVerdict {
    Match,
    BytesMismatch,
    ClassMismatch,
    /// Expected-delta bucket: new Damaged where legacy completed (§3 — the
    /// H3 fix; in this base also reached by reorder-marker-early pairings).
    BucketDamagedOldComplete,
    /// Expected-delta bucket: new Complete where legacy abandoned (§3 —
    /// 12 ms hold / markerless force-out equivalence).
    BucketCompleteOldAbandon,
}

/// Pure classification of a paired outcome tuple (unit-tested).
fn classify_pair(legacy: &ShadowRecord, new_rec: &ShadowRecord) -> PairVerdict {
    use ShadowClass::*;
    match (legacy.class, new_rec.class) {
        (Complete, Complete) => {
            if legacy.bytes_hash == new_rec.bytes_hash {
                PairVerdict::Match
            } else {
                PairVerdict::BytesMismatch
            }
        }
        (Abandoned | Invalid, Damaged | Discarded) => PairVerdict::Match,
        (Complete, Damaged) => PairVerdict::BucketDamagedOldComplete,
        (Abandoned, Complete) => PairVerdict::BucketCompleteOldAbandon,
        _ => PairVerdict::ClassMismatch,
    }
}

#[derive(Debug)]
struct ShadowComparator {
    legacy: Vec<ShadowRecord>,
    new: Vec<ShadowRecord>,
}

impl ShadowComparator {
    fn new() -> Self {
        Self {
            legacy: Vec::new(),
            new: Vec::new(),
        }
    }

    /// 8b (§6.3): legacy-record ingestion — no production caller remains
    /// once the collector is deleted (the comparator's legacy side is inert
    /// by design post-switch; sh_* counters freeze instead of climbing
    /// sh_new_unmatched). Kept for the method's pairing logic and tests.
    #[allow(dead_code)]
    fn observe_legacy(
        &mut self,
        record: ShadowRecord,
        ring: &mismatch_log::ArrivalRing,
    ) {
        self.legacy.push(record);
        if self.legacy.len() > 64 {
            self.legacy.remove(0);
            METRICS
                .sh_legacy_unmatched
                .fetch_add(1, Ordering::Relaxed);
        }
        self.try_match(ring);
    }

    fn observe_new(
        &mut self,
        outcome: AssemblyOutcome,
        digests: Box<[mismatch_log::PacketDigest]>,
        ring: &mismatch_log::ArrivalRing,
    ) {
        let record = ShadowRecord::from_new(outcome, digests);
        self.new.push(record);
        if self.new.len() > 64 {
            self.new.remove(0);
            METRICS.sh_new_unmatched.fetch_add(1, Ordering::Relaxed);
        }
        self.try_match(ring);
    }

    /// Pair pending records by RTP timestamp and apply counters.
    fn try_match(&mut self, ring: &mismatch_log::ArrivalRing) {
        let mut i = 0;
        while i < self.legacy.len() {
            let ts = self.legacy[i].ts;
            if let Some(j) = self.new.iter().position(|record| record.ts == ts) {
                let new_rec = self.new.remove(j);
                let legacy_rec = self.legacy.remove(i);
                self.compare(&legacy_rec, &new_rec, ring);
            } else {
                i += 1;
            }
        }
    }

    fn compare(
        &mut self,
        legacy: &ShadowRecord,
        new_rec: &ShadowRecord,
        ring: &mismatch_log::ArrivalRing,
    ) {
        // Boundary comparison (§11.3a): gated only when at least one side is
        // Complete; both-reject pairs are exempt (their packet sets depend on
        // when each path stopped collecting — neither is submitted) and are
        // counted separately so they stay visible.
        if legacy.packet_count > 0 && new_rec.packet_count > 0 {
            let same_bounds = (legacy.first_seq, legacy.last_seq, legacy.packet_count)
                == (
                    new_rec.first_seq,
                    new_rec.last_seq,
                    new_rec.packet_count,
                );
            if !same_bounds {
                let both_reject = matches!(
                    legacy.class,
                    ShadowClass::Abandoned | ShadowClass::Invalid
                ) && matches!(
                    new_rec.class,
                    ShadowClass::Damaged | ShadowClass::Discarded
                );
                if both_reject {
                    METRICS
                        .sh_reject_bound_artifact
                        .fetch_add(1, Ordering::Relaxed);
                } else {
                    METRICS
                        .sh_bound_mismatch
                        .fetch_add(1, Ordering::Relaxed);
                }
            }
        }
        match classify_pair(legacy, new_rec) {
            PairVerdict::Match => {
                METRICS.sh_match.fetch_add(1, Ordering::Relaxed);
            }
            PairVerdict::BytesMismatch => {
                METRICS.sh_bytes_mismatch.fetch_add(1, Ordering::Relaxed);
                // Evidence snapshot (≤16/session, RAM, flushed at session
                // end): the only allocation point that fires on a mismatch.
                mismatch_log::capture(
                    legacy.ts,
                    legacy.first_seq,
                    legacy.last_seq,
                    legacy.packet_count,
                    &legacy.digests,
                    &new_rec.digests,
                    &legacy.data,
                    &new_rec.data,
                    legacy.bytes_hash,
                    new_rec.bytes_hash,
                    ring
                        .payload_blob_for(legacy.ts)
                        .as_ref()
                        .map(|(lens, bytes)| (lens.as_slice(), bytes.as_slice())),
                );
            }
            PairVerdict::ClassMismatch => {
                METRICS.sh_class_mismatch.fetch_add(1, Ordering::Relaxed);
            }
            PairVerdict::BucketDamagedOldComplete => {
                METRICS
                    .sh_new_damaged_old_complete
                    .fetch_add(1, Ordering::Relaxed);
            }
            PairVerdict::BucketCompleteOldAbandon => {
                METRICS
                    .sh_new_complete_old_abandon
                    .fetch_add(1, Ordering::Relaxed);
            }
        }
    }
}

#[cfg(test)]
mod shadow_tests {
    use super::*;
    use crate::pipeline::media::assembler::DiscardReason;

    fn rec(ts: u32, class: ShadowClass, hash: Option<u64>) -> ShadowRecord {
        ShadowRecord {
            ts,
            class,
            first_seq: 10,
            last_seq: 12,
            packet_count: 3,
            bytes_hash: hash,
            digests: Box::new([]),
            data: bytes::Bytes::new(),
        }
    }

    #[test]
    fn classify_complete_pair_matches_on_equal_hash() {
        let l = rec(1, ShadowClass::Complete, Some(42));
        let n = rec(1, ShadowClass::Complete, Some(42));
        assert_eq!(classify_pair(&l, &n), PairVerdict::Match);
        let n_bad = rec(1, ShadowClass::Complete, Some(43));
        assert_eq!(classify_pair(&l, &n_bad), PairVerdict::BytesMismatch);
    }

    #[test]
    fn classify_legacy_reject_vs_new_reject_matches() {
        let abandoned = rec(2, ShadowClass::Abandoned, None);
        let damaged = rec(2, ShadowClass::Damaged, None);
        assert_eq!(
            classify_pair(&abandoned, &damaged),
            PairVerdict::Match
        );
        let invalid = rec(3, ShadowClass::Invalid, None);
        let discarded = rec(3, ShadowClass::Discarded, None);
        assert_eq!(
            classify_pair(&invalid, &discarded),
            PairVerdict::Match
        );
    }

    #[test]
    fn classify_expected_delta_buckets() {
        // Bucket 1: new Damaged where legacy completed (H3 fix).
        let complete = rec(4, ShadowClass::Complete, Some(7));
        let damaged = rec(4, ShadowClass::Damaged, None);
        assert_eq!(
            classify_pair(&complete, &damaged),
            PairVerdict::BucketDamagedOldComplete
        );
        // Bucket 2: new Complete where legacy abandoned (12 ms hold).
        let abandoned = rec(5, ShadowClass::Abandoned, None);
        let complete_new = rec(5, ShadowClass::Complete, Some(9));
        assert_eq!(
            classify_pair(&abandoned, &complete_new),
            PairVerdict::BucketCompleteOldAbandon
        );
    }

    #[test]
    fn classify_unexpected_pair_is_class_mismatch() {
        let complete = rec(6, ShadowClass::Complete, Some(1));
        let discarded = rec(6, ShadowClass::Discarded, None);
        assert_eq!(
            classify_pair(&complete, &discarded),
            PairVerdict::ClassMismatch
        );
        // (Invalid, Complete) is NOT bucket 2 — only Abandoned is.
        let invalid = rec(7, ShadowClass::Invalid, None);
        let complete_new = rec(7, ShadowClass::Complete, Some(2));
        assert_eq!(
            classify_pair(&invalid, &complete_new),
            PairVerdict::ClassMismatch
        );
    }

    #[test]
    fn record_from_new_outcome_maps_classes_and_ts() {
        let mut assembler = AccessUnitAssembler::new();
        let payload = [0x01u8, 0x00, 0x00];
        let outcome = assembler
            .push(10, 90_000, true, &payload, 1_000)
            .expect("marker resolves");
        let record = ShadowRecord::from_new(outcome, Box::new([]));
        assert_eq!(record.class, ShadowClass::Complete);
        assert_eq!(record.ts, 90_000);
        assert_eq!((record.first_seq, record.last_seq), (10, 10));
        assert!(record.bytes_hash.is_some());

        let discarded = ShadowRecord::from_new(
            AssemblyOutcome::Discarded {
                reason: DiscardReason::Malformed,
                rtp_ts: 180_000,
            },
            Box::new([]),
        );
        assert_eq!(discarded.class, ShadowClass::Discarded);
        assert_eq!(discarded.ts, 180_000);
    }

    #[test]
    fn hash_bytes_is_stable_and_discriminating() {
        assert_eq!(hash_bytes(&[1, 2, 3]), hash_bytes(&[1, 2, 3]));
        assert_ne!(hash_bytes(&[1, 2, 3]), hash_bytes(&[1, 2, 4]));
    }
}

// ---------------------------------------------------------------------------
// Differential fixtures — STAGE8-REDO-SPEC §5.1/§5.2.
//
// Each fixture replays an identical packet stream through (a) the LEGACY
// authority (PendingVideoFrame + H264Packet, mirroring receive()'s control
// flow) and (b) the new AccessUnitAssembler, asserting byte-identical AUs and
// boundaries wherever legacy completes. The two sanctioned expected-delta
// buckets (§3) are asserted explicitly; classification parity for the reject
// class (legacy Invalid ↔ new Discarded) is asserted where relevant.
// ---------------------------------------------------------------------------
#[cfg(test)]
mod diff_fixtures {
    use super::*;
    use crate::pipeline::media::assembler::{
        AccessUnitAssembler, AssemblyOutcome, DiscardReason,
    };
    use rtc::rtp::Header;

    const TS0: u32 = 90_000;
    const TS1: u32 = 180_000;

    #[derive(Debug, PartialEq)]
    pub(crate) enum LegacyEvent {
        Complete { at: usize, ts: u32, data: Vec<u8> },
        Abandoned { at: usize, ts: u32 },
        Invalid { at: usize, ts: u32 },
    }

    pub(crate) fn pkt(seq: u16, ts: u32, marker: bool, payload: &[u8]) -> Packet {
        Packet {
            header: Header {
                sequence_number: seq,
                timestamp: ts,
                marker,
                ..Default::default()
            },
            payload: Bytes::copy_from_slice(payload),
        }
    }

    /// Legacy-valid single NAL: >= 3 bytes (legacy depacketize returns
    /// ErrShortPacket for <= 2-byte non-AUD payloads).
    fn single_nal(nal_type: u8, fill: u8) -> Vec<u8> {
        vec![nal_type, fill, fill.wrapping_add(1)]
    }

    fn stap_a(nals: &[&[u8]]) -> Vec<u8> {
        let mut payload = vec![0x78]; // nri=3, type=24 (STAP-A)
        for nal in nals {
            payload.extend_from_slice(&(nal.len() as u16).to_be_bytes());
            payload.extend_from_slice(nal);
        }
        payload
    }

    /// Split `nal` into FU-A fragments of `frag_size` body bytes, sequenced
    /// from `first_seq` (marker left false; caller sets it).
    fn fu_packets(nal: &[u8], first_seq: u16, frag_size: usize) -> Vec<Packet> {
        let indicator = (nal[0] & 0x60) | 28; // preserve nri, type = FU-A
        let body = &nal[1..];
        let chunks: Vec<&[u8]> = body.chunks(frag_size).collect();
        let total = chunks.len();
        let mut seq = first_seq;
        let mut out = Vec::new();
        for (k, chunk) in chunks.iter().enumerate() {
            let mut fu_header = nal[0] & 0x1F;
            if k == 0 {
                fu_header |= 0x80; // start
            }
            if k + 1 == total {
                fu_header |= 0x40; // end
            }
            let mut payload = vec![indicator, fu_header];
            payload.extend_from_slice(chunk);
            out.push(pkt(seq, 0, false, &payload)); // ts set by caller
            seq = seq.wrapping_add(1);
        }
        out
    }

    /// Mirror of receive()'s byte-relevant control flow: force-out on newer
    /// timestamp, stale-ts drop, last_frame gate, insert dedup, next_sequence
    /// tracking. Damage/PLI side effects are out of scope (no worker here).
    pub(crate) fn legacy_run(packets: &[Packet]) -> Vec<LegacyEvent> {
        let mut events = Vec::new();
        let mut pending: Option<PendingVideoFrame> = None;
        let mut depacketizer = H264Packet::default();
        let mut next_sequence: Option<u16> = None;
        let mut last_frame_ts: Option<u32> = None;
        for (i, packet) in packets.iter().enumerate() {
            if packet.payload.is_empty() {
                continue;
            }
            let ts = packet.header.timestamp;
            if let Some(existing) = &pending
                && existing.timestamp != ts
            {
                if !timestamp_is_newer(ts, existing.timestamp) {
                    continue; // receive(): stale packet dropped
                }
                let incomplete = pending.take().expect("pending frame");
                next_sequence = incomplete.marker_sequence().map(|s| s.wrapping_add(1));
                depacketizer = H264Packet::default();
                events.push(LegacyEvent::Abandoned {
                    at: i,
                    ts: incomplete.timestamp,
                });
            }
            if pending.is_none() {
                if last_frame_ts.is_some_and(|last| !timestamp_is_newer(ts, last)) {
                    continue; // receive(): last_frame_timestamp gate
                }
                pending = Some(PendingVideoFrame::new(packet.clone()));
            } else {
                pending.as_mut().expect("pending frame").insert(packet.clone());
            }
            match pending
                .as_ref()
                .expect("pending frame")
                .assemble(&mut depacketizer, next_sequence)
            {
                FrameAssembly::Pending => {}
                FrameAssembly::Invalid => {
                    pending = None;
                    next_sequence = None;
                    events.push(LegacyEvent::Invalid { at: i, ts });
                }
                FrameAssembly::Complete { data, marker_sequence } => {
                    pending = None;
                    next_sequence = Some(marker_sequence.wrapping_add(1));
                    last_frame_ts = Some(ts);
                    events.push(LegacyEvent::Complete {
                        at: i,
                        ts,
                        data: data.to_vec(),
                    });
                }
            }
        }
        events
    }

    /// Run packets through the new assembler (1 ms inter-arrival), draining
    /// the outcome queue after every push.
    fn new_run(packets: &[Packet]) -> Vec<(usize, AssemblyOutcome)> {
        // Byte-parity harness: grace parking OFF (defers outcomes; this
        // harness compares resolve events 1:1 with legacy).
        let mut assembler = AccessUnitAssembler::new_without_grace();
        let mut events = Vec::new();
        for (i, packet) in packets.iter().enumerate() {
            let arrival_us = 1_000 + i as u64 * 1_000;
            if let Some(outcome) = assembler.push(
                packet.header.sequence_number,
                packet.header.timestamp,
                packet.header.marker,
                &packet.payload,
                arrival_us,
            ) {
                events.push((i, outcome));
            }
            while let Some(outcome) = assembler.poll_outcome() {
                events.push((i, outcome));
            }
        }
        events
    }

    fn legacy_completes(events: &[LegacyEvent]) -> Vec<(usize, Vec<u8>)> {
        events
            .iter()
            .filter_map(|event| match event {
                LegacyEvent::Complete { at, data, .. } => Some((*at, data.clone())),
                _ => None,
            })
            .collect()
    }

    fn new_completes(events: &[(usize, AssemblyOutcome)]) -> Vec<(usize, Vec<u8>)> {
        events
            .iter()
            .filter_map(|(at, outcome)| match outcome {
                AssemblyOutcome::Complete(desc) => Some((*at, desc.data.clone())),
                _ => None,
            })
            .collect()
    }

    /// Core parity assertion: where legacy completes, the new path completes
    /// the same AU bytes at the same packet index — and vice versa.
    fn assert_parity(packets: &[Packet]) {
        let legacy = legacy_completes(&legacy_run(packets));
        let new = new_completes(&new_run(packets));
        assert_eq!(
            legacy, new,
            "new path must produce legacy-identical AU bytes and boundaries"
        );
    }

    // ---- §5.1 synthetic ----------------------------------------------------

    #[test]
    fn probe_annexb_start_code_is_four_bytes() {
        let packets = vec![pkt(10, TS0, true, &single_nal(0x01, 0xAA))];
        let legacy = legacy_run(&packets);
        let data = match &legacy[..] {
            [LegacyEvent::Complete { data, .. }] => data.clone(),
            other => panic!("legacy should complete a marker'd single NAL: {other:?}"),
        };
        assert_eq!(&data[..4], &[0, 0, 0, 1], "legacy emits 4-byte start codes");
        assert_eq!(&data[4..], &packets[0].payload[..], "NAL verbatim after start code");
        assert_parity(&packets);
    }

    #[test]
    fn parity_single_nal_p_frame() {
        assert_parity(&[pkt(10, TS0, true, &single_nal(0x01, 0x01))]);
    }

    #[test]
    fn parity_stap_a_sps_pps_split_in_order() {
        let sps: &[u8] = &[0x67, 0x42, 0x00, 0x1E, 0xAB];
        let pps: &[u8] = &[0x68, 0xCE, 0x06, 0xE2];
        let packets = vec![pkt(20, TS0, true, &stap_a(&[sps, pps]))];
        let legacy = legacy_run(&packets);
        let data = match &legacy[..] {
            [LegacyEvent::Complete { data, .. }] => data.clone(),
            other => panic!("legacy should complete STAP-A: {other:?}"),
        };
        let mut expect = vec![0, 0, 0, 1];
        expect.extend_from_slice(sps);
        expect.extend_from_slice(&[0, 0, 0, 1]);
        expect.extend_from_slice(pps);
        assert_eq!(data, expect, "SPS then PPS, each start-code framed");
        assert_parity(&packets);
    }

    #[test]
    fn parity_idr_fu_a_multi_fragment() {
        let mut idr = vec![0x65]; // nri=3, type=5 (IDR)
        idr.extend(0..40u8);
        let mut packets = fu_packets(&idr, 100, 10);
        assert!(packets.len() >= 3, "need a fragmented IDR");
        packets.last_mut().expect("non-empty").header.marker = true;
        for packet in &mut packets {
            packet.header.timestamp = TS0;
        }
        let legacy = legacy_run(&packets);
        let data = match &legacy[..] {
            [LegacyEvent::Complete { data, .. }] => data.clone(),
            other => panic!("legacy should complete fragmented IDR: {other:?}"),
        };
        let mut expect = vec![0, 0, 0, 1];
        expect.extend_from_slice(&idr);
        assert_eq!(data, expect, "FU-A reassembles to one start-coded IDR NAL");
        assert_parity(&packets);
        let new = new_run(&packets);
        match &new[0].1 {
            AssemblyOutcome::Complete(desc) => assert!(desc.is_idr, "IDR flag must be set"),
            other => panic!("expected Complete, got {other:?}"),
        }
    }

    #[test]
    fn parity_multi_packet_p_frame() {
        let packets = vec![
            pkt(30, TS0, false, &single_nal(0x01, 0x10)),
            pkt(31, TS0, false, &single_nal(0x01, 0x20)),
            pkt(32, TS0, true, &single_nal(0x01, 0x30)),
        ];
        assert_parity(&packets);
    }

    #[test]
    fn parity_sei_single() {
        let sei = [0x06, 0x05, 0x00, 0x80, 0x11];
        assert_parity(&[pkt(40, TS0, true, &sei)]);
    }

    // ---- §5.2 edge cases ---------------------------------------------------

    #[test]
    fn parity_reordered_within_frame_marker_last() {
        // Arrival order 31, 30, 32(marker): legacy sorts by seq around the
        // marker; the new path must apply the same key => identical bytes.
        let packets = vec![
            pkt(31, TS0, false, &single_nal(0x01, 0x20)),
            pkt(30, TS0, false, &single_nal(0x01, 0x10)),
            pkt(32, TS0, true, &single_nal(0x01, 0x30)),
        ];
        assert_parity(&packets);
        let legacy = legacy_run(&packets);
        let data = match &legacy[..] {
            [LegacyEvent::Complete { data, .. }] => data.clone(),
            other => panic!("{other:?}"),
        };
        // NALs must come out in SEQUENCE order (30, 31, 32), not arrival order.
        assert_eq!(&data[5], &0x10, "first NAL fill byte must be seq 30's");
    }

    #[test]
    fn bucket1_lost_middle_new_damaged_legacy_abandons() {
        let packets = vec![
            pkt(50, TS0, false, &single_nal(0x01, 0x10)),
            // seq 51 lost
            pkt(52, TS0, true, &single_nal(0x01, 0x30)),
            pkt(53, TS1, true, &single_nal(0x01, 0x40)), // frame 2
        ];
        let legacy = legacy_run(&packets);
        // Only frame 2 may complete on legacy (it starts exactly at frame-1's
        // marker + 1); the gapped frame 1 must never be submitted.
        let legacy_complete_indices: Vec<usize> = legacy_completes(&legacy)
            .iter()
            .map(|(at, _)| *at)
            .collect();
        assert_eq!(
            legacy_complete_indices,
            vec![2],
            "legacy must never submit the gapped frame 1: {legacy:?}"
        );
        assert!(
            legacy.contains(&LegacyEvent::Abandoned { at: 2, ts: TS0 }),
            "legacy abandons frame 1 on force-out: {legacy:?}"
        );
        let new = new_run(&packets);
        match &new[0].1 {
            AssemblyOutcome::Damaged(desc) => {
                assert_eq!(desc.holes, 1, "one missing sequence number");
                assert!(desc.data.is_empty(), "Damaged must carry no bytes (§4)");
            }
            other => panic!("expected Damaged for frame 1, got {other:?}"),
        }
        // Frame 2 (seq starts exactly at frame-1 marker + 1) completes
        // identically on both paths.
        assert_eq!(legacy_completes(&legacy), new_completes(&new));
    }

    #[test]
    fn bucket1_lost_first_fu_start_never_submitted() {
        let packets = vec![
            pkt(70, TS0, false, &[0x7C, 0x05, 0xAA, 0xBB]), // FU continuation (no start)
            pkt(71, TS0, true, &[0x7C, 0x45, 0xCC, 0xDD]),  // FU end fragment
            pkt(72, TS1, true, &single_nal(0x01, 0x40)),    // frame 2
        ];
        let legacy = legacy_run(&packets);
        assert!(
            legacy_completes(&legacy).len() == 1,
            "only frame 2 may complete on legacy: {legacy:?}"
        );
        assert!(legacy.contains(&LegacyEvent::Abandoned { at: 2, ts: TS0 }));
        let new = new_run(&packets);
        match &new[0].1 {
            AssemblyOutcome::Damaged(desc) => {
                assert!(!desc.had_first_slice, "H3: unverified start");
                assert!(desc.data.is_empty(), "§4: no bytes from Damaged");
            }
            other => panic!("expected Damaged, got {other:?}"),
        }
        assert_eq!(legacy_completes(&legacy), new_completes(&new));
    }

    #[test]
    fn markerless_frame_damaged_on_forceout_never_completes() {
        let packets = vec![
            pkt(80, TS0, false, &single_nal(0x01, 0x10)), // frame 1, marker lost
            pkt(81, TS1, true, &single_nal(0x01, 0x20)),  // frame 2
        ];
        let legacy = legacy_run(&packets);
        assert!(
            legacy.contains(&LegacyEvent::Abandoned { at: 1, ts: TS0 }),
            "legacy abandons the markerless frame: {legacy:?}"
        );
        let new = new_run(&packets);
        // §11 rule 2: the markerless frame resolves Damaged at the force-out —
        // never Complete (the old Complete-on-forceout was the hold defect).
        match &new[0].1 {
            AssemblyOutcome::Damaged(desc) => {
                assert!(
                    desc.data.is_empty(),
                    "§4: Damaged carries no bytes"
                );
            }
            other => panic!("markerless force-out must be Damaged, got {other:?}"),
        }
        // Both paths submit exactly frame 2, byte-identical.
        let legacy_f2 = legacy_completes(&legacy);
        let new_all = new_completes(&new);
        assert_eq!(
            legacy_f2.len(),
            1,
            "legacy completes only frame 2: {legacy:?}"
        );
        assert_eq!(
            new_all.len(),
            1,
            "new completes only frame 2: {new:?}"
        );
        assert_eq!(
            legacy_f2[0],
            new_all[0],
            "frame 2 bytes/boundary parity"
        );
    }

    #[test]
    fn markerless_resolves_damaged_only_at_newer_ts() {
        // §11 rule 2 (amended 2026-10-30): a timer NEVER resolves a
        // markerless AU — flush_due has no markerless deadline at all. The
        // ~40 ms backstop was retired: the marker is a frame's last byte, so
        // slow/stalled links routinely deliver it late and the backstop killed
        // healthy frames mid-transmission, arming rule 4 against their own
        // catch-up packets (8B-GATE-REPORT §3). Markerless AUs now resolve
        // Damaged only on a newer-timestamp force-out — exactly legacy, whose
        // force-out was a stub.
        let payload = single_nal(0x01, 0x10);
        let mut assembler = AccessUnitAssembler::new();
        assert!(assembler.push(90, TS0, false, &payload, 100_000).is_none());
        for t in [113_000, 139_000, 141_000, 500_000, 5_000_000] {
            assert!(
                assembler.flush_due(t).is_none(),
                "markerless must never resolve on a timer ({t} µs)"
            );
        }
        // Legacy parity: a markerless frame never completes by itself.
        assert!(
            legacy_run(&[pkt(90, TS0, false, &payload)]).is_empty(),
            "legacy never completes a markerless frame by itself"
        );
        // A newer timestamp force-outs the markerless AU -> Damaged, no bytes.
        let payload2 = single_nal(0x01, 0x20);
        let direct = assembler.push(91, TS0.wrapping_add(9_000), false, &payload2, 5_100_000);
        let outcome = direct.or_else(|| assembler.poll_outcome());
        match outcome {
            Some(AssemblyOutcome::Damaged(desc)) => {
                assert_eq!(desc.rtp_ts, TS0, "the older markerless AU resolves");
                assert!(desc.data.is_empty(), "§4: Damaged carries no bytes");
                assert!(!desc.marker_seen, "markerless by definition");
            }
            other => panic!("newer-ts force-out must be Damaged, got {other:?}"),
        }
    }

    #[test]
    fn parity_duplicate_packet_mid_frame() {
        let packets = vec![
            pkt(100, TS0, false, &single_nal(0x01, 0x10)),
            pkt(101, TS0, false, &single_nal(0x01, 0x20)),
            pkt(101, TS0, false, &single_nal(0x01, 0x20)), // duplicate
            pkt(102, TS0, true, &single_nal(0x01, 0x30)),
        ];
        assert_parity(&packets);
        let new = new_run(&packets);
        match &new[0].1 {
            AssemblyOutcome::Complete(desc) => assert_eq!(desc.packet_count, 3, "dedup"),
            other => panic!("expected Complete, got {other:?}"),
        }
    }

    #[test]
    fn parity_sequence_wrap_across_frames() {
        let packets = vec![
            pkt(65_534, TS0, false, &single_nal(0x01, 0x10)),
            pkt(65_535, TS0, true, &single_nal(0x01, 0x20)),
            pkt(0, TS1, false, &single_nal(0x01, 0x30)), // wraps; expected seq = 0
            pkt(1, TS1, true, &single_nal(0x01, 0x40)),
        ];
        assert_parity(&packets);
        for (at, outcome) in new_run(&packets) {
            if let AssemblyOutcome::Damaged(desc) = outcome {
                panic!("no holes expected (at {at}): {desc:?}");
            }
        }
    }

    #[test]
    fn parity_timestamp_wrap_between_frames() {
        let packets = vec![
            pkt(10, 0xFFFF_FF00, true, &single_nal(0x01, 0x10)),
            pkt(11, 0x0000_1000, true, &single_nal(0x01, 0x20)), // u32 wrap
        ];
        assert_parity(&packets);
    }

    #[test]
    fn reject_oversize_is_rejected_by_both_paths() {
        let big = vec![0x01u8; 100_000];
        let mut packets = Vec::new();
        for i in 0..21u16 {
            let marker = i == 20;
            packets.push(pkt(200 + i, TS0, marker, &big));
        }
        let legacy = legacy_run(&packets);
        assert!(
            legacy_completes(&legacy).is_empty(),
            "legacy must not submit oversize: {legacy:?}"
        );
        assert!(
            legacy.contains(&LegacyEvent::Invalid { at: 20, ts: TS0 }),
            "legacy rejects at the marker packet: {legacy:?}"
        );
        let new = new_run(&packets);
        assert!(
            new_completes(&new).is_empty(),
            "new must not submit oversize: {new:?}"
        );
        assert!(
            new.iter().any(|(_, outcome)| matches!(
                outcome,
                AssemblyOutcome::Discarded {
                    reason: DiscardReason::Oversize,
                    ..
                }
            )),
            "new classifies oversize: {new:?}"
        );
    }

    #[test]
    fn classification_malformed_stap_maps_legacy_invalid() {
        // STAP-A whose declared size overruns the buffer → legacy depacketize
        // Err → Invalid; new → Discarded{Malformed}. Neither submits bytes.
        let bad_stap = [0x78, 0xFF, 0xFF, 0x67, 0x42];
        let packets = vec![pkt(110, TS0, true, &bad_stap)];
        assert_eq!(
            legacy_run(&packets),
            vec![LegacyEvent::Invalid { at: 0, ts: TS0 }],
            "legacy Invalid for malformed STAP-A"
        );
        assert!(
            matches!(
                new_run(&packets).as_slice(),
                [(0, AssemblyOutcome::Discarded {
                    reason: DiscardReason::Malformed,
                    ..
                })]
            ),
            "new Discarded{{Malformed}} for the same payload"
        );
    }

    #[test]
    fn parity_lost_marker_followed_by_next_timestamp() {
        // §5.2: marker-less frame followed by next timestamp — legacy force-out
        // path with an in-sequence follower (no expected-sequence mismatch).
        let packets = vec![
            pkt(120, TS0, false, &single_nal(0x01, 0x10)), // no marker
            pkt(121, TS0, false, &single_nal(0x01, 0x20)),
            pkt(122, TS1, true, &single_nal(0x01, 0x30)), // frame 2
        ];
        let legacy = legacy_run(&packets);
        assert!(
            legacy.contains(&LegacyEvent::Abandoned { at: 2, ts: TS0 }),
            "legacy force-outs the markerless frame: {legacy:?}"
        );
        let new = new_run(&packets);
        // §11 rule 2: markerless frame 1 resolves Damaged at the force-out
        // (never Complete); only frame 2 is submitted — byte-identical.
        match &new[0].1 {
            AssemblyOutcome::Damaged(desc) => {
                assert!(desc.data.is_empty(), "§4: Damaged carries no bytes");
            }
            other => panic!("markerless force-out must be Damaged, got {other:?}"),
        }
        let legacy_f2 = legacy_completes(&legacy);
        let new_all = new_completes(&new);
        assert_eq!(
            legacy_f2.len(),
            1,
            "legacy completes only frame 2: {legacy:?}"
        );
        assert_eq!(
            new_all.len(),
            1,
            "new completes only frame 2: {new:?}"
        );
        assert_eq!(legacy_f2[0], new_all[0], "frame 2 bytes/boundary parity");
    }
}

// Mismatch-evidence capture (8b investigation): RAM-buffered snapshots of
// sh_bytes_mismatch events, flushed at session end from metrics_log.
#[path = "mismatch_log.rs"]
pub(crate) mod mismatch_log;

// Real-capture fixture replay (§5.3) — separate file, child of this module so
// it can reuse the legacy harness above.
#[cfg(test)]
#[path = "diff_replay.rs"]
mod diff_replay;

#[cfg(test)]
mod twcc_arrival_tests {
    use super::{TWCC_ARRIVALS, take_twcc_arrival};

    #[test]
    fn exact_seq_match_returns_stapled_arrival() {
        let mut fifo = TWCC_ARRIVALS.lock().unwrap();
        fifo.clear();
        fifo.push_back((100, 111_111));
        drop(fifo);
        assert_eq!(take_twcc_arrival(100), 111_111);
    }

    #[test]
    fn stale_heads_are_evicted_until_match() {
        let mut fifo = TWCC_ARRIVALS.lock().unwrap();
        fifo.clear();
        // 50 was dropped before its consumer ever ran (fork discard) — the
        // request for 60 must evict it, not mis-stamp 60 with 50's arrival.
        fifo.push_back((50, 500));
        fifo.push_back((60, 600));
        drop(fifo);
        assert_eq!(take_twcc_arrival(60), 600);
        assert!(TWCC_ARRIVALS.lock().unwrap().is_empty());
    }

    #[test]
    fn unmatched_seq_falls_back_instead_of_stealing() {
        let mut fifo = TWCC_ARRIVALS.lock().unwrap();
        fifo.clear();
        fifo.push_back((5000, 999));
        drop(fifo);
        // 5000 is far AHEAD of 100 (not stale) — must not consume it.
        let fallback = take_twcc_arrival(100);
        assert_eq!(TWCC_ARRIVALS.lock().unwrap().len(), 1);
        assert!(fallback > 0); // SHADOW_CLOCK-domain µs
        TWCC_ARRIVALS.lock().unwrap().clear();
    }
}