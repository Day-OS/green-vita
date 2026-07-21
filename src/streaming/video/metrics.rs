use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::time::Duration;

pub(crate) struct DeltaCounter {
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

    pub(crate) fn increment(&self) {
        self.total.fetch_add(1, Ordering::Relaxed);
    }

    fn take_delta(&self) -> u64 {
        let current = self.total.load(Ordering::Relaxed);
        current.saturating_sub(self.reported.swap(current, Ordering::Relaxed))
    }
}

pub(crate) struct VideoMetrics {
    pub(crate) rtp_assembly_sum_us: AtomicU64,
    pub(crate) rtp_assembly_count: AtomicU64,
    pub(crate) rtp_assembly_max_us: AtomicU64,
    pub(crate) decode_us: AtomicU64,
    pub(crate) pipeline_age_us: AtomicU64,
    pub(crate) decoded: DeltaCounter,
    pub(crate) skipped: DeltaCounter,
    pub(crate) presented: DeltaCounter,
    pub(crate) replaced: DeltaCounter,
    pub(crate) receiver_replaced: DeltaCounter,
    pub(crate) handoff_replaced: DeltaCounter,
    pub(crate) render_loops: DeltaCounter,
    pub(crate) queue_full: DeltaCounter,
    pub(crate) resyncs: AtomicU64,
    pub(crate) resets: AtomicU64,
}

pub(crate) static METRICS: VideoMetrics = VideoMetrics {
    rtp_assembly_sum_us: AtomicU64::new(0),
    rtp_assembly_count: AtomicU64::new(0),
    rtp_assembly_max_us: AtomicU64::new(0),
    decode_us: AtomicU64::new(0),
    pipeline_age_us: AtomicU64::new(0),
    decoded: DeltaCounter::new(),
    skipped: DeltaCounter::new(),
    presented: DeltaCounter::new(),
    replaced: DeltaCounter::new(),
    receiver_replaced: DeltaCounter::new(),
    handoff_replaced: DeltaCounter::new(),
    render_loops: DeltaCounter::new(),
    queue_full: DeltaCounter::new(),
    resyncs: AtomicU64::new(0),
    resets: AtomicU64::new(0),
};

pub(crate) struct DecoderMemoryMetrics {
    pub(crate) frame_size: AtomicU32,
    pub(crate) frame_capacity: AtomicU32,
    pub(crate) output_size: AtomicU32,
    pub(crate) output_capacity: AtomicU32,
    pub(crate) reserved: AtomicU32,
}

pub(crate) static DECODER_MEMORY: DecoderMemoryMetrics = DecoderMemoryMetrics {
    frame_size: AtomicU32::new(0),
    frame_capacity: AtomicU32::new(0),
    output_size: AtomicU32::new(0),
    output_capacity: AtomicU32::new(0),
    reserved: AtomicU32::new(0),
};

pub(crate) fn micros(duration: Duration) -> u64 {
    duration.as_micros().min(u64::MAX as u128) as u64
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

pub fn video_performance_summary() -> String {
    let rtp_sum = METRICS.rtp_assembly_sum_us.swap(0, Ordering::Relaxed);
    let rtp_count = METRICS.rtp_assembly_count.swap(0, Ordering::Relaxed);
    let rtp_average = rtp_sum.checked_div(rtp_count).unwrap_or(0);
    let rtp_max = METRICS.rtp_assembly_max_us.swap(0, Ordering::Relaxed);
    format!(
        "fps d/p:{}/{} loop:{} us r:{rtp_average}/{rtp_max} d/a:{}/{} skip:{} repl:{} recv:{} hand:{} q:{} rs:{} rst:{}",
        METRICS.decoded.take_delta(),
        METRICS.presented.take_delta(),
        METRICS.render_loops.take_delta(),
        METRICS.decode_us.load(Ordering::Relaxed),
        METRICS.pipeline_age_us.load(Ordering::Relaxed),
        METRICS.skipped.take_delta(),
        METRICS.replaced.take_delta(),
        METRICS.receiver_replaced.take_delta(),
        METRICS.handoff_replaced.take_delta(),
        METRICS.queue_full.take_delta(),
        METRICS.resyncs.load(Ordering::Relaxed),
        METRICS.resets.load(Ordering::Relaxed),
    )
}
