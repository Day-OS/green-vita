use super::StreamingSession;
use crate::app::{App, AppState, PollJob, poll_job};
use crate::i18n::{I18n, arg_string};
use crate::{ApiClient, MsalAuth, Stream, StreamKind, StreamState};
use anyhow::Result;
use fluent_bundle::FluentArgs;
use std::time::{Duration, Instant};

const WAIT_ESTIMATE_REFRESH: Duration = Duration::from_secs(15);
const MAX_CONSECUTIVE_POLL_FAILURES: u32 = 10;

pub(crate) fn describe_stream_state(
    i18n: &I18n,
    state: StreamState,
    wait_seconds: Option<u64>,
) -> String {
    match state {
        StreamState::New => i18n.text("stream-state-starting"),
        StreamState::Provisioning => i18n.text("stream-state-provisioning"),
        StreamState::WaitingForResources => match wait_seconds {
            Some(seconds) if seconds < 60 => {
                stream_wait_text(i18n, "stream-state-queued-seconds", seconds, seconds)
            }
            Some(seconds) => stream_wait_text(
                i18n,
                "stream-state-queued-minutes",
                seconds / 60,
                seconds % 60,
            ),
            None => i18n.text("stream-state-queued"),
        },
        StreamState::ReadyToConnect => i18n.text("stream-state-handshake"),
        StreamState::Provisioned => i18n.text("stream-state-ready"),
        StreamState::Error => i18n.text("stream-state-failed"),
    }
}

fn stream_wait_text(i18n: &I18n, key: &'static str, first: u64, seconds: u64) -> String {
    let mut args = FluentArgs::new();
    args.set("first", arg_string(first.to_string()));
    args.set("seconds", arg_string(seconds.to_string()));
    i18n.text_with(key, args)
}

pub(crate) struct ConnectingStream {
    pub(crate) stream: Stream,
    pub(crate) kind: StreamKind,
    pub(crate) target_id: String,
    pub(crate) game_id: Option<String>,
    pub(crate) label: String,
    pub(crate) next_poll_at: Instant,
    pub(crate) wait_estimate: Option<(u64, Instant)>,
    pub(crate) consecutive_failures: u32,
    pub(crate) return_selected: usize,
}

#[derive(Clone)]
pub(crate) struct StreamStartTarget {
    pub(crate) kind: StreamKind,
    pub(crate) target_id: String,
    pub(crate) game_id: Option<String>,
    pub(crate) label: String,
    pub(crate) return_selected: usize,
}

pub(in crate::app) async fn cleanup_active_sessions(api: &ApiClient, kind: StreamKind) {
    eprintln!("Checking for active {kind:?} sessions...");
    let paths = match api.get_active_session_paths(kind).await {
        Ok(paths) => paths,
        Err(error) => {
            if error.to_string().contains("404") {
                eprintln!("No active-sessions endpoint for {kind:?} (404) - skipping");
            } else {
                eprintln!("Failed to check for active {kind:?} sessions: {error:#}");
            }
            return;
        }
    };

    if paths.is_empty() {
        eprintln!("No stale {kind:?} sessions found");
        return;
    }
    eprintln!("Found {} stale {kind:?} session(s), stopping", paths.len());
    for path in paths {
        if let Err(error) = api.stop_session(kind, &path).await {
            eprintln!("Failed to stop stale session {path}: {error:#}");
        }
    }
}

