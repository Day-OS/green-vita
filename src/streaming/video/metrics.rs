use std::sync::{Arc, LazyLock, Mutex};
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};

/// Snapshot of per-second metrics drained from METRICS at status() time.
/// Readings are zeroed after drain so the next status() call starts fresh.
#[derive(Debug, Clone)]
pub(crate) struct MetricsSnapshot {
    pub(crate) rtp_assembly_sum_us: u64,
    pub(crate) rtp_assembly_count: u64,
    pub(crate) rtp_assembly_max_us: u64,
    pub(crate) decode_us: u64,
    pub(crate) pipeline_age_us: u64,
    pub(crate) decoded: u64,
    pub(crate) skipped: u64,
    pub(crate) rate_limited: u64,
    pub(crate) presented: u64,
    pub(crate) replaced: u64,
    pub(crate) queue_full: u64,
    pub(crate) resyncs: u64,
    pub(crate) rtcp_sr_count: u64,
    pub(crate) rtcp_sr_raw: u64,
    pub(crate) rtcp_nack_sent: u64,
    pub(crate) rtx_packets_received: u64,
    pub(crate) rtx_crate_seen: u64,
    pub(crate) resets: u64,
    pub(crate) hw_buffered: u64,
    pub(crate) pipeline_age_max_us: u64,
    pub(crate) stream_lag_us: u64,
    pub(crate) stream_lag_max_us: u64,
    pub(crate) stream_slip_us: i64,
    pub(crate) stream_slip_max_us: u64,
    pub(crate) stream_slip_slope_mpm: i64,
    pub(crate) audio_queue_ms: u64,
    pub(crate) q_us: i64,
    pub(crate) q_us_max: u64,
    pub(crate) q_slope_mpm: i64,
    pub(crate) vt_exit_count: u64,
    pub(crate) vt_max_0: u64,
    pub(crate) vt_max_1: u64,
    pub(crate) vt_max_2: u64,
    pub(crate) vt_max_3: u64,
    pub(crate) vt_max_4: u64,
    pub(crate) vt_max_5: u64,
    pub(crate) rtp_hold_saved: u64,
    pub(crate) rtp_hold_expired: u64,
    pub(crate) rtp_gaps: u64,
    pub(crate) rtp_abandons: u64,
    pub(crate) rtp_wait_drops: u64,
    pub(crate) rtp_invalid: u64,
    pub(crate) dec_queue_full: u64,
    pub(crate) flushes: u64,
    pub(crate) twcc_sent: u64,
    pub(crate) twcc_samples: u64,
    pub(crate) rtp_ext_any: u64,
    pub(crate) twcc_stride: u64,
    pub(crate) video_metadata_sent: u64,
    pub(crate) video_bandwidth_kbps: u64,
    pub(crate) display_pickup_us: u64,
    pub(crate) display_pickup_max_us: u64,
    pub(crate) twcc_delay_med_us: u64,
    pub(crate) twcc_delay_max_us: u64,
    pub(crate) sr_age_ms: i64,
    pub(crate) sr_clock_offset_ms: i64,
    pub(crate) capture_to_arrival_us: i64,
    pub(crate) capture_to_arrival_max_us: i64,
    pub(crate) sr_anchor_local_us: u64,
    pub(crate) sr_anchor_local_wall_us: u64,
    pub(crate) sh_match: u64,
    pub(crate) sh_bound_mismatch: u64,
    pub(crate) sh_bytes_mismatch: u64,
    pub(crate) sh_class_mismatch: u64,
    pub(crate) sh_new_damaged_old_complete: u64,
    pub(crate) sh_new_complete_old_abandon: u64,
    pub(crate) sh_hold_saved: u64,
    pub(crate) sh_legacy_unmatched: u64,
    pub(crate) sh_new_unmatched: u64,
    pub(crate) au_submitted: u64,
    pub(crate) au_no_startcode: u64,
    pub(crate) au_idr_submitted: u64,
    pub(crate) sh_reject_bound_artifact: u64,
    pub(crate) late_after_resolve: u64,
    // §11.4 amendment re-gate: Damaged outcome clause splits (a case may
    // increment several; mirrors the bucket-1 histogram in 8B-GATE-REPORT §3).
    pub(crate) dam_markerless: u64,
    pub(crate) dam_first_slice: u64,
    pub(crate) dam_holes: u64,
    // 2c diagnostics:
    pub(crate) rtp_ext_twcc: u64,   // packets whose negotiated TWCC ext id parsed
    pub(crate) rtcp_rtpfb_any: u64, // any RTCP PT=205 that left the socket
    pub(crate) grace_held: u64,     // times grace was granted for damaged-AU hold
    pub(crate) grace_filled: u64,   // held AU resolved complete
    pub(crate) grace_expired: u64,  // held AU resolved damaged
    // Cap-advisor provenance (constant per session; 0 = no probe ran).
    pub(crate) probe_rtt_ms_x10: u64,
    pub(crate) probe_dl_mbps_x10: u64,
    pub(crate) probe_suggest_kbps: u64,
    // Micro-jb (Step 5): boost slots at last submit, bank depth at last
    // submit, submits admitted under boost.
    pub(crate) jb_boost: u64,
    pub(crate) jb_depth: u64,
    pub(crate) jb_absorbed: u64,
    pub(crate) jb_target: u64,
    // Damaged AUs shed-to-IDR under the Quality/Balanced damage policies.
    pub(crate) damaged_dropped: u64,
    // PLI→IDR latency metrics (Task D): last-value and max-over-session.
    pub(crate) pli_idr_ms_last: u64,
    pub(crate) pli_idr_ms_max: u64,
    // Perf-plan stage 0 probes (2026-11-24): UI loop period and work time,
    // RX datagrams drained per wake (max), RTC-thread CPU in handle_read,
    // IDR bytes submitted, keyframe requests actually sent.
    pub(crate) ui_loop_us: u64,
    pub(crate) ui_work_us: u64,
    pub(crate) rx_drain_max: u64,
    pub(crate) rtc_cpu_us: u64,
    pub(crate) idr_bytes: u64,
    pub(crate) kf_req: u64,
    // Session constants (perf plan 2026-11-24), written once at stream start:
    // clocks the app runs at while streaming, and the kernel's default UDP
    // receive buffer. 0 = unknown / not read.
    pub(crate) clk_arm_mhz: u64,
    pub(crate) clk_bus_mhz: u64,
    pub(crate) clk_gpu_mhz: u64,
    pub(crate) clk_xbar_mhz: u64,
    pub(crate) rcvbuf_def_bytes: u64,
    // Pause probes (perf plan 2026-11-25), per-second maxima in microseconds:
    // longest interval between two polls of the socket by the receive thread,
    // longest interval between two RTC pump passes, and the longest single pump.
    pub(crate) rx_gap_max_us: u64,
    pub(crate) rtc_gap_max_us: u64,
    pub(crate) rtc_pump_max_us: u64,
    // Perf-plan stage 3: access units decoded into a scratch texture and
    // retired before display (a newer access unit was queued behind them).
    pub(crate) au_scratch: u64,
}

