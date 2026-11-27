/// `decode` — pluggable decode scheduling policy.

/// Implements the DecodeScheduler API specified in RENDERING-PIPELINE-ARCHITECTURE-PROPOSITION.md §16.4
/// (A1): pluggable DecodeScheduler with known-good default. DisplayClocked is the default mode
/// until spike S4 promotes PassThrough.

pub mod scheduler;

// Re-exported scheduler types so they're accessible from parent module via use super::*
pub use scheduler::{Admission, DecodeScheduler, SchedulerMode};

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn admit_fps_none_limit_1_len0_accept_len1_reject() {
        let sched = DecodeScheduler::new(SchedulerMode::DisplayClocked);
        assert_eq!(sched.admit(0, None, 0), Admission::Accept);
        assert_eq!(sched.admit(1, None, 0), Admission::RejectQueueFull);
    }

    #[test]
    fn admit_16667_limit_6_len5_accept_len6_reject() {
        let sched = DecodeScheduler::new(SchedulerMode::DisplayClocked);
        assert_eq!(sched.admit(5, Some(16667), 0), Admission::Accept);
        assert_eq!(sched.admit(6, Some(16667), 0), Admission::RejectQueueFull);
    }

    #[test]
    fn admit_20000_limit_5_len4_accept_len5_reject() {
        let sched = DecodeScheduler::new(SchedulerMode::DisplayClocked);
        assert_eq!(sched.admit(4, Some(20000), 0), Admission::Accept);
        assert_eq!(sched.admit(5, Some(20000), 0), Admission::RejectQueueFull);
    }

    #[test]
    fn admit_40000_limit_1() {
        let sched = DecodeScheduler::new(SchedulerMode::DisplayClocked);
        assert_eq!(sched.admit(0, Some(40000), 0), Admission::Accept);
        assert_eq!(sched.admit(1, Some(40000), 0), Admission::RejectQueueFull);
    }

    #[test]
    fn admit_pass_through_always_accept() {
        let sched = DecodeScheduler::new(SchedulerMode::PassThrough);
        assert_eq!(sched.admit(100, None, 0), Admission::Accept);
        assert_eq!(sched.admit(100, Some(40000), 0), Admission::Accept);
    }

    #[test]
    fn yield_displayclocked_2ms_depth0_and_5() {
        let sched = DecodeScheduler::new(SchedulerMode::DisplayClocked);
        assert_eq!(sched.yield_after_decode(0), Duration::from_millis(2));
        assert_eq!(sched.yield_after_decode(5), Duration::from_millis(2));
    }

    #[test]
    fn yield_paced_depths() {
        let sched = DecodeScheduler::new(SchedulerMode::Paced);
        assert_eq!(sched.yield_after_decode(0), Duration::from_millis(13));
        assert_eq!(sched.yield_after_decode(2), Duration::from_millis(11));
        assert_eq!(sched.yield_after_decode(6), Duration::from_millis(7));
        assert_eq!(sched.yield_after_decode(7), Duration::from_millis(2));
        assert_eq!(sched.yield_after_decode(99), Duration::from_millis(2));
    }

    #[test]
    fn yield_pass_through_depth0_is_zero_depth3_is_1ms() {
        let sched = DecodeScheduler::new(SchedulerMode::PassThrough);
        assert_eq!(sched.yield_after_decode(0), Duration::ZERO);
        assert_eq!(sched.yield_after_decode(3), Duration::from_millis(1));
    }

    #[test]
    fn default_mode_is_display_clocked() {
        assert_eq!(DecodeScheduler::default_mode(), SchedulerMode::DisplayClocked);
    }

    #[test]
    fn set_mode_switches_behavior() {
        let mut sched = DecodeScheduler::default();
        assert_eq!(sched.mode(), SchedulerMode::DisplayClocked);
        sched.set_mode(SchedulerMode::Paced);
        assert_eq!(sched.mode(), SchedulerMode::Paced);
        sched.set_mode(SchedulerMode::PassThrough);
        assert_eq!(sched.mode(), SchedulerMode::PassThrough);
    }

    #[test]
    fn admit_extra_limit_raises_limit() {
        let sched = DecodeScheduler::new(SchedulerMode::DisplayClocked);
        // 60 fps limit 6; extra_limit 2 → limit 8. The CALLER owns the
        // profile bound (Fast ≤2 / Smooth ≤6) — admit applies it as given.
        assert_eq!(sched.admit(6, Some(16667), 2), Admission::Accept);
        assert_eq!(sched.admit(7, Some(16667), 2), Admission::Accept);
        assert_eq!(sched.admit(8, Some(16667), 2), Admission::RejectQueueFull);
        assert_eq!(sched.admit(11, Some(16667), 6), Admission::Accept);
        assert_eq!(sched.admit(12, Some(16667), 6), Admission::RejectQueueFull);
    }

    #[test]
    fn admit_boost_zero_is_legacy_identical() {
        let sched = DecodeScheduler::new(SchedulerMode::DisplayClocked);
        assert_eq!(sched.admit(5, Some(16667), 0), Admission::Accept);
        assert_eq!(sched.admit(6, Some(16667), 0), Admission::RejectQueueFull);
    }
}