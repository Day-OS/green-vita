use super::App;
use super::stream_session::{ConnectingStream, StreamStartTarget, StreamingSession};
use crate::api::catalog::Game;
use crate::{
    ConsolesResponse, DeviceCodeAuth, MsalAuth, Stream, StreamState, StreamingCredentials,
    WaitTimeResponse, XboxProfile,
};
use anyhow::Result;
use tokio::task::JoinHandle;

pub(crate) enum AppState {
    InitializeAuthentication,
    LanguageSelect {
        selected: usize,
    },
    RequestingDeviceCode(JoinHandle<Result<DeviceCodeAuth>>),
    WaitingForDeviceAuthorization {
        device_code: DeviceCodeAuth,
        job: JoinHandle<Result<MsalAuth>>,
    },
    LoadingCredentials(JoinHandle<Result<(StreamingCredentials, Option<XboxProfile>, MsalAuth)>>),
    ModeSelect {
        selected: usize,
    },
    LoadingTitles(JoinHandle<Result<Vec<Game>>>),
    TitleList {
        selected: usize,
    },
    LoadingConsoles(JoinHandle<Result<ConsolesResponse>>),
    ConsoleList {
        selected: usize,
    },
    StartingStream {
        target: StreamStartTarget,
        job: Option<JoinHandle<Result<Stream>>>,
    },
    Connecting {
        session: ConnectingStream,
        poll_job: Option<JoinHandle<Result<(Stream, StreamState)>>>,
        wait_estimate_job: Option<JoinHandle<Result<WaitTimeResponse>>>,
    },
    Streaming(StreamingSession),
    Settings {
        return_to: Box<AppState>,
        selected: usize,
        title_id: Option<String>,
        locale_expanded: bool,
    },
    Error {
        reason: String,
        details: String,
    },
}

impl AppState {
    fn keeps_stream_alive(&self) -> bool {
        match self {
            Self::Streaming(_) => true,
            Self::Settings { return_to, .. } => return_to.keeps_stream_alive(),
            _ => false,
        }
    }

    pub(crate) fn streaming(&self) -> Option<&StreamingSession> {
        match self {
            Self::Streaming(streaming) => Some(streaming),
            Self::Settings { return_to, .. } => return_to.streaming(),
            _ => None,
        }
    }

    pub(crate) fn streaming_mut(&mut self) -> Option<&mut StreamingSession> {
        match self {
            Self::Streaming(streaming) => Some(streaming),
            Self::Settings { return_to, .. } => return_to.streaming_mut(),
            _ => None,
        }
    }

    pub(super) fn active_title_id(&self) -> Option<&str> {
        match self {
            Self::StartingStream { target, .. } => target.game_id.as_deref(),
            Self::Connecting { session, .. } => session.game_id.as_deref(),
            Self::Streaming(streaming) => streaming.title_id.as_deref(),
            Self::Settings { return_to, .. } => return_to.active_title_id(),
            _ => None,
        }
    }

    pub(super) fn into_streaming(self) -> Option<StreamingSession> {
        match self {
            Self::Streaming(streaming) => Some(streaming),
            Self::Settings { return_to, .. } => return_to.into_streaming(),
            _ => None,
        }
    }
}

impl App {
    pub async fn tick(&mut self) -> Result<()> {
        match &self.state {
            AppState::InitializeAuthentication => {
                if self.service.auth.has_saved_login() {
                    self.load_credentials();
                } else {
                    let selected = crate::Locale::ALL
                        .iter()
                        .position(|&locale| locale == self.settings.locale)
                        .unwrap_or(0);
                    self.set_state(AppState::LanguageSelect { selected });
                }
            }
            AppState::RequestingDeviceCode(_)
            | AppState::WaitingForDeviceAuthorization { .. }
            | AppState::LoadingCredentials(_)
            | AppState::LoadingTitles(_)
            | AppState::LoadingConsoles(_) => self.pump_entry_state().await?,
            AppState::StartingStream { .. } | AppState::Connecting { .. } => {
                self.pump_connection().await?
            }
            AppState::Streaming(_) => self.pump_rtc_session().await?,
            AppState::Settings { return_to, .. } if return_to.keeps_stream_alive() => {
                self.pump_rtc_session().await?
            }
            AppState::TitleList { .. } => self.pump_title_details().await?,
            AppState::LanguageSelect { .. }
            | AppState::ModeSelect { .. }
            | AppState::ConsoleList { .. }
            | AppState::Settings { .. }
            | AppState::Error { .. } => {}
        }
        Ok(())
    }
}