pub(crate) struct VideoMetrics {
    pub(crate) rtp_assembly_sum_us: AtomicU64,
    pub(crate) rtp_assembly_count: AtomicU64,
    pub(crate) rtp_assembly_max_us: AtomicU64,
    pub(crate) decode_us: AtomicU64,
    pub(crate) pipeline_age_us: AtomicU64,
    pub(crate) decoded: AtomicU64,
    pub(crate) skipped: AtomicU64,
    pub(crate) rate_limited: AtomicU64,
    pub(crate) presented: AtomicU64,
    pub(crate) replaced: AtomicU64,
    pub(crate) queue_full: AtomicU64,
    pub(crate) resyncs: AtomicU64,
    pub(crate) rtcp_sr_count: AtomicU64,
    pub(crate) rtcp_sr_raw: AtomicU64,
    pub(crate) rtcp_nack_sent: AtomicU64,
    pub(crate) rtx_packets_received: AtomicU64,
    pub(crate) rtx_crate_seen: AtomicU64,
    // Absolute latency metrics (Task D)
    // SR anchor timestamps and per-second derived metrics
    pub(crate) sr_anchor_local_us: AtomicU64,     // Local monotonic time when SR arrived (us since boot)
    pub(crate) sr_anchor_local_wall_us: AtomicU64, // Client wall clock at SR arrival (us since UNIX epoch)

    // Per-second absolute latency metrics (microseconds, signed i64)
    pub(crate) sr_age_ms: AtomicI64,              // Age of SR anchor in ms
    pub(crate) sr_clock_offset_ms: AtomicI64,     // Console unix-ms minus client wall-ms

