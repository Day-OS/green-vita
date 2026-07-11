use crate::app::ui::header::show_header_row;
use crate::app::ui::theme::Theme;
use crate::i18n::I18n;
use crate::{App, AppCommand, AppState, InputCommand};

pub(crate) fn show(ctx: &egui::Context, app: &App, commands: &mut Vec<AppCommand>) {
    let theme = Theme::error();
    let i18n = I18n::new(app.settings.locale);
    let mut frame = egui::Frame::central_panel(&ctx.style());
    frame.fill = theme.background;
    egui::CentralPanel::default().frame(frame).show(ctx, |ui| {
        show_header_row(ui, app, theme, &i18n, None);
        ui.separator();
        if let AppState::Error { reason, details } = &app.state {
            ui.colored_label(theme.text_bright, reason);
            ui.add(egui::Label::new(egui::RichText::new(details).color(theme.text)).wrap());
        }
        ui.colored_label(
            theme.text,
            format!("Cloud host: {}", app.service.api.config.cloud.host),
        );
        ui.colored_label(
            theme.text,
            format!("Home host: {}", app.service.api.config.home.host),
        );
        if ui.button(i18n.text("action-back")).clicked() {
            commands.push(InputCommand::Back.into());
        }
    });
}
