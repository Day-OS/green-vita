use crate::api::streaming::rtc::session::{RtcSession, RtcSessionConfig};
use crate::streaming::input::{GamepadFrame, PointerEvent};
use crate::streaming::input_metrics;
use crate::streaming::video::{DecodedFrame, DirectVideoOutput, HW_OUTPUT_HEIGHT, HW_OUTPUT_WIDTH};
use anyhow::{Context, Result};
use bytes::Bytes;
use rtc::peer_connection::RTCPeerConnection;
use rtc::peer_connection::sdp::RTCSessionDescription;
use rtc::peer_connection::state::RTCPeerConnectionState;
use rtc::peer_connection::transport::RTCIceCandidateInit;
use std::collections::VecDeque;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::Ordering;
use std::sync::mpsc::{Receiver, SyncSender, TryRecvError, TrySendError, sync_channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const PUMP_SLEEP: Duration = Duration::from_millis(1);
const PUMP_ERROR_SLEEP: Duration = Duration::from_millis(100);
const GAMEPAD_PULSE_DURATION: Duration = Duration::from_millis(100);
const GAMEPAD_REFRESH_INTERVAL: Duration = Duration::from_millis(50);
const MAX_PENDING_COMMANDS: usize = 16;
const MAX_PENDING_EVENTS: usize = 32;
const MAX_PENDING_AUDIO_BATCHES: usize = 16;
const SDP_NEGOTIATION_TIMEOUT: Duration = Duration::from_secs(45);

pub(crate) trait RtcWorkerProvider: Send + 'static {
    type Protocol: super::session::RtcSessionBackend;

    fn create_peer(&self) -> Result<(RTCPeerConnection, Self::Protocol)>;
    fn session_config(&self) -> RtcSessionConfig;
    async fn exchange_sdp(&self, offer: &RTCSessionDescription) -> Result<String>;
}

pub enum RtcWorkerEvent {
    LocalCandidates(Vec<RTCIceCandidateInit>),
    Status { status: String },
    VideoResolution(u32, u32),
    Closed,
    Error(String),
}

enum RtcWorkerCommand {
    AddRemoteCandidate(RTCIceCandidateInit),
    SendPointerEvent(PointerEvent),
    Stop,
}

struct SampledGamepadFrame {
    frame: GamepadFrame,
    sampled_at: Instant,
}

pub struct RtcWorker {
    commands_tx: SyncSender<RtcWorkerCommand>,
    pub(crate) events_rx: Receiver<RtcWorkerEvent>,
    pub(crate) audio_rx: Receiver<Vec<Bytes>>,
    pub(crate) latest_frame: Arc<Mutex<Option<(u64, DecodedFrame)>>>,
    latest_gamepad: Arc<Mutex<Option<SampledGamepadFrame>>>,
    gamepad_pulses: Arc<Mutex<VecDeque<GamepadFrame>>>,
    pub(crate) direct_video_output: Arc<DirectVideoOutput>,
}

impl RtcWorker {
    pub(crate) fn spawn<P: RtcWorkerProvider>(provider: P) -> Result<Self> {
        let (commands_tx, commands_rx) = sync_channel(MAX_PENDING_COMMANDS);
        let (events_tx, events_rx) = sync_channel(MAX_PENDING_EVENTS);
        let (audio_tx, audio_rx) = sync_channel(MAX_PENDING_AUDIO_BATCHES);
        let latest_frame = Arc::new(Mutex::new(None));
        let worker_latest_frame = Arc::clone(&latest_frame);
        let latest_gamepad = Arc::new(Mutex::new(None));
        let worker_latest_gamepad = Arc::clone(&latest_gamepad);
        let gamepad_pulses = Arc::new(Mutex::new(VecDeque::new()));
        let worker_gamepad_pulses = Arc::clone(&gamepad_pulses);
        let direct_video_output =
            Arc::new(DirectVideoOutput::new(HW_OUTPUT_WIDTH, HW_OUTPUT_HEIGHT));
        let worker_direct_video_output = Arc::clone(&direct_video_output);

        std::thread::Builder::new()
            .name("green-vita-rtc".to_owned())
            .spawn(move || {
                match catch_unwind(AssertUnwindSafe(|| {
                    run_worker_thread(
                        provider,
                        commands_rx,
                        events_tx.clone(),
                        audio_tx,
                        worker_latest_frame,
                        worker_latest_gamepad,
                        worker_gamepad_pulses,
                        worker_direct_video_output,
                    )
                })) {
                    Ok(Ok(())) => {}
                    Ok(Err(error)) => {
                        send_important_event(
                            &events_tx,
                            RtcWorkerEvent::Error(format!("{error:#}")),
                        );
                    }
                    Err(_) => {
                        send_important_event(
                            &events_tx,
                            RtcWorkerEvent::Error("RTC worker thread panicked".to_owned()),
                        );
                    }
                }
            })
            .context("failed to spawn RTC worker thread")?;

        Ok(Self {
            commands_tx,
            events_rx,
            audio_rx,
            latest_frame,
            latest_gamepad,
            gamepad_pulses,
            direct_video_output,
        })
    }

