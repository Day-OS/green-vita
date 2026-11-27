pub mod command;
mod entry;
pub(crate) mod error_classify;
mod image;
mod service;
mod state;
mod stream_session;
mod titles;
pub(crate) mod ui;

pub(crate) use crate::jobs::{PollJob, poll_job};
pub use command::{AppCommand, InputCommand, NavigationCommand};
pub use image::TitleImage;
pub(crate) use state::AppState;
pub(crate) use stream_session::{StreamStartTarget, StreamingSession, describe_stream_state};

use self::service::Service;
use self::ui::header::MenuState;
use crate::i18n::{I18n, arg_string};
use crate::settings::Settings;
use anyhow::Result;
use fluent_bundle::FluentArgs;
use std::fmt::Display;
use std::time::Instant;

pub(crate) struct TitleInitialOverlay {
    pub(crate) label: String,
    pub(crate) shown_at: Instant,
}

pub struct App {
    pub settings: Settings,
    pub(crate) service: Service,
    pub(crate) state: AppState,
    pub(crate) menu: MenuState,
    pub(crate) title_initial_overlay: Option<TitleInitialOverlay>,
    /// Cap advisor (TODO 7b): thread→app mailbox + consumed suggestion.
    /// Advisory only; `cap_probe` holds None while no probe is in flight.
    pub(crate) cap_probe: std::sync::Arc<
        std::sync::Mutex<Option<crate::shell::netprobe::ProbeResult>>,
    >,
    pub(crate) cap_advice: Option<crate::shell::netprobe::ProbeResult>,
    pub(crate) cap_advice_dismissed: bool,
    pub(crate) title_search_query: String,
    /// Set by a screen that wants the Vita on-screen keyboard; the shell opens it
    /// and routes the confirmed text back via [`App::submit_ime_text`].
    pub(crate) ime_request: Option<ImeTarget>,
    /// The region bypass was changed since the last sign-in; credentials are
    /// re-fetched when leaving Settings to a screen where that is safe.
    pub(crate) region_bypass_changed: bool,
}

/// What text the Vita on-screen keyboard is collecting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ImeTarget {
    TitleSearch,
    RegionBypassIp,
}

impl App {
    pub(crate) fn submit_ime_text(&mut self, target: ImeTarget, text: String) {
        match target {
            ImeTarget::TitleSearch => self.set_title_search_query(text),
            ImeTarget::RegionBypassIp => self.set_region_bypass_ip_from_input(&text),
        }
    }

    /// Phase 1 network validation (TODO 7a): the state to enter after any
    /// sign-in entry point. Offline Wi-Fi ⇒ the existing Error screen with
    /// `retry_sign_in` — retry re-runs this gate, so coming back online
    /// flows straight into sign-in. Online/Unknown ⇒ normal sign-in.
    pub(crate) fn state_after_network_check(&self) -> AppState {
        use crate::shell::netctl::{blocks_sign_in, wifi_state};
        if blocks_sign_in(wifi_state()) {
            let i18n = I18n::new(self.settings.locale);
            AppState::Error {
                reason: i18n.text("error-no-wifi"),
                details: i18n.text("error-no-wifi-details"),
                retry_sign_in: true,
            }
        } else {
            AppState::InitializeAuthentication
        }
    }

    pub fn new() -> Result<Self> {
        let settings = Settings::load();
        let service = Service::new(settings.locale.as_str());

        let mut app = Self {
            service,
            state: AppState::InitializeAuthentication,
            menu: MenuState::default(),
            title_initial_overlay: None,
            title_search_query: String::new(),
            ime_request: None,
            region_bypass_changed: false,
            cap_probe: std::sync::Arc::new(std::sync::Mutex::new(None)),
            cap_advice: None,
            cap_advice_dismissed: false,
            settings,
        };
        app.state = app.state_after_network_check();
        Ok(app)
    }

    /// Phase 2 cap advisor (TODO 7b): one probe per sign-in entry, on a
    /// worker thread (blocking socket I/O). A pending mailbox means a probe
    /// is already in flight; `tick` moves finished results into
    /// `cap_advice` and resets the mailbox for the next entry.
    pub(crate) fn maybe_start_cap_probe(&mut self) {
        if self
            .cap_probe
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_some()
        {
            return;
        }
        let Some(gateway) = crate::shell::netctl::default_route() else {
            return;
        };
        let mailbox = std::sync::Arc::clone(&self.cap_probe);
        let spawned = std::thread::Builder::new()
            .name("cap-probe".into())
            .spawn(move || {
                let result = crate::shell::netprobe::run_probe(gateway);
                *mailbox.lock().unwrap_or_else(|e| e.into_inner()) = Some(result);
            });
        if let Err(error) = spawned {
            eprintln!("cap-probe thread failed to spawn: {error}");
        }
    }

    fn set_state(&mut self, state: AppState) {
        // Probe POST-auth (TitleList/ConsoleList entry), never during
        // InitializeAuthentication — the auth cascade's own traffic inflates
        // the RTT sample (2026-11-02: probe measured 35.5 ms during auth and
        // suggested 1500 on a link that ran 5 M at dam 0.14/s).
        if matches!(
            state,
            AppState::TitleList { .. } | AppState::ConsoleList { .. }
        ) {
            self.maybe_start_cap_probe();
        }
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
        self.set_state(AppState::Error {
            reason,
            details,
            retry_sign_in: false,
        });
    }

    fn localized_error(&self, reason_key: &'static str, error: impl Display) -> (String, String) {
        let i18n = I18n::new(self.settings.locale);
        let raw = error.to_string();
        // TODO 16c: a known failure class overrides the caller's generic
        // reason with a friendlier localized message; the raw text always
        // stays in `details` for diagnosis.
        let reason_key = error_classify::classify_error(&raw).unwrap_or(reason_key);
        let mut args = FluentArgs::new();
        args.set("error", arg_string(raw));
        (
            i18n.text(reason_key),
            i18n.text_with("error-technical-details", args),
        )
    }

    fn set_localized_error_screen(&mut self, reason_key: &'static str, error: impl Display) {
        let (reason, details) = self.localized_error(reason_key, error);
        self.set_error_screen(reason, details);
    }

    fn set_sign_in_error_screen(&mut self, error: impl Display) {
        let (reason, details) = self.localized_error("error-sign-in-request", error);
        self.set_state(AppState::Error {
            reason,
            details,
            retry_sign_in: true,
        });
    }

    fn localized_error_state(&self, reason_key: &'static str, error: impl Display) -> AppState {
        let (reason, details) = self.localized_error(reason_key, error);
        AppState::Error {
            reason,
            details,
            retry_sign_in: false,
        }
    }

    /// The active cloud title's display name - `None` for Home streams.
    fn active_title_name(&self) -> Option<String> {
        let title_id = self.state.active_title_id()?;
        Some(self.service.title_name_or_id(title_id))
    }
}
