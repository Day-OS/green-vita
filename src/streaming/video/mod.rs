mod decoder;
mod memory;
mod metrics;
mod worker;

pub use memory::{decoder_memory_summary, reserve_decoder_cdram};
pub use metrics::{record_video_upload, video_performance_summary};
pub use worker::VideoDecodeWorker;

pub struct DecodedFrame {
    pub pixels: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub pitch: u32,
}

#[derive(Clone, Copy)]
pub struct DecoderConfig {
    pub decode_width: u32,
    pub decode_height: u32,
    pub output_width: u32,
    pub output_height: u32,
}