    // True end-to-end capture-to-arrival latency (microseconds, signed i64)
    // Measures console capture queue + transport + assembly; NOT blind to pre-RTP-stamp queue
    pub(crate) capture_to_arrival_us: AtomicI64,
    pub(crate) capture_to_arrival_max_us: AtomicI64,
    pub(crate) resets: AtomicU64,
    pub(crate) hw_buffered: AtomicU64,
    pub(crate) pipeline_age_max_us: AtomicU64,
    pub(crate) stream_lag_us: AtomicU64,
    pub(crate) stream_lag_max_us: AtomicU64,
    pub(crate) stream_slip_us: AtomicI64,
    pub(crate) stream_slip_max_us: AtomicU64,
    pub(crate) stream_slip_slope_mpm: AtomicI64,
    pub(crate) audio_queue_ms: AtomicU64,
    pub(crate) q_us: AtomicI64,
    pub(crate) q_us_max: AtomicU64,
    pub(crate) q_slope_mpm: AtomicI64,
    pub(crate) vt_exit_count: u64,
    pub(crate) vt_max_0: u64,
    pub(crate) vt_max_1: u64,
    pub(crate) vt_max_2: u64,
    pub(crate) vt_max_3: u64,
    pub(crate) vt_max_4: u64,
    pub(crate) vt_max_5: u64,
    pub(crate) rtp_hold_saved: AtomicU64,
    pub(crate) rtp_hold_expired: AtomicU64,
    pub(crate) rtp_gaps: AtomicU64,
    pub(crate) rtp_abandons: AtomicU64,
    pub(crate) rtp_wait_drops: AtomicU64,
    pub(crate) rtp_invalid: AtomicU64,
    pub(crate) dec_queue_full: AtomicU64,
    pub(crate) flushes: AtomicU64,
    pub(crate) twcc_sent: AtomicU64,
    pub(crate) twcc_samples: AtomicU64,
    pub(crate) rtp_ext_any: AtomicU64,
    pub(crate) twcc_stride: AtomicU64,
    pub(crate) video_metadata_sent: AtomicU64,
    pub(crate) video_bandwidth_kbps: AtomicU64,
    pub(crate) display_pickup_us: AtomicU64,
    pub(crate) display_pickup_max_us: AtomicU64,
    pub(crate) twcc_delay_med_us: AtomicU64,
    pub(crate) twcc_delay_max_us: AtomicU64,
    pub(crate) sh_match: AtomicU64,
    pub(crate) sh_bound_mismatch: AtomicU64,
    pub(crate) sh_bytes_mismatch: AtomicU64,
    pub(crate) sh_class_mismatch: AtomicU64,
    pub(crate) sh_new_damaged_old_complete: AtomicU64,
    pub(crate) sh_new_complete_old_abandon: AtomicU64,
    pub(crate) sh_hold_saved: AtomicU64,
    pub(crate) sh_legacy_unmatched: AtomicU64,
    pub(crate) sh_new_unmatched: AtomicU64,
    pub(crate) au_submitted: AtomicU64,
    pub(crate) au_no_startcode: AtomicU64,
    pub(crate) au_idr_submitted: AtomicU64,
    pub(crate) sh_reject_bound_artifact: AtomicU64,
    pub(crate) late_after_resolve: AtomicU64,
    pub(crate) dam_markerless: AtomicU64,
    pub(crate) dam_first_slice: AtomicU64,
    pub(crate) dam_holes: AtomicU64,
    pub(crate) rtp_ext_twcc: AtomicU64,
    pub(crate) rtcp_rtpfb_any: AtomicU64,
    // Grace hold metrics for damaged-AU grace hold feature:
    pub(crate) grace_held: AtomicU64,
    pub(crate) grace_filled: AtomicU64,
    pub(crate) grace_expired: AtomicU64,
    // Cap-advisor provenance (set at stream start, NOT reset mid-session).
    pub(crate) probe_rtt_ms_x10: AtomicU64,
    pub(crate) probe_dl_mbps_x10: AtomicU64,
    pub(crate) probe_suggest_kbps: AtomicU64,
    // Micro-jb (Step 5): boost/depth are last-value stores, absorbed counts.
    pub(crate) jb_boost: AtomicU64,
    pub(crate) jb_depth: AtomicU64,
    pub(crate) jb_absorbed: AtomicU64,
    pub(crate) jb_target: AtomicU64,
    // PLI→IDR latency metrics (Task D): last-value and max-over-session.
    pub(crate) pli_idr_ms_last: AtomicU64,
    pub(crate) pli_idr_ms_max: AtomicU64,
    /// Damaged AUs shed-to-IDR under the Quality/Balanced damage policies.
    pub(crate) damaged_dropped: AtomicU64,
    // Perf-plan stage 0 probes (2026-11-24).
    pub(crate) ui_loop_us: AtomicU64,
    pub(crate) ui_work_us: AtomicU64,
    pub(crate) rx_drain_max: AtomicU64,
    pub(crate) rtc_cpu_us: AtomicU64,
    pub(crate) idr_bytes: AtomicU64,
    pub(crate) kf_req: AtomicU64,
    // Session constants (perf plan 2026-11-24); see MetricsSnapshot.
    pub(crate) clk_arm_mhz: AtomicU64,
    pub(crate) clk_bus_mhz: AtomicU64,
    pub(crate) clk_gpu_mhz: AtomicU64,
    pub(crate) clk_xbar_mhz: AtomicU64,
    pub(crate) rcvbuf_def_bytes: AtomicU64,
    // Pause probes (perf plan 2026-11-25); see MetricsSnapshot.
    pub(crate) rx_gap_max_us: AtomicU64,
    pub(crate) rtc_gap_max_us: AtomicU64,
    pub(crate) rtc_pump_max_us: AtomicU64,
    // Perf-plan stage 3; see MetricsSnapshot.
    pub(crate) au_scratch: AtomicU64,
}

