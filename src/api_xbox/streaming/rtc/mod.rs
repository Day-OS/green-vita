pub mod peer;
pub(in crate::api_xbox::streaming) mod protocol;
pub mod worker;

pub(super) const STUN_SERVER: &str = "stun.l.google.com:19302";
pub(super) const ROUTE_PROBE: &str = "8.8.8.8:80";
pub(super) const AUDIO_PAYLOAD_TYPE: u8 = 111;
