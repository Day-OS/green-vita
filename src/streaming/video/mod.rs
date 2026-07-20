mod decoder;
mod memory;
mod metrics;
mod worker;

pub use memory::{decoder_memory_summary, reserve_decoder_cdram};
pub(crate) use metrics::record_rtp_assembly;
pub use metrics::{record_video_presented, video_performance_summary};
pub use worker::VideoDecodeWorker;

use std::sync::{Mutex, MutexGuard};

#[derive(Clone, Copy)]
pub(crate) struct VideoTextureTarget {
    pub(crate) ptr: usize,
    pub(crate) pitch: u32,
    pub(crate) capacity: u32,
}

struct DirectVideoOutputState {
    targets: Option<[VideoTextureTarget; 2]>,
    displayed: Option<usize>,
    pending: Option<usize>,
    decoder_ready: bool,
}

/// Synchronizes the decoder thread with the two SDL/GXM textures owned by the render thread.
/// Pointers are stored as integers so the platform-specific unsafe boundary stays in the code
/// that registers and consumes the textures.
pub(crate) struct DirectVideoOutput {
    state: Mutex<DirectVideoOutputState>,
    width: u32,
    height: u32,
}

impl DirectVideoOutput {
    pub(crate) fn new(width: u32, height: u32) -> Self {
        Self {
            state: Mutex::new(DirectVideoOutputState {
                targets: None,
                displayed: None,
                pending: None,
                decoder_ready: false,
            }),
            width,
            height,
        }
    }

    pub(crate) fn dimensions(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    pub(super) fn mark_decoder_ready(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.decoder_ready = true;
        }
    }

    pub(crate) fn decoder_ready(&self) -> bool {
        self.state.lock().is_ok_and(|state| state.decoder_ready)
    }

    pub(crate) fn set_targets(&self, targets: [VideoTextureTarget; 2]) {
        if let Ok(mut state) = self.state.lock() {
            let frame_bytes = self.width.saturating_mul(self.height).saturating_mul(2);
            let capacity = targets
                .iter()
                .fold(0u32, |total, target| total.saturating_add(target.capacity));
            metrics::record_decoder_output_memory(frame_bytes.saturating_mul(2), capacity);
            state.targets = Some(targets);
            state.displayed = None;
            state.pending = None;
        }
    }

    pub(crate) fn clear_targets(&self) {
        if let Ok(mut state) = self.state.lock() {
            metrics::record_decoder_output_memory(0, 0);
            state.targets = None;
            state.displayed = None;
            state.pending = None;
        }
    }

    pub(crate) fn mark_displayed(&self, index: usize) {
        if let Ok(mut state) = self.state.lock() {
            state.displayed = Some(index);
            if state.pending == Some(index) {
                state.pending = None;
            }
        }
    }

    pub(super) fn lock_decode_target(&self) -> Option<DirectVideoTargetGuard<'_>> {
        let state = self.state.lock().ok()?;
        let targets = state.targets?;
        let index = state
            .pending
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
    pub(super) fn target(&self) -> VideoTextureTarget {
        self.target
    }

    pub(super) fn publish(mut self) -> usize {
        self.state.pending = Some(self.index);
        self.index
    }
}

pub struct DecodedFrame {
    pub texture_index: usize,
    pub width: u32,
    pub height: u32,
}

#[derive(Clone, Copy)]
pub struct DecoderConfig {
    pub decode_width: u32,
    pub decode_height: u32,
    pub output_width: u32,
    pub output_height: u32,
}