pub(crate) static METRICS: VideoMetrics = VideoMetrics {
    rtp_assembly_sum_us: AtomicU64::new(0),
    rtp_assembly_count: AtomicU64::new(0),
    rtp_assembly_max_us: AtomicU64::new(0),
    decode_us: AtomicU64::new(0),
    pipeline_age_us: AtomicU64::new(0),
    decoded: AtomicU64::new(0),
    skipped: AtomicU64::new(0),
    rate_limited: AtomicU64::new(0),
    presented: AtomicU64::new(0),
    replaced: AtomicU64::new(0),
    queue_full: AtomicU64::new(0),
    resyncs: AtomicU64::new(0),
    rtcp_sr_count: AtomicU64::new(0),
    rtcp_sr_raw: AtomicU64::new(0),
    rtcp_nack_sent: AtomicU64::new(0),
    rtx_packets_received: AtomicU64::new(0),
    rtx_crate_seen: AtomicU64::new(0),
    resets: AtomicU64::new(0),
    hw_buffered: AtomicU64::new(0),
    pipeline_age_max_us: AtomicU64::new(0),
    stream_lag_us: AtomicU64::new(0),
    stream_lag_max_us: AtomicU64::new(0),
    stream_slip_us: AtomicI64::new(0),
    stream_slip_max_us: AtomicU64::new(0),
    stream_slip_slope_mpm: AtomicI64::new(0),
    audio_queue_ms: AtomicU64::new(0),
    q_us: AtomicI64::new(0),
    q_us_max: AtomicU64::new(0),
    q_slope_mpm: AtomicI64::new(0),
    vt_exit_count: 0,
    vt_max_0: 0,
    vt_max_1: 0,
    vt_max_2: 0,
    vt_max_3: 0,
    vt_max_4: 0,
    vt_max_5: 0,
    rtp_hold_saved: AtomicU64::new(0),
    rtp_hold_expired: AtomicU64::new(0),
    rtp_gaps: AtomicU64::new(0),
    rtp_abandons: AtomicU64::new(0),
    rtp_wait_drops: AtomicU64::new(0),
    rtp_invalid: AtomicU64::new(0),
    dec_queue_full: AtomicU64::new(0),
    flushes: AtomicU64::new(0),
    twcc_sent: AtomicU64::new(0),
    twcc_samples: AtomicU64::new(0),
    rtp_ext_any: AtomicU64::new(0),
    twcc_stride: AtomicU64::new(0),
    video_metadata_sent: AtomicU64::new(0),
    video_bandwidth_kbps: AtomicU64::new(0),
    // Absolute latency metrics (Task D) - SR anchor and per-second derived metrics
    sr_anchor_local_us: AtomicU64::new(0),
    sr_anchor_local_wall_us: AtomicU64::new(0),
    sr_age_ms: AtomicI64::new(0),
    sr_clock_offset_ms: AtomicI64::new(0),
    capture_to_arrival_us: AtomicI64::new(0),
    capture_to_arrival_max_us: AtomicI64::new(0),
    display_pickup_us: AtomicU64::new(0),
    display_pickup_max_us: AtomicU64::new(0),
    twcc_delay_med_us: AtomicU64::new(0),
    twcc_delay_max_us: AtomicU64::new(0),
    sh_match: AtomicU64::new(0),
    sh_bound_mismatch: AtomicU64::new(0),
    sh_bytes_mismatch: AtomicU64::new(0),
    sh_class_mismatch: AtomicU64::new(0),
    sh_new_damaged_old_complete: AtomicU64::new(0),
    sh_new_complete_old_abandon: AtomicU64::new(0),
    sh_hold_saved: AtomicU64::new(0),
    sh_legacy_unmatched: AtomicU64::new(0),
    sh_new_unmatched: AtomicU64::new(0),
    au_submitted: AtomicU64::new(0),
    au_no_startcode: AtomicU64::new(0),
    au_idr_submitted: AtomicU64::new(0),
    sh_reject_bound_artifact: AtomicU64::new(0),
    late_after_resolve: AtomicU64::new(0),
    dam_markerless: AtomicU64::new(0),
    dam_first_slice: AtomicU64::new(0),
    dam_holes: AtomicU64::new(0),
    rtp_ext_twcc: AtomicU64::new(0),
    grace_held: AtomicU64::new(0),
    grace_filled: AtomicU64::new(0),
    grace_expired: AtomicU64::new(0),
    probe_rtt_ms_x10: AtomicU64::new(0),
    probe_dl_mbps_x10: AtomicU64::new(0),
    probe_suggest_kbps: AtomicU64::new(0),
    jb_boost: AtomicU64::new(0),
    jb_depth: AtomicU64::new(0),
    jb_absorbed: AtomicU64::new(0),
    jb_target: AtomicU64::new(0),
    // PLI→IDR latency metrics (Task D).
    pli_idr_ms_last: AtomicU64::new(0),
    pli_idr_ms_max: AtomicU64::new(0),
    damaged_dropped: AtomicU64::new(0),
    rtcp_rtpfb_any: AtomicU64::new(0),
    ui_loop_us: AtomicU64::new(0),
    ui_work_us: AtomicU64::new(0),
    rx_drain_max: AtomicU64::new(0),
    rtc_cpu_us: AtomicU64::new(0),
    idr_bytes: AtomicU64::new(0),
    kf_req: AtomicU64::new(0),
    clk_arm_mhz: AtomicU64::new(0),
    clk_bus_mhz: AtomicU64::new(0),
    clk_gpu_mhz: AtomicU64::new(0),
    clk_xbar_mhz: AtomicU64::new(0),
    rcvbuf_def_bytes: AtomicU64::new(0),
    rx_gap_max_us: AtomicU64::new(0),
    rtc_gap_max_us: AtomicU64::new(0),
    rtc_pump_max_us: AtomicU64::new(0),
    au_scratch: AtomicU64::new(0),
};



/// RAM-buffered CSV log for a streaming session.
/// Rows are accumulated at 1 Hz from status() and flushed once at session end.
/// Owned by the video module; flushed from RtcWorker::drop.
pub(crate) static METRICS_CSV_LOG: LazyLock<Mutex<Option<crate::streaming::video::metrics_log::MetricsCsvLog>>> =
    LazyLock::new(|| Mutex::new(None));

/// Initialize the RAM-buffered CSV log for a new streaming session.
/// Sets the global static to a freshly opened log (header-on-open semantics).
/// Called once from RtcSession::new so the first CSV rows cannot report
/// the previous session's values.
pub(crate) fn init_csv_log() {
    // Flush any previous session's log that never got flushed, then replace with a fresh log.
    if let Some(old) = METRICS_CSV_LOG.lock().unwrap().take() {
        old.flush();
    }
    *METRICS_CSV_LOG.lock().unwrap() = Some(crate::streaming::video::metrics_log::MetricsCsvLog::new());
}

