use crate::Stream;
use crate::streaming::control::input::{GamepadFrame, PointerEvent};
use crate::streaming::rtc::peer::RtcPeer;
use crate::streaming::rtc::session::{HW_OUTPUT_HEIGHT, HW_OUTPUT_WIDTH, RtcSession};
use crate::streaming::video::{DecodedFrame, DirectVideoOutput};
use anyhow::{Context, Result};
use bytes::Bytes;
use rtc::peer_connection::state::RTCPeerConnectionState;
use rtc::peer_connection::transport::RTCIceCandidateInit;
use std::collections::VecDeque;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::mpsc::{Receiver, SyncSender, TryRecvError, TrySendError, sync_channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const PUMP_SLEEP: Duration = Duration::from_millis(1);
const PUMP_ERROR_SLEEP: Duration = Duration::from_millis(100);
const GAMEPAD_PULSE_DURATION: Duration = Duration::from_millis(100);
const MAX_PENDING_COMMANDS: usize = 16;
const MAX_PENDING_EVENTS: usize = 32;
const MAX_PENDING_AUDIO_BATCHES: usize = 16;
const SDP_NEGOTIATION_TIMEOUT: Duration = Duration::from_secs(45);

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

pub struct RtcWorker {
    commands_tx: SyncSender<RtcWorkerCommand>,
    events_rx: Receiver<RtcWorkerEvent>,
    audio_rx: Receiver<Vec<Bytes>>,
    latest_frame: Arc<Mutex<Option<(u64, DecodedFrame)>>>,
    latest_gamepad: Arc<Mutex<Option<GamepadFrame>>>,
    gamepad_pulses: Arc<Mutex<VecDeque<GamepadFrame>>>,
    direct_video_output: Arc<DirectVideoOutput>,
}

impl RtcWorker {
    pub fn spawn(stream: Stream) -> Result<Self> {
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
                        stream,
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

    pub fn try_recv(&self) -> Option<RtcWorkerEvent> {
        self.events_rx.try_recv().ok()
    }

    pub fn try_recv_audio_packets(&self) -> Option<Vec<Bytes>> {
        self.audio_rx.try_recv().ok()
    }

    pub fn take_latest_frame(&self) -> Option<(u64, DecodedFrame)> {
        self.latest_frame.lock().ok()?.take()
    }

    pub fn direct_video_output(&self) -> Arc<DirectVideoOutput> {
        Arc::clone(&self.direct_video_output)
    }

    pub fn add_remote_candidate(&self, candidate: RTCIceCandidateInit) {
        send_lossy(
            &self.commands_tx,
            RtcWorkerCommand::AddRemoteCandidate(candidate),
        );
    }

    pub fn send_gamepad_frame(&self, frame: GamepadFrame) {
        if let Ok(mut latest) = self.latest_gamepad.lock() {
            *latest = Some(frame);
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

fn run_worker_thread(
    stream: Stream,
    commands_rx: Receiver<RtcWorkerCommand>,
    events_tx: SyncSender<RtcWorkerEvent>,
    audio_tx: SyncSender<Vec<Bytes>>,
    latest_frame: Arc<Mutex<Option<(u64, DecodedFrame)>>>,
    latest_gamepad: Arc<Mutex<Option<GamepadFrame>>>,
    gamepad_pulses: Arc<Mutex<VecDeque<GamepadFrame>>>,
    direct_video_output: Arc<DirectVideoOutput>,
) -> Result<()> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("failed to build RTC worker runtime")?;

    runtime.block_on(async move {
        let peer = RtcPeer::new()?;
        let mut session = RtcSession::new(peer, direct_video_output).await?;

        let offer = session.peer.create_offer()?;
        let answer_sdp =
            tokio::time::timeout(SDP_NEGOTIATION_TIMEOUT, stream.send_sdp_offer(&offer.sdp))
                .await
                .context("timed out waiting for xCloud SDP answer")??;
        session.peer.set_remote_answer(answer_sdp)?;

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

async fn run_session(
    mut session: RtcSession,
    commands_rx: Receiver<RtcWorkerCommand>,
    events_tx: SyncSender<RtcWorkerEvent>,
    audio_tx: SyncSender<Vec<Bytes>>,
    latest_frame: Arc<Mutex<Option<(u64, DecodedFrame)>>>,
    latest_gamepad: Arc<Mutex<Option<GamepadFrame>>>,
    gamepad_pulses: Arc<Mutex<VecDeque<GamepadFrame>>>,
) -> Result<()> {
    let mut last_frame_sent = None;
    let mut last_status = session.status.clone();
    let mut last_connection_state = session.connection_state;
    let mut last_server_video_size = session.server_video_size;
    let mut consecutive_pump_errors = 0u32;
    let mut active_gamepad_pulse: Option<(GamepadFrame, Instant)> = None;

    loop {
        if !drain_commands(&mut session, &commands_rx) {
            let _ = session.peer.close();
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
                session.send_gamepad_frame(latest.unwrap_or_default());
            } else if pulse_started || latest.is_some() {
                let mut frame = latest.unwrap_or_default();
                // Guide/Nexus is currently the only pulsed input. Keep it asserted while normal
                // gamepad frames continue to flow instead of immediately overwriting the press.
                frame.nexus = frame.nexus.max(pulse.nexus);
                session.send_gamepad_frame(frame);
            }
        } else if let Some(frame) = latest {
            session.send_gamepad_frame(frame);
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

        let audio_packets = session.drain_audio_packets();
        if !audio_packets.is_empty() {
            send_lossy(&audio_tx, audio_packets);
        }

        if let Some(frame) = session.take_new_video_frame(&mut last_frame_sent)
            && let Ok(mut latest) = latest_frame.lock()
        {
            *latest = Some(frame);
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

        if session.server_video_size != last_server_video_size
            && let Some((width, height)) = session.server_video_size
        {
            last_server_video_size = session.server_video_size;
            send_lossy(&events_tx, RtcWorkerEvent::VideoResolution(width, height));
        }

        if session.connection_state == RTCPeerConnectionState::Closed {
            send_important_event(&events_tx, RtcWorkerEvent::Closed);
            return Ok(());
        }

        tokio::time::sleep(PUMP_SLEEP).await;
    }
}

fn drain_commands(session: &mut RtcSession, commands_rx: &Receiver<RtcWorkerCommand>) -> bool {
    loop {
        match commands_rx.try_recv() {
            Ok(RtcWorkerCommand::AddRemoteCandidate(candidate)) => {
                if let Err(error) = session.add_remote_candidate(candidate) {
                    eprintln!("Failed to add remote ICE candidate: {error:#}");
                }
            }
            Ok(RtcWorkerCommand::SendPointerEvent(event)) => {
                session.send_pointer_event(event);
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