    pub fn add_remote_candidate(&self, candidate: RTCIceCandidateInit) {
        send_lossy(
            &self.commands_tx,
            RtcWorkerCommand::AddRemoteCandidate(candidate),
        );
    }

    pub fn send_gamepad_frame(&self, frame: GamepadFrame) {
        if let Ok(mut latest) = self.latest_gamepad.lock() {
            *latest = Some(SampledGamepadFrame {
                frame,
                sampled_at: Instant::now(),
            });
        }
    }

    pub fn send_gamepad_pulse(&self, frame: GamepadFrame) {
        if let Ok(mut pulses) = self.gamepad_pulses.lock() {
            pulses.push_back(frame);
        }
    }

    pub fn send_pointer_event(&self, event: PointerEvent) {
        send_lossy(&self.commands_tx, RtcWorkerCommand::SendPointerEvent(event));
    }
}

impl Drop for RtcWorker {
    fn drop(&mut self) {
        send_lossy(&self.commands_tx, RtcWorkerCommand::Stop);
    }
}

fn run_worker_thread<P: RtcWorkerProvider>(
    provider: P,
    commands_rx: Receiver<RtcWorkerCommand>,
    events_tx: SyncSender<RtcWorkerEvent>,
    audio_tx: SyncSender<Vec<Bytes>>,
    latest_frame: Arc<Mutex<Option<(u64, DecodedFrame)>>>,
    latest_gamepad: Arc<Mutex<Option<SampledGamepadFrame>>>,
    gamepad_pulses: Arc<Mutex<VecDeque<GamepadFrame>>>,
    direct_video_output: Arc<DirectVideoOutput>,
) -> Result<()> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("failed to build RTC worker runtime")?;

    runtime.block_on(async move {
        let (peer, backend) = provider.create_peer()?;
        let config = provider.session_config();
        let mut session = RtcSession::new(peer, backend, config, direct_video_output).await?;

        let offer = session.create_offer()?;
        let answer_sdp =
            tokio::time::timeout(SDP_NEGOTIATION_TIMEOUT, provider.exchange_sdp(&offer))
                .await
                .context("timed out waiting for RTC SDP answer")??;
        session.set_remote_answer(answer_sdp)?;
        #[cfg(target_os = "vita")]
        prioritize_rtc_thread();

        send_lossy(
            &events_tx,
            RtcWorkerEvent::Status {
                status: session.status.clone(),
            },
        );

        run_session(
            session,
            commands_rx,
            events_tx,
            audio_tx,
            latest_frame,
            latest_gamepad,
            gamepad_pulses,
        )
        .await
    })
}

