use super::decoder::HwVideoDecoder;
use super::metrics;
use super::{DecodedFrame, DecoderConfig, DirectVideoOutput, DirectVideoTargetGuard};
use anyhow::{Context, Result};
use crossbeam_channel::{Receiver, Sender, TrySendError, bounded, select_biased, unbounded};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

/// Backlog depth at which an access unit is considered stale and retired into
/// the scratch texture instead of being displayed. Depth 1 is ordinary jitter
/// (the queue oscillates 0<->1 every frame), so retiring there starves the
/// display; only a genuine burst (>= 2 already queued) is worth skipping.
const RETIRE_DEPTH: usize = 2;
/// Smooth profile keeps a deep bank by design (admission limit up to 13, i.e.
/// ~+100-200 ms of intentional buffering). Retiring at `RETIRE_DEPTH` there
/// would treat the bank itself as a burst and starve the display -- the
/// 2026-11-26 "Smooth gives frame drops and freezes" report. Retire at the
/// bank target (6): keep the intended 6-frame bank, retire beyond it. A
/// threshold of 12 (the admission edge) disabled retire entirely, so the
/// consumer could not drain and the queue backed up (queue_full 46/s, 52/s
/// decoded in the 4e1cd3a Smooth session).
const RETIRE_DEPTH_SMOOTH: usize = 6;
use std::time::{Duration, Instant};
use crate::pipeline::decode::{Admission, DecodeScheduler, SchedulerMode};

// Keep only one compressed frame queued at the default 30 FPS to minimize glass-to-glass
// latency. Faster streams may still use a larger burst buffer.
const MAX_PENDING_ACCESS_UNITS: usize = 6;
/// Target decoder pace range: 14-20ms (50-71 fps). Wider than the console's
/// typical 58-62 fps so brief excursions don't clip.
const MIN_DECODE_INTERVAL_US: u64 = 14_000;
const MAX_DECODE_INTERVAL_US: u64 = 20_000;
const DEFAULT_DECODE_INTERVAL_US: u64 = 16_667;

struct QueuedAccessUnit {
    data: Vec<u8>,
    queued_at: Instant,
    generation: u64,
}

enum DecoderCommand {
    Reset,
    Stop,
}

pub(crate) type DecodeResult = Result<DecodedFrame, String>;

pub struct VideoDecodeWorker {
    access_units: Sender<QueuedAccessUnit>,
    commands: Sender<DecoderCommand>,
    generation: Arc<AtomicU64>,
    pub(crate) latest_result: Arc<Mutex<Option<DecodeResult>>>,
    pub(crate) result_ready: Arc<tokio::sync::Notify>,
    /// Updated from RTP timestamps to pace the decoder to the console's encoder rate.
    pub(crate) last_source_duration_us: Arc<AtomicU64>,
    scheduler: Arc<DecodeScheduler>,
    /// Micro-jb (Step 5): burst-event boost slots + last-event wall ms.
    jb_boost: Arc<AtomicU64>,
    jb_last_event_ms: Arc<AtomicU64>,
}

/// Micro-jb event feeder: grace parks / damage bursts increment this; the
/// submit path swaps it to grow the bank's admission boost (Step 5).
pub(crate) static MICRO_JB_EVENTS: AtomicU64 = AtomicU64::new(0);

/// Latency profile (item 15, settings → stream start + live settings
/// changes): 0 = Fast (shallow bank, boost ≤2), 1 = Smooth (deep bank,
/// boost ≤6 — bursts and damage-shed stalls absorbed; +100–200 ms floor).
pub(crate) static LATENCY_PROFILE: AtomicU8 = AtomicU8::new(0);

