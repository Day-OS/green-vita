use crate::app::ui::header;
use crate::app::ui::screens::{language_select, paused_overlay, settings, title_list};
use crate::{App, AppState};
use anyhow::Result;

#[derive(Clone, PartialEq)]
pub enum AppCommand {
    Input(InputCommand),
    Menu(header::Command),
    Navigate(NavigationCommand),
    Screen(ScreenCommand),
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum InputCommand {
    MoveUp,
    MoveDown,
    MoveLeft,
    MoveRight,
    Confirm,
    Back,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum NavigationCommand {
    OpenPauseOverlay,
}

#[derive(Clone, PartialEq)]
pub enum ScreenCommand {
    LanguageSelect(language_select::Command),
    PausedOverlay(paused_overlay::Command),
    Settings(settings::Command),
    TitleList(title_list::Command),
}

impl From<InputCommand> for AppCommand {
    fn from(command: InputCommand) -> Self {
        Self::Input(command)
    }
}

impl From<header::Command> for AppCommand {
    fn from(command: header::Command) -> Self {
        Self::Menu(command)
    }
}

impl From<NavigationCommand> for AppCommand {
    fn from(command: NavigationCommand) -> Self {
        Self::Navigate(command)
    }
}

impl From<ScreenCommand> for AppCommand {
    fn from(command: ScreenCommand) -> Self {
        Self::Screen(command)
    }
}

impl From<paused_overlay::Command> for AppCommand {
    fn from(command: paused_overlay::Command) -> Self {
        Self::Screen(ScreenCommand::PausedOverlay(command))
    }
}

impl From<language_select::Command> for AppCommand {
    fn from(command: language_select::Command) -> Self {
        Self::Screen(ScreenCommand::LanguageSelect(command))
    }
}

impl From<settings::Command> for AppCommand {
    fn from(command: settings::Command) -> Self {
        Self::Screen(ScreenCommand::Settings(command))
    }
}

impl From<title_list::Command> for AppCommand {
    fn from(command: title_list::Command) -> Self {
        Self::Screen(ScreenCommand::TitleList(command))
    }
}

impl App {
    pub async fn handle_command(&mut self, command: AppCommand) -> Result<()> {
        match command {
            AppCommand::Input(command) => self.handle_input_command(command).await?,
            AppCommand::Menu(command) => self.handle_menu_command(command)?,
            AppCommand::Navigate(command) => self.handle_navigation_command(command)?,
            AppCommand::Screen(command) => self.handle_screen_command(command).await?,
        }

        Ok(())
    }

    async fn handle_input_command(&mut self, command: InputCommand) -> Result<()> {
        if self.menu.open {
            return self.handle_menu_input(command);
        }

        match &self.state {
            AppState::LanguageSelect { .. } => self.handle_language_select_input(command),
            AppState::StartingStream { .. } | AppState::Connecting { .. } => {
                self.handle_connecting_input(command).await
            }
            AppState::ConsoleList { .. } | AppState::LoadingConsoles(_) => {
                self.handle_console_list_input(command).await
            }
            AppState::ModeSelect { .. } => self.handle_mode_select_input(command),
            AppState::Streaming(streaming) if streaming.paused => {
                self.handle_paused_overlay_input(command).await
            }
            AppState::Settings { .. } => self.handle_settings_input(command),
            AppState::TitleList { .. } | AppState::LoadingTitles(_) => {
                self.handle_title_list_input(command).await
            }
            AppState::Error { retry_sign_in, .. } => {
                if command == InputCommand::Back {
                    if *retry_sign_in {
                        self.set_state(AppState::InitializeAuthentication);
                    } else {
                        self.set_state(AppState::ModeSelect { selected: 0 });
                    }
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }

    async fn handle_screen_command(&mut self, command: ScreenCommand) -> Result<()> {
        match command {
            ScreenCommand::LanguageSelect(command) => {
                self.handle_language_select_command(command);
            }
            ScreenCommand::PausedOverlay(command) => {
                self.handle_paused_overlay_command(command).await?;
            }
            ScreenCommand::Settings(command) => {
                self.handle_settings_command(command)?;
            }
            ScreenCommand::TitleList(command) => {
                self.handle_title_list_command(command)?;
            }
        }

        Ok(())
    }

    fn handle_navigation_command(&mut self, command: NavigationCommand) -> Result<()> {
        match command {
            NavigationCommand::OpenPauseOverlay if matches!(&self.state, AppState::Streaming(streaming) if !streaming.paused) =>
            {
                if let AppState::Streaming(streaming) = &mut self.state {
                    streaming.pause_selected = 0;
                    streaming.set_paused(true);
                }
                self.menu.open = false;
            }
            NavigationCommand::OpenPauseOverlay => {}
        }

        Ok(())
    }

    /// Opens Settings scoped to the relevant game (highlighted or actively streaming, if any),
    /// remembering the current screen to return to.
    pub(crate) fn open_settings(&mut self) {
        let selected = match &self.state {
            AppState::TitleList { selected }
            | AppState::ConsoleList { selected }
            | AppState::ModeSelect { selected } => *selected,
            _ => 0,
        };
        let title_id = match &self.state {
            AppState::TitleList { .. } | AppState::LoadingTitles(_) => self
                .service
                .titles
                .get(selected)
                .map(|title| title.id.clone()),
            AppState::StartingStream { .. }
            | AppState::Connecting { .. }
            | AppState::Streaming(_) => self.state.active_title_id().map(str::to_owned),
            _ => None,
        };
        let return_to = std::mem::replace(&mut self.state, AppState::ModeSelect { selected: 0 });
        self.set_state(AppState::Settings {
            return_to: Box::new(return_to),
            selected: 0,
            title_id,
            locale_expanded: false,
        });
    }
}

/// One step back through a `count`-item list, wrapping from the first item to the last.
pub(super) fn move_prev(index: usize, count: usize) -> usize {
    if count == 0 {
        0
    } else {
        (index + count - 1) % count
    }
}

/// One step forward through a `count`-item list, wrapping from the last item to the first.
pub(super) fn move_next(index: usize, count: usize) -> usize {
    if count == 0 { 0 } else { (index + 1) % count }
}
