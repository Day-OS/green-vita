use crate::app::ui::theme::Theme;
use crate::app::ui::widgets::{draw_title_image, menu_item};
use crate::i18n::I18n;
use crate::{App, AppCommand, AppState, InputCommand};
use anyhow::Result;

#[derive(Default)]
pub(crate) struct MenuState {
    pub(crate) open: bool,
    pub(crate) selected: usize,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Command {
    Toggle,
    Close,
    Select(MenuItem),
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum MenuItem {
    Settings,
    Logout,
}

pub const MENU_ITEMS: [MenuItem; 2] = [MenuItem::Settings, MenuItem::Logout];

pub(crate) fn show_header_row(
    ui: &mut egui::Ui,
    app: &App,
    theme: Theme,
    i18n: &I18n,
    menu_commands: Option<&mut Vec<AppCommand>>,
) {
    egui::Sides::new().show(
        ui,
        |ui| show_header_identity(ui, app, theme, i18n),
        |ui| {
            if let Some(commands) = menu_commands {
                show_hamburger_menu(ui, app, theme, i18n, commands);
            }
        },
    );
}

pub(crate) fn show_header_row_with_action(
    ui: &mut egui::Ui,
    app: &App,
    theme: Theme,
    i18n: &I18n,
    action_label: String,
    action_selected: bool,
) -> bool {
    let mut clicked = false;
    egui::Sides::new().show(
        ui,
        |ui| show_header_identity(ui, app, theme, i18n),
        |ui| {
            let fill = if action_selected {
                theme.accent
            } else {
                egui::Color32::from_rgb(0x26, 0x27, 0x2c)
            };
            let stroke = if action_selected {
                egui::Stroke::new(2.0, egui::Color32::WHITE)
            } else {
                egui::Stroke::NONE
            };
            let button = egui::Button::new(
                egui::RichText::new(action_label)
                    .size(16.0)
                    .color(egui::Color32::WHITE),
            )
            .fill(fill)
            .stroke(stroke);
            clicked = ui.add_sized(egui::vec2(280.0, 36.0), button).clicked();
        },
    );
    clicked
}

fn show_header_identity(ui: &mut egui::Ui, app: &App, theme: Theme, i18n: &I18n) {
    let logo_size = 36.0;
    let (logo_rect, _) =
        ui.allocate_exact_size(egui::vec2(logo_size, logo_size), egui::Sense::hover());
    draw_title_image(ui, &app.service.logo, logo_rect, "xbox-logo");
    ui.add_space(8.0);
    ui.heading(
        egui::RichText::new(i18n.screen_title(&app.state))
            .size(28.0)
            .color(theme.text_bright),
    );
}

impl MenuItem {
    fn icon(self) -> &'static str {
        match self {
            Self::Settings => "\u{2699}",
            Self::Logout => "\u{21a9}",
        }
    }

    fn label_key(self) -> &'static str {
        match self {
            Self::Settings => "menu-settings",
            Self::Logout => "menu-logout",
        }
    }
}

