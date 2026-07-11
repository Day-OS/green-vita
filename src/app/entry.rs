use super::stream_session::cleanup_active_sessions;
use super::titles::extract_titles;
use super::{App, AppState, PollJob, poll_job};
use crate::xbox_api::catalog_worker::ImageKind;
use crate::{
    DeviceCodeAuth, DeviceCodePoll, MsalAuth, StreamKind, StreamingCredentials, XboxProfile,
};
use anyhow::Result;
use tokio::task::JoinHandle;

const AVATAR_KEY: &str = "__avatar__";

impl App {
    pub(super) fn load_credentials(&mut self) {
        let mut auth = self.service.auth.clone();
        self.set_state(AppState::LoadingCredentials(tokio::spawn(async move {
            let credentials = auth.fetch_streaming_credentials().await?;
            // Profile data is decorative and must not prevent sign-in.
            let profile = match auth.fetch_xbox_profile().await {
                Ok(profile) => Some(profile),
                Err(error) => {
                    eprintln!("Failed to fetch Xbox profile (avatar/gamertag): {error:#}");
                    None
                }
            };
            Ok((credentials, profile, auth))
        })));
    }

    pub(super) fn request_device_code(&mut self) {
        let auth = self.service.auth.clone();
        self.set_state(AppState::RequestingDeviceCode(tokio::spawn(async move {
            auth.request_device_code().await
        })));
    }

    pub(super) fn choose_stream_kind(&mut self, kind: StreamKind) -> Result<()> {
        self.enter_entrypoint(kind)
    }

    fn enter_entrypoint(&mut self, kind: StreamKind) -> Result<()> {
        match kind {
            StreamKind::Cloud => {
                let api = self.service.api.clone();
                self.set_state(AppState::LoadingTitles(tokio::spawn(async move {
                    api.get_titles().await
                })));
            }
            StreamKind::Home => {
                let api = self.service.api.clone();
                self.set_state(AppState::LoadingConsoles(tokio::spawn(async move {
                    api.get_consoles().await
                })));
            }
        }
        Ok(())
    }

    pub(super) async fn pump_entry_state(&mut self) -> Result<()> {
        let state = std::mem::replace(&mut self.state, AppState::InitializeAuthentication);

        match state {
            AppState::RequestingDeviceCode(job) => self.pump_device_code_request(job).await,
            AppState::WaitingForDeviceAuthorization { device_code, job } => {
                self.pump_device_authorization_check(device_code, job).await;
            }
            AppState::LoadingCredentials(job) => self.pump_credentials(job).await,
            AppState::LoadingTitles(job) => self.pump_titles(job).await,
            AppState::LoadingConsoles(job) => self.pump_consoles(job).await,
            state => self.state = state,
        }

        Ok(())
    }

    async fn pump_device_code_request(&mut self, job: JoinHandle<Result<DeviceCodeAuth>>) {
        match poll_job(job).await {
            PollJob::Pending(job) => self.state = AppState::RequestingDeviceCode(job),
            PollJob::Done(Ok(device_code)) => {
                let job = self.spawn_device_authorization_poll(&device_code);
                self.set_state(AppState::WaitingForDeviceAuthorization { device_code, job });
            }
            PollJob::Done(Err(error)) => {
                self.set_error_screen(
                    "Sign-in request failed",
                    format!("Failed to start xCloud sign-in: {error:#}"),
                );
            }
        }
    }

    async fn pump_device_authorization_check(
        &mut self,
        device_code: DeviceCodeAuth,
        job: JoinHandle<Result<MsalAuth>>,
    ) {
        match poll_job(job).await {
            PollJob::Pending(job) => {
                self.state = AppState::WaitingForDeviceAuthorization { device_code, job };
            }
            PollJob::Done(Ok(auth)) => {
                self.service.auth = auth;
                self.load_credentials();
            }
            PollJob::Done(Err(error)) => {
                eprintln!("Sign-in check failed: {error:#}");
                self.request_device_code();
            }
        }
    }

    fn spawn_device_authorization_poll(
        &self,
        device_code: &DeviceCodeAuth,
    ) -> JoinHandle<Result<MsalAuth>> {
        let mut auth = self.service.auth.clone();
        let device_code = device_code.clone();

        tokio::spawn(async move {
            let mut retry_after = device_code.poll_interval;
            loop {
                tokio::time::sleep(retry_after).await;
                if device_code.is_expired() {
                    anyhow::bail!("device code expired");
                }

                match auth.poll_device_code(&device_code).await {
                    Ok(DeviceCodePoll::Authorized) => return Ok(auth),
                    Ok(DeviceCodePoll::Pending(next_retry)) => retry_after = next_retry,
                    Ok(DeviceCodePoll::Restart) => anyhow::bail!("device code expired"),
                    Err(error) => {
                        eprintln!("Sign-in poll failed, retrying: {error:#}");
                        retry_after = device_code.poll_interval;
                    }
                }
            }
        })
    }

    async fn pump_credentials(
        &mut self,
        job: JoinHandle<Result<(StreamingCredentials, Option<XboxProfile>, MsalAuth)>>,
    ) {
        match poll_job(job).await {
            PollJob::Pending(job) => self.state = AppState::LoadingCredentials(job),
            PollJob::Done(Ok((credentials, profile, auth))) => {
                self.service.api.config.home = credentials.home;
                self.service.api.config.cloud = credentials.cloud;
                self.service.api.config.cloud_f2p = credentials.cloud_f2p;
                self.service.auth = auth;
                if let Some(profile) = profile {
                    self.service.gamertag = profile.gamertag;
                    self.service.gamerscore = profile.gamerscore;
                    if let Some(url) = profile.avatar_url {
                        self.service.catalog_worker.request_image(
                            AVATAR_KEY.to_owned(),
                            ImageKind::Avatar,
                            url,
                            None,
                        );
                    }
                }
                let api = self.service.api.clone();
                tokio::spawn(async move {
                    cleanup_active_sessions(&api, StreamKind::Cloud).await;
                    cleanup_active_sessions(&api, StreamKind::Home).await;
                });
                self.set_state(AppState::ModeSelect { selected: 0 });
            }
            PollJob::Done(Err(error)) => {
                eprintln!("Saved xCloud login failed to refresh: {error:#}");
                self.request_device_code();
            }
        }
    }

    async fn pump_titles(&mut self, job: JoinHandle<Result<serde_json::Value>>) {
        match poll_job(job).await {
            PollJob::Pending(job) => self.state = AppState::LoadingTitles(job),
            PollJob::Done(Ok(response)) => {
                self.service.titles = extract_titles(&response);
                self.set_state(AppState::TitleList { selected: 0 });
            }
            PollJob::Done(Err(error)) => {
                self.set_error_screen(
                    "Title request failed",
                    format!("Failed to load cloud titles: {error:#}"),
                );
            }
        }
    }

    async fn pump_consoles(&mut self, job: JoinHandle<Result<crate::ConsolesResponse>>) {
        match poll_job(job).await {
            PollJob::Pending(job) => self.state = AppState::LoadingConsoles(job),
            PollJob::Done(Ok(response)) => {
                self.service.consoles = response.results;
                self.set_state(AppState::ConsoleList { selected: 0 });
            }
            PollJob::Done(Err(error)) => {
                self.set_error_screen(
                    "Console request failed",
                    format!("Failed to load consoles: {error:#}"),
                );
            }
        }
    }
}
