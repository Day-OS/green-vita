/// `scheduler` — pluggable decode scheduling policy.

use std::time::Duration;

/// Boost decay: the event boost returns to 0 after this long without a
/// burst event (ms). (The micro-jb const gate and JB_BOOST_MAX were
/// superseded 2026-11-03 by the settings-driven LatencyProfile — see
/// VideoDecodeWorker::submit.)
pub(crate) const JB_DECAY_MS: u64 = 2_000;

/// Admission decision for a new access unit into the decode queue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Admission {
    /// The access unit may be queued.
    Accept,
    /// The queue is full; the access unit should be rejected.
    RejectQueueFull,
}

/// Scheduling mode for the decode pipeline.
///
/// - `DisplayClocked`: adaptive admission (1..=6 queue slots by source fps)
///   + unconditional 13 ms post-decode pace. Default until S4 promotes PassThrough.
/// - `Paced`: depth-scaled pace table for A/B testing; admission identical to
///   DisplayClocked.
/// - `PassThrough`: no admission reject (caller keeps only channel capacity),
///   1 ms yield only while backlogged. Not a default: §16.5 gates promotion on
///   spike S4 (no accumulation ≥20 min with feedback levers).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SchedulerMode {
    DisplayClocked,
    Paced,
    PassThrough,
}

/// Decode scheduler policy core.
///
/// Implements the API specified in RENDERING-PIPELINE-ARCHITECTURE-PROPOSITION.md §16.4 (A1):
/// pluggable DecodeScheduler with known-good default. DisplayClocked is the default mode
/// until S4 promotes PassThrough.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DecodeScheduler {
    mode: SchedulerMode,
}

impl DecodeScheduler {
    pub fn new(mode: SchedulerMode) -> Self {
        Self { mode }
    }

    /// DEFAULT = f1bf114 known-good: adaptive admission (1..=6 queue slots by
    /// source fps) + unconditional 13 ms post-decode pace.
    pub fn default_mode() -> SchedulerMode {
        SchedulerMode::DisplayClocked
    }

    pub fn set_mode(&mut self, mode: SchedulerMode) {
        self.mode = mode;
    }

    pub fn mode(&self) -> SchedulerMode {
        self.mode
    }

    /// Submit-side admission. `queue_len` = queued AUs; `source_frame_duration_us`
    /// = per-frame source duration as passed today (raw, not range-filtered).
    /// `extra_limit` = caller-side latency-profile slots added on top of the
    /// fps-derived limit (Fast: 0–2; Smooth: 0–6 — see VideoDecodeWorker::submit;
    /// NOT clamped here — the caller owns the profile bound).
    pub fn admit(
        &self,
        queue_len: usize,
        source_frame_duration_us: Option<u64>,
        extra_limit: usize,
    ) -> Admission {
        match self.mode {
            SchedulerMode::PassThrough => Admission::Accept,

            SchedulerMode::DisplayClocked | SchedulerMode::Paced => {
                let source_fps = source_frame_duration_us
                    .filter(|d| *d > 0)
                    .map(|d| 1_000_000u64 / d)
                    .unwrap_or(30u64);

                let extra_capacity = source_fps
                    .saturating_sub(30)
                    .min(25)
                    .saturating_mul(5)
                    .saturating_add(12)
                    / 25;

                let pending_limit = 1usize + extra_capacity as usize + extra_limit;

                if queue_len >= pending_limit {
                    Admission::RejectQueueFull
                } else {
                    Admission::Accept
                }
            }
        }
    }

    /// Post-decode yield. `queue_depth` = AUs still queued when this frame finished decoding.
    pub fn yield_after_decode(&self, queue_depth: usize) -> Duration {
        match self.mode {
            // Stage 6 / latency (2026-11-26): 13 ms was the f1bf114 known-good
            // default, but it is a second, avoidable buffer on top of the ring
            // gate (lock_decode_target already waits for a free display slot).
            // The 2026-11-26 end-to-end measurement (button -> local square ->
            // streamed hover) was 190-220 ms; the client share is ~27 ms, of
            // which this sleep is 13. 2 ms keeps the decoder paced (the rule
            // that prevents hw_buffered accumulation is "not zero") while
            // returning ~11 ms. The scratch/retire path is still paced by the
            // same call, so it cannot free-run.
            SchedulerMode::DisplayClocked => Duration::from_millis(2),

            SchedulerMode::Paced => {
                const TABLE: [u64; 7] = [13, 13, 11, 10, 9, 8, 7];
                if queue_depth >= 7 {
                    Duration::from_millis(2) // catch-up
                } else {
                    Duration::from_millis(TABLE[queue_depth.min(6)])
                }
            }

            SchedulerMode::PassThrough => {
                if queue_depth > 0 {
                    Duration::from_millis(1)
                } else {
                    Duration::ZERO
                }
            }
        }
    }
}

impl Default for DecodeScheduler {
    fn default() -> Self {
        Self::new(SchedulerMode::DisplayClocked)
    }
}