pub(crate) mod fonts;
pub mod header;
pub mod screens;
mod theme;
mod widgets;

use crate::{App, AppCommand, AppState};

pub fn build_ui(ctx: &egui::Context, app: &App, hold_progress: Option<f32>) -> Vec<AppCommand> {
    let mut commands = Vec::new();

    match &app.state {
        AppState::Streaming(streaming) if streaming.paused => {
            screens::paused_overlay::show(ctx, app, &mut commands);
        }
        AppState::Streaming(_) => {
            screens::streaming::show(ctx, app, hold_progress);
        }
        AppState::TitleList { .. } | AppState::LoadingTitles(_) => {
            screens::title_list::show(ctx, app, &mut commands);
        }
        AppState::ConsoleList { .. } | AppState::LoadingConsoles(_) => {
            screens::console_list::show(ctx, app, &mut commands);
        }
        AppState::InitializeAuthentication
        | AppState::RequestingDeviceCode(_)
        | AppState::LoadingCredentials(_) => {
            screens::signing_in::show(ctx, app);
        }
        AppState::StartingStream { .. } | AppState::Connecting { .. } => {
            screens::connecting::show(ctx, app);
        }
        AppState::Error { .. } => {
            screens::error::show(ctx, app, &mut commands);
        }
        AppState::WaitingForDeviceAuthorization { .. } => {
            screens::token_setup::show(ctx, app);
        }
        AppState::ModeSelect { .. } => {
            screens::mode_select::show(ctx, app, &mut commands);
        }
        AppState::Settings { .. } => {
            screens::settings::show(ctx, app, &mut commands);
        }
    }

    commands
}
