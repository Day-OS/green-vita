use crate::app::command::{move_next, move_prev};
use crate::app::ui::header::show_header_row;
use crate::app::ui::theme::Theme;
use crate::app::ui::widgets::show_selectable_list;
use crate::i18n::I18n;
use crate::{App, AppCommand, AppState, InputCommand, Locale};
use anyhow::Result;

const CONTINUE_INDEX: usize = Locale::ALL.len();
const ROW_COUNT: usize = CONTINUE_INDEX + 1;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Command {
    ActivateSelected,
}

pub(crate) fn show(ctx: &egui::Context, app: &App, commands: &mut Vec<AppCommand>) {
    let AppState::LanguageSelect { selected } = &app.state else {
        return;
    };
    let selected = *selected;
    let theme = Theme::dark();
    let i18n = I18n::new(app.settings.locale);
    let mut rows: Vec<_> = Locale::ALL
        .iter()
        .copied()
        .map(|locale| {
            let marker = if locale == app.settings.locale {
                "[x] "
            } else {
                "[ ] "
            };
            (format!("{marker}{}", locale.label()), None)
        })
        .collect();
    rows.push((i18n.text("language-select-continue"), None));

    let mut frame = egui::Frame::central_panel(&ctx.style());
    frame.fill = theme.background;
    egui::CentralPanel::default().frame(frame).show(ctx, |ui| {
        show_header_row(ui, app, theme, &i18n, None);
        ui.separator();
        ui.add_space(8.0);
        ui.colored_label(theme.text, i18n.text("language-select-prompt"));
        ui.add_space(8.0);
        show_selectable_list(
            ui,
            &rows,
            selected,
            theme,
            commands,
            Some(Command::ActivateSelected.into()),
        );
    });
}

impl App {
    pub(crate) fn handle_language_select_input(&mut self, command: InputCommand) -> Result<()> {
        let AppState::LanguageSelect { selected } = &mut self.state else {
            return Ok(());
        };

        match command {
            InputCommand::MoveUp => *selected = move_prev(*selected, ROW_COUNT),
            InputCommand::MoveDown => *selected = move_next(*selected, ROW_COUNT),
            InputCommand::MoveLeft | InputCommand::MoveRight | InputCommand::Back => {}
            InputCommand::Confirm => self.activate_selected_language_row(),
        }

        Ok(())
    }

    pub(crate) fn handle_language_select_command(&mut self, command: Command) {
        match command {
            Command::ActivateSelected => self.activate_selected_language_row(),
        }
    }

    fn activate_selected_language_row(&mut self) {
        let AppState::LanguageSelect { selected } = &self.state else {
            return;
        };
        let selected = *selected;

        if let Some(locale) = Locale::ALL.get(selected).copied() {
            self.set_locale(locale);
        } else if selected == CONTINUE_INDEX {
            self.request_device_code();
        }
    }
}
