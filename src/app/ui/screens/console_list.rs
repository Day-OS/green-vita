use crate::app::command::{move_next, move_prev};
use crate::app::ui::header::show_header_row;
use crate::app::ui::theme::Theme;
use crate::app::ui::widgets::show_selectable_list;
use crate::app::{AppState, TitleImage};
use crate::i18n::I18n;
use crate::{App, AppCommand, InputCommand};
use anyhow::Result;
use std::sync::Arc;

pub(crate) fn show(ctx: &egui::Context, app: &App, commands: &mut Vec<AppCommand>) {
    let selected = match &app.state {
        AppState::ConsoleList { selected } => *selected,
        AppState::LoadingConsoles(_) => 0,
        _ => return,
    };
    let theme = Theme::dark();
    let i18n = I18n::new(app.settings.locale);
    let mut frame = egui::Frame::central_panel(&ctx.style());
    frame.fill = theme.background;
    egui::CentralPanel::default().frame(frame).show(ctx, |ui| {
        show_header_row(ui, app, theme, &i18n, Some(commands));
        ui.separator();
        let rows = console_rows(app);
        show_selectable_list(
            ui,
            &rows,
            selected,
            theme,
            commands,
            Some(InputCommand::Confirm.into()),
        );
    });
}

fn console_rows(app: &App) -> Vec<(String, Option<Arc<TitleImage>>)> {
    if app.service.consoles.is_empty() && matches!(&app.state, AppState::LoadingConsoles(_)) {
        return vec![("Requesting /v6/servers/home".to_owned(), None)];
    }
    if app.service.consoles.is_empty() {
        return vec![
            ("No consoles returned by /v6/servers/home".to_owned(), None),
            ("Check Remote Play is enabled on your Xbox".to_owned(), None),
            ("Use xHome gsToken with xHome baseUri".to_owned(), None),
            (
                "Same Microsoft account must own the console".to_owned(),
                None,
            ),
        ];
    }
    app.service
        .consoles
        .iter()
        .map(|console| {
            (
                format!(
                    "{} [{}] {}",
                    console.device_name, console.console_type, console.power_state
                ),
                None,
            )
        })
        .collect()
}

impl App {
    pub(crate) async fn handle_console_list_input(&mut self, command: InputCommand) -> Result<()> {
        let item_count = self.service.consoles.len();
        match command {
            InputCommand::MoveUp => {
                if let AppState::ConsoleList { selected } = &mut self.state {
                    *selected = move_prev(*selected, item_count);
                }
            }
            InputCommand::MoveDown => {
                if let AppState::ConsoleList { selected } = &mut self.state {
                    *selected = move_next(*selected, item_count);
                }
            }
            InputCommand::MoveLeft | InputCommand::MoveRight => {}
            InputCommand::Confirm => self.start_selected_console_stream(),
            InputCommand::Back => {
                self.set_state(AppState::ModeSelect { selected: 1 });
            }
        }

        Ok(())
    }
}
