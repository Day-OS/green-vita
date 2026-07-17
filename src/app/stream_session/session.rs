use crate::app::{PollJob, poll_job};
use crate::settings::Settings;
use crate::streaming::rtc::worker::{RtcWorker, RtcWorkerEvent};
use crate::streaming::video::DecodedFrame;
use crate::{GamepadFrame, PointerEvent, Stream, StreamKind};
use anyhow::Result;
use bytes::Bytes;
use rtc::peer_connection::transport::RTCIceCandidateInit;
use std::collections::HashSet;
use std::time::{Duration, Instant};
use tokio::task::JoinHandle;

const STREAM_KEEPALIVE_INTERVAL: Duration = Duration::from_secs(30);

pub(crate) struct StreamingSession {
    pub(crate) paused: bool,
    pub(crate) status: String,
    pub(crate) hint_started_at: Instant,
    pub(in crate::app) pause_selected: usize,
    pub(super) kind: StreamKind,
    pub(super) stream: Stream,
    pub(in crate::app) title_id: Option<String>,
    pub(super) return_selected: usize,
    rtc_worker: RtcWorker,
    latest_video_frame: Option<u64>,
    current_video_frame: Option<DecodedFrame>,
    stream_video_size: Option<(u32, u32)>,
    pending_audio_packets: Vec<Bytes>,
    rtc_ice_next_poll_at: Instant,
    remote_ice_candidates: HashSet<String>,
    pending_local_ice_candidates: Vec<RTCIceCandidateInit>,
    rtc_ice_post_job: Option<JoinHandle<Result<()>>>,
    rtc_ice_poll_job: Option<JoinHandle<Result<Option<Vec<RTCIceCandidateInit>>>>>,
    keepalive_next_at: Instant,
    keepalive_job: Option<JoinHandle<Result<serde_json::Value>>>,
    ignore_confirm_until_release: bool,
}

