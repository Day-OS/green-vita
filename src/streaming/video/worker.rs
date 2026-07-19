use super::decoder::HwVideoDecoder;
use super::metrics;
use super::{DecodedFrame, DecoderConfig};
use anyhow::{Context, Result};
use crossbeam_channel::{Receiver, Sender, TrySendError, bounded, select_biased, unbounded};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

const MAX_PENDING_ACCESS_UNITS: usize = 1;

struct QueuedAccessUnit {
    data: Vec<u8>,
    queued_at: Instant,
    generation: u64,
}

enum DecoderCommand {
    Reset,
    Resync,
    Stop,
}

type DecodeResult = Result<DecodedFrame, String>;

impl DecoderConfig {
    fn create(self) -> Result<HwVideoDecoder> {
        HwVideoDecoder::new(
            self.decode_width,
            self.decode_height,
            self.output_width,
            self.output_height,
        )
    }
}

pub struct VideoDecodeWorker {
    access_units: Sender<QueuedAccessUnit>,
    commands: Sender<DecoderCommand>,
    generation: Arc<AtomicU64>,
    latest_result: Arc<Mutex<Option<DecodeResult>>>,
}

impl VideoDecodeWorker {
    pub fn spawn(config: DecoderConfig) -> Result<Self> {
        let decoder = config
            .create()
            .context("failed to create hardware H264 decoder")?;
        let (access_units, worker_access_units) = bounded(MAX_PENDING_ACCESS_UNITS);
        let (commands, worker_commands) = unbounded();
        let generation = Arc::new(AtomicU64::new(0));
        let worker_generation = Arc::clone(&generation);
        let latest_result = Arc::new(Mutex::new(None));
        let worker_latest_result = Arc::clone(&latest_result);

        std::thread::Builder::new()
            .name("green-vita-video-decode".to_owned())
            .spawn(move || {
                run_decode_loop(
                    worker_access_units,
                    worker_commands,
                    worker_generation,
                    worker_latest_result,
                    decoder,
                    config,
                )
            })
            .context("failed to spawn video decode worker")?;

        Ok(Self {
            access_units,
            commands,
            generation,
            latest_result,
        })
    }

    pub fn submit_access_unit(&self, data: Vec<u8>) -> bool {
        let access_unit = QueuedAccessUnit {
            data,
            queued_at: Instant::now(),
            generation: self.generation.load(Ordering::Acquire),
        };
        match self.access_units.try_send(access_unit) {
            Ok(()) => true,
            Err(TrySendError::Full(_)) => {
                metrics::record_queue_full();
                false
            }
            Err(TrySendError::Disconnected(_)) => false,
        }
    }

    pub fn reset_decoder(&self) {
        self.generation.fetch_add(1, Ordering::AcqRel);
        metrics::record_reset();
        let _ = self.commands.send(DecoderCommand::Reset);
    }

    pub fn begin_resync(&self) {
        self.generation.fetch_add(1, Ordering::AcqRel);
        metrics::record_resync();
        let _ = self.commands.send(DecoderCommand::Resync);
    }

    pub fn try_recv(&self) -> Option<DecodeResult> {
        self.latest_result.lock().ok()?.take()
    }
}

impl Drop for VideoDecodeWorker {
    fn drop(&mut self) {
        let _ = self.commands.send(DecoderCommand::Stop);
    }
}

fn decode_access_unit(
    decoder: &mut HwVideoDecoder,
    data: &[u8],
) -> std::thread::Result<Result<bool>> {
    let started_at = Instant::now();
    let result = catch_unwind(AssertUnwindSafe(|| decoder.decode(data)));
    metrics::record_decode(started_at.elapsed());
    result
}

fn publish_decoded_frame(
    decoder: &HwVideoDecoder,
    access_units: &Receiver<QueuedAccessUnit>,
    latest_result: &Mutex<Option<DecodeResult>>,
    access_unit: &QueuedAccessUnit,
    skipped_last_frame: &mut bool,
) {
    let result_is_pending = latest_result.lock().is_ok_and(|result| result.is_some());
    if !access_units.is_empty() && result_is_pending && !*skipped_last_frame {
        *skipped_last_frame = true;
        metrics::record_skipped();
        return;
    }
    *skipped_last_frame = false;

    let copy_started_at = Instant::now();
    let pixels = decoder.copy_frame_bytes();
    metrics::record_copy(copy_started_at.elapsed());
    metrics::record_pipeline_age(access_unit.queued_at.elapsed());
    publish_result(
        latest_result,
        Ok(DecodedFrame {
            pixels,
            width: decoder.width,
            height: decoder.height,
            pitch: decoder.pitch,
        }),
    );
}

fn run_decode_loop(
    access_units: Receiver<QueuedAccessUnit>,
    commands: Receiver<DecoderCommand>,
    generation: Arc<AtomicU64>,
    latest_result: Arc<Mutex<Option<DecodeResult>>>,
    initial_decoder: HwVideoDecoder,
    config: DecoderConfig,
) {
    let mut decoder = Some(initial_decoder);
    let mut skipped_last_frame = false;

    loop {
        select_biased! {
            recv(commands) -> command => match command {
                Ok(DecoderCommand::Reset) => {
                    decoder = None;
                    continue;
                }
                Ok(DecoderCommand::Resync) => {
                    continue;
                }
                Ok(DecoderCommand::Stop) | Err(_) => break,
            },
            recv(access_units) -> access_unit => {
                let Ok(access_unit) = access_unit else { break };
                decode_queued_access_unit(
                    &mut decoder,
                    config,
                    &generation,
                    &access_units,
                    &latest_result,
                    access_unit,
                    &mut skipped_last_frame,
                );
            }
        }
    }
}

fn decode_queued_access_unit(
    decoder: &mut Option<HwVideoDecoder>,
    config: DecoderConfig,
    generation: &AtomicU64,
    access_units: &Receiver<QueuedAccessUnit>,
    latest_result: &Mutex<Option<DecodeResult>>,
    access_unit: QueuedAccessUnit,
    skipped_last_frame: &mut bool,
) {
    if access_unit.generation != generation.load(Ordering::Acquire) {
        return;
    }

    if decoder.is_none() {
        match config.create() {
            Ok(new_decoder) => *decoder = Some(new_decoder),
            Err(error) => {
                publish_result(
                    latest_result,
                    Err(format!("failed to recreate H264 decoder: {error:#}")),
                );
                return;
            }
        }
    }

    let decode_result = decode_access_unit(
        decoder.as_mut().expect("decoder recreated above"),
        &access_unit.data,
    );
    if access_unit.generation != generation.load(Ordering::Acquire) {
        return;
    }

    match decode_result {
        Ok(Ok(true)) => {
            metrics::record_decoded();
            publish_decoded_frame(
                decoder.as_ref().expect("decoder exists after decode"),
                access_units,
                latest_result,
                &access_unit,
                skipped_last_frame,
            );
        }
        Ok(Ok(false)) => {}
        Ok(Err(error)) => {
            *decoder = None;
            publish_result(latest_result, Err(error.to_string()));
        }
        Err(_) => {
            eprintln!("H264 decoder panicked; recreating decoder on next frame");
            *decoder = None;
            publish_result(
                latest_result,
                Err("H264 decoder panicked and was restarted".to_owned()),
            );
        }
    }
}

fn publish_result(slot: &Mutex<Option<DecodeResult>>, result: DecodeResult) {
    if let Ok(mut latest) = slot.lock()
        && latest.replace(result).is_some()
    {
        metrics::record_replaced();
    }
}
