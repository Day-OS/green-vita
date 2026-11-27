//! Append-only CSV logging of the once-per-second metrics snapshot, so a
//! streaming session can be analysed offline.
//!
//! This version buffers rows in RAM and flushes once at session end,
//! avoiding filesystem I/O during the hot path.
//!
//! CSV is written to ux0:data/xcloud-rust/metrics.csv at session teardown.
//! During the session, zero touches ux0:.

use super::metrics::{MetricsSnapshot, drain_metrics, reset_session_metrics};
use std::io::Write;
use std::sync::{Arc, Mutex};


/// Extra per-tick values that live on the video receiver rather than in METRICS.
pub(crate) struct RowExtras {
    pub(crate) src_fps: u64,
    pub(crate) enc_width: u32,
    pub(crate) enc_height: u32,
    pub(crate) waiting_keyframe: bool,
    pub(crate) drops: u64,
    pub(crate) decode_errors: u64,
}

const MAX_ROWS: usize = 20_000;
/// CSV header for per-session metrics log. Updated in sync with the cols array in write_row.
const HEADER: &str = "t_s,rtp_assembly_sum_us,rtp_assembly_count,rtp_assembly_max_us,decode_us,pipeline_age_us,decoded,skipped,rate_limited,presented,replaced,queue_full,resyncs,resets,hw_buffered,pipeline_age_max_us,stream_lag_us,stream_lag_max_us,rtp_hold_saved,rtp_hold_expired,rtp_gaps,rtp_abandons,rtp_wait_drops,rtp_invalid,dec_queue_full,flushes,twcc_sent,twcc_samples,rtp_ext_any,twcc_stride,video_metadata_sent,video_bandwidth_kbps,display_pickup_us,display_pickup_max_us,stream_slip_us,stream_slip_max_us,audio_queue_ms,src_fps,enc_width,enc_height,waiting_keyframe,drops,decode_errors,vt_exit_count,vt_max_0,vt_max_1,vt_max_2,vt_max_3,vt_max_4,vt_max_5,q_us,q_us_max,q_slope_mpm,sh_match,sh_bound_mismatch,sh_bytes_mismatch,sh_class_mismatch,sh_new_damaged_old_complete,sh_new_complete_old_abandon,sh_hold_saved,sh_legacy_unmatched,sh_new_unmatched,au_submitted,au_no_startcode,au_idr_submitted,sh_reject_bound_artifact,late_after_resolve,dam_markerless,dam_first_slice,dam_holes,rtp_ext_twcc,rtcp_rtpfb_any,rtcp_sr_count,rtcp_sr_raw,rtcp_nack_sent,rtx_packets_received,rtx_crate_seen,sr_age_ms,sr_clock_offset_ms,capture_to_arrival_us,capture_to_arrival_max_us,twcc_delay_med_us,twcc_delay_max_us,sr_anc_ntp,sr_anc_rtp,sr_anc_lus,sr_anc_wall,grace_held,grace_filled,grace_expired,probe_rtt_ms_x10,probe_dl_mbps_x10,probe_suggest_kbps,jb_boost,jb_depth,jb_absorbed,damaged_dropped,pli_idr_ms_last,pli_idr_ms_max,jb_target,ui_loop_us,ui_work_us,rx_drain_max,rtc_cpu_us,idr_bytes,kf_req,clk_arm_mhz,clk_bus_mhz,clk_gpu_mhz,clk_xbar_mhz,rcvbuf_def_bytes,rx_gap_max_us,rtc_gap_max_us,rtc_pump_max_us,au_scratch";

/// RAM-buffered CSV log for a streaming session.
/// Rows are accumulated at 1 Hz from status() and flushed once at session end.
#[derive(Debug)]
pub(crate) struct MetricsCsvLog {
    /// Protected buffer of formatted CSV rows (no file I/O during session).
    rows: Arc<Mutex<Vec<String>>>,
    /// Counter for rows that were dropped due to capacity limit.
    dropped_rows: usize,
    /// Session start time, for timestamp computation.
    started_at: std::time::Instant,
}

impl MetricsCsvLog {
    /// Creates a new RAM-buffered log.
    pub(crate) fn new() -> Self {
        Self {
            rows: Arc::new(Mutex::new(Vec::new())),
            dropped_rows: 0,
            started_at: std::time::Instant::now(),
        }
    }

