use std::sync::atomic::{AtomicU64, Ordering};

pub(crate) static GAMEPAD_SEND_AGE_US: AtomicU64 = AtomicU64::new(0);

pub(crate) fn performance_summary() -> String {
    format!(
        "input us in:{}",
        GAMEPAD_SEND_AGE_US.load(Ordering::Relaxed)
    )
}