#[cfg(target_os = "vita")]
fn prioritize_rtc_thread() {
    let thread_id = unsafe { vitasdk_sys::sceKernelGetThreadId() };
    let affinity_result = unsafe {
        vitasdk_sys::sceKernelChangeThreadCpuAffinityMask(
            thread_id,
            vitasdk_sys::SCE_KERNEL_CPU_MASK_USER_1 as i32,
        )
    };
    if affinity_result < 0 {
        eprintln!("Failed to pin RTC thread to user CPU 1: {affinity_result:#x}");
    }

    let result = unsafe {
        sdl2::sys::SDL_SetThreadPriority(
            sdl2::sys::SDL_ThreadPriority::SDL_THREAD_PRIORITY_TIME_CRITICAL,
        )
    };
    if result < 0 {
        eprintln!("Failed to maximize RTC thread priority: {result}");
    }
}

async fn run_session<B: super::session::RtcSessionBackend>(
    mut session: RtcSession<B>,
    commands_rx: Receiver<RtcWorkerCommand>,
    events_tx: SyncSender<RtcWorkerEvent>,
    audio_tx: SyncSender<Vec<Bytes>>,
    latest_frame: Arc<Mutex<Option<(u64, DecodedFrame)>>>,
    latest_gamepad: Arc<Mutex<Option<SampledGamepadFrame>>>,
    gamepad_pulses: Arc<Mutex<VecDeque<GamepadFrame>>>,
) -> Result<()> {
    let mut last_status = session.status.clone();
    let mut last_connection_state = session.connection_state;
    let mut last_server_video_size = session.backend.server_video_size();
    let mut consecutive_pump_errors = 0u32;
    let mut active_gamepad_pulse: Option<(GamepadFrame, Instant)> = None;
    let mut last_gamepad_sent: Option<(GamepadFrame, Instant)> = None;

    loop {
        if !drain_commands(&mut session, &commands_rx) {
            let _ = session.close();
            return Ok(());
        }
        let pulses = gamepad_pulses
            .lock()
            .map(|mut pulses| pulses.drain(..).collect::<Vec<_>>())
            .unwrap_or_default();
        let pulse_started = !pulses.is_empty();
        for frame in pulses {
            active_gamepad_pulse = Some((frame, Instant::now() + GAMEPAD_PULSE_DURATION));
        }

        let latest = latest_gamepad
            .lock()
            .ok()
            .and_then(|mut latest| latest.take());
        if let Some((pulse, release_at)) = &active_gamepad_pulse {
            if Instant::now() >= *release_at {
                active_gamepad_pulse = None;
                // A pulse must have a distinct release packet even when normal controller input
                // is temporarily suppressed (for example, until Confirm is released).
                let sampled = latest.unwrap_or_else(|| SampledGamepadFrame {
                    frame: GamepadFrame::default(),
                    sampled_at: Instant::now(),
                });
                send_sampled_gamepad_frame(&mut session, sampled, &mut last_gamepad_sent, true);
            } else if pulse_started || latest.is_some() {
                let mut sampled = latest.unwrap_or_else(|| SampledGamepadFrame {
                    frame: GamepadFrame::default(),
                    sampled_at: Instant::now(),
                });
                // Guide/Nexus is currently the only pulsed input. Keep it asserted while normal
                // gamepad frames continue to flow instead of immediately overwriting the press.
                sampled.frame.nexus = sampled.frame.nexus.max(pulse.nexus);
                send_sampled_gamepad_frame(
                    &mut session,
                    sampled,
                    &mut last_gamepad_sent,
                    pulse_started,
                );
            }
        } else if let Some(sampled) = latest {
            send_sampled_gamepad_frame(&mut session, sampled, &mut last_gamepad_sent, false);
        }

        let local_candidates = match session.pump().await {
            Ok(candidates) => {
                if consecutive_pump_errors > 0 {
                    consecutive_pump_errors = 0;
                    session.status = "WebRTC pump recovered".to_owned();
                }
                candidates
            }
            Err(error) => {
                consecutive_pump_errors = consecutive_pump_errors.saturating_add(1);
                session.status =
                    format!("WebRTC pump error; retrying ({consecutive_pump_errors}): {error:#}");
                eprintln!("{}", session.status);
                send_lossy(
                    &events_tx,
                    RtcWorkerEvent::Status {
                        status: session.status.clone(),
                    },
                );
                tokio::time::sleep(PUMP_ERROR_SLEEP).await;
                continue;
            }
        };
        if !local_candidates.is_empty() {
            send_lossy(
                &events_tx,
                RtcWorkerEvent::LocalCandidates(local_candidates),
            );
        }

        let audio_packets = std::mem::take(&mut session.audio.packets);
        if !audio_packets.is_empty() {
            send_lossy(&audio_tx, audio_packets);
        }

        if let Some(frame) = session.video.latest_frame.take()
            && let Ok(mut latest) = latest_frame.lock()
        {
            if latest.replace(frame).is_some() {
                crate::streaming::video::metrics::METRICS
                    .handoff_replaced
                    .increment();
            }
        }

        if session.status != last_status || session.connection_state != last_connection_state {
            last_status = session.status.clone();
            last_connection_state = session.connection_state;
            send_lossy(
                &events_tx,
                RtcWorkerEvent::Status {
                    status: last_status.clone(),
                },
            );
        }

        let server_video_size = session.backend.server_video_size();
        if server_video_size != last_server_video_size
            && let Some((width, height)) = server_video_size
        {
            last_server_video_size = server_video_size;
            send_lossy(&events_tx, RtcWorkerEvent::VideoResolution(width, height));
        }

        if session.connection_state == RTCPeerConnectionState::Closed {
            send_important_event(&events_tx, RtcWorkerEvent::Closed);
            return Ok(());
        }

        tokio::select! {
            readable = session.transport.socket.readable() => {
                readable.context("failed waiting for WebRTC UDP socket")?;
            }
            _ = session.video.decoder.result_ready.notified() => {}
            _ = tokio::time::sleep(PUMP_SLEEP) => {}
        }
    }
}