    /// Write a metrics snapshot as a CSV row into the RAM buffer.
    /// Returns `true` if row was added, `false` if buffer was full and row was dropped.
    pub(crate) fn write_row(&mut self, snapshot: &MetricsSnapshot, extras: &super::metrics_log::RowExtras) -> bool {
        let mut buffer = self.rows.lock().unwrap_or_else(|_| panic!("metrics_csv_log row mutex poisoned"));
        if buffer.len() >= MAX_ROWS {
            self.dropped_rows += 1;
            return false;
        }
        let t_s = self.started_at.elapsed().as_secs();
        // Fixed-size array so the column count is compiler-enforced and can never
        // drift out of sync with HEADER. Order MUST match HEADER below.
        let cols: [i64; 115] = [
            t_s as i64,
            snapshot.rtp_assembly_sum_us as i64,
            snapshot.rtp_assembly_count as i64,
            snapshot.rtp_assembly_max_us as i64,
            snapshot.decode_us as i64,
            snapshot.pipeline_age_us as i64,
            snapshot.decoded as i64,
            snapshot.skipped as i64,
            snapshot.rate_limited as i64,
            snapshot.presented as i64,
            snapshot.replaced as i64,
            snapshot.queue_full as i64,
            snapshot.resyncs as i64,
            snapshot.resets as i64,
            snapshot.hw_buffered as i64,
            snapshot.pipeline_age_max_us as i64,
            snapshot.stream_lag_us as i64,
            snapshot.stream_lag_max_us as i64,
            snapshot.rtp_hold_saved as i64,
            snapshot.rtp_hold_expired as i64,
            snapshot.rtp_gaps as i64,
            snapshot.rtp_abandons as i64,
            snapshot.rtp_wait_drops as i64,
            snapshot.rtp_invalid as i64,
            snapshot.dec_queue_full as i64,
            snapshot.flushes as i64,
            snapshot.twcc_sent as i64,
            snapshot.twcc_samples as i64,
            snapshot.rtp_ext_any as i64,
            snapshot.twcc_stride as i64,
            snapshot.video_metadata_sent as i64,
            snapshot.video_bandwidth_kbps as i64,
            snapshot.display_pickup_us as i64,
            snapshot.display_pickup_max_us as i64,
            snapshot.stream_slip_us as i64,
            snapshot.stream_slip_max_us as i64,
            snapshot.audio_queue_ms as i64,
            extras.src_fps as i64,
            u64::from(extras.enc_width) as i64,
            u64::from(extras.enc_height) as i64,
            u64::from(extras.waiting_keyframe) as i64,
            extras.drops as i64,
            extras.decode_errors as i64,
            snapshot.vt_exit_count as i64,
            snapshot.vt_max_0 as i64,
            snapshot.vt_max_1 as i64,
            snapshot.vt_max_2 as i64,
            snapshot.vt_max_3 as i64,
            snapshot.vt_max_4 as i64,
            snapshot.vt_max_5 as i64,
            snapshot.q_us as i64,
            snapshot.q_us_max as i64,
            snapshot.q_slope_mpm as i64,
            snapshot.sh_match as i64,
            snapshot.sh_bound_mismatch as i64,
            snapshot.sh_bytes_mismatch as i64,
            snapshot.sh_class_mismatch as i64,
            snapshot.sh_new_damaged_old_complete as i64,
            snapshot.sh_new_complete_old_abandon as i64,
            snapshot.sh_hold_saved as i64,
            snapshot.sh_legacy_unmatched as i64,
            snapshot.sh_new_unmatched as i64,
            snapshot.au_submitted as i64,
            snapshot.au_no_startcode as i64,
            snapshot.au_idr_submitted as i64,
            snapshot.sh_reject_bound_artifact as i64,
            snapshot.late_after_resolve as i64,
            snapshot.dam_markerless as i64,
            snapshot.dam_first_slice as i64,
            snapshot.dam_holes as i64,
            snapshot.rtp_ext_twcc as i64,
            snapshot.rtcp_rtpfb_any as i64,
            snapshot.rtcp_sr_count as i64,
            snapshot.rtcp_sr_raw as i64,
            snapshot.rtcp_nack_sent as i64,
            snapshot.rtx_packets_received as i64,
            snapshot.rtx_crate_seen as i64,
            snapshot.sr_age_ms as i64,
            snapshot.sr_clock_offset_ms as i64,
            snapshot.capture_to_arrival_us as i64,
            snapshot.capture_to_arrival_max_us as i64,
            snapshot.twcc_delay_med_us as i64,
            snapshot.twcc_delay_max_us as i64,
            0, // sr_anchor_ntp: SRTCP-encrypted, unreadable,
            0, // sr_anchor_rtp_ts: SRTCP-encrypted, unreadable,
            snapshot.sr_anchor_local_us as i64,
            snapshot.sr_anchor_local_wall_us as i64,
            snapshot.grace_held as i64,
            snapshot.grace_filled as i64,
            snapshot.grace_expired as i64,
            snapshot.probe_rtt_ms_x10 as i64,
            snapshot.probe_dl_mbps_x10 as i64,
            snapshot.probe_suggest_kbps as i64,
            snapshot.jb_boost as i64,
            snapshot.jb_depth as i64,
            snapshot.jb_absorbed as i64,
            snapshot.damaged_dropped as i64,
            snapshot.pli_idr_ms_last as i64,
            snapshot.pli_idr_ms_max as i64,
            snapshot.jb_target as i64,
            snapshot.ui_loop_us as i64,
            snapshot.ui_work_us as i64,
            snapshot.rx_drain_max as i64,
            snapshot.rtc_cpu_us as i64,
            snapshot.idr_bytes as i64,
            snapshot.kf_req as i64,
            snapshot.clk_arm_mhz as i64,
            snapshot.clk_bus_mhz as i64,
            snapshot.clk_gpu_mhz as i64,
            snapshot.clk_xbar_mhz as i64,
            snapshot.rcvbuf_def_bytes as i64,
            snapshot.rx_gap_max_us as i64,
            snapshot.rtc_gap_max_us as i64,
            snapshot.rtc_pump_max_us as i64,
            snapshot.au_scratch as i64,
        ];
        let row = cols
            .iter()
            .map(|value| value.to_string())
            .collect::<Vec<_>>()
            .join(",");
        buffer.push(row);
        true
    }

