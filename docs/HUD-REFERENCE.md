# HUD Reference

**Source**: `src/app/ui/screens/streaming.rs:build_debug_rows()` and `src/streaming/video/metrics.rs:video_performance_summary()`.
**Total HUD fields**: 41 (35 + six performance-plan probes added 2026-11-24: `RxCPU`, `UIloop`, `UIwork`, `KfReq`, `IDR`, `RxdMax`). These are the fields displayed in the two-column debug overlay on the Vita, and the multi-line summary in the performance stats.
**Style**: Left-column fields (left half of overlay) + right-column fields (right half). Panel height adapts to the taller column.

| Field | Type/Units | Meaning | "Bad" Looks Like |
|-------|---|---|---|
| `d/p` | Ratio (decoded/presented) | Decoded frames vs presented frames ratio. | decoded >> presented = many drops; presented >> decoded = heavy backlog/queuing. |
| `d/a` | `i64` ms | Decode age / pipeline age in milliseconds. `d/a:{decode_us/1000}/{pipeline_age_us/1000}`. | High values = decoder falling behind; frame is old by time it's decoded. |
| `skip` | `i64` (count) | Frames skipped by the pipeline this second. | High count = backpressure forcing skips to maintain pace. |
| `cap` | `i64` (count) | Rate-limited flag: number of frames rate-limited this second. | Non-zero = bitrate cap (b=AS) is constraining output. |
| `repl` | `i64` (count) | Frames replaced (dropped + new) this second. | High count = damaged frames being swapped out frequently. |
| `q` | `i64` µs (signed) | Decode queue depth (`q_us`). | Positive values = queue backing up; negative = unusual state. |
| `hwb` | `i64` (count) | Hardware-buffered frames in AVC decoder. | Zero = decoder not using HW buffers; high = buffer pressure. |
| `ageMax` | `i64` ms | Maximum pipeline age observed this session (`pipeline_age_max_us / 1000`). | High max = occasional severe backlog even if average is low. |
| `lag` | `i64` ms | Stream slip in ms (`stream_lag_us / 1000`). Positive = falling behind schedule. | Large positive = accumulating drift; chronic positive = pipeline falling behind. |
| `lagMax` | `i64` ms | Maximum stream_lag_us / 1000 observed this session. | Large max = worst-case drift event recorded. |
| `bw` | `i64` kbps | Achieved video bitrate (`video_bandwidth_kbps`). | Low values (<1000 kbps on 2.4 GHz) = constrained link; quality loss. |
| `aq` | `i64` ms | SDL audio queue depth in ms. | High values = audio lag dragging; A/V sync issues. |
| `slip` | `i64` µs (signed) | Per-frame slip: wall time since anchor minus RTP media time advanced. Positive = behind. | Large positive = accumulating drift; chronic positive = pipeline falling behind. |
| `slipS` | `i64` µs (signed) | Maximum stream_slip_us observed this session. | Large max = single severe drift event. |
| `qS` | `i64` µs/min (signed) | Queue depth slope per minute (`q_slope_mpm`). Positive = accumulating backlog. | Positive slope = queue growing over time (accumulation risk). |
| `cta` | `i64` ms | Capture-to-arrival latency in ms (`capture_to_arrival_us / 1000`). | High values = high end-to-end latency; includes console queue time. |
| `ctaMax` | `i64` ms | Maximum capture_to_arrival_ms observed this session. | Large max = worst-case latency event. |
| `srAge` | `i64` ms (signed) | Age of SR (Sender Report) anchor in ms (`sr_age_ms`). | Large positive = stale SR; clock model out of date. |
| `srCoff` | `i64` ms (signed) | Console-client clock offset in ms (`sr_clock_offset_ms`). | Large offset = clock skew between console and client. |
| `TcD` | `i64` µs (unsigned) | TWCC delay median microseconds (`twcc_delay_med_us`). | High median = consistent congestion delay. |
| `TcDMx` | `i64` µs (unsigned) | TWCC delay maximum microseconds (`twcc_delay_max_us`). | Large max = occasional severe delay spikes (36 ms Gauss tail). |
| `r:` | Overview RTP stats | RTP statistics overview (`r:{rtp_average}/{rtp_max}` from `video_performance_summary()`). Meaning uncertain; part of RTP stats line. | — |
| `rs` | `i64` (count) | Session resets count (`METRICS.resets`). | High count = frequent session resets; unstable connection. |
| `rst` | `i64` (count) | RTP invalid packets count (`METRICS.rtp_invalid`). | Non-zero = corrupted or malformed RTP headers received. |
| `wk` | `i64` (count) | RTP wait-drops count (`METRICS.rtp_wait_drops`). | Non-zero = patience timeout exceeded; gaps not filled. |
| `ab` | `i64` (count) | RTP abandons count (`METRICS.rtp_abandons`). | Non-zero = frames dropped before assembly completion. |
| `hold` | `i64` / `i64` (two values) | Hold stats: `rtp_hold_saved / rtp_hold_expired`. First = AUs saved by hold; second = held AUs expired. | High expired = damaged AUs lost without repair. |
| `iv` | `i64` (count) | RTP gaps count (`METRICS.rtp_gaps`) — *uncertain mapping*. | Non-zero = sequence gaps detected (loss or reordering). |
| `qf` | `i64` (count) | Queue full count (`METRICS.queue_full`). | Non-zero = decode queue overflow; backpressure active. |
| `fl` | `i64` (count) | Flush count (`METRICS.flushes`). | High count = frequent flush events = stall recovery activity. |
| `tw` | `i64` / `i64` / `i64` (three values) | TWCC stats: typically `twcc_sent / twcc_samples / rtp_ext_any`. | High values = heavy TWCC feedback load; congestion signaling. |
| `stride` | `i64` µs | TWCC stride (`twcc_stride`). | High value = high reordering/variability in packet arrivals. |
| `md` | `i64` (count) | Slice/header match metric (`sh_match`) — *uncertain mapping*. | Mismatch = slice header inconsistency; bitstream corruption. |
| `pick` | `i64` µs | Display pickup us (`display_pickup_us`). | High values = display lag; A/V desynchronization. |
| `pickMax` | `i64` µs | Maximum display_pickup_us observed this session. | Large max = worst-case display delay event. |
| `RxCPU` | `i64` ms | RTC-thread time in the WebRTC read path during the last second (`rtc_cpu_us / 1000`). | A large fraction of a second = the RTC thread is CPU-bound. |
| `UIloop` | `i64` ms | Last UI-loop period (`ui_loop_us / 1000`). | Not ~17 ms, or jumpy = loop is timer-paced, not vblank-locked. |
| `UIwork` | `i64` ms | Last UI-loop work time, iteration start to after present (`ui_work_us / 1000`). | Close to the loop period = no headroom. |
| `KfReq` | `i64` (count) | Keyframe requests sent in the last second (`kf_req`). | Many per second = keyframe storms. |
| `IDR` | `i64` KB | Keyframe bytes submitted in the last second (`idr_bytes / 1024`). | Large = bitrate spent on keyframes. |
| `RxdMax` | `i64` (count) | Largest datagram batch the receive thread drained at once in the last second (`rx_drain_max`). | Large = packets pile up faster than they are drained. |