pub fn video_performance_summary() -> String {
    // Use load() instead of swap(0) to read current metrics without draining.
    // The actual draining is handled by drain_metrics() in the status() method.
    let rtp_sum = METRICS.rtp_assembly_sum_us.load(Ordering::Relaxed);
    let rtp_count = METRICS.rtp_assembly_count.load(Ordering::Relaxed);
    let rtp_average = rtp_sum.checked_div(rtp_count).unwrap_or(0);
    let rtp_max = METRICS.rtp_assembly_max_us.load(Ordering::Relaxed);
    // Two lines: the HUD label clips long single-line output on the Vita panel.
    let line1 = format!(
        "d/p:{}/{} d/a:{}/{} skip:{} cap:{} repl:{} q:{} hwb:{} ageMax:{} lag:{} lagMax:{} bw:{} aq:{} slip:{} slipS:{} qS:{} cta:{} ctaMax:{} srAge:{} srCoff:{}",
        METRICS.decoded.load(Ordering::Relaxed),
        METRICS.presented.load(Ordering::Relaxed),
        METRICS.decode_us.load(Ordering::Relaxed) / 1000,
        METRICS.pipeline_age_us.load(Ordering::Relaxed) / 1000,
        METRICS.skipped.load(Ordering::Relaxed),
        METRICS.rate_limited.load(Ordering::Relaxed),
        METRICS.replaced.load(Ordering::Relaxed),
        METRICS.q_us.load(Ordering::Relaxed),
        METRICS.hw_buffered.load(Ordering::Relaxed),
        METRICS.pipeline_age_max_us.load(Ordering::Relaxed),
        METRICS.stream_lag_us.load(Ordering::Relaxed) / 1000,
        METRICS.stream_lag_max_us.load(Ordering::Relaxed) / 1000,
        METRICS.video_bandwidth_kbps.load(Ordering::Relaxed),
        METRICS.audio_queue_ms.load(Ordering::Relaxed),
        METRICS.stream_slip_us.load(Ordering::Relaxed),
        METRICS.stream_slip_slope_mpm.load(Ordering::Relaxed),
        METRICS.q_slope_mpm.load(Ordering::Relaxed),
        METRICS.capture_to_arrival_us.load(Ordering::Relaxed) / 1000,
        METRICS.capture_to_arrival_max_us.load(Ordering::Relaxed) / 1000,
        METRICS.sr_age_ms.load(Ordering::Relaxed),
        METRICS.sr_clock_offset_ms.load(Ordering::Relaxed),
    );
    let line2 = format!(
        "r:{rtp_average}/{rtp_max} rs:{} rst:{} wk:{} ab:{} hold:{}/{}/{} iv:{} qf:{} fl:{} tw:{}/{}/{} stride:{} md:{} pick:{} pickMax:{} rxd:{} rcpu:{} ui:{} uw:{} kf:{} idr:{} scr:{}",
        METRICS.resyncs.load(Ordering::Relaxed),
        METRICS.resets.load(Ordering::Relaxed),
        METRICS.rtp_wait_drops.load(Ordering::Relaxed),
        METRICS.rtp_abandons.load(Ordering::Relaxed),
        METRICS.rtp_hold_saved.load(Ordering::Relaxed),
        METRICS.rtp_hold_expired.load(Ordering::Relaxed),
        METRICS.rtp_gaps.load(Ordering::Relaxed),
        METRICS.rtp_invalid.load(Ordering::Relaxed),
        METRICS.dec_queue_full.load(Ordering::Relaxed),
        METRICS.flushes.load(Ordering::Relaxed),
        METRICS.twcc_sent.load(Ordering::Relaxed),
        METRICS.twcc_samples.load(Ordering::Relaxed),
        METRICS.rtp_ext_any.load(Ordering::Relaxed),
        METRICS.twcc_stride.load(Ordering::Relaxed),
        METRICS.video_metadata_sent.load(Ordering::Relaxed),
        METRICS.display_pickup_us.load(Ordering::Relaxed),
        METRICS.display_pickup_max_us.load(Ordering::Relaxed),
        METRICS.rx_drain_max.load(Ordering::Relaxed),
        METRICS.rtc_cpu_us.load(Ordering::Relaxed) / 1000,
        METRICS.ui_loop_us.load(Ordering::Relaxed) / 1000,
        METRICS.ui_work_us.load(Ordering::Relaxed) / 1000,
        METRICS.kf_req.load(Ordering::Relaxed),
        METRICS.idr_bytes.load(Ordering::Relaxed) / 1024,
        METRICS.au_scratch.load(Ordering::Relaxed),
    );
    // HUD vt segment: vt_max_0..5 and vt_exit_count
    let vt_segment = format!(
        " vt_max:{}/{}/{}/{}/{}/{} vt_exit:{}",
        METRICS.vt_max_0,
        METRICS.vt_max_1,
        METRICS.vt_max_2,
        METRICS.vt_max_3,
        METRICS.vt_max_4,
        METRICS.vt_max_5,
        METRICS.vt_exit_count,
    );
    format!("{line1}\n{line2}{vt_segment}")
}