impl StreamingSession {
    pub(super) fn start(
        stream: Stream,
        kind: StreamKind,
        title_id: Option<String>,
        return_selected: usize,
    ) -> Result<Self> {
        let rtc_worker = RtcWorker::spawn(stream.clone())?;
        Ok(Self {
            paused: false,
            status: "Starting WebRTC worker".to_owned(),
            hint_started_at: Instant::now(),
            pause_selected: 0,
            kind,
            stream,
            title_id,
            return_selected,
            rtc_worker,
            latest_video_frame: None,
            current_video_frame: None,
            stream_video_size: None,
            pending_audio_packets: Vec::new(),
            rtc_ice_next_poll_at: Instant::now(),
            remote_ice_candidates: HashSet::new(),
            pending_local_ice_candidates: Vec::new(),
            rtc_ice_post_job: None,
            rtc_ice_poll_job: None,
            keepalive_next_at: Instant::now(),
            keepalive_job: None,
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

    pub(crate) fn send_gamepad_frame(&mut self, mut frame: GamepadFrame, settings: &Settings) {
        if self.ignore_confirm_until_release {
            if frame.a > 0.0 {
                return;
            }
            self.ignore_confirm_until_release = false;
        }

        let swap_shoulders_and_triggers = self
            .title_id
            .as_deref()
            .and_then(|title_id| settings.game_profile(title_id))
            .is_some_and(|profile| profile.swap_shoulders_and_triggers);
        if swap_shoulders_and_triggers {
            std::mem::swap(&mut frame.left_shoulder, &mut frame.left_trigger);
            std::mem::swap(&mut frame.right_shoulder, &mut frame.right_trigger);
        }
        self.rtc_worker.send_gamepad_frame(frame);
    }

    pub(crate) fn front_touch_auxiliary_buttons(&self, settings: &Settings) -> bool {
        self.title_id
            .as_deref()
            .and_then(|title_id| settings.game_profile(title_id))
            .is_some_and(|profile| profile.front_touch_auxiliary_buttons)
    }

    pub(crate) fn press_guide_button(&mut self) {
        self.ignore_confirm_until_release = true;
        self.rtc_worker.send_gamepad_pulse(GamepadFrame {
            nexus: 1.0,
            ..Default::default()
        });
    }

    pub(crate) fn send_pointer_event(&self, event: PointerEvent) {
        self.rtc_worker.send_pointer_event(event);
    }

    pub(super) fn drain_worker_events(&mut self) -> (bool, Option<String>) {
        let mut events = Vec::new();
        while let Some(event) = self.rtc_worker.try_recv() {
            events.push(event);
        }
        while let Some(mut packets) = self.rtc_worker.try_recv_audio_packets() {
            self.pending_audio_packets.append(&mut packets);
        }

        if let Some((frame_id, frame)) = self.rtc_worker.take_latest_frame() {
            self.latest_video_frame = Some(frame_id);
            self.current_video_frame = Some(frame);
        }

        let mut closed = false;
        let mut error = None;
        for event in events {
            match event {
                RtcWorkerEvent::LocalCandidates(candidates) => {
                    self.pending_local_ice_candidates.extend(candidates);
                }
                RtcWorkerEvent::Status {
                    status: new_status, ..
                } => {
                    self.status = new_status;
                }
                RtcWorkerEvent::VideoResolution(width, height) => {
                    self.stream_video_size = Some((width, height));
                }
                RtcWorkerEvent::Closed => closed = true,
                RtcWorkerEvent::Error(message) => error = Some(message),
            }
        }

        (closed, error)
    }

    pub(super) async fn post_local_ice(&mut self) {
        if let Some(job) = self.rtc_ice_post_job.take() {
            match poll_job(job).await {
                PollJob::Pending(job) => {
                    self.rtc_ice_post_job = Some(job);
                    return;
                }
                PollJob::Done(Ok(())) => {}
                PollJob::Done(Err(error)) => {
                    eprintln!("Failed to post local ICE candidates: {error:#}");
                }
            }
        }

        if self.pending_local_ice_candidates.is_empty() {
            return;
        }

        let stream = self.stream.clone();
        let candidates = std::mem::take(&mut self.pending_local_ice_candidates);
        let count = candidates.len();
        self.rtc_ice_post_job = Some(tokio::spawn(async move {
            stream.post_ice_candidates(candidates).await?;
            eprintln!("Posted {count} local ICE candidate(s) to xCloud");
            Ok(())
        }));
    }

    pub(super) async fn poll_remote_ice(&mut self) {
        if let Some(job) = self.rtc_ice_poll_job.take() {
            match poll_job(job).await {
                PollJob::Pending(job) => self.rtc_ice_poll_job = Some(job),
                PollJob::Done(result) => {
                    let response = result.unwrap_or_else(|error| {
                        eprintln!("Failed to poll remote ICE candidates: {error:#}");
                        None
                    });
                    for candidate in response.into_iter().flatten() {
                        let key = format!(
                            "{}|{}|{}",
                            candidate.candidate,
                            candidate.sdp_mid.as_deref().unwrap_or(""),
                            candidate.sdp_mline_index.unwrap_or(0)
                        );
                        if self.remote_ice_candidates.insert(key) {
                            self.rtc_worker.add_remote_candidate(candidate);
                        }
                    }
                }
            }
        }

        if Instant::now() < self.rtc_ice_next_poll_at || self.rtc_ice_poll_job.is_some() {
            return;
        }

        let stream = self.stream.clone();
        self.rtc_ice_next_poll_at = Instant::now() + Duration::from_secs(1);
        self.rtc_ice_poll_job = Some(tokio::spawn(
            async move { stream.poll_ice_candidates().await },
        ));
    }

    pub(super) async fn keep_alive(&mut self) -> Option<String> {
        if let Some(job) = self.keepalive_job.take() {
            match poll_job(job).await {
                PollJob::Pending(job) => {
                    self.keepalive_job = Some(job);
                    return None;
                }
                PollJob::Done(Ok(response)) => {
                    if let Some(code) = response.get("code").and_then(serde_json::Value::as_str)
                        && matches!(code, "SessionNotActive" | "SessionNotFound")
                    {
                        return Some(code.to_owned());
                    }
                }
                PollJob::Done(Err(error)) => {
                    eprintln!("Failed to send xCloud keepalive: {error:#}");
                }
            }
        }

        if Instant::now() < self.keepalive_next_at {
            return None;
        }
        self.keepalive_next_at = Instant::now() + STREAM_KEEPALIVE_INTERVAL;

        let stream = self.stream.clone();
        self.keepalive_job = Some(tokio::spawn(async move { stream.send_keepalive().await }));
        None
    }
}
