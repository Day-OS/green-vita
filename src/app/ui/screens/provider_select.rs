use crate::api::CloudProvider;
use crate::app::command::{move_next, move_prev};
use crate::app::ui::header::show_header_row;
use crate::app::ui::theme::Theme;
use crate::i18n::I18n;
use crate::{App, AppCommand, AppState, InputCommand};
use anyhow::Result;

const PROVIDERS: [CloudProvider; 2] = [CloudProvider::Xbox, CloudProvider::GeForceNow];

pub(crate) fn show(ctx: &egui::Context, app: &App, commands: &mut Vec<AppCommand>) {
    let AppState::ProviderSelect { selected } = &app.state else {
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
            ui.set_max_width(460.0);
            for (index, provider) in PROVIDERS.into_iter().enumerate() {
                let (title, subtitle) = match provider {
                    CloudProvider::Xbox => (
                        i18n.text("provider-select-xbox"),
                        i18n.text("provider-select-xbox-subtitle"),
                    ),
                    CloudProvider::GeForceNow => (
                        i18n.text("provider-select-geforce-now"),
                        i18n.text("provider-select-geforce-now-subtitle"),
                    ),
                };
                let is_selected = selected == index;
                let button = egui::Button::new(
                    egui::RichText::new(format!("{title}\n{subtitle}"))
                        .size(20.0)
                        .color(egui::Color32::WHITE),
                )
                .fill(if is_selected {
                    theme.accent
                } else {
                    egui::Color32::from_rgb(0x26, 0x27, 0x2c)
                });
                if ui
                    .add_sized(egui::vec2(ui.available_width(), 100.0), button)
                    .clicked()
                {
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

impl App {
    pub(crate) fn handle_provider_select_input(&mut self, command: InputCommand) -> Result<()> {
        let AppState::ProviderSelect { selected } = &self.state else {
            return Ok(());
        };
        let selected = *selected;
        match command {
            InputCommand::MoveUp => {
                self.state = AppState::ProviderSelect {
                    selected: move_prev(selected, PROVIDERS.len()),
                }
            }
            InputCommand::MoveDown => {
                self.state = AppState::ProviderSelect {
                    selected: move_next(selected, PROVIDERS.len()),
                }
            }
            InputCommand::Confirm => self.choose_cloud_provider(PROVIDERS[selected]),
            InputCommand::MoveLeft | InputCommand::MoveRight | InputCommand::Back => {}
        }
        Ok(())
    }
}
