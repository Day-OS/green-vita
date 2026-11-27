# METRICS CSV Reference

**Source**: `src/streaming/video/metrics_log.rs:27` — HEADER constant, compiler-enforced `[i64; 106]` array in `write_row()`.
**Total columns**: 106 (100 on the 0.3.2 line; 101–106 added 2026-11-24 by the xHome performance plan, stage 0). Order must match the `cols: [i64; 106]` arrays in `write_row()` and `csv_row_from_snapshot()`; `test_csv_header_row_column_count_match` asserts the count.
**Scope**: Per-second snapshot drained at `status()` time via `drain_metrics()`. Appended to `ux0:data/xcloud-rust/metrics.csv` at session end.

| # | Column Name | Type/Units | Meaning | "Bad" Looks Like |
|---|---|---|---|---|
| 1 | `t_s` | `i64` (seconds) | Session elapsed time since start. | Negative or absurdly large values indicate clock skew. |
| 2 | `rtp_assembly_sum_us` | `i64` µs | Cumulative RTP assembly time across all AUs this second. | High sum + high count = slow assembly (fragment processing bottleneck). |
| 3 | `rtp_assembly_count` | `i64` (count) | Number of RTP assembly operations this second. | Zero = no packets received; extremely high = packet storm. |
| 4 | `rtp_assembly_max_us` | `i64` µs | Maximum single AU assembly time this second. | Large values (>500 µs) indicate pathological fragment processing. |
| 5 | `decode_us` | `i64` µs | Time spent decoding this second. | High values indicate decoder struggling (complex frames, low hw acceleration). |
| 6 | `pipeline_age_us` | `i64` µs | Age of the decode pipeline (time since first AU in current pipeline). | High age = pipeline backing up; chronic high = under-run. |
| 7 | `decoded` | `i64` (count) | Frames successfully decoded this second. | Far below `presented` = decode bottleneck; far above = possible double-count. |
| 8 | `skipped` | `i64` (count) | Frames skipped by the pipeline this second. | High count = backpressure forcing skips; indicates queue full condition. |
| 9 | `rate_limited` | `i64` (count) | Number of frames rate-limited this second. | Non-zero = bitrate cap (b=AS) is constraining output. |
| 10 | `presented` | `i64` (count) | Frames presented to display this second. | Significantly below `decoded` = frame drops; significantly above = possible duplication. |
| 11 | `replaced` | `i64` (count) | Frames replaced (dropped + new) this second. | High count = damaged frames being swapped out frequently. |
| 12 | `queue_full` | `i64` (count) | Number of times the access unit queue was full this second. | Non-zero = queue overflow risk; indicates congestion/backpressure. |
| 13 | `resyncs` | `i64` (count) | Number of resynchronization events (IDR/keyframe requests) this second. | Non-zero = decoder reference chain broken; recovery in progress. |
| 14 | `resets` | `i64` (count) | Number of metric/reset counters this session. | High count = frequent session resets; unstable connection. |
| 15 | `hw_buffered` | `i64` (count) | Number of frames hardware-buffered in the AVC decoder. | Zero = decoder not using hardware buffers; high = buffer pressure. |
| 16 | `pipeline_age_max_us` | `i64` µs | Maximum pipeline age observed this second. | High max = occasional severe backlog even if average is low. |
| 17 | `stream_lag_us` | `i64` µs | Stream slip: wall-time minus media-time (RTP clock at 90 kHz). Positive = falling behind. | Large positive = accumulating drift; negative = ahead of schedule. |
| 18 | `stream_lag_max_us` | `i64` µs | Maximum stream_lag_us observed since session start. | Large max = worst-case drift event recorded. |
| 19 | `rtp_hold_saved` | `i64` (count) | Number of AUs saved by the RTP hold mechanism (avoiding decode). | High count = many AUs held back, indicating queue pressure. |
| 20 | `rtp_hold_expired` | `i64` (count) | Number of held AUs that expired before resolution. | Non-zero = held AUs lost without repair; damage proceeds unmitigated. |
| 21 | `rtp_gaps` | `i64` (count) | RTP sequence gaps detected this second. | Non-zero = missing sequence numbers (loss or reordering). |
| 22 | `rtp_abandons` | `i64` (count) | RTP frames abandoned (not processed to completion). | Non-zero = frames dropped before assembly completion. |
| 23 | `rtp_wait_drops` | `i64` (count) | Frames dropped while waiting for missing packets/sequences. | Non-zero = patience timeout exceeded; gaps not filled. |
| 24 | `rtp_invalid` | `i64` (count) | Invalid RTP packets rejected this second. | Non-zero = corrupted or malformed RTP headers received. |
| 25 | `dec_queue_full` | `i64` (count) | Number of times the decode queue was full this second. | Non-zero = decode pipeline backpressure; frames discarded. |
| 26 | `flushes` | `i64` (count) | Number of encoder/decoder flush events this second. | High count = frequent flushes = stall recovery activity. |
| 27 | `twcc_sent` | `i64` (count) | Number of TWCC feedback packets sent this second. | Zero = TWCC disabled/blind; high = heavy feedback load. |
| 28 | `twcc_samples` | `i64` (count) | Number of TWCC sample observations this second. | Zero = no TWCC samples; indicates feedback path not active. |
| 29 | `rtp_ext_any` | `i64` (count) | Number of RTP packets with any extension header this second. | Low count = fewer extensions; high = more header overhead. |
| 30 | `twcc_stride` | `i64` (µs) | TWCC median stride (gap between observed sequence numbers). | High value = high reordering/variability in packet arrivals. |
| 31 | `video_metadata_sent` | `i64` (count) | Number of video metadata packets sent this second. | Non-keyframe metadata can add overhead; high = frequent updates. |
| 32 | `video_bandwidth_kbps` | `i64` kbps | Achieved video bitrate this second. | Low values (<1000 kbps on 2.4 GHz) = constrained link; high = exceeding cap. |
| 33 | `display_pickup_us` | `i64` µs | Time from decode completion to display presentation this frame. | High values = display lag; desynchronization A/V. |
| 34 | `display_pickup_max_us` | `i64` µs | Maximum display_pickup_us observed this session. | Large max = worst-case display delay event. |
| 35 | `stream_slip_us` | `i64` µs (signed) | Per-frame slip: wall time since anchor minus RTP media time advanced. Positive = behind. | Large positive = accumulating drift; chronic positive = pipeline falling behind. |
| 36 | `stream_slip_max_us` | `i64` µs (signed) | Maximum stream_slip_us observed this session. | Large max = worst slip event; indicates single severe drift. |
| 37 | `audio_queue_ms` | `i64` ms | SDL audio queue depth this second. | High values = audio lag dragging; sync issues with video. |
| 38 | `src_fps` | `i64` (count) | Source FPS count (often fixed at 1; analyzed via decoded column). | Value of 1 is expected; other values indicate analyzer shift bug. |
| 39 | `enc_width` | `i64` pixels | Encoded frame width. | Change from expected = resolution switch or error. |
| 40 | `enc_height` | `i64` pixels | Encoded frame height. | Change from expected = resolution switch or error. |
| 41 | `waiting_keyframe` | `i64` (count) | Number of frames waiting for a keyframe this second. | Non-zero = decoder starving for IDR; quality degradation. |
| 42 | `drops` | `i64` (count) | Frame drops this second (decoder- or queue-related). | Non-zero = frames being dropped; artefact risk. |
| 43 | `decode_errors` | `i64` (count) | Decode errors this second (H.264 parsing failures). | Non-zero = corrupted bitstream or parser bug. |
| 44 | `vt_exit_count` | `i64` (count) | Vita video track exit count this second. | Non-zero = video track exited unexpectedly. |
| 45 | `vt_max_0`..`vt_max_5` | `i64` (count each) | Per-segment max values for Vita video track segments 0-5. | High individual values = segment-specific issues. |
| 46 | `q_us` | `i64` µs (signed) | decode queue depth estimate (microseconds of pending AUs). | Positive values = queue backing up; negative = unusual. |
| 47 | `q_us_max` | `i64` µs (unsigned peak) | Maximum q_us observed this session. | High max = prolonged queue accumulation. |
| 48 | `q_slope_mpm` | `i64` µs/min (signed) | Queue depth slope per minute. Positive = accumulating backlog. | Positive slope = queue growing over time (accumulation risk). |
| 49 | `sh_match` | `i64` (count) | Sequence header match count (H.264 SH consistency). | Low count = mismatched slice headers; corruption. |
| 50 | `sh_bound_mismatch` | `i64` (count) | Slice header boundary mismatch events. | Non-zero = bitstream boundary issues; parser trouble. |
| 51 | `sh_bytes_mismatch` | `i64` (count) | Slice header byte count mismatch. | Non-byte-aligned data; corruption. |
| 52 | `sh_class_mismatch` | `i64` (count) | Slice class/type mismatch (e.g., unexpected NAL unit type). | Non-zero = unexpected NAL types in stream. |
| 53 | `sh_new_damaged_old_complete` | `i64` (count) | New AU: damaged old frame became complete (rare cross-over). | Non-zero = anomalous state transition. |
| 54 | `sh_new_complete_old_abandon` | `i64` (count) | New AU: complete old frame was abandoned. | Non-zero = frame state transition oddity. |
| 55 | `sh_hold_saved` | `i64` (count) | Slice-level hold-saved count (concealment avoided). | High count = concealment active on many frames. |
| 56 | `sh_legacy_unmatched` | `i64` (count) | Legacy depacketizer unmatched events. | Non-zero = legacy path compatibility issues. |
| 57 | `sh_new_unmatched` | `i64` (count) | New depacketizer unmatched events. | Non-zero = current path compatibility issues. |
| 58 | `au_submitted` | `i64` (count) | Access units submitted for decoding this second. | Low count = few AUs reaching decoder; high = overflow. |
| 59 | `au_no_startcode` | `i64` (count) | AUs received without start code (00 00 00 01). | Non-zero = malformed AUs; downstream parse failures. |
| 60 | `au_idr_submitted` | `i64` (count) | IDR access units submitted this second. | Non-zero = keyframe request active; quality recovery. |
| 61 | `sh_reject_bound_artifact` | `i64` (count) | Bound-artifact rejection events (H3 fix related). | Non-zero = H3 fix actively rejecting problematic AUs. |
| 62 | `late_after_resolve` | `i64` (count) | Packets arriving at/older than last resolved AU timestamp (phantom guard). | Non-zero = stale-ts fall-through (historical root cause). |
| 63 | `dam_markerless` | `i64` (count) | Damaged AUs with no marker bit (marker-loss tail). | Non-zero = tail packets lost; never NACKed (marker never seen). |
| 64 | `dam_first_slice` | `i64` (count) | Damaged AUs where the first slice was lost (had_first_slice=false). | Non-zero = leading-slice loss; quality floor contributor. |
| 65 | `dam_holes` | `i64` (count) | Damaged AUs with intra-AU holes (missing sequences). | Non-zero = lost packets within a frame; RTX repair candidates. |
| 66 | `rtp_ext_twcc` | `i64` (count) | RTP packets with TWCC extension ID parsed/negotiated. | Low count = TWCC extension not active; loss of feedback. |
| 67 | `rtcp_rtpfb_any` | `i64` (count) | Any RTCP PT=205 (RTP FB) packets that left the socket this second. | Zero = no RTCP feedback sent; blind to congestion. |
| 68 | `rtcp_sr_count` | `i64` (count) | Number of RTCP Sender Report blocks this second. | Zero = no SRs received; clock model blind. |
| 69 | `rtcp_sr_raw` | `i64` (count) | Raw RTCP SR count from wire tap (before fork parsing). | Zero/fork-blind = RTCP-read fork cannot parse SRs. |
| 70 | `rtcp_nack_sent` | `i64` (count) | Number of RTCP NACK packets sent this second. | Non-zero = NACK feedback active (rare; our client rarely sends). |
| 71 | `rtx_packets_received` | `i64` (count) | RTX packets received this second (if RTX enabled). | Non-zero = RTX repair path active; loss recovery. |
| 72 | `rtx_crate_seen` | `i64` (count) | Number of times RTX crate/pattern observed. | Non-zero = RTX packets seen on wire. |
| 73 | `sr_age_ms` | `i64` ms (signed) | Age of SR anchor in milliseconds. | Large positive = stale SR; clock model out of date. |
| 74 | `sr_clock_offset_ms` | `i64` ms (signed) | Console unix-ms minus client wall-ms clock offset. | Large offset = clock skew between console and client. |
| 75 | `capture_to_arrival_us` | `i64` µs (signed) | End-to-end capture-to-arrival latency (console capture + transport + assembly). | High values = high latency path; includes console queue time. |
| 76 | `capture_to_arrival_max_us` | `i64` µs (signed) | Maximum capture_to_arrival_us observed this session. | Large max = worst-case latency event. |
| 77 | `twcc_delay_med_us` | `i64` µs (unsigned) | TWCC delay median microseconds. | High median = consistent congestion delay. |
| 78 | `twcc_delay_max_us` | `i64` µs (unsigned) | TWCC delay maximum microseconds (worst-case per second). | Large max = occasional severe delay spikes. |
| 79 | `sr_anchor_ntp` | `i64` u64 (NTP ms since 1900) | NTP timestamp from SR (ms since 1900). | Uniform-random values = SR encrypted (fork-only readable). |
| 80 | `sr_anchor_rtp_ts` | `i64` u64 (RTP timestamp) | RTP timestamp from SR. | Wrapping/odd values = clock or parse issue. |
| 81 | `sr_anchor_local_us` | `i64` u64 (local µs since boot) | Local monotonic time when SR arrived (us). | Drift from expected = timing calibration issue. |
| 82 | `sr_anchor_local_wall_us` | `i64` u64 (wall µs since UNIX epoch) | Client wall clock at SR arrival (us since epoch). | Skew from NTP = clock sync issue. |
| 83 | `grace_held` | `i64` (count) | Number of damaged AUs held in grace park (v2+). | High count = many damaged AUs awaiting RTX/RTX fill. |
| 84 | `grace_filled` | `i64` (count) | Held AUs resolved Complete via grace (RTX filled holes). | Low fill rate = RTX not landing or holes too deep. |
| 85 | `grace_expired` | `i64` (count) | Held AUs resolved Damaged via grace expiry. | Non-zero = parked AU expired without repair. |
| 86 | `probe_rtt_ms_x10` | `i64` (x10 ms) | Cap-advisor probe RTT (×10, so 36 = ~3.6 ms). | Constant ~360 (=36 ms) forces low bitrate suggestions session-wide. |
| 87 | `probe_dl_mbps_x10` | `i64` (x10 Mbps) | Cap-advisor probe downlink measured (×10, so 50 = ~5 Mbps). | Low measured = advisor suggests low cap. |
| 88 | `probe_suggest_kbps` | `i64` kbps | Cap-advisor suggested bitrate cap for this session. | Low suggestion = conservative cap; high = aggressive cap. |
| 89 | `jb_boost` | `i64` (count) | Micro-jb boost slots active at last submit. | Non-zero = burst-mode micro-dejitter engaged. |
| 90 | `jb_depth` | `i64` (count) | Micro-jb decode bank depth at last submit. | High depth = bank grew to absorb burst; latent accumulation. |
| 91 | `jb_absorbed` | `i64` (count) | Number of submits admitted under micro-jb boost. | High count = boost actively absorbing burst events. |
| 92 | `damaged_dropped` | `i64` (count) | Damaged AUs dropped+PLI requested (Step 6 policy). | High count = many damaged frames triggering PLI→IDR. |
| 93 | `rtp_abandons` | `i64` (count) | *Duplicate of col 22?* RTP frames abandoned. | — |
| 94 | `rtp_wait_drops` | `i64` (count) | *Duplicate of col 23?* Frames dropped while waiting. | — |
| 95 | `rtp_invalid` | `i64` (count) | *Duplicate of col 24?* Invalid RTP packets. | — |
| 96 | `dec_queue_full` | `i64` (count) | *Duplicate of col 25?* Decode queue full events. | — |
| 97 | `flushes` | `i64` (count) | *Duplicate of col 26?* Flush events. | — |