impl VideoDecodeWorker {
    pub fn spawn(config: DecoderConfig, direct_output: Arc<DirectVideoOutput>) -> Result<Self> {
        // Session teardown is asynchronous: the previous session's RTC thread must observe
        // channel disconnect, drop its VideoDecodeWorker, and let the decode thread run
        // sceVideodecTermLibrary before a fresh sceVideodecInitLibrary can succeed. A
        // frozen/wedged previous session widens that window, so retry instead of failing
        // the whole session with 0x80620808-class errors.
        const DECODER_INIT_ATTEMPTS: usize = 20;
        const DECODER_INIT_RETRY_DELAY: Duration = Duration::from_millis(250);
        let mut decoder = None;
        for attempt in 1..=DECODER_INIT_ATTEMPTS {
            match HwVideoDecoder::new(config) {
                Ok(created) => {
                    decoder = Some(created);
                    break;
                }
                Err(error) if attempt < DECODER_INIT_ATTEMPTS => {
                    eprintln!(
                        "H264 decoder init failed (attempt {attempt}/{DECODER_INIT_ATTEMPTS}): {error:#}; retrying"
                    );
                    std::thread::sleep(DECODER_INIT_RETRY_DELAY);
                }
                Err(error) => {
                    return Err(error).context("failed to create hardware H264 decoder");
                }
            }
        }
        let decoder = decoder.expect("decoder created after retry loop");
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
        // S0 arm 1: Paced mode (depth-scaled pace) for the S0 scheduler matrix.
        let scheduler = Arc::new(DecodeScheduler::new(SchedulerMode::Paced));
        let worker_scheduler = Arc::clone(&scheduler);

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
                    worker_scheduler,
                )
            })
            .context("failed to spawn video decode worker")?;

        Ok(Self {
            access_units,
            commands,
            generation,
            latest_result,
            result_ready,
            // source_duration removed; last_source_duration_us initialized with default interval
            last_source_duration_us: Arc::new(AtomicU64::new(DEFAULT_DECODE_INTERVAL_US)),
            scheduler,
            jb_boost: Arc::new(AtomicU64::new(0)),
            jb_last_event_ms: Arc::new(AtomicU64::new(0)),
        })
    }

    pub fn submit_access_unit(&self, data: Vec<u8>, source_frame_duration_us: Option<u64>) -> bool {
        if let Some(duration) = source_frame_duration_us
            .filter(|d| (MIN_DECODE_INTERVAL_US..=MAX_DECODE_INTERVAL_US).contains(d))
        {
            self.last_source_duration_us.store(duration, Ordering::Relaxed);
        }
        // Latency profile (item 15): Fast = shallow bank (boost ≤2, no extra
        // limit — bursts shed or conceal; minimal floor); Smooth = deep bank
        // (boost ≤6, +6 slots on the fps-derived limit ⇒ capacity 12 ≈ +200 ms
        // — bursts and damage-shed stalls absorbed invisibly). Read per-submit
        // ⇒ live-switchable from Settings. Accumulation-safe by construction:
        // the admission limit is the hard bound on bank depth, and TWCC
        // arrival stamps are taken pre-bank (console feedback undistorted).
        let smooth = LATENCY_PROFILE.load(Ordering::Relaxed) == 1;
        let boost_max = if smooth { 6 } else { 2 };
        let jb_target: u64 = if smooth { 6 } else { 0 };
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let burst_events = MICRO_JB_EVENTS.swap(0, Ordering::Relaxed);
        let mut boost = self.jb_boost.load(Ordering::Relaxed) as usize;
        if burst_events > 0 {
            boost = (boost + 1).min(boost_max);
            self.jb_last_event_ms.store(now_ms, Ordering::Relaxed);
            self.jb_boost.store(boost as u64, Ordering::Relaxed);
        } else if boost > 0
            && now_ms.saturating_sub(self.jb_last_event_ms.load(Ordering::Relaxed))
                >= crate::pipeline::decode::scheduler::JB_DECAY_MS
        {
            boost = 0;
            self.jb_boost.store(0, Ordering::Relaxed);
        }
        metrics::METRICS
            .jb_boost
            .store(boost as u64, Ordering::Relaxed);
        metrics::METRICS
            .jb_depth
            .store(self.access_units.len() as u64, Ordering::Relaxed);
        metrics::METRICS
            .jb_target
            .store(jb_target, Ordering::Relaxed);
        if let Admission::RejectQueueFull = self
            .scheduler
            .admit(self.access_units.len(), source_frame_duration_us, boost)
        {
            metrics::METRICS.queue_full.fetch_add(1, Ordering::Relaxed);
            return false;
        }
        if boost > 0 {
            metrics::METRICS.jb_absorbed.fetch_add(1, Ordering::Relaxed);
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
    scheduler: Arc<DecodeScheduler>,
) {
    use std::thread::sleep;
    // 13 ms DisplayClocked pace is the f1bf114 known-good default,
    // now selected by the scheduler (pipeline::decode). The yield
    // duration is chosen per-mode: DisplayClocked → 13 ms, Paced →
    // depth-scaled, PassThrough → 1 ms or 0 if idle.
    let mut decoder = Some(initial_decoder);

    loop {
        select_biased! {
            recv(commands) -> command => match command {
                Ok(DecoderCommand::Reset) => {
                    decoder = None;
                    continue;
                }
                Ok(DecoderCommand::Stop) | Err(_) => break,
            },
            recv(access_units) -> access_unit => {
                let Ok(access_unit) = access_unit else { break };
                // Perf-plan stage 3c (2026-11-26): when a newer access unit is
                // already queued, this one is stale. Decode it anyway — the
                // H.264 reference chain must stay intact — into the
                // decoder-private scratch texture, and never publish it, never
                // pace-sleep after it. The scratch texture is outside the
                // display ring and needs no ring state lock, so a stale decode
                // no longer waits for the UI to consume `pending`: the backlog
                // drains at decode speed instead of the 58 fps presentation
                // rate. (Before: stale AUs blocked on the ring lock, so a
                // 60 fps arrival rate outran the loop and `pipeline_age`
                // drifted upward ~2.5 ms/min.)
                //
                // Revision 3 (2026-11-26): the retire decision needs a DEPTH
                // threshold, not "any". With `!is_empty()` the queue's normal
                // 0<->1 jitter retired 27 % of all access units (4350 of 16205
                // in the bd8904d session), and a retired AU is one the display
                // never gets -- 45 decoded/s instead of 60, seen as 10 fps
                // freezes and jumps. Depth >= 2 only triggers on a real burst.
                // Revision 4 (2026-11-26): the threshold is profile-aware. The
                // Smooth profile deliberately holds a deep bank (admission limit
                // up to 13), so a fixed depth of 2 retired the bank itself and
                // starved the display -- the reported Smooth freezes. Fast keeps
                // depth 2 (its queue genuinely oscillates 0<->1); Smooth retires
                // only at the admission edge.
                let retire_depth = if LATENCY_PROFILE.load(Ordering::Relaxed) == 1 {
                    RETIRE_DEPTH_SMOOTH
                } else {
                    RETIRE_DEPTH
                };
                let retire = access_units.len() >= retire_depth;
                let scratch = if retire {
                    direct_output.lock_scratch_target()
                } else {
                    None
                };
                if retire && scratch.is_none() {
                    // Renderer has not registered its textures yet.
                    metrics::METRICS.skipped.fetch_add(1, Ordering::Relaxed);
                    continue;
                }
                decode_queued_access_unit(
                    &mut decoder,
                    config,
                    &generation,
                    &latest_result,
                    &result_ready,
                    access_unit,
                    &direct_output,
                    retire,
                    scratch,
                );
                // Stage 3c, revision 2 (2026-11-26): the retire path is kept but
                // the pace sleep is NO LONGER skipped for retired AUs. The first
                // revision slept only for fresh frames, so a backlog drained at
                // full decode speed; the device A/B showed that unpaced feeding
                // makes sceAvcdec buffer pictures internally (`hw_buffered`
                // 0.86/s vs 0.008/s), which is the 1 s accumulation. Pacing every
                // decode keeps the decoder gated by the display while still
                // retiring stale pictures without publishing them.
                sleep(scheduler.yield_after_decode(access_units.len()));
            }
        }
    }
}

fn decode_queued_access_unit(
    decoder: &mut Option<HwVideoDecoder>,
    config: DecoderConfig,
    generation: &AtomicU64,
    latest_result: &Mutex<Option<DecodeResult>>,
    result_ready: &tokio::sync::Notify,
    access_unit: QueuedAccessUnit,
    direct_output: &DirectVideoOutput,
    retire: bool,
    scratch: Option<DirectVideoTargetGuard<'_>>,
) {
    if access_unit.generation != generation.load(Ordering::Acquire) {
        return;
    }

    if decoder.is_none() {
        match HwVideoDecoder::new(config) {
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

    // Stage 3: a stale frame arrives with its scratch texture pre-locked by
    // the caller; the normal path locks the display ring as before.
    let direct_target = match scratch {
        Some(target) => target,
        None => {
            let Some(target) = direct_output.lock_decode_target() else {
                // Do not decode until the renderer has registered its two GXM textures. There is no
                // legacy output buffer to copy from anymore.
                metrics::METRICS.skipped.fetch_add(1, Ordering::Relaxed);
                return;
            };
            target
        }
    };
    // Measure the hardware call and contain an unexpected decoder panic inside its worker.
    let decode_started_at = Instant::now();
    let decode_result = catch_unwind(AssertUnwindSafe(|| {
        decoder
            .as_mut()
            .expect("decoder recreated above")
            .decode(&access_unit.data, direct_target.target)
    }));
    metrics::METRICS.decode_us.store(
        decode_started_at.elapsed().as_micros() as u64,
        Ordering::Relaxed,
    );
    if access_unit.generation != generation.load(Ordering::Acquire) {
        return;
    }

    match decode_result {
        Ok(Ok(true)) => {
            if retire {
                // Stage 3: the picture is stale (a newer access unit was
                // queued behind it). It is retired before display — the
                // texture guard drops without publish, so the slot stays
                // free — while the decoder's reference state stays correct.
                metrics::METRICS.au_scratch.fetch_add(1, Ordering::Relaxed);
            } else {
                metrics::METRICS.decoded.fetch_add(1, Ordering::Relaxed);
                let (texture_index, generation) = direct_target.publish();
                let pipeline_age_us = access_unit.queued_at.elapsed().as_micros() as u64;
                metrics::METRICS
                    .pipeline_age_us
                    .store(pipeline_age_us, Ordering::Relaxed);
                metrics::METRICS
                    .pipeline_age_max_us
                    .fetch_max(pipeline_age_us, Ordering::Relaxed);
                publish_result(
                    latest_result,
                    result_ready,
                    Ok(DecodedFrame {
                        texture_index,
                        generation,
                        published_at: Instant::now(),
                    }),
                );
            }
        }
        Ok(Ok(false)) => {
            metrics::METRICS.hw_buffered.fetch_add(1, Ordering::Relaxed);
        }
        Ok(Err(error)) => {
            *decoder = None;
            publish_result(latest_result, result_ready, Err(error.to_string()));
        }
        Err(_) => {
            eprintln!("H264 decoder panicked; recreating decoder on next frame");
            *decoder = None;
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