/// Forced-square button (so avatar/circle overlays land right) with a hand-built popup instead of `ui.menu_button`.
pub(crate) fn show_hamburger_menu(
    ui: &mut egui::Ui,
    app: &App,
    theme: Theme,
    i18n: &I18n,
    commands: &mut Vec<AppCommand>,
) {
    const BUTTON_SIZE: f32 = 36.0;

    let previous_widgets = ui.style().visuals.widgets.clone();
    {
        // egui clamps rounding to half the side, so 255 just means "fully round".
        let widgets = &mut ui.style_mut().visuals.widgets;
        let full_round = egui::CornerRadius::same(255);
        widgets.inactive.corner_radius = full_round;
        widgets.hovered.corner_radius = full_round;
        widgets.active.corner_radius = full_round;
        widgets.open.corner_radius = full_round;
    }

    let button_content = if app.service.avatar.is_some() {
        egui::RichText::new("")
    } else {
        egui::RichText::new("\u{2630}").size(20.0)
    };
    let button = egui::Button::new(button_content);
    let response = ui.add_sized(egui::vec2(BUTTON_SIZE, BUTTON_SIZE), button);

    ui.style_mut().visuals.widgets = previous_widgets;

    let popup_id = ui.make_persistent_id("hamburger_menu_popup");
    if response.clicked() {
        commands.push(Command::Toggle.into());
    }

    // `App::menu_open` is the source of truth; synced into egui's popup memory each frame.
    ui.memory_mut(|memory| {
        let currently_open = memory.is_popup_open(popup_id);
        if app.menu.open && !currently_open {
            memory.open_popup(popup_id);
        } else if !app.menu.open && currently_open {
            memory.close_popup();
        }
    });

    // `popup_below_widget` reads its frame from this `ui`, not the popup content's own.
    let previous_visuals = ui.style().visuals.clone();
    {
        let visuals = &mut ui.style_mut().visuals;
        visuals.window_fill = egui::Color32::from_rgb(0x1a, 0x1b, 0x1e);
        visuals.window_stroke = egui::Stroke::NONE;
        visuals.menu_corner_radius = egui::CornerRadius::same(10);
    }
    egui::popup_below_widget(
        ui,
        popup_id,
        &response,
        egui::PopupCloseBehavior::CloseOnClickOutside,
        |ui| {
            // Wider than the fixed 36x36 button it's anchored to, so menu text fits.
            ui.set_min_width(180.0);
            for (index, item) in MENU_ITEMS.iter().copied().enumerate() {
                if index > 0 {
                    ui.add_space(4.0);
                }
                if menu_item(
                    ui,
                    theme,
                    item.icon(),
                    &i18n.text(item.label_key()),
                    app.menu.selected == index,
                ) {
                    commands.push(Command::Select(item).into());
                }
            }
        },
    );
    ui.style_mut().visuals = previous_visuals;

    // egui may have closed the popup itself (click outside) - keep `App` in sync.
    if app.menu.open && !ui.memory(|memory| memory.is_popup_open(popup_id)) {
        commands.push(Command::Close.into());
    }

    if let Some(avatar) = app.service.avatar.as_ref() {
        draw_title_image(ui, avatar, response.rect, "avatar");
    }
    // Added after the button since this row is right-to-left.
    if app.service.gamertag.is_some() || app.service.gamerscore.is_some() {
        ui.add_space(8.0);
    }
    if let Some(gamerscore) = &app.service.gamerscore {
        ui.with_layout(egui::Layout::top_down(egui::Align::Max), |ui| {
            ui.add_space(2.0);
            if let Some(gamertag) = &app.service.gamertag {
                ui.colored_label(theme.text_bright, egui::RichText::new(gamertag).size(15.0));
            }
            ui.colored_label(
                theme.text,
                egui::RichText::new(format!("{gamerscore} G")).size(13.0),
            );
        });
    } else if let Some(gamertag) = &app.service.gamertag {
        ui.colored_label(theme.text_bright, egui::RichText::new(gamertag).size(15.0));
    }
}

impl App {
    pub(crate) fn handle_menu_input(&mut self, command: InputCommand) -> Result<()> {
        match command {
            InputCommand::MoveUp => {
                self.menu.selected =
                    crate::app::command::move_prev(self.menu.selected, MENU_ITEMS.len());
            }
            InputCommand::MoveDown => {
                self.menu.selected =
                    crate::app::command::move_next(self.menu.selected, MENU_ITEMS.len());
            }
            InputCommand::MoveLeft | InputCommand::MoveRight => {}
            InputCommand::Confirm => {
                let item = MENU_ITEMS
                    .get(self.menu.selected)
                    .copied()
                    .unwrap_or(MenuItem::Logout);
                self.activate_menu_item(item);
            }
            InputCommand::Back => {
                self.menu.open = false;
            }
        }

        Ok(())
    }

    pub(crate) fn handle_menu_command(&mut self, command: Command) -> Result<()> {
        match command {
            Command::Toggle
                if matches!(
                    &self.state,
                    AppState::TitleList { .. }
                        | AppState::LoadingTitles(_)
                        | AppState::ConsoleList { .. }
                        | AppState::LoadingConsoles(_)
                ) || matches!(&self.state, AppState::Streaming(streaming) if streaming.paused) =>
            {
                self.menu.open = !self.menu.open;
                self.menu.selected = 0;
            }
            Command::Toggle => {}
            Command::Close => {
                self.menu.open = false;
            }
            Command::Select(item) => {
                self.activate_menu_item(item);
            }
        }

        Ok(())
    }

    fn activate_menu_item(&mut self, item: MenuItem) {
        self.menu.open = false;
        match item {
            MenuItem::Settings => {
                self.open_settings();
            }
            MenuItem::Logout => {
                self.service.logout();
                self.set_state(AppState::InitializeAuthentication);
            }
        }
    }
}
