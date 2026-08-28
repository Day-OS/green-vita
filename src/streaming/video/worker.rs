use super::decoder::HwVideoDecoder;
use super::metrics;
use super::{DecodedFrame, DecoderConfig, DirectVideoOutput};
use anyhow::{Context, Result};
use crossbeam_channel::{
    Receiver, RecvTimeoutError, Sender, TryRecvError, TrySendError, bounded, unbounded,
};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

// Faster streams arrive in larger bursts. Keep the normal 30 fps limit small, but
// allow enough room for roughly 100 ms of compressed video near the observed 55 fps.
const MIN_PENDING_ACCESS_UNITS: usize = 2;
const MAX_PENDING_ACCESS_UNITS: usize = 6;
/// How often to re-poll the hardware decoder for a picture once an access unit has been
/// submitted. Only meaningful once a future internal AVCDEC path can genuinely leave a picture
/// outstanding across polls - today's public-API-backed decoder always resolves on the first
/// poll, so this timer rarely fires in practice.
const PICTURE_POLL_INTERVAL: Duration = Duration::from_millis(1);
/// How long to block waiting for the next access unit when no picture is outstanding. Not a real
/// timeout - just bounds the blocking wait so the loop periodically re-checks for commands
/// without needing a second channel merged into the wait (`crossbeam_channel::select!`'s
/// timer-channel machinery pulls in enough extra code to trip the Vita linker's segment layout
/// on this binary; plain `recv_timeout`/`try_recv` avoid that).
const IDLE_WAIT: Duration = Duration::from_millis(250);
/// If a single access unit goes this many consecutive polls (~50 ms at `PICTURE_POLL_INTERVAL`)
/// without producing a picture or an error, the decoder is treated as stuck rather than polled
/// forever. This is the safety net a prior attempt at the internal submit/poll AVCDEC path was
/// missing: that path can report success on submit yet never produce a picture, and unlike an
/// explicit error return there is nothing else that would ever stop the poll loop.
const STUCK_DECODER_POLL_THRESHOLD: u32 = 50;

struct QueuedAccessUnit {
    data: Vec<u8>,
    queued_at: Instant,
    generation: u64,
}

enum DecoderCommand {
    Reset,
    Stop,
}

/// Tracks the access unit currently submitted to the hardware and awaiting its picture, across
/// the submit call and however many picture-poll ticks it takes to come back.
#[derive(Default)]
struct DecoderOutputState {
    picture_pending: bool,
    pending_generation: u64,
    pending_queued_at: Option<Instant>,
    /// How many consecutive polls have returned "not ready yet" for the current access unit.
    /// Reset on every new submit and on every picture/error result - see
    /// `STUCK_DECODER_POLL_THRESHOLD`.
    consecutive_not_ready: u32,
}

pub(crate) type DecodeResult = Result<DecodedFrame, String>;

pub struct VideoDecodeWorker {
    access_units: Sender<QueuedAccessUnit>,
    commands: Sender<DecoderCommand>,
    generation: Arc<AtomicU64>,
    pub(crate) latest_result: Arc<Mutex<Option<DecodeResult>>>,
    pub(crate) result_ready: Arc<tokio::sync::Notify>,
}

impl VideoDecodeWorker {
    pub fn spawn(config: DecoderConfig, direct_output: Arc<DirectVideoOutput>) -> Result<Self> {
        let decoder = HwVideoDecoder::new(config, false)
            .context("failed to create hardware H264 decoder")?;
        direct_output.decoder_ready.store(true, Ordering::Release);
        let (access_units, worker_access_units) = bounded(MAX_PENDING_ACCESS_UNITS);
        let (commands, worker_commands) = unbounded();
        let generation = Arc::new(AtomicU64::new(0));
        let worker_generation = Arc::clone(&generation);
        let latest_result = Arc::new(Mutex::new(None));
        let worker_latest_result = Arc::clone(&latest_result);
        let result_ready = Arc::new(tokio::sync::Notify::new());
        let worker_result_ready = Arc::clone(&result_ready);
        let worker_direct_output = Arc::clone(&direct_output);

        std::thread::Builder::new()
            .name("green-vita-video-decode".to_owned())
            .spawn(move || {
                #[cfg(target_os = "vita")]
                pin_decoder_thread();
                run_decode_loop(
                    worker_access_units,
                    worker_commands,
                    worker_generation,
                    worker_latest_result,
                    worker_result_ready,
                    decoder,
                    config,
                    worker_direct_output,
                )
            })
            .context("failed to spawn video decode worker")?;

        Ok(Self {
            access_units,
            commands,
            generation,
            latest_result,
            result_ready,
        })
    }