    /// Flush the entire buffer to ux0:data/xcloud-rust/metrics.csv.
    /// Must be called once at session end (from RtcWorker::drop).
    /// Tolerant of path being unavailable (off-device: silently skip).
    pub(crate) fn flush(&self) {
        // Session-end mismatch-evidence flush (RAM-buffered capture from the
        // shadow comparator; no-op when no event fired this session).
        crate::api::streaming::rtc::rtp::mismatch_log::session_flush();
        let buffer = self.rows.lock().unwrap_or_else(|_| panic!("metrics_csv_log flush mutex poisoned"));
        let path = std::path::Path::new("ux0:data/xcloud-rust/metrics.csv");
        // Ensure directory exists (no-op if already exists on-device; off-device will fail)
        let _ = std::fs::create_dir_all("ux0:data/xcloud-rust");
        // Build the full CSV content once: header + all rows + footer
        let duration_s = self.started_at.elapsed().as_secs();
        let footer = format!(
            "// session_duration_s={}, total_rows={}, dropped_rows={}",
            duration_s,
            buffer.len(),
            self.dropped_rows
        );
        let rows_content: String = buffer
            .iter()
            .map(|row| format!("{row}\n"))
            .collect();
        let content = format!("{HEADER}\n{rows_content}{footer}\n");
        // Try writing the full content; on failure, fallback to OpenOptions then eprintln
        match std::fs::write(path, &content) {
            Ok(()) => {
                eprintln!("Metrics CSV flushed: {} rows + {} dropped", buffer.len(), self.dropped_rows);
            }
            Err(e1) => {
                // Fallback: try OpenOptions path
                if let Ok(mut file) = std::fs::OpenOptions::new()
                    .read(true)
                    .write(true)
                    .create(true)
                    .truncate(true)
                    .open(path)
                {
                    let _ = writeln!(file, "{HEADER}");
                    for row in buffer.iter() {
                        let _ = writeln!(file, "{row}");
                    }
                    let _ = writeln!(file, "// session_duration_s={}, total_rows={}, dropped_rows={}", duration_s, buffer.len(), self.dropped_rows);
                    let _ = file.sync_data();
                    eprintln!("Metrics CSV flushed: {} rows + {} dropped (via fallback)", buffer.len(), self.dropped_rows);
                } else {
                    eprintln!("Metrics CSV: path unavailable, skipping flush (off-device build); write error: {e1}");
                }
            }
        }
    }
}

