use crate::app::command::{move_next, move_prev};
use crate::app::ui::header::show_header_row;
use crate::app::ui::theme::Theme;
use crate::i18n::I18n;
use crate::{App, AppCommand, AppState, InputCommand, StreamKind};
use anyhow::Result;

const OPTIONS: [StreamKind; 2] = [StreamKind::Cloud, StreamKind::Home];
const OPTION_COUNT: usize = OPTIONS.len();

pub(crate) fn show(ctx: &egui::Context, app: &App, commands: &mut Vec<AppCommand>) {
    let AppState::ModeSelect { selected } = &app.state else {
        return;
    };
    let selected = *selected;
    let theme = Theme::dark();
    let i18n = I18n::new(app.settings.locale);
    let mut frame = egui::Frame::central_panel(&ctx.style());
    frame.fill = theme.background;
    egui::CentralPanel::default().frame(frame).show(ctx, |ui| {
        show_header_row(ui, app, theme, &i18n, None);
        ui.separator();
        ui.vertical_centered(|ui| {
            ui.add_space(28.0);
            ui.set_max_width(420.0);
            for (index, kind) in OPTIONS.into_iter().enumerate() {
                if mode_card(ui, theme, &i18n, kind, selected == index) {
                    if selected != index {
                        commands.push(if index > selected {
                            InputCommand::MoveDown.into()
                        } else {
                            InputCommand::MoveUp.into()
                        });
                    }
                    commands.push(InputCommand::Confirm.into());
                }
                ui.add_space(18.0);
            }
        });
    });
}

fn mode_card(
    ui: &mut egui::Ui,
    theme: Theme,
    i18n: &I18n,
    kind: StreamKind,
    selected: bool,
) -> bool {
    let (title_key, subtitle_key) = match kind {
        StreamKind::Cloud => ("settings-cloud", "mode-select-cloud-subtitle"),
        StreamKind::Home => ("settings-home", "mode-select-home-subtitle"),
    };

    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), 110.0),
        egui::Sense::click(),
    );

    if ui.is_rect_visible(rect) {
        let fill = if selected {
            theme.accent
        } else {
            egui::Color32::from_rgb(0x26, 0x27, 0x2c)
        };
        let text_color = if selected {
            egui::Color32::WHITE
        } else {
            theme.text_bright
        };

        ui.painter().rect_filled(rect, 14.0, fill);
        if selected {
            ui.painter().rect_stroke(
                rect,
                14.0,
                egui::Stroke::new(2.0_f32, egui::Color32::WHITE),
                egui::StrokeKind::Inside,
            );
        }
        ui.painter().text(
            rect.center() - egui::vec2(0.0, 14.0),
            egui::Align2::CENTER_CENTER,
            i18n.text(title_key),
            egui::FontId::proportional(28.0),
            text_color,
        );
        ui.painter().text(
            rect.center() + egui::vec2(0.0, 18.0),
            egui::Align2::CENTER_CENTER,
            i18n.text(subtitle_key),
            egui::FontId::proportional(15.0),
            text_color,
        );
    }

    response.clicked()
}

impl App {
    pub(crate) fn handle_mode_select_input(&mut self, command: InputCommand) -> Result<()> {
        let AppState::ModeSelect { selected } = &mut self.state else {
            return Ok(());
        };
        match command {
            InputCommand::MoveUp => *selected = move_prev(*selected, OPTION_COUNT),
            InputCommand::MoveDown => *selected = move_next(*selected, OPTION_COUNT),
            InputCommand::Confirm => {
                let kind = OPTIONS[*selected];
                self.choose_stream_kind(kind)?;
            }
            InputCommand::Back => {}
        }

        Ok(())
    }
}