fn send_sampled_gamepad_frame<B: super::session::RtcSessionBackend>(
    session: &mut RtcSession<B>,
    sampled: SampledGamepadFrame,
    last_sent: &mut Option<(GamepadFrame, Instant)>,
    force: bool,
) {
    let now = Instant::now();
    let unchanged_and_fresh = last_sent.as_ref().is_some_and(|(previous, sent_at)| {
        *previous == sampled.frame && now.duration_since(*sent_at) < GAMEPAD_REFRESH_INTERVAL
    });
    if !force && unchanged_and_fresh {
        return;
    }

    if session
        .backend
        .send_gamepad_frame(&mut session.peer, sampled.frame.clone())
    {
        *last_sent = Some((sampled.frame, now));
        // Capture how old the newest controller sample was when it reached the RTC channel.
        input_metrics::GAMEPAD_SEND_AGE_US.store(
            sampled
                .sampled_at
                .elapsed()
                .as_micros()
                .min(u64::MAX as u128) as u64,
            Ordering::Relaxed,
        );
    }
}

fn drain_commands<B: super::session::RtcSessionBackend>(
    session: &mut RtcSession<B>,
    commands_rx: &Receiver<RtcWorkerCommand>,
) -> bool {
    loop {
        match commands_rx.try_recv() {
            Ok(RtcWorkerCommand::AddRemoteCandidate(candidate)) => {
                if let Err(error) = session.add_remote_candidate(candidate) {
                    eprintln!("Failed to add remote ICE candidate: {error:#}");
                }
            }
            Ok(RtcWorkerCommand::SendPointerEvent(event)) => {
                session.backend.send_pointer_event(&mut session.peer, event);
            }
            Ok(RtcWorkerCommand::Stop) | Err(TryRecvError::Disconnected) => return false,
            Err(TryRecvError::Empty) => return true,
        }
    }
}

/// Sends without blocking - a full or disconnected channel just drops the value, since every
/// event/command here is either superseded by the next tick or, for `Stop`, best-effort anyway.
fn send_lossy<T>(sender: &SyncSender<T>, value: T) {
    match sender.try_send(value) {
        Ok(()) | Err(TrySendError::Full(_)) | Err(TrySendError::Disconnected(_)) => {}
    }
}

fn send_important_event(sender: &SyncSender<RtcWorkerEvent>, event: RtcWorkerEvent) {
    let _ = sender.send(event);
}
