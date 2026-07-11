use crate::app::ui::theme::Theme;
use crate::{App, AppState};

pub(crate) fn show(ctx: &egui::Context, app: &App) {
    let theme = Theme::dark();
    let message = match &app.state {
        AppState::InitializeAuthentication => "Starting GreenVita...",
        AppState::RequestingDeviceCode(_) => "Requesting Xbox sign-in...",
        AppState::LoadingCredentials(_) => "Signing in to Xbox...",
        _ => return,
    };
    let mut frame = egui::Frame::central_panel(&ctx.style());
    frame.fill = theme.background;
    egui::CentralPanel::default().frame(frame).show(ctx, |ui| {
        ui.vertical_centered(|ui| {
            let available_height = ui.available_height();
            ui.add_space(((available_height - 160.0) / 2.0).max(0.0));
            ui.add(egui::Spinner::new().size(48.0));
            ui.add_space(20.0);
            ui.label(
                egui::RichText::new(message)
                    .color(theme.text_bright)
                    .size(24.0),
            );
        });
    });
}