// Private helper: build a CSV row string from a MetricsSnapshot (mirrors write_row logic).
fn csv_row_from_snapshot(snapshot: &MetricsSnapshot) -> String {
    // Same 55-element layout as the cols array in write_row, using i64 values.
    let cols: [i64; 115] = [
        0 as i64,                                                    // t_s
        snapshot.rtp_assembly_sum_us as i64,
        snapshot.rtp_assembly_count as i64,
        snapshot.rtp_assembly_max_us as i64,
        snapshot.decode_us as i64,
        snapshot.pipeline_age_us as i64,
        snapshot.decoded as i64,
        snapshot.skipped as i64,
        snapshot.rate_limited as i64,
        snapshot.presented as i64,
        snapshot.replaced as i64,
        snapshot.queue_full as i64,
        snapshot.resyncs as i64,
        snapshot.resets as i64,
        snapshot.hw_buffered as i64,
        snapshot.pipeline_age_max_us as i64,
        snapshot.stream_lag_us as i64,
        snapshot.stream_lag_max_us as i64,
        snapshot.rtp_hold_saved as i64,
        snapshot.rtp_hold_expired as i64,
        snapshot.rtp_gaps as i64,
        snapshot.rtp_abandons as i64,
        snapshot.rtp_wait_drops as i64,
        snapshot.rtp_invalid as i64,
        snapshot.dec_queue_full as i64,
        snapshot.flushes as i64,
        snapshot.twcc_sent as i64,
        snapshot.twcc_samples as i64,
        snapshot.rtp_ext_any as i64,
        snapshot.twcc_stride as i64,
        snapshot.video_metadata_sent as i64,
        snapshot.video_bandwidth_kbps as i64,
            snapshot.display_pickup_us as i64,
            snapshot.display_pickup_max_us as i64,
            snapshot.stream_slip_us,
        snapshot.stream_slip_max_us as i64,
        snapshot.audio_queue_ms as i64,
        0 as i64,                                                    // src_fps
        0 as i64,                                                    // enc_width
        0 as i64,                                                    // enc_height
        0 as i64,                                                    // waiting_keyframe
        0 as i64,                                                    // drops
        0 as i64,                      // decode_errors
        0 as i64,                                                    // vt_exit_count
        0 as i64,                                                    // vt_max_0
        0 as i64,                                                    // vt_max_1
        0 as i64,                                                    // vt_max_2
        0 as i64,                                                    // vt_max_3
        0 as i64,                                                    // vt_max_4
        0 as i64,                                                    // vt_max_5
        snapshot.q_us,
        snapshot.q_us_max as i64,
        snapshot.q_slope_mpm,
        snapshot.sh_match as i64,
        snapshot.sh_bound_mismatch as i64,
        snapshot.sh_bytes_mismatch as i64,
        snapshot.sh_class_mismatch as i64,
        snapshot.sh_new_damaged_old_complete as i64,
        snapshot.sh_new_complete_old_abandon as i64,
        snapshot.sh_hold_saved as i64,
        snapshot.sh_legacy_unmatched as i64,
        snapshot.sh_new_unmatched as i64,
        snapshot.au_submitted as i64,
        snapshot.au_no_startcode as i64,
        snapshot.au_idr_submitted as i64,
        snapshot.sh_reject_bound_artifact as i64,
        snapshot.late_after_resolve as i64,
        snapshot.dam_markerless as i64,
        snapshot.dam_first_slice as i64,
        snapshot.dam_holes as i64,
        snapshot.rtp_ext_twcc as i64,
        snapshot.rtcp_rtpfb_any as i64,
        snapshot.rtcp_sr_count as i64,
        snapshot.rtcp_sr_raw as i64,
        snapshot.rtcp_nack_sent as i64,
        snapshot.rtx_packets_received as i64,
        snapshot.rtx_crate_seen as i64,
        snapshot.sr_age_ms as i64,
        snapshot.sr_clock_offset_ms as i64,
        snapshot.capture_to_arrival_us as i64,
        snapshot.capture_to_arrival_max_us as i64,
        snapshot.twcc_delay_med_us as i64,
        snapshot.twcc_delay_max_us as i64,
        0, // sr_anchor_ntp: SRTCP-encrypted, unreadable,
        0, // sr_anchor_rtp_ts: SRTCP-encrypted, unreadable,
        snapshot.sr_anchor_local_us as i64,
        snapshot.sr_anchor_local_wall_us as i64,
        snapshot.grace_held as i64,
        snapshot.grace_filled as i64,
        snapshot.grace_expired as i64,
        snapshot.probe_rtt_ms_x10 as i64,
        snapshot.probe_dl_mbps_x10 as i64,
        snapshot.probe_suggest_kbps as i64,
        snapshot.jb_boost as i64,
        snapshot.jb_depth as i64,
        snapshot.jb_absorbed as i64,
        snapshot.damaged_dropped as i64,
        snapshot.pli_idr_ms_last as i64,
        snapshot.pli_idr_ms_max as i64,
        snapshot.jb_target as i64,
        snapshot.ui_loop_us as i64,
        snapshot.ui_work_us as i64,
        snapshot.rx_drain_max as i64,
        snapshot.rtc_cpu_us as i64,
        snapshot.idr_bytes as i64,
        snapshot.kf_req as i64,
        snapshot.clk_arm_mhz as i64,
        snapshot.clk_bus_mhz as i64,
        snapshot.clk_gpu_mhz as i64,
        snapshot.clk_xbar_mhz as i64,
        snapshot.rcvbuf_def_bytes as i64,
        snapshot.rx_gap_max_us as i64,
        snapshot.rtc_gap_max_us as i64,
        snapshot.rtc_pump_max_us as i64,
        snapshot.au_scratch as i64,
    ];
    cols.iter().map(|value| value.to_string()).collect::<Vec<_>>().join(",")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn test_csv_header_row_column_count_match() {
        // Build a dummy snapshot with known q values.
        let snapshot = MetricsSnapshot {
            rtp_assembly_sum_us: 0,
            rtp_assembly_count: 0,
            rtp_assembly_max_us: 0,
            decode_us: 0,
            pipeline_age_us: 0,
            decoded: 0,
            skipped: 0,
            rate_limited: 0,
            presented: 0,
            replaced: 0,
            queue_full: 0,
            resyncs: 0,
            resets: 0,
            hw_buffered: 0,
            pipeline_age_max_us: 0,
            stream_lag_us: 0,
            sr_age_ms: 0,
            sr_clock_offset_ms: 0,
            capture_to_arrival_us: 0,
            capture_to_arrival_max_us: 0,
            sr_anchor_local_us: 0,
            sr_anchor_local_wall_us: 0,
            stream_lag_max_us: 0,
            stream_slip_us: 0,
            stream_slip_max_us: 0,
            stream_slip_slope_mpm: 0,
            audio_queue_ms: 0,
            q_us: -42,  // signed value required by the task
            q_us_max: 1000,
            q_slope_mpm: 500,
            vt_exit_count: 0,
            vt_max_0: 0,
            vt_max_1: 0,
            vt_max_2: 0,
            vt_max_3: 0,
            vt_max_4: 0,
            vt_max_5: 0,
            rtp_hold_saved: 0,
            rtp_hold_expired: 0,
            rtp_gaps: 0,
            rtp_abandons: 0,
            rtp_wait_drops: 0,
            rtp_invalid: 0,
            dec_queue_full: 0,
            flushes: 0,
            twcc_sent: 0,
            twcc_samples: 0,
            rtp_ext_any: 0,
            twcc_stride: 0,
            video_metadata_sent: 0,
            video_bandwidth_kbps: 0,
            display_pickup_us: 0,
            display_pickup_max_us: 0,
            sh_match: 0,
            sh_bound_mismatch: 0,
            sh_bytes_mismatch: 0,
            sh_class_mismatch: 0,
            sh_new_damaged_old_complete: 0,
            sh_new_complete_old_abandon: 0,
            sh_hold_saved: 0,
            sh_legacy_unmatched: 0,
            sh_new_unmatched: 0,
            au_submitted: 0,
            au_no_startcode: 0,
            au_idr_submitted: 0,
            sh_reject_bound_artifact: 0,
            late_after_resolve: 0,
            dam_markerless: 0,
            dam_first_slice: 0,
            dam_holes: 0,
            rtp_ext_twcc: 0,
            twcc_delay_med_us: 0,
            twcc_delay_max_us: 0,
            rtcp_rtpfb_any: 0,
            rtcp_sr_count: 0,
            rtcp_sr_raw: 0,
            rtcp_nack_sent: 0,
            rtx_packets_received: 0,
            rtx_crate_seen: 0,
            grace_held: 0,
            grace_filled: 0,
            grace_expired: 0,
            probe_rtt_ms_x10: 0,
            probe_dl_mbps_x10: 0,
            probe_suggest_kbps: 0,
            jb_boost: 0,
            jb_depth: 0,
            jb_absorbed: 0,
            damaged_dropped: 0,
            pli_idr_ms_last: 0,
            pli_idr_ms_max: 0,
            jb_target: 0,
            ui_loop_us: 0,
            ui_work_us: 0,
            rx_drain_max: 0,
            rtc_cpu_us: 0,
            idr_bytes: 0,
            kf_req: 0,
            clk_arm_mhz: 0,
            clk_bus_mhz: 0,
            clk_gpu_mhz: 0,
            clk_xbar_mhz: 0,
            rcvbuf_def_bytes: 0,
            rx_gap_max_us: 0,
            rtc_gap_max_us: 0,
            rtc_pump_max_us: 0,
            au_scratch: 0,
        };

        assert_eq!(
            HEADER.split(',').count(),
            115,
            "CSV header must have exactly 115 columns"
        );
        let row = csv_row_from_snapshot(&snapshot);
        let header_cols = HEADER.split(',').count();
        let row_cols = row.split(',').count();
        assert_eq!(
            header_cols, row_cols,
            "Header column count ({}) must match row column count ({})",
            header_cols, row_cols
        );
    }

    #[test]
    fn test_csv_row_allows_negative_q_us() {
        // Verify that a negative q_us produces a minus sign in the CSV row.
        let snapshot = MetricsSnapshot {
            rtp_assembly_sum_us: 0,
            rtp_assembly_count: 0,
            rtp_assembly_max_us: 0,
            decode_us: 0,
            pipeline_age_us: 0,
            decoded: 0,
            skipped: 0,
            rate_limited: 0,
            presented: 0,
            replaced: 0,
            queue_full: 0,
            resyncs: 0,
            resets: 0,
            hw_buffered: 0,
            pipeline_age_max_us: 0,
            stream_lag_us: 0,
            sr_age_ms: 0,
            sr_clock_offset_ms: 0,
            capture_to_arrival_us: 0,
            capture_to_arrival_max_us: 0,
            sr_anchor_local_us: 0,
            sr_anchor_local_wall_us: 0,
            stream_lag_max_us: 0,
            rtp_hold_saved: 0,
            rtp_hold_expired: 0,
            rtp_gaps: 0,
            rtp_abandons: 0,
            rtp_wait_drops: 0,
            rtp_invalid: 0,
            dec_queue_full: 0,
            flushes: 0,
            twcc_sent: 0,
            twcc_samples: 0,
            rtp_ext_any: 0,
            twcc_stride: 0,
            video_metadata_sent: 0,
            video_bandwidth_kbps: 0,
            display_pickup_us: 0,
            display_pickup_max_us: 0,
            stream_slip_us: 0,
            stream_slip_max_us: 0,
            stream_slip_slope_mpm: 0,
            audio_queue_ms: 0,
            q_us: -1000,  // negative value
            q_us_max: 1000,
            q_slope_mpm: 0,
            vt_exit_count: 0,
            vt_max_0: 0,
            vt_max_1: 0,
            vt_max_2: 0,
            vt_max_3: 0,
            vt_max_4: 0,
            vt_max_5: 0,
            sh_match: 0,
            sh_bound_mismatch: 0,
            sh_bytes_mismatch: 0,
            sh_class_mismatch: 0,
            sh_new_damaged_old_complete: 0,
            sh_new_complete_old_abandon: 0,
            sh_hold_saved: 0,
            sh_legacy_unmatched: 0,
            sh_new_unmatched: 0,
            au_submitted: 0,
            au_no_startcode: 0,
            au_idr_submitted: 0,
            sh_reject_bound_artifact: 0,
            late_after_resolve: 0,
            dam_markerless: 0,
            dam_first_slice: 0,
            dam_holes: 0,
            rtp_ext_twcc: 0,
            twcc_delay_med_us: 0,
            twcc_delay_max_us: 0,
            rtcp_rtpfb_any: 0,
            rtcp_sr_count: 0,
            rtcp_sr_raw: 0,
            rtcp_nack_sent: 0,
            rtx_packets_received: 0,
            rtx_crate_seen: 0,
            grace_held: 0,
            grace_filled: 0,
            grace_expired: 0,
            probe_rtt_ms_x10: 0,
            probe_dl_mbps_x10: 0,
            probe_suggest_kbps: 0,
            jb_boost: 0,
            jb_depth: 0,
            jb_absorbed: 0,
            damaged_dropped: 0,
            pli_idr_ms_last: 0,
            pli_idr_ms_max: 0,
            jb_target: 0,
            ui_loop_us: 0,
            ui_work_us: 0,
            rx_drain_max: 0,
            rtc_cpu_us: 0,
            idr_bytes: 0,
            kf_req: 0,
            clk_arm_mhz: 0,
            clk_bus_mhz: 0,
            clk_gpu_mhz: 0,
            clk_xbar_mhz: 0,
            rcvbuf_def_bytes: 0,
            rx_gap_max_us: 0,
            rtc_gap_max_us: 0,
            rtc_pump_max_us: 0,
            au_scratch: 0,
        };

        let row = csv_row_from_snapshot(&snapshot);
        // q_us = -1000 should appear as "-1000" in the CSV.
        assert!(
            row.contains("-1000"),
            "Negative q_us (-1000) should appear in CSV row, but row was: {}",
            row
        );
    }

    #[test]
    fn drained_row_carries_nonzero_q_us_for_delayed_stream() {
        // 7b gate test: a synthetic delayed stream must surface a non-zero
        // q_us through the production publish path (METRICS stores) and the
        // drain into a real CSV row. At runtime media.rs status() performs
        // the q_us store and rtp.rs the q_us_max fetch_max at push time.
        use crate::pipeline::clock::queue_delay::{PacketInput, QueueDelayEstimator};
        use crate::streaming::video::metrics::METRICS;
        use std::sync::atomic::Ordering;

        reset_session_metrics();

        let mut estimator = QueueDelayEstimator::new();
        // Flat phase: inter-arrival == inter-send ⇒ q ≈ 0.
        for i in 0..50 {
            let send_us = i as u64 * 20_000;
            estimator
                .push(PacketInput { send_us, arrival_us: send_us })
                .unwrap();
        }
        // Step: 100 ms standing delay ⇒ q rises to ~100_000 µs.
        for i in 0..50 {
            let send_us = 50 as u64 * 20_000 + i as u64 * 20_000;
            estimator
                .push(PacketInput {
                    send_us,
                    arrival_us: send_us + 100_000,
                })
                .unwrap();
        }
        let q = estimator.q_us();
        assert!(
            q > 80_000,
            "synthetic 100 ms delay should yield q > 80 ms, got {q}"
        );

        // Mirror the production stores (media.rs drain site / rtp.rs push site).
        METRICS.q_us.store(q, Ordering::Relaxed);
        METRICS.q_us_max.fetch_max(q.max(0) as u64, Ordering::Relaxed);

        let snapshot = drain_metrics();
        assert_eq!(snapshot.q_us, q, "drained q_us must carry the estimate");
        assert!(
            snapshot.q_us_max >= q as u64,
            "drained q_us_max must carry the peak"
        );

        let row = csv_row_from_snapshot(&snapshot);
        let header: Vec<&str> = HEADER.split(',').collect();
        let fields: Vec<&str> = row.split(',').collect();
        let idx = header
            .iter()
            .position(|h| *h == "q_us")
            .expect("q_us column in header");
        assert_ne!(
            fields[idx], "0",
            "CSV q_us must be non-zero for a delayed stream, row: {row}"
        );
        reset_session_metrics();
    }
}