use crate::app::ui::theme::Theme;
use crate::i18n::I18n;
use crate::{App, AppState};

pub(crate) fn show(ctx: &egui::Context, app: &App) {
    let theme = Theme::dark();
    let message_key = match &app.state {
        AppState::InitializeAuthentication => "signing-in-starting",
        AppState::RequestingDeviceCode(_) => "signing-in-requesting-code",
        AppState::LoadingCredentials(_) => "signing-in-loading-credentials",
        _ => return,
    };
    let i18n = I18n::new(app.settings.locale);
    let mut frame = egui::Frame::central_panel(&ctx.style());
    frame.fill = theme.background;
    egui::CentralPanel::default().frame(frame).show(ctx, |ui| {
        ui.vertical_centered(|ui| {
            let available_height = ui.available_height();
            ui.add_space(((available_height - 160.0) / 2.0).max(0.0));
            ui.add(egui::Spinner::new().size(48.0));
            ui.add_space(20.0);
            ui.label(
                egui::RichText::new(i18n.text(message_key))
                    .color(theme.text_bright)
                    .size(24.0),
            );
        });
    });
}
