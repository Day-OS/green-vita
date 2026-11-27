mod decoder;
pub(crate) mod memory;
pub(crate) mod metrics;
pub(crate) mod worker;
pub(crate) mod metrics_log;
pub(crate) mod rtp_dump_log;

pub const STREAM_WIDTH: u32 = 1280;
pub const STREAM_HEIGHT: u32 = 720;
pub const HW_OUTPUT_WIDTH: u32 = 960;
pub const HW_OUTPUT_HEIGHT: u32 = 544;
pub(crate) const DEFAULT_VIDEO_FPS: u32 = 30;
pub(crate) const UNLOCKED_VIDEO_FPS: u32 = 60;
pub(crate) const NUM_TEXTURES: usize = 3;
/// Display ring size. Index `NUM_DISPLAY_TEXTURES` (the last texture) is the
/// decoder-private scratch target used to retire stale access units; it is
/// never displayed, so the ring bookkeeping below ignores it.
pub(crate) const NUM_DISPLAY_TEXTURES: usize = NUM_TEXTURES - 1;

pub use memory::reserve_decoder_cdram;
pub use metrics::video_performance_summary;
pub use worker::VideoDecodeWorker;

use crate::settings::ColorProfile;
use std::sync::atomic::AtomicBool;
use std::sync::{Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

// With double buffering (2 textures), the decoder writes into one texture while
// the renderer displays the other.  It must wait up to one VSYNC interval for the
// renderer to finish with the pending texture.  16.7ms matches 60 Hz VSYNC.
const MAX_PENDING_TEXTURE_WAIT: Duration = Duration::from_micros(16_700);

#[derive(Clone, Copy)]
pub(crate) struct VideoTextureTarget {
    pub(crate) ptr: usize,
    pub(crate) pitch: u32,
    pub(crate) capacity: u32,
}

struct DirectVideoOutputState {
    targets: Option<[VideoTextureTarget; NUM_TEXTURES]>,
    displayed: Option<usize>,
    last_displayed: Option<usize>,
    pending: Option<(usize, u64)>,
    next_generation: u64,
}

/// Synchronizes the decoder thread with the two SDL/GXM textures owned by the render thread.
/// Pointers are stored as integers so the platform-specific unsafe boundary stays in the code
/// that registers and consumes the textures.
pub(crate) struct DirectVideoOutput {
    state: Mutex<DirectVideoOutputState>,
    frame_displayed: Condvar,
    pub(crate) decoder_ready: AtomicBool,
    pub(crate) width: u32,
    pub(crate) height: u32,
    /// Hardware decoder output profile; the SDL renderer creates the direct
    /// video textures in the matching pixel format (shell/surface.rs).
    pub(crate) color_profile: ColorProfile,
}

impl DirectVideoOutput {
    pub(crate) fn new(width: u32, height: u32, color_profile: ColorProfile) -> Self {
        Self {
            state: Mutex::new(DirectVideoOutputState {
                targets: None,
                displayed: None,
                last_displayed: None,
                pending: None,
                next_generation: 0,
            }),
            frame_displayed: Condvar::new(),
            decoder_ready: AtomicBool::new(false),
            width,
            height,
            color_profile,
        }
    }

    pub(crate) fn set_targets(&self, targets: [VideoTextureTarget; NUM_TEXTURES]) {
        if let Ok(mut state) = self.state.lock() {
            state.targets = Some(targets);
            state.displayed = None;
            state.last_displayed = None;
            state.pending = None;
        }
    }

    pub(crate) fn clear_targets(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.targets = None;
            state.displayed = None;
            state.pending = None;
        }
        self.frame_displayed.notify_all();
    }

    /// 2b: the raw target for a slot (used by direct present to get the
        /// framebuffer pointer/pitch without the state lock held).
        pub(crate) fn target(&self, index: usize) -> Option<VideoTextureTarget> {
            let state = self.state.lock().ok()?;
            state.targets?.get(index).copied()
        }

        pub(crate) fn mark_displayed(&self, index: usize, generation: u64) {
        let mut cleared_pending = false;
        if let Ok(mut state) = self.state.lock() {
            state.last_displayed = state.displayed;
            state.displayed = Some(index);
            if state.pending == Some((index, generation)) {
                state.pending = None;
                cleared_pending = true;
            }
        }
        if cleared_pending {
            self.frame_displayed.notify_one();
        }
    }

    /// Releases a decoded frame intentionally omitted by the presentation-rate limiter.
    /// The currently displayed texture remains unchanged, so the decoder cannot overwrite it.
    pub(crate) fn discard_pending(&self, index: usize, generation: u64) {
        let mut discarded = false;
        if let Ok(mut state) = self.state.lock()
            && state.pending == Some((index, generation))
        {
            state.pending = None;
            discarded = true;
        }
        if discarded {
            self.frame_displayed.notify_one();
        }
    }

    pub(super) fn lock_decode_target(&self) -> Option<DirectVideoTargetGuard<'_>> {
        let mut state = self.state.lock().ok()?;
        let targets = state.targets?;

        // Fast path: find a texture that is neither displayed nor pending.
        // Also skip the most-recently-displayed texture: the GXM GPU may still
        // be DMA-reading it from the previous VSYNC cycle.  Waiting one full
        // display interval before recycling that slot prevents texture-race
        // artifacts that appear as blocky corruption.
        let free = (0..NUM_DISPLAY_TEXTURES).find(|&i| {
            state.displayed != Some(i)
                && state.last_displayed != Some(i)
                && state.pending.as_ref().map(|(idx, _)| *idx) != Some(i)
        });

        let index = if let Some(i) = free {
            i
        } else {
            // All NUM_TEXTURES are busy (should be impossible with > 2).
            // Fall through to the old condvar wait path.
            let (waited, _) = self
                .frame_displayed
                .wait_timeout_while(state, MAX_PENDING_TEXTURE_WAIT, |s| {
                    s.targets.is_some() && s.pending.is_some()
                })
                .ok()?;
            state = waited;
            (0..NUM_DISPLAY_TEXTURES).find(|&i| {
                state.displayed != Some(i)
                    && state.pending.as_ref().map(|(idx, _)| *idx) != Some(i)
            })?
        };

        Some(DirectVideoTargetGuard {
            state: Some(state),
            target: targets[index],
            index,
        })
    }

    /// Perf-plan stage 3: non-blocking variant of `lock_decode_target` —
    /// fast path only. Returns None when every texture is displayed, pending
    /// or still DMA-read by the GPU, without waiting.
    pub(super) fn try_lock_decode_target(&self) -> Option<DirectVideoTargetGuard<'_>> {
        let state = self.state.lock().ok()?;
        let targets = state.targets?;
        let index = (0..NUM_DISPLAY_TEXTURES).find(|&i| {
            state.displayed != Some(i)
                && state.last_displayed != Some(i)
                && state.pending.as_ref().map(|(idx, _)| *idx) != Some(i)
        })?;
        Some(DirectVideoTargetGuard {
            state: Some(state),
            target: targets[index],
            index,
        })
    }

    /// Decoder-private scratch target for retired (stale) access units.
    ///
    /// It is not part of the display ring: no `displayed`/`pending`
    /// bookkeeping, and — unlike `lock_decode_target` — the state mutex is NOT
    /// held across the hardware decode. Holding it serialized the decoder with
    /// the UI/RTC threads (blocking up to a vblank and, in the worst measured
    /// case, 66–172 ms), which slaved the decode drain rate to presentation and
    /// let a 60 fps arrival rate outrun a 58 fps display rate — the residual
    /// pipeline-age drift.
    pub(super) fn lock_scratch_target(&self) -> Option<DirectVideoTargetGuard<'_>> {
        let target = {
            let state = self.state.lock().ok()?;
            *state.targets.as_ref()?.get(NUM_DISPLAY_TEXTURES)?
        };
        Some(DirectVideoTargetGuard {
            state: None,
            target,
            index: NUM_DISPLAY_TEXTURES,
        })
    }
}

pub(super) struct DirectVideoTargetGuard<'a> {
    /// `None` for the decoder-private scratch target: it holds no ring state,
    /// so the state mutex is released before the hardware decode and
    /// `publish` must never be called on it (a retired frame is not displayed).
    state: Option<MutexGuard<'a, DirectVideoOutputState>>,
    target: VideoTextureTarget,
    index: usize,
}

impl DirectVideoTargetGuard<'_> {
    pub(super) fn publish(mut self) -> (usize, u64) {
        let state = self
            .state
            .as_mut()
            .expect("scratch target must never be published");
        state.next_generation = state.next_generation.wrapping_add(1);
        let generation = state.next_generation;
        state.pending = Some((self.index, generation));
        (self.index, generation)
    }
}

pub struct DecodedFrame {
    pub texture_index: usize,
    pub generation: u64,
    pub published_at: Instant,
}

#[derive(Clone, Copy)]
pub struct DecoderConfig {
    pub decode_width: u32,
    pub decode_height: u32,
    pub output_width: u32,
    pub output_height: u32,
    /// Hardware decoder output color profile (see `ColorProfile`).
    pub color_profile: ColorProfile,
}