impl App {
    pub(in crate::app) fn start_stream_for_target(&mut self, target: StreamStartTarget) {
        // Cap-advisor provenance for this session's CSV rows (constant per
        // session; 0 = no probe/none accepted). Overwritten every start.
        {
            use crate::streaming::video::metrics::METRICS;
            use std::sync::atomic::Ordering;
            if let Some(advice) = &self.cap_advice {
                if let Some(rtt) = advice.rtt_ms {
                    METRICS.probe_rtt_ms_x10.store((rtt * 10.0) as u64, Ordering::Relaxed);
                }
                if let Some(dl) = advice.downlink_mbps {
                    METRICS.probe_dl_mbps_x10.store((dl * 10.0) as u64, Ordering::Relaxed);
                }
                METRICS.probe_suggest_kbps.store(advice.suggested_kbps as u64, Ordering::Relaxed);
            }
        }
        // Damage policy (Step 6): settings → stream start → the RTP Damaged arm.
        crate::api::streaming::rtc::rtp::DAMAGE_POLICY.store(
            match self.settings.damage_policy {
                crate::settings::DamagePolicy::LowLatency => 0,
                crate::settings::DamagePolicy::Balanced => 1,
                crate::settings::DamagePolicy::Quality => 2,
            },
            std::sync::atomic::Ordering::Relaxed,
        );
        // Repair-window policy (perf plan stage 3b): settings → stream start → the assembler's
        // repair window preset. 0=Off, 1=Short, 2=Balanced, 3=Patient.
        crate::pipeline::media::assembler::REPAIR_WINDOW_POLICY.store(
            match self.settings.repair_window {
                crate::settings::RepairWindow::Off => 0,
                crate::settings::RepairWindow::Short => 1,
                crate::settings::RepairWindow::Balanced => 2,
                crate::settings::RepairWindow::Patient => 3,
            },
            std::sync::atomic::Ordering::Relaxed,
        );
        // Manual TWCC (2c): settings → stream start → the per-packet gate in
        // rtp::receive + media::send_twcc.
        // Hidden config (no settings row): manual 60 ms TWCC, default off.
        crate::api::streaming::rtc::rtp::set_manual_twcc(self.settings.manual_twcc_60ms);
        // Latency profile (item 15): settings → stream start → the decode
        // bank admission limit (Fast = shallow, Smooth = deep).
        crate::streaming::video::worker::LATENCY_PROFILE.store(
            match self.settings.latency_profile {
                crate::settings::LatencyProfile::Fast => 0,
                crate::settings::LatencyProfile::Smooth => 1,
            },
            std::sync::atomic::Ordering::Relaxed,
        );
        // 2b: direct present on/off (settings). Off = SDL path, HUD visible.
        crate::shell::direct_present::DIRECT_MODE
            .store(self.settings.direct_present, std::sync::atomic::Ordering::Relaxed);
        let api = self.service.api.clone();
        let kind = target.kind;
        let target_id = target.target_id.clone();
        let job = Some(tokio::spawn(async move {
            cleanup_active_sessions(&api, kind).await;
            api.start_stream(kind, &target_id).await
        }));
        self.set_state(AppState::StartingStream { target, job });
    }

    pub(in crate::app) fn start_selected_console_stream(&mut self) {
        let AppState::ConsoleList { selected } = &self.state else {
            return;
        };
        let selected = *selected;
        let Some(console) = self.service.consoles.get(selected).cloned() else {
            return;
        };

        let label = console.device_name;
        self.start_stream_for_target(StreamStartTarget {
            kind: StreamKind::Home,
            target_id: console.server_id,
            game_id: None,
            label,
            return_selected: selected,
        });
    }

    pub(in crate::app) async fn pump_connection(&mut self) -> Result<()> {
        let state = std::mem::replace(&mut self.state, AppState::ModeSelect { selected: 0 });
        let next_state = match state {
            AppState::StartingStream { target, job } => match job {
                Some(job) => match poll_job(job).await {
                    PollJob::Pending(job) => AppState::StartingStream {
                        target,
                        job: Some(job),
                    },
                    PollJob::Done(Ok(stream)) => AppState::Connecting {
                        session: ConnectingStream {
                            stream,
                            kind: target.kind,
                            target_id: target.target_id,
                            game_id: target.game_id,
                            label: target.label,
                            next_poll_at: Instant::now(),
                            wait_estimate: None,
                            consecutive_failures: 0,
                            return_selected: target.return_selected,
                        },
                        poll_job: None,
                        wait_estimate_job: None,
                    },
                    PollJob::Done(Err(error)) => {
                        eprintln!(
                            "Failed to start {:?} stream for {}: {error:#}",
                            target.kind, target.label
                        );
                        self.localized_error_state("error-start-stream", format!("{error:#}"))
                    }
                },
                None => AppState::StartingStream { target, job: None },
            },
            AppState::Connecting {
                mut session,
                poll_job: mut poll_task,
                mut wait_estimate_job,
            } => {
                if let Some(job) = poll_task.take() {
                    match poll_job(job).await {
                        PollJob::Pending(job) => AppState::Connecting {
                            session,
                            poll_job: Some(job),
                            wait_estimate_job,
                        },
                        PollJob::Done(result) => {
                            self.handle_connecting_poll_result(session, wait_estimate_job, result)
                                .await?
                        }
                    }
                } else {
                    self.pump_wait_estimate_job(&mut session, &mut wait_estimate_job)
                        .await;

                    if Instant::now() < session.next_poll_at {
                        AppState::Connecting {
                            session,
                            poll_job: poll_task,
                            wait_estimate_job,
                        }
} else {
                        session.next_poll_at = Instant::now() + Duration::from_millis(500);

                        let mut stream = session.stream.clone();
                        let mut auth = self.service.auth.clone();
                        poll_task = Some(tokio::spawn(async move {
                            let state = stream.poll_provisioning(&mut auth).await?;
                            Ok((stream, state))
                        }));
                        AppState::Connecting {
                            session,
                            poll_job: poll_task,
                            wait_estimate_job,
                        }
                    }
                }
            }
            state => state,
        };
        if matches!(&next_state, AppState::Streaming(_) | AppState::Error { .. }) {
            self.set_state(next_state);
        } else {
            self.state = next_state;
        }

        Ok(())
    }

