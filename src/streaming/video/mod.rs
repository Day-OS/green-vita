mod decoder;
mod memory;
pub(crate) mod metrics;
mod worker;

// Back to the known-working 1280x720. Two custom-resolution experiments below native have failed
// for two different reasons, so don't retry either without new information:
//   - 960x540: sceVideodecInitLibrary rejected it outright with SCE_VIDEODEC_ERROR_INVALID_PARAM
//     (0x80620802). The Vita's hardware H.264 decoder requires coded dimensions to be whole
//     16x16 macroblocks; 540/16 = 33.75 is not an integer, so this is a hard local constraint,
//     unrelated to what Xbox Cloud Gaming does or doesn't honor. Any future resolution attempt
//     MUST have both width and height evenly divisible by 16.
//   - 960x544 (960/16=60, 544/16=34 - both exact, so it doesn't hit the constraint above):
//     decoder init succeeded, but produced a black screen with audio still working - a silent
//     decode failure (no error, numOfOutput presumably never leaving 0), not a hardware rejection.
//     That points at Xbox Cloud Gaming not actually encoding at the requested custom resolution,
//     but this was never confirmed - the on-screen `srv:WxH` stats readout (parse_server_video_size)
//     would show what the server actually reports sending, and wasn't checked before reverting.
// Do not guess another resolution value without checking that readout first - it's the one piece
// of evidence that would tell us definitively whether the server is honoring the request at all.
pub const STREAM_WIDTH: u32 = 1280;
pub const STREAM_HEIGHT: u32 = 720;
pub const HW_OUTPUT_WIDTH: u32 = 960;
pub const HW_OUTPUT_HEIGHT: u32 = 544;

pub use memory::reserve_decoder_cdram;
pub use metrics::video_performance_summary;
pub use worker::VideoDecodeWorker;

use std::sync::atomic::AtomicBool;
use std::sync::{Condvar, Mutex, MutexGuard};
use std::time::Duration;

// Let a short render hitch absorb at most two 30 fps intervals. Together with the
// frame already pending for presentation, this caps the microbuffer at three frames.
const MAX_PENDING_TEXTURE_WAIT: Duration = Duration::from_millis(67);

#[derive(Clone, Copy)]
pub(crate) struct VideoTextureTarget {
    pub(crate) ptr: usize,
    pub(crate) pitch: u32,
    pub(crate) capacity: u32,
}

struct DirectVideoOutputState {
    targets: Option<[VideoTextureTarget; 2]>,
    displayed: Option<usize>,
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
}

impl DirectVideoOutput {
    pub(crate) fn new(width: u32, height: u32) -> Self {
        Self {
            state: Mutex::new(DirectVideoOutputState {
                targets: None,
                displayed: None,
                pending: None,
                next_generation: 0,
            }),
            frame_displayed: Condvar::new(),
            decoder_ready: AtomicBool::new(false),
            width,
            height,
        }
    }

    pub(crate) fn set_targets(&self, targets: [VideoTextureTarget; 2]) {
        if let Ok(mut state) = self.state.lock() {
            state.targets = Some(targets);
            state.displayed = None;
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

    pub(crate) fn mark_displayed(&self, index: usize, generation: u64) {
        let mut cleared_pending = false;
        if let Ok(mut state) = self.state.lock() {
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

    pub(super) fn lock_decode_target(&self) -> Option<DirectVideoTargetGuard<'_>> {
        let mut state = self.state.lock().ok()?;
        if state.pending.is_some() {
            let (waited_state, _) = self
                .frame_displayed
                .wait_timeout_while(state, MAX_PENDING_TEXTURE_WAIT, |state| {
                    state.targets.is_some() && state.pending.is_some()
                })
                .ok()?;
            state = waited_state;
        }
        let targets = state.targets?;
        let index = state
            .pending
            .map(|(index, _)| index)
            .unwrap_or_else(|| state.displayed.map_or(0, |displayed| 1 - displayed));
        Some(DirectVideoTargetGuard {
            state,
            target: targets[index],
            index,
        })
    }
}

pub(super) struct DirectVideoTargetGuard<'a> {
    state: MutexGuard<'a, DirectVideoOutputState>,
    target: VideoTextureTarget,
    index: usize,
}

impl DirectVideoTargetGuard<'_> {
    pub(super) fn publish(mut self) -> (usize, u64) {
        self.state.next_generation = self.state.next_generation.wrapping_add(1);
        let generation = self.state.next_generation;
        self.state.pending = Some((self.index, generation));
        (self.index, generation)
    }
}

pub struct DecodedFrame {
    pub texture_index: usize,
    pub generation: u64,
}

#[derive(Clone, Copy)]
pub struct DecoderConfig {
    pub decode_width: u32,
    pub decode_height: u32,
    pub output_width: u32,
    pub output_height: u32,
}
