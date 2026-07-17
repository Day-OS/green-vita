use crate::App;
use crate::app::AppState;
use crate::app::ui::header::show_header_row;
use crate::app::ui::theme::Theme;
use crate::i18n::{I18n, arg_string};
use fluent_bundle::FluentArgs;
use std::sync::Arc;

#[derive(Clone)]
struct QrImage {
    uri: String,
    modules: Vec<bool>,
    size: u32,
}

/// Device-code sign-in prompt: instructions, a large code box, and a QR code for `verification_uri`.
pub(crate) fn show(ctx: &egui::Context, app: &App) {
    let theme = Theme::dark();
    let i18n = I18n::new(app.settings.locale);
    let mut frame = egui::Frame::central_panel(&ctx.style());
    frame.fill = theme.background;
    egui::CentralPanel::default().frame(frame).show(ctx, |ui| {
        show_header_row(ui, app, theme, &i18n, None);
        ui.separator();

        if let AppState::WaitingForDeviceAuthorization { device_code, .. } = &app.state {
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.set_width(ui.available_width() - 220.0);
                    ui.add_space(8.0);
                    ui.label(
                        egui::RichText::new(&device_code.message)
                            .size(20.0)
                            .color(theme.text),
                    );
                    ui.add_space(24.0);
                    egui::Frame::NONE
                        .fill(egui::Color32::from_rgb(0x26, 0x27, 0x2c))
                        .corner_radius(12.0)
                        .inner_margin(egui::Margin::symmetric(28, 20))
                        .show(ui, |ui| {
                            ui.label(
                                egui::RichText::new(&device_code.user_code)
                                    .size(56.0)
                                    .monospace()
                                    .strong()
                                    .color(theme.text_bright),
                            );
                        });
                    ui.add_space(24.0);
                    let mut args = FluentArgs::new();
                    args.set("uri", arg_string(&device_code.verification_uri));
                    ui.label(
                        egui::RichText::new(i18n.text_with("token-at-uri", args))
                            .size(18.0)
                            .color(theme.text),
                    );
                });

                ui.with_layout(egui::Layout::top_down(egui::Align::Center), |ui| {
                    ui.add_space(8.0);
                    draw_qr(ui, &device_code.verification_uri, 200.0);
                });
            });
        } else {
            ui.vertical_centered(|ui| {
                ui.add_space(24.0);
                ui.spinner();
                ui.add_space(12.0);
                ui.label(
                    egui::RichText::new(i18n.text("token-starting-sign-in"))
                        .size(20.0)
                        .color(theme.text),
                );
            });
        }

        ui.with_layout(egui::Layout::bottom_up(egui::Align::Center), |ui| {
            ui.add_space(6.0);
            ui.label(
                egui::RichText::new(i18n.text("token-source-warning"))
                    .size(12.0)
                    .color(theme.text),
            );
            ui.add_space(6.0);
            ui.separator();
        });
    });
}

/// Draws a QR code's module grid as plain filled rects (not an SDL texture blit).
fn draw_qr(ui: &mut egui::Ui, verification_uri: &str, target_size: f32) {
    const QUIET_ZONE_MODULES: u32 = 2;
    let cache_id = egui::Id::new("device_code_qr");
    let cached = ui.ctx().data_mut(|data| {
        if let Some(cached) = data.get_temp::<Arc<QrImage>>(cache_id)
            && cached.uri == verification_uri
        {
            return Some(cached);
        }

        let code = qrcode::QrCode::new(verification_uri).ok()?;
        let image = Arc::new(QrImage {
            uri: verification_uri.to_owned(),
            size: code.width() as u32,
            modules: code
                .to_colors()
                .into_iter()
                .map(|color| color == qrcode::Color::Dark)
                .collect(),
        });
        data.insert_temp(cache_id, image.clone());
        Some(image)
    });
    let Some(cached) = cached else {
        ui.spinner();
        return;
    };
    let total_modules = cached.size + QUIET_ZONE_MODULES * 2;
    let module_size = target_size / total_modules as f32;

    let (rect, _) =
        ui.allocate_exact_size(egui::vec2(target_size, target_size), egui::Sense::hover());
    let painter = ui.painter();
    painter.rect_filled(rect, 4.0, egui::Color32::WHITE);
    for y in 0..cached.size {
        for x in 0..cached.size {
            if !cached.modules[(y * cached.size + x) as usize] {
                continue;
            }
            let module_rect = egui::Rect::from_min_size(
                rect.min
                    + egui::vec2(
                        (QUIET_ZONE_MODULES + x) as f32 * module_size,
                        (QUIET_ZONE_MODULES + y) as f32 * module_size,
                    ),
                egui::vec2(module_size, module_size),
            );
            painter.rect_filled(module_rect, 0.0, egui::Color32::BLACK);
        }
    }
}