/// Drain all per-second metrics into a snapshot, zeroing METRICS so the next
/// status() call starts clean.  The snapshot is shared with metrics_log.rs
/// for CSV row generation.
pub(crate) fn drain_metrics() -> MetricsSnapshot {
    MetricsSnapshot {
        rtp_assembly_sum_us: METRICS.rtp_assembly_sum_us.swap(0, Ordering::Relaxed),
        rtp_assembly_count: METRICS.rtp_assembly_count.swap(0, Ordering::Relaxed),
        rtp_assembly_max_us: METRICS.rtp_assembly_max_us.swap(0, Ordering::Relaxed),
        decode_us: METRICS.decode_us.swap(0, Ordering::Relaxed),
        pipeline_age_us: METRICS.pipeline_age_us.swap(0, Ordering::Relaxed),
        decoded: METRICS.decoded.swap(0, Ordering::Relaxed),
        skipped: METRICS.skipped.swap(0, Ordering::Relaxed),
        rate_limited: METRICS.rate_limited.swap(0, Ordering::Relaxed),
        presented: METRICS.presented.swap(0, Ordering::Relaxed),
        replaced: METRICS.replaced.swap(0, Ordering::Relaxed),
        queue_full: METRICS.queue_full.swap(0, Ordering::Relaxed),
        resyncs: METRICS.resyncs.swap(0, Ordering::Relaxed),
        rtcp_sr_count: METRICS.rtcp_sr_count.swap(0, Ordering::Relaxed),
        rtcp_sr_raw: METRICS.rtcp_sr_raw.swap(0, Ordering::Relaxed),
        rtcp_nack_sent: METRICS.rtcp_nack_sent.swap(0, Ordering::Relaxed),
        rtx_packets_received: METRICS.rtx_packets_received.swap(0, Ordering::Relaxed),
        rtx_crate_seen: METRICS.rtx_crate_seen.swap(0, Ordering::Relaxed),
        resets: METRICS.resets.swap(0, Ordering::Relaxed),
        hw_buffered: METRICS.hw_buffered.swap(0, Ordering::Relaxed),
        pipeline_age_max_us: METRICS.pipeline_age_max_us.swap(0, Ordering::Relaxed),
        stream_lag_us: METRICS.stream_lag_us.swap(0, Ordering::Relaxed),
        stream_lag_max_us: METRICS.stream_lag_max_us.swap(0, Ordering::Relaxed),
        stream_slip_us: METRICS.stream_slip_us.swap(0, Ordering::Relaxed),
        stream_slip_max_us: METRICS.stream_slip_max_us.swap(0, Ordering::Relaxed),
        stream_slip_slope_mpm: METRICS.stream_slip_slope_mpm.swap(0, Ordering::Relaxed),
        audio_queue_ms: METRICS.audio_queue_ms.swap(0, Ordering::Relaxed),
        q_us: METRICS.q_us.swap(0, Ordering::Relaxed),
        q_us_max: METRICS.q_us_max.swap(0, Ordering::Relaxed),
        q_slope_mpm: METRICS.q_slope_mpm.swap(0, Ordering::Relaxed),
        vt_exit_count: METRICS.vt_exit_count,
        vt_max_0: METRICS.vt_max_0,
        vt_max_1: METRICS.vt_max_1,
        vt_max_2: METRICS.vt_max_2,
        vt_max_3: METRICS.vt_max_3,
        vt_max_4: METRICS.vt_max_4,
        vt_max_5: METRICS.vt_max_5,
        rtp_hold_saved: METRICS.rtp_hold_saved.swap(0, Ordering::Relaxed),
        rtp_hold_expired: METRICS.rtp_hold_expired.swap(0, Ordering::Relaxed),
        rtp_gaps: METRICS.rtp_gaps.swap(0, Ordering::Relaxed),
        grace_held: METRICS.grace_held.swap(0, Ordering::Relaxed),
        grace_filled: METRICS.grace_filled.swap(0, Ordering::Relaxed),
        grace_expired: METRICS.grace_expired.swap(0, Ordering::Relaxed),
        // Provenance constants: loaded, never swapped (survive session reset).
        probe_rtt_ms_x10: METRICS.probe_rtt_ms_x10.load(Ordering::Relaxed),
        probe_dl_mbps_x10: METRICS.probe_dl_mbps_x10.load(Ordering::Relaxed),
        probe_suggest_kbps: METRICS.probe_suggest_kbps.load(Ordering::Relaxed),
        // Micro-jb: boost/depth last-value loads; absorbed is a counter.
        jb_boost: METRICS.jb_boost.load(Ordering::Relaxed),
        jb_depth: METRICS.jb_depth.load(Ordering::Relaxed),
        jb_absorbed: METRICS.jb_absorbed.swap(0, Ordering::Relaxed),
        jb_target: METRICS.jb_target.load(Ordering::Relaxed),
        damaged_dropped: METRICS.damaged_dropped.swap(0, Ordering::Relaxed),
        rtp_abandons: METRICS.rtp_abandons.swap(0, Ordering::Relaxed),
        rtp_wait_drops: METRICS.rtp_wait_drops.swap(0, Ordering::Relaxed),
        rtp_invalid: METRICS.rtp_invalid.swap(0, Ordering::Relaxed),
        dec_queue_full: METRICS.dec_queue_full.swap(0, Ordering::Relaxed),
        flushes: METRICS.flushes.swap(0, Ordering::Relaxed),
        twcc_sent: METRICS.twcc_sent.swap(0, Ordering::Relaxed),
        twcc_samples: METRICS.twcc_samples.swap(0, Ordering::Relaxed),
        rtp_ext_any: METRICS.rtp_ext_any.swap(0, Ordering::Relaxed),
        twcc_stride: METRICS.twcc_stride.swap(0, Ordering::Relaxed),
        video_metadata_sent: METRICS.video_metadata_sent.swap(0, Ordering::Relaxed),
        video_bandwidth_kbps: METRICS.video_bandwidth_kbps.swap(0, Ordering::Relaxed),
        display_pickup_us: METRICS.display_pickup_us.swap(0, Ordering::Relaxed),
        display_pickup_max_us: METRICS.display_pickup_max_us.swap(0, Ordering::Relaxed),
        twcc_delay_med_us: METRICS.twcc_delay_med_us.swap(0, Ordering::Relaxed),
        twcc_delay_max_us: METRICS.twcc_delay_max_us.swap(0, Ordering::Relaxed),
        sr_age_ms: METRICS.sr_age_ms.swap(0, Ordering::Relaxed) as i64,
        sr_clock_offset_ms: METRICS.sr_clock_offset_ms.swap(0, Ordering::Relaxed) as i64,
        capture_to_arrival_us: METRICS.capture_to_arrival_us.swap(0, Ordering::Relaxed) as i64,
        capture_to_arrival_max_us: METRICS.capture_to_arrival_max_us.swap(0, Ordering::Relaxed) as i64,
        sr_anchor_local_us: METRICS.sr_anchor_local_us.load(Ordering::Relaxed),
        sr_anchor_local_wall_us: METRICS.sr_anchor_local_wall_us.load(Ordering::Relaxed),
        sh_match: METRICS.sh_match.swap(0, Ordering::Relaxed),
        sh_bound_mismatch: METRICS.sh_bound_mismatch.swap(0, Ordering::Relaxed),
        sh_bytes_mismatch: METRICS.sh_bytes_mismatch.swap(0, Ordering::Relaxed),
        sh_class_mismatch: METRICS.sh_class_mismatch.swap(0, Ordering::Relaxed),
        sh_new_damaged_old_complete: METRICS
            .sh_new_damaged_old_complete
            .swap(0, Ordering::Relaxed),
        sh_new_complete_old_abandon: METRICS
            .sh_new_complete_old_abandon
            .swap(0, Ordering::Relaxed),
        sh_hold_saved: METRICS.sh_hold_saved.swap(0, Ordering::Relaxed),
        sh_legacy_unmatched: METRICS.sh_legacy_unmatched.swap(0, Ordering::Relaxed),
        sh_new_unmatched: METRICS.sh_new_unmatched.swap(0, Ordering::Relaxed),
        au_submitted: METRICS.au_submitted.swap(0, Ordering::Relaxed),
        au_no_startcode: METRICS.au_no_startcode.swap(0, Ordering::Relaxed),
        au_idr_submitted: METRICS.au_idr_submitted.swap(0, Ordering::Relaxed),
        sh_reject_bound_artifact: METRICS
            .sh_reject_bound_artifact
            .swap(0, Ordering::Relaxed),
        late_after_resolve: METRICS.late_after_resolve.swap(0, Ordering::Relaxed),
        dam_markerless: METRICS.dam_markerless.swap(0, Ordering::Relaxed),
        dam_first_slice: METRICS.dam_first_slice.swap(0, Ordering::Relaxed),
        dam_holes: METRICS.dam_holes.swap(0, Ordering::Relaxed),
        // PLI→IDR latency metrics (Task D): last-value and max-over-session.
        pli_idr_ms_last: METRICS.pli_idr_ms_last.load(Ordering::Relaxed),
        pli_idr_ms_max: METRICS.pli_idr_ms_max.load(Ordering::Relaxed),
        rtp_ext_twcc: METRICS.rtp_ext_twcc.swap(0, Ordering::Relaxed),
        rtcp_rtpfb_any: METRICS.rtcp_rtpfb_any.swap(0, Ordering::Relaxed),
        ui_loop_us: METRICS.ui_loop_us.swap(0, Ordering::Relaxed),
        ui_work_us: METRICS.ui_work_us.swap(0, Ordering::Relaxed),
        rx_drain_max: METRICS.rx_drain_max.swap(0, Ordering::Relaxed),
        rtc_cpu_us: METRICS.rtc_cpu_us.swap(0, Ordering::Relaxed),
        idr_bytes: METRICS.idr_bytes.swap(0, Ordering::Relaxed),
        kf_req: METRICS.kf_req.swap(0, Ordering::Relaxed),
        // Session constants: loaded, never swapped.
        clk_arm_mhz: METRICS.clk_arm_mhz.load(Ordering::Relaxed),
        clk_bus_mhz: METRICS.clk_bus_mhz.load(Ordering::Relaxed),
        clk_gpu_mhz: METRICS.clk_gpu_mhz.load(Ordering::Relaxed),
        clk_xbar_mhz: METRICS.clk_xbar_mhz.load(Ordering::Relaxed),
        rcvbuf_def_bytes: METRICS.rcvbuf_def_bytes.load(Ordering::Relaxed),
        // Pause probes: per-second maxima, swapped like the other counters.
        rx_gap_max_us: METRICS.rx_gap_max_us.swap(0, Ordering::Relaxed),
        rtc_gap_max_us: METRICS.rtc_gap_max_us.swap(0, Ordering::Relaxed),
        rtc_pump_max_us: METRICS.rtc_pump_max_us.swap(0, Ordering::Relaxed),
        au_scratch: METRICS.au_scratch.swap(0, Ordering::Relaxed),
    }
}

