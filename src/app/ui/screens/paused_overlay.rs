use crate::app::ui::header::show_header_row;
use crate::app::ui::theme::Theme;
use crate::app::ui::widgets::menu_item;
use crate::i18n::I18n;
use crate::{App, AppCommand, AppState, InputCommand};
use anyhow::Result;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Command {
    Resume,
    PressGuideButton,
    ExitGame,
}

pub const MENU_ITEMS: [Command; 3] = [
    Command::Resume,
    Command::PressGuideButton,
    Command::ExitGame,
];

impl Command {
    fn icon(self) -> &'static str {
        match self {
            Self::Resume => "\u{25b6}",
            Self::PressGuideButton => "\u{2302}",
            Self::ExitGame => "\u{2715}",
        }
    }

    fn label_key(self) -> &'static str {
        match self {
            Self::Resume => "paused-resume",
            Self::PressGuideButton => "paused-xbox-button",
            Self::ExitGame => "paused-exit-game",
        }
    }
}

pub(crate) fn show(ctx: &egui::Context, app: &App, commands: &mut Vec<AppCommand>) {
    let theme = Theme::dark();
    let i18n = I18n::new(app.settings.locale);
    let mut frame = egui::Frame::central_panel(&ctx.style());
    frame.fill = egui::Color32::from_rgba_unmultiplied(0x2a, 0x2a, 0x2e, 190);
    egui::CentralPanel::default().frame(frame).show(ctx, |ui| {
        show_header_row(ui, app, theme, &i18n, Some(commands));
        ui.separator();
        ui.vertical_centered(|ui| {
            ui.add_space(40.0);
            match app.active_title_name() {
                Some(title_name) => {
                    ui.heading(egui::RichText::new(title_name).color(theme.text_bright));
                    ui.colored_label(theme.accent, i18n.text("screen-paused"));
                }
                None => {
                    ui.heading(egui::RichText::new(i18n.text("screen-paused")).color(theme.accent));
                }
            }
            ui.add_space(12.0);
            ui.set_max_width(240.0);
            for (index, item) in MENU_ITEMS.iter().copied().enumerate() {
                if index > 0 {
                    ui.add_space(6.0);
                }
                if menu_item(
                    ui,
                    theme,
                    item.icon(),
                    &i18n.text(item.label_key()),
                    matches!(&app.state, AppState::Streaming(streaming) if streaming.pause_selected == index),
                ) {
                    commands.push(item.into());
                }
            }
        });
    });
}

impl App {
    pub(crate) async fn handle_paused_overlay_input(
        &mut self,
        command: InputCommand,
    ) -> Result<()> {
        let Some(selected) = (match &self.state {
            AppState::Streaming(streaming) => Some(streaming.pause_selected),
            _ => None,
        }) else {
            return Ok(());
        };
        match command {
            InputCommand::MoveUp => {
                if let AppState::Streaming(streaming) = &mut self.state {
                    streaming.pause_selected =
                        crate::app::command::move_prev(selected, MENU_ITEMS.len());
                }
            }
            InputCommand::MoveDown => {
                if let AppState::Streaming(streaming) = &mut self.state {
                    streaming.pause_selected =
                        crate::app::command::move_next(selected, MENU_ITEMS.len());
                }
            }
            InputCommand::MoveLeft | InputCommand::MoveRight => {}
            InputCommand::Confirm => {
                let command = MENU_ITEMS
                    .get(selected)
                    .copied()
                    .unwrap_or(Command::ExitGame);
                self.handle_paused_overlay_command(command).await?;
            }
            // Only the Resume row / pause toggle resumes; Back is sent through to the game while
            // streaming and does not close this overlay.
            InputCommand::Back => {}
        }

        Ok(())
    }

    pub(crate) async fn handle_paused_overlay_command(&mut self, command: Command) -> Result<()> {
        match command {
            Command::Resume => {
                if let Some(streaming) = self.state.streaming_mut() {
                    streaming.set_paused(false);
                }
                self.menu.open = false;
            }
            Command::PressGuideButton => {
                if let Some(streaming) = self.state.streaming_mut() {
                    streaming.set_paused(false);
                    streaming.press_guide_button();
                }
                self.menu.open = false;
            }
            Command::ExitGame => {
                self.exit_stream().await;
            }
        }

        Ok(())
    }
}
