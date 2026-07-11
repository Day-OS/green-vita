use crate::StreamKind;
use crate::app::{App, AppState};
use anyhow::Result;

impl App {
    pub(in crate::app) async fn pump_rtc_session(&mut self) -> Result<()> {
        let Some(streaming) = self.state.streaming_mut() else {
            return Ok(());
        };

        let (closed, error) = streaming.drain_worker_events();
        if let Some(error) = error {
            self.stop_stream_with_error(
                "WebRTC session failed",
                format!("WebRTC worker failed: {error}"),
            )
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
        streaming.post_local_ice().await;
        streaming.poll_remote_ice().await;
        if let Some(code) = streaming.keep_alive().await {
            self.stop_stream_with_error(
                "Stream session ended",
                format!("Stream session ended: {code}"),
            )
            .await;
        }
        Ok(())
    }

    async fn stop_stream_with_error(&mut self, reason: &'static str, details: String) {
        let state = std::mem::replace(&mut self.state, AppState::ModeSelect { selected: 0 });
        if let Some(streaming) = state.into_streaming() {
            let _ = streaming.stream.stop().await;
        }
        self.set_error_screen(reason, details);
    }

    pub(in crate::app) async fn exit_stream(&mut self) {
        let state = std::mem::replace(&mut self.state, AppState::ModeSelect { selected: 0 });
        let return_kind = if let Some(streaming) = state.into_streaming() {
            let kind = streaming.kind;
            let return_selected = streaming.return_selected;
            let session_id = streaming.stream.session_id.clone();
            eprintln!("Exiting stream: stopping session {session_id}...");
            match streaming.stream.stop().await {
                Ok(response) => eprintln!("Stopped session {session_id}: {response}"),
                Err(error) => eprintln!("Failed to stop session {session_id}: {error:#}"),
            }
            (kind, return_selected)
        } else {
            self.set_state(AppState::ModeSelect { selected: 0 });
            return;
        };
        self.set_state(match return_kind {
            (StreamKind::Cloud, selected) => AppState::TitleList { selected },
            (StreamKind::Home, selected) => AppState::ConsoleList { selected },
        });
    }
}