/// Reset all per-second session metrics to zero.  Called at session start
/// (from RtcSession::new) so values do not leak across sessions.
pub(crate) fn reset_session_metrics() {
    METRICS.rtp_assembly_sum_us.store(0, Ordering::Relaxed);
    METRICS.rtp_assembly_count.store(0, Ordering::Relaxed);
    METRICS.rtp_assembly_max_us.store(0, Ordering::Relaxed);
    METRICS.jb_absorbed.store(0, Ordering::Relaxed);
    METRICS.jb_target.store(0, Ordering::Relaxed);
    METRICS.damaged_dropped.store(0, Ordering::Relaxed);
    METRICS.decode_us.store(0, Ordering::Relaxed);
    METRICS.pipeline_age_us.store(0, Ordering::Relaxed);
    METRICS.decoded.store(0, Ordering::Relaxed);
    METRICS.skipped.store(0, Ordering::Relaxed);
    METRICS.rate_limited.store(0, Ordering::Relaxed);
    METRICS.presented.store(0, Ordering::Relaxed);
    METRICS.replaced.store(0, Ordering::Relaxed);
    METRICS.queue_full.store(0, Ordering::Relaxed);
    METRICS.resyncs.store(0, Ordering::Relaxed);
    METRICS.rtcp_sr_count.store(0, Ordering::Relaxed);
    METRICS.rtcp_sr_raw.store(0, Ordering::Relaxed);
    METRICS.rtcp_nack_sent.store(0, Ordering::Relaxed);
    METRICS.rtx_packets_received.store(0, Ordering::Relaxed);
    METRICS.rtx_crate_seen.store(0, Ordering::Relaxed);
    METRICS.resets.store(0, Ordering::Relaxed);
    METRICS.hw_buffered.store(0, Ordering::Relaxed);
    METRICS.pipeline_age_max_us.store(0, Ordering::Relaxed);
    METRICS.stream_lag_us.store(0, Ordering::Relaxed);
    METRICS.stream_lag_max_us.store(0, Ordering::Relaxed);
    METRICS.stream_slip_us.store(0, Ordering::Relaxed);
    METRICS.stream_slip_max_us.store(0, Ordering::Relaxed);
    METRICS.q_us.store(0, Ordering::Relaxed);
    METRICS.q_us_max.store(0, Ordering::Relaxed);
    METRICS.q_slope_mpm.store(0, Ordering::Relaxed);
    METRICS.audio_queue_ms.store(0, Ordering::Relaxed);
    METRICS.rtp_hold_saved.store(0, Ordering::Relaxed);
    METRICS.rtp_hold_expired.store(0, Ordering::Relaxed);
    METRICS.rtp_gaps.store(0, Ordering::Relaxed);
    METRICS.grace_held.store(0, Ordering::Relaxed);
    METRICS.grace_filled.store(0, Ordering::Relaxed);
    METRICS.grace_expired.store(0, Ordering::Relaxed);
    METRICS.rtp_abandons.store(0, Ordering::Relaxed);
    METRICS.rtp_wait_drops.store(0, Ordering::Relaxed);
    METRICS.rtp_invalid.store(0, Ordering::Relaxed);
    METRICS.dec_queue_full.store(0, Ordering::Relaxed);
    METRICS.flushes.store(0, Ordering::Relaxed);
    METRICS.twcc_sent.store(0, Ordering::Relaxed);
    METRICS.twcc_samples.store(0, Ordering::Relaxed);
    METRICS.twcc_delay_med_us.store(0, Ordering::Relaxed);
    METRICS.twcc_delay_max_us.store(0, Ordering::Relaxed);
    METRICS.rtp_ext_any.store(0, Ordering::Relaxed);
    METRICS.twcc_stride.store(0, Ordering::Relaxed);
    METRICS.video_metadata_sent.store(0, Ordering::Relaxed);
    METRICS.video_bandwidth_kbps.store(0, Ordering::Relaxed);
    METRICS.display_pickup_us.store(0, Ordering::Relaxed);
    METRICS.display_pickup_max_us.store(0, Ordering::Relaxed);
    METRICS.sr_age_ms.store(0, Ordering::Relaxed);
    METRICS.sr_clock_offset_ms.store(0, Ordering::Relaxed);
    METRICS.capture_to_arrival_us.store(0, Ordering::Relaxed);
    METRICS.capture_to_arrival_max_us.store(0, Ordering::Relaxed);
    METRICS.sh_match.store(0, Ordering::Relaxed);
    METRICS.sh_bound_mismatch.store(0, Ordering::Relaxed);
    METRICS.sh_bytes_mismatch.store(0, Ordering::Relaxed);
    METRICS.sh_class_mismatch.store(0, Ordering::Relaxed);
    METRICS.sh_new_damaged_old_complete.store(0, Ordering::Relaxed);
    METRICS.sh_new_complete_old_abandon.store(0, Ordering::Relaxed);
    METRICS.sh_hold_saved.store(0, Ordering::Relaxed);
    METRICS.sh_legacy_unmatched.store(0, Ordering::Relaxed);
    METRICS.sh_new_unmatched.store(0, Ordering::Relaxed);
    METRICS.au_submitted.store(0, Ordering::Relaxed);
    METRICS.au_no_startcode.store(0, Ordering::Relaxed);
    METRICS.au_idr_submitted.store(0, Ordering::Relaxed);
    METRICS.sh_reject_bound_artifact.store(0, Ordering::Relaxed);
    METRICS.late_after_resolve.store(0, Ordering::Relaxed);
    METRICS.dam_markerless.store(0, Ordering::Relaxed);
    METRICS.dam_first_slice.store(0, Ordering::Relaxed);
    METRICS.dam_holes.store(0, Ordering::Relaxed);
    // PLI→IDR latency metrics (Task D).
    METRICS.pli_idr_ms_last.store(0, Ordering::Relaxed);
    METRICS.pli_idr_ms_max.store(0, Ordering::Relaxed);
    METRICS.rtp_ext_twcc.store(0, Ordering::Relaxed);
    METRICS.rtcp_rtpfb_any.store(0, Ordering::Relaxed);
    METRICS.ui_loop_us.store(0, Ordering::Relaxed);
    METRICS.ui_work_us.store(0, Ordering::Relaxed);
    METRICS.rx_drain_max.store(0, Ordering::Relaxed);
    METRICS.rtc_cpu_us.store(0, Ordering::Relaxed);
    METRICS.idr_bytes.store(0, Ordering::Relaxed);
    METRICS.kf_req.store(0, Ordering::Relaxed);
    METRICS.rx_gap_max_us.store(0, Ordering::Relaxed);
    METRICS.rtc_gap_max_us.store(0, Ordering::Relaxed);
    METRICS.rtc_pump_max_us.store(0, Ordering::Relaxed);
    METRICS.au_scratch.store(0, Ordering::Relaxed);
}

    /// Flush the RAM-buffered metrics CSV log to disk once at session end.
    /// Called from RtcWorker::drop after session teardown; no filesystem I/O during session.
    /// Sets the global log to None after flush so a new session starts clean.
    pub(crate) fn flush_csv_log() {
        let mut log = METRICS_CSV_LOG.lock().unwrap();
        if let Some(log) = log.as_ref() {
            log.flush();
        }
        *log = None;
    }
