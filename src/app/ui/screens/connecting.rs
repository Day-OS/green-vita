use crate::app::AppState;
use crate::app::ui::header::show_header_row;
use crate::app::ui::theme::Theme;
use crate::i18n::I18n;
use crate::{App, InputCommand, StreamKind};
use anyhow::Result;

pub(crate) fn show(ctx: &egui::Context, app: &App) {
    let theme = Theme::dark();
    let i18n = I18n::new(app.settings.locale);
    let mut frame = egui::Frame::central_panel(&ctx.style());
    frame.fill = theme.background;
    egui::CentralPanel::default().frame(frame).show(ctx, |ui| {
        show_header_row(ui, app, theme, &i18n, None);
        ui.separator();
        ui.vertical_centered(|ui| {
            ui.add_space(24.0);
            ui.spinner();
            match &app.state {
                AppState::StartingStream { target, .. } => {
                    ui.colored_label(theme.text, format!("Target: {}", target.label));
                    ui.colored_label(theme.text, "Session: preparing");
                    ui.colored_label(theme.text, format!("Starting stream for {}", target.label));
                }
                AppState::Connecting { session, .. } => {
                    let wait_seconds = session.wait_estimate.map(|(total, fetched_at)| {
                        total.saturating_sub(fetched_at.elapsed().as_secs())
                    });
                    ui.colored_label(theme.text, format!("Target: {}", session.label));
                    ui.colored_label(
                        theme.text,
                        format!("Session: {}", session.stream.session_id),
                    );
                    ui.colored_label(
                        theme.text,
                        format!(
                            "Status: {}",
                            crate::app::describe_stream_state(session.stream.state, wait_seconds)
                        ),
                    );
                }
                _ => {}
            }
            ui.add_space(16.0);
            ui.colored_label(theme.text, i18n.text("connecting-cancel"));
        });
    });
}

impl App {
    pub(crate) async fn handle_connecting_input(&mut self, command: InputCommand) -> Result<()> {
        match command {
            InputCommand::Back => {
                let state =
                    std::mem::replace(&mut self.state, AppState::ModeSelect { selected: 0 });
                match state {
                    AppState::StartingStream { target, job } => {
                        if let Some(job) = job {
                            job.abort();
                        }
                        self.set_state(return_screen(target.kind, target.return_selected));
                    }
                    AppState::Connecting { session, .. } => {
                        let return_to = return_screen(session.kind, session.return_selected);
                        let session_id = session.stream.session_id.clone();
                        eprintln!("Cancelling connecting session {session_id}...");
                        match session.stream.stop().await {
                            Ok(response) => {
                                eprintln!("Cancelled session {session_id}: {response}");
                            }
                            Err(error) => {
                                eprintln!("Failed to cancel session {session_id}: {error:#}");
                            }
                        }
                        self.set_state(return_to);
                    }
                    state => self.state = state,
                }
            }
            InputCommand::MoveUp
            | InputCommand::MoveDown
            | InputCommand::MoveLeft
            | InputCommand::MoveRight
            | InputCommand::Confirm => {}
        }

        Ok(())
    }
}

fn return_screen(kind: StreamKind, selected: usize) -> AppState {
    match kind {
        StreamKind::Cloud => AppState::TitleList { selected },
        StreamKind::Home => AppState::ConsoleList { selected },
    }
}