    pub fn submit_access_unit(&self, data: Vec<u8>, source_frame_duration_us: Option<u64>) -> bool {
        let source_fps = source_frame_duration_us
            .filter(|duration| *duration > 0)
            .map(|duration| 1_000_000 / duration)
            .unwrap_or(30);
        let extra_capacity = source_fps
            .saturating_sub(30)
            .min(25)
            .saturating_mul((MAX_PENDING_ACCESS_UNITS - MIN_PENDING_ACCESS_UNITS) as u64)
            .saturating_add(12)
            / 25;
        let pending_limit = MIN_PENDING_ACCESS_UNITS + extra_capacity as usize;
        if self.access_units.len() >= pending_limit {
            metrics::METRICS.queue_full.fetch_add(1, Ordering::Relaxed);
            return false;
        }

        let access_unit = QueuedAccessUnit {
            data,
            queued_at: Instant::now(),
            generation: self.generation.load(Ordering::Acquire),
        };
        match self.access_units.try_send(access_unit) {
            Ok(()) => true,
            Err(TrySendError::Full(_)) => {
                metrics::METRICS.queue_full.fetch_add(1, Ordering::Relaxed);
                false
            }
            Err(TrySendError::Disconnected(_)) => false,
        }
    }

    pub fn reset_decoder(&self) {
        self.generation.fetch_add(1, Ordering::AcqRel);
        metrics::METRICS.resets.fetch_add(1, Ordering::Relaxed);
        let _ = self.commands.send(DecoderCommand::Reset);
    }

    pub fn begin_resync(&self) {
        self.generation.fetch_add(1, Ordering::AcqRel);
        metrics::METRICS.resyncs.fetch_add(1, Ordering::Relaxed);
    }
}

impl Drop for VideoDecodeWorker {
    fn drop(&mut self) {
        let _ = self.commands.send(DecoderCommand::Stop);
    }
}

#[cfg(target_os = "vita")]
fn pin_decoder_thread() {
    let thread_id = unsafe { vitasdk_sys::sceKernelGetThreadId() };
    let result = unsafe {
        vitasdk_sys::sceKernelChangeThreadCpuAffinityMask(
            thread_id,
            vitasdk_sys::SCE_KERNEL_CPU_MASK_USER_2 as i32,
        )
    };
    if result < 0 {
        eprintln!("Failed to pin video decoder thread to user CPU 2: {result:#x}");
    }
}

