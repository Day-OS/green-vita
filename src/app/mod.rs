pub mod command;
mod entry;
mod image;
mod jobs;
mod service;
mod state;
mod stream_session;
mod titles;
pub(crate) mod ui;

pub use command::{AppCommand, InputCommand, NavigationCommand};
pub use image::TitleImage;
pub(crate) use jobs::{PollJob, poll_job};
pub(crate) use state::AppState;
pub(crate) use stream_session::{StreamStartTarget, StreamingSession, describe_stream_state};

use self::service::Service;
use self::ui::header::MenuState;
use crate::settings::Settings;
use anyhow::Result;

pub struct App {
    pub settings: Settings,
    pub(crate) service: Service,
    pub(crate) state: AppState,
    pub(crate) menu: MenuState,
}

impl App {
    pub fn new() -> Result<Self> {
        let settings = Settings::load();
        let service = Service::new(settings.locale.as_str());

        Ok(Self {
            service,
            state: AppState::InitializeAuthentication,
            menu: MenuState::default(),
            settings,
        })
    }

    fn set_state(&mut self, state: AppState) {
        self.state = state;
        self.menu.open = false;
    }

    fn set_error_screen(&mut self, reason: impl Into<String>, details: impl Into<String>) {
        let reason = reason.into();
        let details = details.into();
        eprintln!("ERROR: {reason}");
        if !details.is_empty() {
            eprintln!("{details}");
        }
        self.set_state(AppState::Error { reason, details });
    }

    /// The active cloud title's display name - `None` for Home streams.
    fn active_title_name(&self) -> Option<String> {
        let title_id = self.state.active_title_id()?;
        Some(self.service.title_name_or_id(title_id))
    }
}
