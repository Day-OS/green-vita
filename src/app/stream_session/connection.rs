use super::StreamingSession;
use crate::app::{App, AppState, PollJob, poll_job};
use crate::{ApiClient, MsalAuth, Stream, StreamKind, StreamState};
use anyhow::Result;
use std::time::{Duration, Instant};

const WAIT_ESTIMATE_REFRESH: Duration = Duration::from_secs(15);
const MAX_CONSECUTIVE_POLL_FAILURES: u32 = 10;

pub(crate) fn describe_stream_state(state: StreamState, wait_seconds: Option<u64>) -> String {
    match state {
        StreamState::New => "Starting".to_owned(),
        StreamState::Provisioning => "Provisioning your session".to_owned(),
        StreamState::WaitingForResources => match wait_seconds {
            Some(seconds) if seconds < 60 => format!("Queued - estimated wait {seconds}s"),
            Some(seconds) => format!(
                "Queued - estimated wait {}m {}s",
                seconds / 60,
                seconds % 60
            ),
            None => "Queued - waiting for a free server".to_owned(),
        },
        StreamState::ReadyToConnect => "Finishing sign-in handshake".to_owned(),
        StreamState::Provisioned => "Ready".to_owned(),
        StreamState::Error => "Failed".to_owned(),
    }
}

pub(crate) struct ConnectingStream {
    pub(crate) stream: Stream,
    pub(crate) kind: StreamKind,
    pub(crate) target_id: String,
    pub(crate) label: String,
    pub(crate) next_poll_at: Instant,
    pub(crate) wait_estimate: Option<(u64, Instant)>,
    pub(crate) consecutive_failures: u32,
    pub(crate) return_selected: usize,
}

pub(crate) struct StreamStartTarget {
    pub(crate) kind: StreamKind,
    pub(crate) target_id: String,
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
                        let reason = "Start stream failed".to_owned();
                        let details = format!(
                            "Failed to start {:?} stream for {}: {error:#}",
                            target.kind, target.label
                        );
                        eprintln!("ERROR: {reason}\n{details}");
                        AppState::Error { reason, details }
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
                self.service.auth = MsalAuth::new();
                let title_id =
                    (session.kind == StreamKind::Cloud).then(|| session.target_id.clone());
                match StreamingSession::start(
                    session.stream.clone(),
                    session.kind,
                    title_id,
                    session.return_selected,
                ) {
                    Ok(streaming) => Ok(AppState::Streaming(streaming)),
                    Err(error) => {
                        let reason = "WebRTC negotiation failed".to_owned();
                        let details = format!("Failed to start WebRTC session: {error:#}");
                        let _ = session.stream.stop().await;
                        eprintln!("ERROR: {reason}\n{details}");
                        Ok(AppState::Error { reason, details })
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
                    let reason = "Stream state failed".to_owned();
                    let details = format!("Stream state failed: {error:#}");
                    eprintln!("ERROR: {reason}\n{details}");
                    Ok(AppState::Error { reason, details })
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