    async fn handle_connecting_poll_result(
        &mut self,
        mut session: ConnectingStream,
        wait_estimate_job: Option<tokio::task::JoinHandle<Result<crate::WaitTimeResponse>>>,
        result: Result<(Stream, StreamState)>,
    ) -> Result<AppState> {
        match result {
            Ok((stream, StreamState::Provisioned)) => {
                session.stream = stream;
                if let Some(ip) = self
                    .settings
                    .home_console_ip
                    .as_deref()
                    .and_then(|value| value.trim().parse::<std::net::Ipv4Addr>().ok())
                {
                    session.stream.set_console_lan_ip(ip);
                }
                self.service.auth = MsalAuth::new();
                let title_id = session.game_id.clone();
                match StreamingSession::start_xbox(
                    session.stream.clone(),
                    session.kind,
                    title_id,
                    session.return_selected,
                    true, // always request 60: the console encodes ~60 regardless of a=framerate (54-session src_fps survey)
                    self.settings.video_bitrate_kbps,
                    self.settings.video_h264_profile,
                    self.settings.color_profile,
                ) {
                    Ok(streaming) => Ok(AppState::Streaming(streaming)),
                    Err(error) => {
                        let _ = session.stream.stop().await;
                        eprintln!("Failed to start WebRTC session: {error:#}");
                        Ok(self.localized_error_state(
                            "error-webrtc-negotiation",
                            format!("{error:#}"),
                        ))
                    }
                }
            }
            Ok((stream, _)) => {
                session.stream = stream;
                self.service.auth = MsalAuth::new();
                session.consecutive_failures = 0;
                Ok(AppState::Connecting {
                    session,
                    poll_job: None,
                    wait_estimate_job,
                })
            }
            Err(error) => {
                let message = error.to_string();
                session.consecutive_failures += 1;
                eprintln!(
                    "Stream state check failed (attempt {}): {message:#}",
                    session.consecutive_failures
                );

                let session_gone = message.contains("404");
                let too_many_failures =
                    session.consecutive_failures >= MAX_CONSECUTIVE_POLL_FAILURES;
                if session_gone || too_many_failures {
                    Ok(self.localized_error_state("error-stream-state", format!("{error:#}")))
                } else {
                    Ok(AppState::Connecting {
                        session,
                        poll_job: None,
                        wait_estimate_job,
                    })
                }
            }
        }
    }

    async fn pump_wait_estimate_job(
        &mut self,
        session: &mut ConnectingStream,
        wait_estimate_job: &mut Option<tokio::task::JoinHandle<Result<crate::WaitTimeResponse>>>,
    ) {
        if let Some(job) = wait_estimate_job.take() {
            match poll_job(job).await {
                PollJob::Pending(job) => *wait_estimate_job = Some(job),
                PollJob::Done(result) => match result {
                    Ok(response) => {
                        session.wait_estimate = Some((
                            response.estimated_total_wait_time_in_seconds,
                            Instant::now(),
                        ));
                    }
                    Err(error) => {
                        eprintln!("Failed to fetch xCloud wait estimate: {error:#}");
                    }
                },
            }
            return;
        }

        let should_refresh = session.stream.state == StreamState::WaitingForResources
            && session
                .wait_estimate
                .is_none_or(|(_, fetched_at)| fetched_at.elapsed() >= WAIT_ESTIMATE_REFRESH);
        if !should_refresh {
            return;
        }

        let api = self.service.api.clone();
        let kind = session.kind;
        let target_id = session.target_id.clone();
        *wait_estimate_job = Some(tokio::spawn(async move {
            api.get_wait_time(kind, &target_id).await
        }));
    }
}
