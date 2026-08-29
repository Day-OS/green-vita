# Input lag and video quality investigation

Session notes from debugging input lag and blocky video during fast motion,
compared against the sibling `OpenNOW-vita` (Nvidia GeForce Now) client.
Kept for context on what was tried, what worked, and what didn't - so the
same dead ends aren't re-explored later without new information.

## Kept

- **Gamepad dedup-throttle removed** (`src/api_xbox/streaming/rtc/worker.rs`).
  Held/unchanged controller state used to be resent at most every 50ms; now
  every tick is resent. The input channel is unreliable
  (`max_retransmits: 0`), so a held control could get stuck until it changed
  if a packet dropped. Confirmed on hardware: real, lower input lag, no
  downside.
- **Audio delivered directly from the RTC thread** (`src/streaming/audio.rs`,
  `src/api/streaming/rtc/rtp.rs`). Audio packets used to be batched and only
  picked up on the shell's render-frame cadence; they now reach the jitter
  buffer's decode worker as soon as they're received. Fixed audible
  clipping/glitching that traced back to that batching delay compounding
  with the jitter buffer's own reorder window.
- **Opus jitter buffer with RED recovery and PLC**, replacing the old
  `SampleBuilder` (`src/streaming/audio.rs`). Kept alongside the direct
  delivery fix above.
- **NACK-based retransmission for video** (`src/api/streaming/rtc/peer.rs`).
  The H264 codec already advertised `nack`/`nack pli` in its SDP feedback,
  but nothing acted on it - the peer connection ran with a no-op interceptor
  chain, so a lost packet only ever got recovered via a full keyframe
  request once `damage_score` crossed its threshold. Wired up
  `rtc-interceptor`'s NACK generator so a missing sequence number now
  triggers a targeted retransmission request instead. Its concrete type has
  to be spelled out (`Interceptor` requires `Sized`, so it can't be boxed as
  `dyn`) - every module touching a peer connection now imports the
  `RTCPeerConnection` alias from `api::streaming::rtc::peer` instead of the
  crate's bare type. On hardware this helped only marginally - see
  "Investigated and rejected" below for why.

## Reverted

Tried and confirmed net-negative (or no measurable benefit) via back-to-back
A/B hardware testing against the pre-session baseline:

- **Video RTP reorder-hold + RFC 3550 jitter estimator**
  (`src/api/streaming/rtc/rtp.rs`). Held an incomplete frame for a short
  grace period before giving up on it, to tolerate packet reordering rather
  than treating every gap as loss. On this network real loss dominates over
  reordering, so the hold neither rescued packets often enough to earn its
  keep nor avoided keyframe requests - it only added input delay.
- **Damage-score "just damaged" guard**. A fix layered on top of the
  reorder-hold work so a drop and an immediately-following successful
  delivery in the same tick wouldn't net-cancel `damage_score`. Reverted
  together with the reorder-hold it was patching, since the underlying
  mechanism it was refining is gone.
- **Submit/poll decoder scaffolding + stuck-decoder safety net**
  (`src/streaming/video/decoder.rs`, `src/streaming/video/worker.rs`,
  `src/streaming/video/metrics.rs`). Split the single blocking
  `sceAvcdecDecode` call into a submit/poll shape with a bounded retry
  counter, originally meant to support retrying the private/internal AVCDEC
  API safely. The internal API path itself was built, worked cleanly on
  hardware (zero stuck-decoder events), but made no difference to
  blockiness - so both the internal path and the scaffolding around it were
  reverted rather than carrying the extra complexity for no benefit.
- **Native/custom resolution** (960x544, matching the Vita panel exactly).
  Decoder init succeeded (960 and 544 are both multiples of 16, so no
  macroblock-alignment issue) but produced a black screen with audio still
  working - Xbox Cloud Gaming likely doesn't honor a non-16:9 custom
  resolution despite advertising `supportsCustomResolution`. Never
  conclusively confirmed server-side; the on-screen `srv:WxH` stats readout
  (`parse_server_video_size` in `src/api_xbox/streaming/control/channel.rs`)
  would show what the server actually reports sending, if this is revisited.
- **960x540** ("exact 16:9" follow-up guess). Hard local failure this time:
  `sceVideodecInitLibrary` rejected it with `SCE_VIDEODEC_ERROR_INVALID_PARAM`
  (`0x80620802`) because 540 isn't a multiple of 16 - H264 hardware decode
  requires whole macroblocks. Any future resolution attempt must keep both
  dimensions divisible by 16.
- **Raised bitrate ceiling** (`maxBitrateKbps` 2000 -> 4000 in
  `src/api_xbox/streaming/control/channel.rs`). Made blockiness worse, not
  better, at the same 1280x720 resolution - a higher announced ceiling just
  gave the encoder license to push more data onto a link that was already
  dropping packets.

## Investigated and rejected without implementing

- **Holding incomplete frames open long enough for a NACK-triggered
  retransmission to arrive.** Doesn't fit this pipeline: the hardware
  decoder is configured for a single reference frame
  (`AVCDEC_NUM_REF_FRAMES: u32 = 1` in `src/streaming/video/decoder.rs`), so
  decoding frames out of order once a delayed retransmission lands would
  scramble the reference chain and likely cause more corruption, not less.
  Properly buffering for reordering instead would mean adding a full RTT's
  worth of constant latency (60-100ms+) to every frame, which fights the
  session's other priority (lower input lag) far more than it would fix
  occasional blockiness. This is a known reason real-time interactive video
  generally leans on forward error correction or fast keyframe recovery
  instead of retransmission for video specifically.
- **goog-remb congestion-control feedback.** The H264 codec advertises
  `goog-remb` in its SDP feedback, same gap in spirit as the NACK one
  (advertised but never generated). Unlike NACK, `rtc-interceptor` has no
  ready-made builder for it - it would mean implementing the delay-based
  bandwidth-estimation algorithm from scratch, for a legacy mechanism mostly
  superseded industry-wide by TWCC, with no guarantee Xbox Cloud Gaming's
  proprietary server even reacts to it. Not attempted.

## Root cause of remaining motion blockiness

Confirmed via the on-screen `drop:` counter (`video_performance_summary` in
`src/streaming/video/metrics.rs`) rising in step with visible blockiness
during fast camera motion: it is real packet loss, not encoder-side
quantization. But every loss-recovery lever tried (reorder tolerance,
damage-score tuning, NACK retransmission) failed to meaningfully reduce it,
which points at the round-trip cost of any recovery mechanism being too
slow relative to how little time this pipeline can afford to wait on a
single video frame at 30-60fps. Treated as an accepted network/latency
trade-off rather than a bug to keep chasing.
