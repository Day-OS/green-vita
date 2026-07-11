use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::time::Duration;

struct DeltaCounter {
    total: AtomicU64,
    reported: AtomicU64,
}

impl DeltaCounter {
    const fn new() -> Self {
        Self {
            total: AtomicU64::new(0),
            reported: AtomicU64::new(0),
        }
    }

    fn increment(&self) {
        self.total.fetch_add(1, Ordering::Relaxed);
    }

    fn take_delta(&self) -> u64 {
        let current = self.total.load(Ordering::Relaxed);
        current.saturating_sub(self.reported.swap(current, Ordering::Relaxed))
    }
}

struct VideoMetrics {
    decode_us: AtomicU64,
    copy_us: AtomicU64,
    pipeline_age_us: AtomicU64,
    upload_us: AtomicU64,
    decoded: DeltaCounter,
    skipped: DeltaCounter,
    uploaded: DeltaCounter,
    replaced: DeltaCounter,
    queue_full: DeltaCounter,
    resyncs: AtomicU64,
    resets: AtomicU64,
}

static METRICS: VideoMetrics = VideoMetrics {
    decode_us: AtomicU64::new(0),
    copy_us: AtomicU64::new(0),
    pipeline_age_us: AtomicU64::new(0),
    upload_us: AtomicU64::new(0),
    decoded: DeltaCounter::new(),
    skipped: DeltaCounter::new(),
    uploaded: DeltaCounter::new(),
    replaced: DeltaCounter::new(),
    queue_full: DeltaCounter::new(),
    resyncs: AtomicU64::new(0),
    resets: AtomicU64::new(0),
};

struct DecoderMemoryMetrics {
    frame_size: AtomicU32,
    frame_capacity: AtomicU32,
    output_size: AtomicU32,
    output_capacity: AtomicU32,
    reserved: AtomicU32,
}

static DECODER_MEMORY: DecoderMemoryMetrics = DecoderMemoryMetrics {
    frame_size: AtomicU32::new(0),
    frame_capacity: AtomicU32::new(0),
    output_size: AtomicU32::new(0),
    output_capacity: AtomicU32::new(0),
    reserved: AtomicU32::new(0),
};

fn micros(duration: Duration) -> u64 {
    duration.as_micros().min(u64::MAX as u128) as u64
}

pub(super) fn record_decode(duration: Duration) {
    METRICS.decode_us.store(micros(duration), Ordering::Relaxed);
}

pub(super) fn record_copy(duration: Duration) {
    METRICS.copy_us.store(micros(duration), Ordering::Relaxed);
}

pub(super) fn record_pipeline_age(duration: Duration) {
    METRICS
        .pipeline_age_us
        .store(micros(duration), Ordering::Relaxed);
}

pub(super) fn record_decoded() {
    METRICS.decoded.increment();
}

pub(super) fn record_skipped() {
    METRICS.skipped.increment();
}

pub(super) fn record_replaced() {
    METRICS.replaced.increment();
}

pub(super) fn record_queue_full() {
    METRICS.queue_full.increment();
}

pub(super) fn record_resync() {
    METRICS.resyncs.fetch_add(1, Ordering::Relaxed);
}

pub(super) fn record_reset() {
    METRICS.resets.fetch_add(1, Ordering::Relaxed);
}

pub(super) fn record_decoder_reservation(size: u32) {
    DECODER_MEMORY.reserved.store(size, Ordering::Relaxed);
}

pub(super) fn record_decoder_frame_memory(size: u32, capacity: u32) {
    DECODER_MEMORY.frame_size.store(size, Ordering::Relaxed);
    DECODER_MEMORY
        .frame_capacity
        .store(capacity, Ordering::Relaxed);
}

pub(super) fn record_decoder_output_memory(size: u32, capacity: u32) {
    DECODER_MEMORY.output_size.store(size, Ordering::Relaxed);
    DECODER_MEMORY
        .output_capacity
        .store(capacity, Ordering::Relaxed);
}

pub(super) fn decoder_memory_summary(free_memory: &str) -> String {
    format!(
        "mem need:{} out:{} cap:{} outcap:{} res:{} {free_memory}",
        DECODER_MEMORY.frame_size.load(Ordering::Relaxed),
        DECODER_MEMORY.output_size.load(Ordering::Relaxed),
        DECODER_MEMORY.frame_capacity.load(Ordering::Relaxed),
        DECODER_MEMORY.output_capacity.load(Ordering::Relaxed),
        DECODER_MEMORY.reserved.load(Ordering::Relaxed),
    )
}

pub fn record_video_upload(duration: Duration) {
    METRICS.upload_us.store(micros(duration), Ordering::Relaxed);
    METRICS.uploaded.increment();
}

pub fn video_performance_summary() -> String {
    format!(
        "fps d/u:{}/{} us d/c/u/a:{}/{}/{}/{} skip:{} repl:{} q:{} rs:{} rst:{}",
        METRICS.decoded.take_delta(),
        METRICS.uploaded.take_delta(),
        METRICS.decode_us.load(Ordering::Relaxed),
        METRICS.copy_us.load(Ordering::Relaxed),
        METRICS.upload_us.load(Ordering::Relaxed),
        METRICS.pipeline_age_us.load(Ordering::Relaxed),
        METRICS.skipped.take_delta(),
        METRICS.replaced.take_delta(),
        METRICS.queue_full.take_delta(),
        METRICS.resyncs.load(Ordering::Relaxed),
        METRICS.resets.load(Ordering::Relaxed),
    )
}