**Note**: Columns 93–97 appear to be duplicates of earlier columns (22–26), likely from a prior header version or overlapping metric names. The authoritative column count is 97, enforced by the `[i64; 97]` array type.

**Uncertain columns**: If a column's meaning could not be determined from the code + HANDOFF context, it is marked as such. See the "Uncertain Mappings" section in `docs/HUD-REFERENCE.md` for HUD fields with similar treatment.
| 98 | `pli_idr_ms_last` | `i64` ms | Latency of the first IDR after a damage-policy shed (PLI→IDR). Last event. | Sustained high = console slow to answer keyframe requests. |
| 99 | `pli_idr_ms_max` | `i64` ms | Worst PLI→IDR latency this session. | High max = occasional long Quality-mode stalls. |

## Columns 100–106 (latency profile + performance-plan probes)

Columns 101–106 are observe-only probes: they change no streaming behaviour. `scripts/session-report.py` (outer repo) reads them.

| # | Column Name | Type/Units | Meaning | "Bad" Looks Like |
|---|---|---|---|---|
| 100 | `jb_target` | `i64` (count) | Latency-profile target bank depth at last submit (Fast = 0, Smooth = 6). | — (informational) |
| 101 | `ui_loop_us` | `i64` µs | Last UI-loop period (start of one iteration to the start of the next), sampled in the second. `0` = no sample landed before the drain. | Far from ~16 700 µs, or a wide spread: the loop is timer-paced, not vblank-locked. |
| 102 | `ui_work_us` | `i64` µs | Last UI-loop work time (iteration start to just after the frame is presented). | High = UI/render work leaves little of the frame budget; raises the loop period. |
| 103 | `rx_drain_max` | `i64` (count) | Largest number of datagrams the receive thread drained between two idle moments: how much had queued in the kernel socket. | Large values = the receive thread is not keeping up (starved or buffer too small). |
| 104 | `rtc_cpu_us` | `i64` µs | Time the RTC thread spent inside the WebRTC read path (`peer.handle_read`: SRTP decrypt, RTP demux) during the second. | A large share of 1 000 000 µs = the RTC thread is CPU-bound. |
| 105 | `idr_bytes` | `i64` bytes | Bytes of IDR (keyframe) access units submitted in the second. Divide by `video_bandwidth_kbps * 125` for the keyframe share of the stream. | A high share = bitrate spent on keyframes instead of picture. |
| 106 | `kf_req` | `i64` (count) | Keyframe requests actually sent in the second (after the cooldown gate). Compare with `au_idr_submitted`. | IDRs per request well above 1 = the console answers one request with several keyframes. |
