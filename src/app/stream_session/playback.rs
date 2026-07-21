use crate::app::stream_session::session::StreamReturnTarget;
use crate::app::{App, AppState};
use anyhow::Result;

impl App {
    pub(in crate::app) async fn pump_rtc_session(&mut self) -> Result<()> {
        let Some(streaming) = self.state.streaming_mut() else {
            return Ok(());
        };

        let (closed, error) = streaming.drain_backend_events();
        if let Some(error) = error {
            self.stop_stream_with_error("error-webrtc-session", error.to_string())
                .await;
            return Ok(());
        }
        if closed {
            self.exit_stream().await;
            return Ok(());
        }

        let Some(streaming) = self.state.streaming_mut() else {
            return Ok(());
        };
        if let Some(code) = streaming.maintain_backend().await {
            self.stop_stream_with_error("error-stream-ended", code)
                .await;
        }
        Ok(())
    }

    async fn stop_stream_with_error(&mut self, reason_key: &'static str, error: String) {
        let state = std::mem::replace(&mut self.state, AppState::ModeSelect { selected: 0 });
        if let Some(streaming) = state.into_streaming() {
            let _ = streaming.stop().await;
        }
        self.set_localized_error_screen(reason_key, error);
    }

    pub(in crate::app) async fn exit_stream(&mut self) {
        let state = std::mem::replace(&mut self.state, AppState::ModeSelect { selected: 0 });
        let return_target = if let Some(streaming) = state.into_streaming() {
            let return_target = streaming.return_target;
            let description = streaming.backend_description();
            eprintln!("Exiting stream: stopping {description}...");
            match streaming.stop().await {
                Ok(()) => {}
                Err(error) => eprintln!("Failed to stop {description}: {error:#}"),
            }
            return_target
        } else {
            self.set_state(AppState::ModeSelect { selected: 0 });
            return;
        };
        self.set_state(match return_target {
            StreamReturnTarget::Titles(selected) => AppState::TitleList { selected },
            StreamReturnTarget::Consoles(selected) => AppState::ConsoleList { selected },
        });
    }
}