## Uncertain HUD Field Mappings

The following fields have meanings that could not be definitively determined from the code + HANDOFF context alone. They are marked as such rather than guessed silently:

| Field | Uncertainty | Possible Alternatives |
|-------|---|---|
| `r:` | The format `r:{rtp_average}/{rtp_max}` uses named placeholders in `format!()` whose argument resolution could not be fully traced. The `video_performance_summary()` call passes `METRICS.resyncs.load()` as arg0, but `{rtp_average}` and `{rtp_max}` do not obviously map to any `MetricsSnapshot` field. Likely means "RTP average/max" some aggregate, but exact meaning uncertain. | Could be `resyncs/presented` ratio, or some computed RTP overview. |
| `iv` | Mapped as `rtp_gaps` based on HANDOFF.md §9 description "RTP stats" grouping, but the code's `format!()` argument indexing for `iv:{}` is ambiguous (named placeholders `{rtp_average}/{rtp_max}` confuse the positional arg count). Alternative candidates: `rtcp_rtpfb_any`, `dec_queue_full`. | `rtp_gaps` (sequence gaps), `rtcp_rtpfb_any` (any RTCP FB), `dec_queue_full` (queue pressure). |
| `md` | Mapped as `sh_match` based on HANDOFF.md §9 listing `md` alongside other HUD fields, but the `format!()` argument position for `md:{}` is unclear due to the `{rtp_average}/{rtp_max}` named placeupshifted the positional arg count. Alternative candidates: `sh_bound_mismatch`, `sh_bytes_mismatch`, `sh_class_mismatch`. | `sh_match` (header match), `sh_bound_mismatch` (boundary), `sh_bytes_mismatch` (byte count), `sh_class_mismatch` (NAL type). |

**How to resolve uncertain fields**: Re-read the `format!()` argument list in `video_performance_summary()` and cross-reference with `MetricsSnapshot` field names. The `{rtp_average}/{rtp_max}` named placeholders appear to reference variables not obviously in scope, suggesting either a Rust format feature variant or a code evolution where these were added later.