use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

static GAMEPAD_SEND_AGE_US: AtomicU64 = AtomicU64::new(0);

pub(crate) fn record_gamepad_send_age(age: Duration) {
    GAMEPAD_SEND_AGE_US.store(
        age.as_micros().min(u64::MAX as u128) as u64,
        Ordering::Relaxed,
    );
}

pub(crate) fn input_performance_summary() -> String {
    format!(
        "input us in:{}",
        GAMEPAD_SEND_AGE_US.load(Ordering::Relaxed)
    )
}
