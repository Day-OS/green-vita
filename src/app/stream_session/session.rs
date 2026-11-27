use crate::api::streaming::{PlaybackBackend, PlaybackBackendEvent};
use crate::settings::{ColorProfile, H264Profile};
use crate::settings::Settings;
use crate::streaming::input::{GamepadFrame, PointerEvent};
use crate::streaming::video::{DecodedFrame, DirectVideoOutput};
use crate::{Stream, StreamKind};
use anyhow::Result;
use bytes::Bytes;
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Clone, Copy)]
pub(super) enum StreamReturnTarget {
    Titles(usize),
    Consoles(usize),
}

/// Absolute frame-silence threshold before a stream is declared stalled
/// (idle/screensaver goal): a healthy console sends frames even on static
/// scenes (measured floors 72–122 kbps WITH frames, HUD `src` ≈ 60 fps), so
/// 20 s of zero decoded frames means the media path is dead — typically the
/// console's screensaver idle state, after which it never resumes sending.
pub(crate) const STREAM_STALL_THRESHOLD: Duration = Duration::from_secs(20);

/// Pure decision: is this much frame silence a stall?
pub(crate) fn frame_silence_is_stall(silence: Duration) -> bool {
    silence >= STREAM_STALL_THRESHOLD
}

/// Outcome of one stall-poll tick (`StreamingSession::poll_stall`).
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum StallEvent {
    /// No change.
    None,
    /// Frame silence crossed the threshold — caller should attempt a wake
    /// (guide pulse) and surface the reconnect overlay.
    Stalled,
    /// Frames resumed after a stall — caller may close a stall-opened
    /// overlay.
    Recovered,
}

pub(crate) struct StreamingSession {
    pub(crate) paused: bool,
    pub(crate) status: String,
    pub(crate) hint_started_at: Instant,
    pub(in crate::app) pause_selected: usize,
    pub(in crate::app) title_id: Option<String>,
    pub(super) return_target: StreamReturnTarget,
    backend: PlaybackBackend,
    latest_video_frame: Option<u64>,
    current_video_frame: Option<DecodedFrame>,
    stream_video_size: Option<(u32, u32)>,
    pending_audio_packets: Vec<Bytes>,
    ignore_confirm_until_release: bool,
    /// Stall detection (idle/screensaver goal): last decoded-frame time,
    /// whether the first frame ever arrived (no stall verdict before it —
    /// handshake legitimately takes seconds), current stall flag, and
    /// whether WE auto-opened the pause overlay for it (we close it again
    /// on recovery).
    last_frame_at: Instant,
    received_first_frame: bool,
    pub(in crate::app) stalled: bool,
    stall_opened_overlay: bool,
}

impl StreamingSession {
    pub(super) fn start_xbox(
        stream: Stream,
        kind: StreamKind,
        title_id: Option<String>,
        return_selected: usize,
        request_60fps: bool,
        video_bitrate_kbps: u32,
        video_h264_profile: H264Profile,
        color_profile: ColorProfile,
    ) -> Result<Self> {
        let backend = PlaybackBackend::start_xbox(
            stream,
            request_60fps,
            video_bitrate_kbps,
            video_h264_profile,
            color_profile,
        )?;
        let return_target = match kind {
            StreamKind::Cloud => StreamReturnTarget::Titles(return_selected),
            StreamKind::Home => StreamReturnTarget::Consoles(return_selected),
        };
        Ok(Self {
            paused: false,
            status: "Starting streaming backend".to_owned(),
            hint_started_at: Instant::now(),
            last_frame_at: Instant::now(),
            received_first_frame: false,
            stalled: false,
            stall_opened_overlay: false,
            pause_selected: 0,
            title_id,
            return_target,
            backend,
            latest_video_frame: None,
            current_video_frame: None,
            stream_video_size: None,
            pending_audio_packets: Vec::new(),
            ignore_confirm_until_release: false,
        })
    }

    pub(crate) fn set_paused(&mut self, paused: bool) {
        self.paused = paused;
        self.hint_started_at = Instant::now();
    }

    pub(crate) fn take_audio_packets(&mut self) -> Vec<Bytes> {
        std::mem::take(&mut self.pending_audio_packets)
    }

    pub(crate) fn video_frame(&self) -> Option<(u64, &DecodedFrame)> {
        Some((self.latest_video_frame?, self.current_video_frame.as_ref()?))
    }

    pub(crate) fn video_size(&self) -> Option<(u32, u32)> {
        self.stream_video_size
    }

    pub(crate) fn direct_video_output(&self) -> Arc<DirectVideoOutput> {
        self.backend.direct_video_output()
    }