fn run_decode_loop(
    access_units: Receiver<QueuedAccessUnit>,
    commands: Receiver<DecoderCommand>,
    generation: Arc<AtomicU64>,
    latest_result: Arc<Mutex<Option<DecodeResult>>>,
    result_ready: Arc<tokio::sync::Notify>,
    initial_decoder: HwVideoDecoder,
    config: DecoderConfig,
    direct_output: Arc<DirectVideoOutput>,
) {
    let mut decoder = Some(initial_decoder);
    let mut output_state = DecoderOutputState::default();
    // Sticky for the life of this worker (one streaming session) once tripped by the
    // stuck-decoder safety net below. Deliberately kept out of `DecoderOutputState`, which a
    // `Reset` command clears, since a decoder that just proved itself stuck should not be
    // retried the same way on the very next access unit.
    let mut force_public = false;

    loop {
        // Checked non-blocking on every iteration so a `Stop`/`Reset` is never stuck behind the
        // blocking recv below for longer than one wait period.
        match commands.try_recv() {
            Ok(DecoderCommand::Reset) => {
                decoder = None;
                output_state = DecoderOutputState::default();
                continue;
            }
            Ok(DecoderCommand::Stop) => break,
            Err(TryRecvError::Disconnected) => break,
            Err(TryRecvError::Empty) => {}
        }

        // Only wait as long as `PICTURE_POLL_INTERVAL` while a picture is actually outstanding,
        // so the poll below runs promptly; otherwise just block for up to `IDLE_WAIT` so this
        // loop still comes back around to check `commands` periodically while idle.
        let wait = if output_state.picture_pending {
            PICTURE_POLL_INTERVAL
        } else {
            IDLE_WAIT
        };

        match access_units.recv_timeout(wait) {
            Ok(access_unit) => {
                submit_queued_access_unit(
                    &mut decoder,
                    config,
                    &mut force_public,
                    &generation,
                    &latest_result,
                    &result_ready,
                    access_unit,
                    &direct_output,
                    &mut output_state,
                );
            }
            Err(RecvTimeoutError::Timeout) => {
                if output_state.picture_pending {
                    drain_picture(
                        &mut decoder,
                        &mut force_public,
                        &generation,
                        &latest_result,
                        &result_ready,
                        &direct_output,
                        &mut output_state,
                    );
                }
            }
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn submit_queued_access_unit(
    decoder: &mut Option<HwVideoDecoder>,
    config: DecoderConfig,
    force_public: &mut bool,
    generation: &AtomicU64,
    latest_result: &Mutex<Option<DecodeResult>>,
    result_ready: &tokio::sync::Notify,
    access_unit: QueuedAccessUnit,
    direct_output: &DirectVideoOutput,
    output_state: &mut DecoderOutputState,
) {
    if access_unit.generation != generation.load(Ordering::Acquire) {
        return;
    }

    if decoder.is_none() {
        match HwVideoDecoder::new(config, *force_public) {
            Ok(new_decoder) => *decoder = Some(new_decoder),
            Err(error) => {
                publish_result(
                    latest_result,
                    result_ready,
                    Err(format!("failed to recreate H264 decoder: {error:#}")),
                );
                return;
            }
        }
    }

    let submit_result = catch_unwind(AssertUnwindSafe(|| {
        decoder
            .as_mut()
            .expect("decoder recreated above")
            .submit_access_unit(&access_unit.data)
    }));
    if access_unit.generation != generation.load(Ordering::Acquire) {
        return;
    }

    match submit_result {
        Ok(Ok(())) => {
            output_state.picture_pending = true;
            output_state.pending_generation = access_unit.generation;
            output_state.pending_queued_at = Some(access_unit.queued_at);
            output_state.consecutive_not_ready = 0;
        }
        Ok(Err(error)) => {
            *decoder = None;
            output_state.picture_pending = false;
            publish_result(latest_result, result_ready, Err(error.to_string()));
            return;
        }
        Err(_) => {
            eprintln!("H264 decoder panicked; recreating decoder on next frame");
            *decoder = None;
            output_state.picture_pending = false;
            publish_result(
                latest_result,
                result_ready,
                Err("H264 decoder panicked and was restarted".to_owned()),
            );
            return;
        }
    }

    // A fast decoder often has a picture ready immediately after submit; try once here so it
    // doesn't wait out a full poll tick for no reason.
    drain_picture(
        decoder,
        force_public,
        generation,
        latest_result,
        result_ready,
        direct_output,
        output_state,
    );
}

#[allow(clippy::too_many_arguments)]
fn drain_picture(
    decoder: &mut Option<HwVideoDecoder>,
    force_public: &mut bool,
    generation: &AtomicU64,
    latest_result: &Mutex<Option<DecodeResult>>,
    result_ready: &tokio::sync::Notify,
    direct_output: &DirectVideoOutput,
    output_state: &mut DecoderOutputState,
) {
    let Some(decoder_instance) = decoder.as_mut() else {
        output_state.picture_pending = false;
        return;
    };
    if decoder_instance.submitted_sequence() == 0 {
        output_state.picture_pending = false;
        return;
    }
    if output_state.pending_generation != generation.load(Ordering::Acquire) {
        output_state.picture_pending = false;
        return;
    }

    let Some(direct_target) = direct_output.lock_decode_target() else {
        // Do not decode until the renderer has registered its two GXM textures. There is no
        // legacy output buffer to copy from anymore.
        metrics::METRICS.skipped.fetch_add(1, Ordering::Relaxed);
        return;
    };
    // Measure the hardware call and contain an unexpected decoder panic inside its worker.
    let decode_started_at = Instant::now();
    let picture_result = catch_unwind(AssertUnwindSafe(|| {
        decoder_instance.get_picture(direct_target.target)
    }));
    metrics::METRICS.decode_us.store(
        decode_started_at.elapsed().as_micros() as u64,
        Ordering::Relaxed,
    );
    if output_state.pending_generation != generation.load(Ordering::Acquire) {
        return;
    }

    match picture_result {
        Ok(Ok(Some(_pts))) => {
            output_state.consecutive_not_ready = 0;
            output_state.picture_pending = false;
            metrics::METRICS.decoded.fetch_add(1, Ordering::Relaxed);
            let (texture_index, texture_generation) = direct_target.publish();
            if let Some(queued_at) = output_state.pending_queued_at {
                metrics::METRICS
                    .pipeline_age_us
                    .store(queued_at.elapsed().as_micros() as u64, Ordering::Relaxed);
            }
            publish_result(
                latest_result,
                result_ready,
                Ok(DecodedFrame {
                    texture_index,
                    generation: texture_generation,
                }),
            );
        }
        Ok(Ok(None)) => {
            output_state.consecutive_not_ready = output_state.consecutive_not_ready.saturating_add(1);
            if output_state.consecutive_not_ready > STUCK_DECODER_POLL_THRESHOLD {
                let stuck_for_ms =
                    u64::from(output_state.consecutive_not_ready) * PICTURE_POLL_INTERVAL.as_millis() as u64;
                eprintln!(
                    "H264 decoder produced no picture after {} consecutive polls (~{stuck_for_ms}ms); \
                     treating it as stuck, tearing it down, and forcing the public decode path for \
                     the rest of this session",
                    output_state.consecutive_not_ready,
                );
                metrics::METRICS.stuck_resets.fetch_add(1, Ordering::Relaxed);
                *force_public = true;
                *decoder = None;
                output_state.picture_pending = false;
                output_state.consecutive_not_ready = 0;
                publish_result(
                    latest_result,
                    result_ready,
                    Err("H264 decoder stuck: no picture after repeated polls".to_owned()),
                );
                return;
            }
            output_state.picture_pending = true;
        }
        Ok(Err(error)) => {
            *decoder = None;
            output_state.picture_pending = false;
            output_state.consecutive_not_ready = 0;
            publish_result(latest_result, result_ready, Err(error.to_string()));
        }
        Err(_) => {
            eprintln!("H264 decoder panicked; recreating decoder on next frame");
            *decoder = None;
            output_state.picture_pending = false;
            output_state.consecutive_not_ready = 0;
            publish_result(
                latest_result,
                result_ready,
                Err("H264 decoder panicked and was restarted".to_owned()),
            );
        }
    }
}

fn publish_result(
    slot: &Mutex<Option<DecodeResult>>,
    result_ready: &tokio::sync::Notify,
    result: DecodeResult,
) {
    if let Ok(mut latest) = slot.lock()
        && latest.replace(result).is_some()
    {
        metrics::METRICS.replaced.fetch_add(1, Ordering::Relaxed);
    }
    result_ready.notify_one();
}
