mod ice;
mod media;
pub mod peer;
mod rtp;
pub mod session;
pub mod worker;

pub const AUDIO_SAMPLE_RATE: i32 = 48_000;
pub const AUDIO_CHANNELS: usize = 2;
pub(super) const STUN_SERVER: &str = "stun.l.google.com:19302";