    pub(crate) fn send_gamepad_frame(&mut self, mut frame: GamepadFrame, settings: &Settings) {
        if self.ignore_confirm_until_release {
            if frame.a > 0.0 {
                return;
            }
            self.ignore_confirm_until_release = false;
        }

        let per_title_swap = self
            .title_id
            .as_deref()
            .and_then(|title_id| settings.game_profile(title_id))
            .is_some_and(|profile| profile.swap_shoulders_and_triggers);
        let swap_shoulders_and_triggers = per_title_swap || settings.swap_shoulders_and_triggers;
        if swap_shoulders_and_triggers {
            std::mem::swap(&mut frame.left_shoulder, &mut frame.left_trigger);
            std::mem::swap(&mut frame.right_shoulder, &mut frame.right_trigger);
        }
        self.backend.send_gamepad_frame(frame);
    }

    pub(crate) fn front_touch_auxiliary_buttons(&self, settings: &Settings) -> bool {
        self.title_id
            .as_deref()
            .and_then(|title_id| settings.game_profile(title_id))
            .is_some_and(|profile| profile.front_touch_auxiliary_buttons)
    }

    pub(crate) fn rear_touch_enabled(&self, settings: &Settings) -> bool {
        self.title_id
            .as_deref()
            .and_then(|title_id| settings.game_profile(title_id))
            .is_none_or(|profile| profile.rear_touch_enabled)
    }

    pub(crate) fn press_guide_button(&mut self) {
        self.ignore_confirm_until_release = true;
        self.backend.send_gamepad_pulse(GamepadFrame {
            nexus: 1.0,
            ..Default::default()
        });
    }

    pub(crate) fn send_pointer_event(&self, event: PointerEvent) {
        self.backend.send_pointer_event(event);
    }

    pub(super) fn drain_backend_events(&mut self) -> (bool, Option<String>) {
        let mut events = Vec::new();
        while let Some(event) = self.backend.try_recv_event() {
            events.push(event);
        }
        while let Some(mut packets) = self.backend.try_recv_audio_packets() {
            self.pending_audio_packets.append(&mut packets);
        }

        if let Some((frame_id, frame)) = self.backend.take_latest_frame() {
            if self.latest_video_frame != Some(frame_id) {
                // Fresh decode: feed the stall detector.
                self.last_frame_at = Instant::now();
                self.received_first_frame = true;
            }
            self.latest_video_frame = Some(frame_id);
            self.current_video_frame = Some(frame);
        }

        let mut closed = false;
        let mut error = None;
        for event in events {
            match event {
                PlaybackBackendEvent::Status(status) => self.status = status,
                PlaybackBackendEvent::VideoResolution(width, height) => {
                    self.stream_video_size = Some((width, height));
                }
                PlaybackBackendEvent::Closed => closed = true,
                PlaybackBackendEvent::Error(message) => error = Some(message),
            }
        }

        (closed, error)
    }

    pub(super) async fn maintain_backend(&mut self) -> Option<String> {
        self.backend.maintain().await
    }

    /// Single decision point for the stall state machine (call once per pump
    /// tick, after `drain_backend_events`). No verdict before the first
    /// frame — the SDP/ICE handshake legitimately takes seconds.
    pub(super) fn poll_stall(&mut self) -> StallEvent {
        if !self.received_first_frame {
            return StallEvent::None;
        }
        let silent = frame_silence_is_stall(self.last_frame_at.elapsed());
        if self.stalled {
            if !silent {
                self.stalled = false;
                let close_overlay = self.stall_opened_overlay;
                self.stall_opened_overlay = false;
                if close_overlay && self.paused {
                    // We auto-opened the overlay for the stall; close it now
                    // that media flows again. If the user opened it manually
                    // in the meantime we leave it alone (flag was cleared).
                    self.set_paused(false);
                }
                return StallEvent::Recovered;
            }
        } else if silent {
            self.stalled = true;
            return StallEvent::Stalled;
        }
        StallEvent::None
    }

    /// Marks the pause overlay as stall-opened, so recovery can close it.
    pub(super) fn note_stall_opened_overlay(&mut self) {
        self.stall_opened_overlay = true;
    }

    pub(super) fn backend_description(&self) -> String {
        self.backend.description()
    }

    pub(super) async fn stop(self) -> Result<()> {
        self.backend.stop().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_silence_threshold_decides_stall() {
        assert!(!frame_silence_is_stall(Duration::ZERO));
        assert!(!frame_silence_is_stall(STREAM_STALL_THRESHOLD - Duration::from_millis(1)));
        assert!(frame_silence_is_stall(STREAM_STALL_THRESHOLD));
        // The user scenario is minutes of screensaver silence; the threshold
        // must sit well below it but above any healthy-scene frame gap
        // (floors are 72–122 kbps WITH frames).
        assert!(frame_silence_is_stall(Duration::from_secs(300)));
    }
}
