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
    rtp_assembly_sum_us: AtomicU64,
    rtp_assembly_count: AtomicU64,
    rtp_assembly_max_us: AtomicU64,
    decode_us: AtomicU64,
    pipeline_age_us: AtomicU64,
    decoded: DeltaCounter,
    skipped: DeltaCounter,
    presented: DeltaCounter,
    replaced: DeltaCounter,
    queue_full: DeltaCounter,
    resyncs: AtomicU64,
    resets: AtomicU64,
}

static METRICS: VideoMetrics = VideoMetrics {
    rtp_assembly_sum_us: AtomicU64::new(0),
    rtp_assembly_count: AtomicU64::new(0),
    rtp_assembly_max_us: AtomicU64::new(0),
    decode_us: AtomicU64::new(0),
    pipeline_age_us: AtomicU64::new(0),
    decoded: DeltaCounter::new(),
    skipped: DeltaCounter::new(),
    presented: DeltaCounter::new(),
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

pub(crate) fn record_rtp_assembly(duration: Duration) {
    let elapsed_us = micros(duration);
    METRICS
        .rtp_assembly_sum_us
        .fetch_add(elapsed_us, Ordering::Relaxed);
    METRICS.rtp_assembly_count.fetch_add(1, Ordering::Relaxed);
    METRICS
        .rtp_assembly_max_us
        .fetch_max(elapsed_us, Ordering::Relaxed);
}

pub(super) fn record_decode(duration: Duration) {
    METRICS.decode_us.store(micros(duration), Ordering::Relaxed);
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

pub fn record_video_presented() {
    METRICS.presented.increment();
}

pub fn video_performance_summary() -> String {
    let rtp_sum = METRICS.rtp_assembly_sum_us.swap(0, Ordering::Relaxed);
    let rtp_count = METRICS.rtp_assembly_count.swap(0, Ordering::Relaxed);
    let rtp_average = rtp_sum.checked_div(rtp_count).unwrap_or(0);
    let rtp_max = METRICS.rtp_assembly_max_us.swap(0, Ordering::Relaxed);
    format!(
        "fps d/p:{}/{} us r:{rtp_average}/{rtp_max} d/a:{}/{} skip:{} repl:{} q:{} rs:{} rst:{}",
        METRICS.decoded.take_delta(),
        METRICS.presented.take_delta(),
        METRICS.decode_us.load(Ordering::Relaxed),
        METRICS.pipeline_age_us.load(Ordering::Relaxed),
        METRICS.skipped.take_delta(),
        METRICS.replaced.take_delta(),
        METRICS.queue_full.take_delta(),
        METRICS.resyncs.load(Ordering::Relaxed),
        METRICS.resets.load(Ordering::Relaxed),
    )
}
