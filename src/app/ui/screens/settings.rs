use crate::app::InputCommand;
use crate::app::command::{move_next, move_prev};
use crate::app::ui::header::show_header_row;
use crate::app::ui::theme::Theme;
use crate::app::ui::widgets::smooth_scroll_area;
use crate::i18n::{I18n, arg_string};
use crate::settings::{ColorProfile, H264Profile, RepairWindow, StreamDebugMode, parse_region_bypass_ip};
use crate::{App, AppCommand, AppState, Locale};
use anyhow::Result;
use fluent_bundle::FluentArgs;

/// Build identity compiled in by `build.rs` (see scripts/docker-build.sh, which
/// passes the host's git sha into the container).
const APP_VERSION: &str = env!("CARGO_PKG_VERSION");
const GIT_COMMIT: &str = env!("GIT_SHA");
const BUILD_TIMESTAMP: &str = env!("BUILD_TIMESTAMP");

#[derive(Clone, PartialEq)]
pub enum Command {
    ToggleLocaleExpanded,
    SetLocale(Locale),
    SetSwapShouldersAndTriggers { title_id: String, enabled: bool },
    SetSwapShouldersTriggers(bool),
    SetRearTouchEnabled { title_id: String, enabled: bool },
    SetFrontTouchAuxiliaryButtons { title_id: String, enabled: bool },
    SetStreamDebugMode(StreamDebugMode),
    SetVideoBitrateCap(u32),
    SetVideoH264Profile(H264Profile),
    SetDamagePolicy(crate::settings::DamagePolicy),
    SetRepairWindow(crate::settings::RepairWindow),
    SetLatencyProfile(crate::settings::LatencyProfile),
    SetColorProfile(crate::settings::ColorProfile),
    SetDirectPresent(bool),
    SetLatencyTestMode(bool),
    SetPsButtonCapture(bool),
    SetRegionBypassIp(Option<String>),
    EnterRegionBypassIp,
}

#[derive(Clone)]
enum SettingsRow {
    LocaleToggle,
    LocaleOption(Locale),
    GameSwap { title_id: String, enabled: bool },
    GameRearTouch { title_id: String, enabled: bool },
    GameFrontTouchAuxiliary { title_id: String, enabled: bool },
    SwapShouldersTriggers(bool),
    StreamDebug(StreamDebugMode),
    VideoBitrateCap(u32),
    VideoH264Profile(H264Profile),
    DamagePolicy(crate::settings::DamagePolicy),
    RepairWindow(crate::settings::RepairWindow),
    LatencyProfile(crate::settings::LatencyProfile),
    ColorProfile(crate::settings::ColorProfile),
    DirectPresent(bool),
    LatencyTest(bool),
    PsButtonCapture(bool),
    RegionBypass,
    RegionBypassEnterIp,
    Back,
}

fn settings_rows(app: &App) -> Vec<SettingsRow> {
    let AppState::Settings {
        title_id,
        locale_expanded,
        ..
    } = &app.state
    else {
        return Vec::new();
    };
    let mut rows = Vec::new();
    rows.push(SettingsRow::LocaleToggle);
    if *locale_expanded {
        rows.extend(Locale::ALL.iter().copied().map(SettingsRow::LocaleOption));
    }
    if let Some(title_id) = title_id.clone() {
        let enabled = app
            .settings
            .game_profile(&title_id)
            .is_some_and(|profile| profile.swap_shoulders_and_triggers);
        rows.push(SettingsRow::GameSwap {
            title_id: title_id.clone(),
            enabled,
        });
        let enabled = app
            .settings
            .game_profile(&title_id)
            .is_none_or(|profile| profile.rear_touch_enabled);
        rows.push(SettingsRow::GameRearTouch {
            title_id: title_id.clone(),
            enabled,
        });
        let enabled = app
            .settings
            .game_profile(&title_id)
            .is_some_and(|profile| profile.front_touch_auxiliary_buttons);
        rows.push(SettingsRow::GameFrontTouchAuxiliary { title_id, enabled });
    }
    // Global swap-shoulders toggle (for xHome, no title_id selected).
    // Per-game GameSwap above applies when a specific title is selected.
    if title_id.is_none() {
        rows.push(SettingsRow::SwapShouldersTriggers(app.settings.swap_shoulders_and_triggers));
    }
    rows.push(SettingsRow::PsButtonCapture(app.settings.ps_button_capture));
    rows.push(SettingsRow::VideoBitrateCap(app.settings.video_bitrate_kbps));
    rows.push(SettingsRow::VideoH264Profile(app.settings.video_h264_profile));
    rows.push(SettingsRow::ColorProfile(app.settings.color_profile));
    rows.push(SettingsRow::LatencyProfile(app.settings.latency_profile));
    rows.push(SettingsRow::DamagePolicy(app.settings.damage_policy));
    rows.push(SettingsRow::RepairWindow(app.settings.repair_window));
    rows.push(SettingsRow::DirectPresent(app.settings.direct_present));
    rows.push(SettingsRow::StreamDebug(app.settings.stream_debug_mode));
    rows.push(SettingsRow::LatencyTest(app.settings.latency_test_mode));
    // 0.4.0 networking settings stay last (user request).
    if title_id.is_none() {
        rows.push(SettingsRow::RegionBypass);
        rows.push(SettingsRow::RegionBypassEnterIp);
    }
    rows.push(SettingsRow::Back);
    rows
}

/// Editable settings, persisted immediately on each change. Every interactive row is a
/// `SelectableLabel` (`focus_row`) addressed by one flat index, so
/// D-pad + Confirm can reach every row, not just touch.
pub(crate) fn show(ctx: &egui::Context, app: &App, commands: &mut Vec<AppCommand>) {
    let theme = Theme::dark();
    let i18n = I18n::new(app.settings.locale);
    let AppState::Settings {
        title_id,
        locale_expanded,
        selected,
        ..
    } = &app.state
    else {
        return;
    };
    let title_id = title_id.clone();
    let locale_expanded = *locale_expanded;
    let selected_index = *selected;
    let mut frame = egui::Frame::central_panel(&ctx.style());
    frame.fill = theme.background;
    egui::CentralPanel::default().frame(frame).show(ctx, |ui| {
        show_header_row(ui, app, theme, &i18n, None);
        ui.separator();
        let locale = app.settings.locale;
        let mut row_index = 0;

        ui.visuals_mut().selection.bg_fill = theme.accent;
        ui.visuals_mut().selection.stroke.color = egui::Color32::WHITE;

        smooth_scroll_area(ui, "settings_scroll", |ui| {
            ui.heading(egui::RichText::new(i18n.text("settings-locale")).color(theme.text_bright));
            ui.vertical(|ui| {
                let toggle_label = format!(
                    "{}  {}",
                    locale.label(),
                    if locale_expanded { "v" } else { ">" }
                );
                if focus_row(ui, selected_index == row_index, toggle_label) {
                    commands.push(Command::ToggleLocaleExpanded.into());
                }
                row_index += 1;

                if locale_expanded {
                    for option in Locale::ALL.iter().copied() {
                        let mut text = egui::RichText::new(option.label());
                        if locale == option {
                            text = text.color(theme.accent).strong();
                        }
                        if focus_row(ui, selected_index == row_index, text) {
                            commands.push(Command::SetLocale(option).into());
                        }
                        row_index += 1;
                    }
                }

                if let Some(title_id) = title_id.clone() {
                    let swap_shoulders_and_triggers = app
                        .settings
                        .game_profile(&title_id)
                        .is_some_and(|profile| profile.swap_shoulders_and_triggers);

                    ui.add_space(14.0);
                    ui.separator();
                    ui.heading(
                        egui::RichText::new(i18n.text("settings-game")).color(theme.text_bright),
                    );
                    ui.colored_label(theme.text_bright, app.service.title_name_or_id(&title_id));

                    if checkbox_row(
                        ui,
                        selected_index == row_index,
                        swap_shoulders_and_triggers,
                        i18n.text("settings-swap-shoulders-triggers"),
                    ) {
                        commands.push(
                            Command::SetSwapShouldersAndTriggers {
                                title_id: title_id.clone(),
                                enabled: !swap_shoulders_and_triggers,
                            }
                            .into(),
                        );
                    }
                    row_index += 1;

                    let rear_touch_enabled = app
                        .settings
                        .game_profile(&title_id)
                        .is_none_or(|profile| profile.rear_touch_enabled);
                    if checkbox_row(
                        ui,
                        selected_index == row_index,
                        rear_touch_enabled,
                        i18n.text("settings-rear-touch-enabled"),
                    ) {
                        commands.push(
                            Command::SetRearTouchEnabled {
                                title_id: title_id.clone(),
                                enabled: !rear_touch_enabled,
                            }
                            .into(),
                        );
                    }
                    row_index += 1;

                    let front_touch_auxiliary_buttons = app
                        .settings
                        .game_profile(&title_id)
                        .is_some_and(|profile| profile.front_touch_auxiliary_buttons);
                    if checkbox_row(
                        ui,
                        selected_index == row_index,
                        front_touch_auxiliary_buttons,
                        i18n.text("settings-front-touch-auxiliary-buttons"),
                    ) {
                        commands.push(
                            Command::SetFrontTouchAuxiliaryButtons {
                                title_id,
                                enabled: !front_touch_auxiliary_buttons,
                            }
                            .into(),
                        );
                    }
                    row_index += 1;
                }

                // Global swap-shoulders toggle (all games, no title selected).
                if title_id.is_none() {
                    let global_enabled = app.settings.swap_shoulders_and_triggers;
                    if checkbox_row(
                        ui,
                        selected_index == row_index,
                        global_enabled,
                        i18n.text("settings-swap-shoulders-global"),
                    ) {
                        commands.push(Command::SetSwapShouldersTriggers(!global_enabled).into());
                    }
                    row_index += 1;
                }

                ui.add_space(14.0);
                ui.separator();
                ui.colored_label(
                    theme.text,
                    host_text(
                        &i18n,
                        "settings-cloud-host",
                        &app.service.api.config.cloud.host,
                    ),
                );
                ui.colored_label(
                    theme.text,
                    host_text(
                        &i18n,
                        "settings-home-host",
                        &app.service.api.config.home.host,
                    ),
                );

                ui.add_space(14.0);
                ui.separator();
                ui.heading(
                    egui::RichText::new(i18n.text("settings-instream-controls"))
                        .color(theme.text_bright),
                );
                if checkbox_row(
                    ui,
                    selected_index == row_index,
                    app.settings.ps_button_capture,
                    i18n.text("settings-ps-button-capture"),
                ) {
                    commands.push(
                        Command::SetPsButtonCapture(!app.settings.ps_button_capture).into(),
                    );
                }
                row_index += 1;

                ui.add_space(14.0);
                ui.separator();
                {
                    let mut args = FluentArgs::new();
                    let label = if app.settings.video_bitrate_kbps == 0 {
                        i18n.text("settings-video-bitrate-nocap")
                    } else {
                        args.set(
                            "kbps",
                            arg_string(app.settings.video_bitrate_kbps.to_string()),
                        );
                        i18n.text_with("settings-video-bitrate-cap", args)
                    };
                    if focus_row(ui, selected_index == row_index, label) {
                        let next = next_bitrate_preset(app.settings.video_bitrate_kbps);
                        commands.push(Command::SetVideoBitrateCap(next).into());
                    }
                }
                row_index += 1;

                ui.add_space(14.0);
                ui.separator();
                {
                    let label = format!("H264 profile: {}", match app.settings.video_h264_profile {
                        H264Profile::Baseline => "Baseline",
                        H264Profile::Main => "Main",
                    });
                    if focus_row(ui, selected_index == row_index, &label) {
                        let next = match app.settings.video_h264_profile {
                            H264Profile::Baseline => H264Profile::Main,
                            H264Profile::Main => H264Profile::Baseline,
                        };
                        commands.push(Command::SetVideoH264Profile(next).into());
                    }
                }
                row_index += 1;

                ui.add_space(14.0);
                ui.separator();
                {
                    let mut args = FluentArgs::new();
                    args.set(
                        "mode",
                        arg_string(match app.settings.color_profile {
                            ColorProfile::Rgb565 => "RGB565",
                            ColorProfile::Rgba8888 => "RGBA8888",
                        }),
                    );
                    let label = i18n.text_with("settings-color-profile", args);
                    if focus_row(ui, selected_index == row_index, label) {
                        let next = match app.settings.color_profile {
                            ColorProfile::Rgb565 => ColorProfile::Rgba8888,
                            ColorProfile::Rgba8888 => ColorProfile::Rgb565,
                        };
                        commands.push(Command::SetColorProfile(next).into());
                    }
                }
                row_index += 1;

                ui.add_space(14.0);
                ui.separator();
                {
                    let label = format!(
                        "Latency profile: {}",
                        match app.settings.latency_profile {
                            crate::settings::LatencyProfile::Fast => "Fast",
                            crate::settings::LatencyProfile::Smooth => "Smooth",
                        }
                    );
                    if focus_row(ui, selected_index == row_index, label) {
                        let next = match app.settings.latency_profile {
                            crate::settings::LatencyProfile::Fast => {
                                crate::settings::LatencyProfile::Smooth
                            }
                            crate::settings::LatencyProfile::Smooth => {
                                crate::settings::LatencyProfile::Fast
                            }
                        };
                        commands.push(Command::SetLatencyProfile(next).into());
                    }
                }
                row_index += 1;

                ui.add_space(14.0);
                ui.separator();
                {
                    let label = format!(
                        "Damage policy: {}",
                        match app.settings.damage_policy {
                            crate::settings::DamagePolicy::LowLatency => "Low latency",
                            crate::settings::DamagePolicy::Balanced => "Balanced",
                            crate::settings::DamagePolicy::Quality => "Quality",
                        }
                    );
                    if focus_row(ui, selected_index == row_index, &label) {
                        let next = match app.settings.damage_policy {
                            crate::settings::DamagePolicy::LowLatency => {
                                crate::settings::DamagePolicy::Balanced
                            }
                            crate::settings::DamagePolicy::Balanced => {
                                crate::settings::DamagePolicy::Quality
                            }
                            crate::settings::DamagePolicy::Quality => {
                                crate::settings::DamagePolicy::LowLatency
                            }
                        };
                        commands.push(Command::SetDamagePolicy(next).into());
                    }
                }
                row_index += 1;

                ui.add_space(14.0);
                ui.separator();
                {
                    let label = format!(
                        "Repair window: {}",
                        match app.settings.repair_window {
                            crate::settings::RepairWindow::Off => "Off",
                            crate::settings::RepairWindow::Short => "Short",
                            crate::settings::RepairWindow::Balanced => "Balanced",
                            crate::settings::RepairWindow::Patient => "Patient",
                        }
                    );
                    if focus_row(ui, selected_index == row_index, &label) {
                        let next = match app.settings.repair_window {
                            crate::settings::RepairWindow::Off => {
                                crate::settings::RepairWindow::Short
                            }
                            crate::settings::RepairWindow::Short => {
                                crate::settings::RepairWindow::Balanced
                            }
                            crate::settings::RepairWindow::Balanced => {
                                crate::settings::RepairWindow::Patient
                            }
                            crate::settings::RepairWindow::Patient => {
                                crate::settings::RepairWindow::Off
                            }
                        };
                        commands.push(Command::SetRepairWindow(next).into());
                    }
                }
                row_index += 1;

                ui.add_space(14.0);
                ui.separator();
                if checkbox_row(
                    ui,
                    selected_index == row_index,
                    app.settings.direct_present,
                    i18n.text("settings-direct-present"),
                ) {
                    commands
                        .push(Command::SetDirectPresent(!app.settings.direct_present).into());
                }
                row_index += 1;

                ui.add_space(14.0);
                ui.separator();
                {
                    let label = format!(
                        "Debug: {}",
                        match app.settings.stream_debug_mode {
                            StreamDebugMode::Off => "Off",
                            StreamDebugMode::Minimal => "FPS+Bitrate",
                            StreamDebugMode::Full => "Full",
                        }
                    );
                    if focus_row(ui, selected_index == row_index, label) {
                        let next = match app.settings.stream_debug_mode {
                            StreamDebugMode::Off => StreamDebugMode::Minimal,
                            StreamDebugMode::Minimal => StreamDebugMode::Full,
                            StreamDebugMode::Full => StreamDebugMode::Off,
                        };
                        commands.push(Command::SetStreamDebugMode(next).into());
                    }
                }
                row_index += 1;

                if checkbox_row(
                    ui,
                    selected_index == row_index,
                    app.settings.latency_test_mode,
                    i18n.text("settings-latency-test"),
                ) {
                    commands.push(
                        Command::SetLatencyTestMode(!app.settings.latency_test_mode).into(),
                    );
                }
                row_index += 1;

                // 0.4.0 networking settings (last, per user request).
                if title_id.is_none() {
                    ui.add_space(14.0);
                    ui.separator();
                    let label = match app.settings.active_region_bypass_ip().as_deref() {
                        Some(ip) => {
                            let mut args = FluentArgs::new();
                            args.set("ip", arg_string(ip));
                            i18n.text_with("settings-region-bypass-ip", args)
                        }
                        None => i18n.text("settings-region-bypass-off"),
                    };
                    if focus_row(ui, selected_index == row_index, label) {
                        commands.push(
                            Command::SetRegionBypassIp(app.settings.next_region_bypass_choice())
                                .into(),
                        );
                    }
                    row_index += 1;

                    if focus_row(
                        ui,
                        selected_index == row_index,
                        i18n.text("settings-region-bypass-enter-ip"),
                    ) {
                        commands.push(Command::EnterRegionBypassIp.into());
                    }
                    row_index += 1;

                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(i18n.text("settings-region-bypass-warning"))
                                .size(14.0)
                                .color(theme.text),
                        )
                        .wrap(),
                    );
                    if app.region_bypass_changed {
                        ui.add(
                            egui::Label::new(
                                egui::RichText::new(
                                    i18n.text("settings-region-bypass-pending"),
                                )
                                .size(14.0)
                                .color(theme.accent),
                            )
                            .wrap(),
                        );
                    }
                }

                // Descriptions (read-only, at the bottom).
                ui.add_space(14.0);
                ui.separator();
                ui.heading(
                    egui::RichText::new(i18n.text("settings-help")).color(theme.text_bright),
                );
                for key in [
                    "settings-swap-shoulders-triggers",
                    "settings-ps-button-capture-sub",
                    "settings-bitrate",
                    "settings-color-profile",
                    "settings-direct-present-sub",
                    "settings-latency-profile",
                    "settings-damage-policy",
                    "settings-repair-window",
                    "settings-debug-modes",
                    "settings-latency-test",
                ] {
                    ui.colored_label(
                        theme.text,
                        egui::RichText::new(i18n.text(key)).size(14.0),
                    );
                    ui.add_space(6.0);
                }

                ui.add_space(14.0);
                ui.separator();

                // Build identity (non-interactive): version | short commit | build date.
                let version_text = format!(
                    "v{}  |  {}  |  {}",
                    APP_VERSION,
                    &GIT_COMMIT[..8.min(GIT_COMMIT.len())],
                    BUILD_TIMESTAMP
                );
                ui.add_sized(
                    egui::vec2(ui.available_width(), 28.0),
                    egui::Label::new(egui::RichText::new(version_text).size(14.0)),
                );

                ui.add_space(14.0);
                ui.separator();
                if focus_row(ui, selected_index == row_index, i18n.text("action-back")) {
                    commands.push(InputCommand::Back.into());
                }
            });
        });
    });
}

fn focus_row(ui: &mut egui::Ui, selected: bool, text: impl Into<egui::WidgetText>) -> bool {
    ui.add_sized(
        egui::vec2(ui.available_width(), 28.0),
        egui::SelectableLabel::new(selected, text),
    )
    .clicked()
}

/// A full-width focusable row with a drawn checkbox and its label side by side.
fn checkbox_row(ui: &mut egui::Ui, selected: bool, checked: bool, label: String) -> bool {
    const ROW_HEIGHT: f32 = 32.0;
    const BOX_SIZE: f32 = 18.0;

    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), ROW_HEIGHT),
        egui::Sense::click(),
    );

    if ui.is_rect_visible(rect) {
        let selection = ui.visuals().selection;
        let row_fill = if selected {
            selection.bg_fill
        } else if response.hovered() {
            ui.visuals().widgets.hovered.bg_fill
        } else {
            egui::Color32::TRANSPARENT
        };
        ui.painter().rect_filled(rect, 4.0, row_fill);

        let box_rect = egui::Rect::from_center_size(
            egui::pos2(rect.left() + 8.0 + BOX_SIZE / 2.0, rect.center().y),
            egui::vec2(BOX_SIZE, BOX_SIZE),
        );
        let accent = selection.bg_fill;
        let box_fill = if checked {
            if selected {
                egui::Color32::WHITE
            } else {
                accent
            }
        } else {
            egui::Color32::TRANSPARENT
        };
        let outline = if selected {
            egui::Color32::WHITE
        } else {
            ui.visuals().widgets.inactive.fg_stroke.color
        };
        ui.painter().rect_filled(box_rect, 3.0, box_fill);
        ui.painter().rect_stroke(
            box_rect,
            3.0,
            egui::Stroke::new(1.5, outline),
            egui::StrokeKind::Inside,
        );

        if checked {
            let check_color = if selected {
                accent
            } else {
                egui::Color32::WHITE
            };
            let check_stroke = egui::Stroke::new(2.5, check_color);
            let first = egui::pos2(box_rect.left() + 3.5, box_rect.center().y);
            let middle = egui::pos2(box_rect.left() + 7.5, box_rect.bottom() - 4.0);
            let last = egui::pos2(box_rect.right() - 3.0, box_rect.top() + 4.0);
            ui.painter().line_segment([first, middle], check_stroke);
            ui.painter().line_segment([middle, last], check_stroke);
        }

        let text_color = if selected {
            egui::Color32::WHITE
        } else {
            ui.visuals().text_color()
        };
        ui.painter().text(
            egui::pos2(box_rect.right() + 10.0, rect.center().y),
            egui::Align2::LEFT_CENTER,
            label,
            egui::FontId::proportional(16.0),
            text_color,
        );
    }

    response.clicked()
}

fn host_text(i18n: &I18n, id: &'static str, host: &str) -> String {
    let mut args = FluentArgs::new();
    args.set("host", arg_string(host));
    i18n.text_with(id, args)
}

// Preset ladder for the bitrate cap: 0 = no cap, then 500 kbps steps to 5 Mbps,
/// then 1000 kbps steps to 8 Mbps.
/// Order: 0, 500, 1000, 1500, 2000, 2500, 3000, 3500, 4000, 4500, 5000, 6000, 7000, 8000.
const BITRATE_PRESETS_KBPS: &[u32] = &[
    0, 500, 1_000, 1_500, 2_000, 2_500, 3_000, 3_500, 4_000, 4_500, 5_000, 6_000, 7_000, 8_000,
];

fn next_bitrate_preset(current: u32) -> u32 {
    // Walk the ladder in order and wrap: after 8 Mbps comes 0 (= no cap).
    // A value from an older settings file that is not on the ladder lands on the first preset.
    match BITRATE_PRESETS_KBPS.iter().position(|&p| p == current) {
        Some(index) if index + 1 < BITRATE_PRESETS_KBPS.len() => BITRATE_PRESETS_KBPS[index + 1],
        _ => BITRATE_PRESETS_KBPS[0],
    }
}

impl App {
    pub(crate) fn handle_settings_input(&mut self, command: InputCommand) -> Result<()> {
        let rows = settings_rows(self);
        match command {
            InputCommand::MoveUp => {
                if let AppState::Settings { selected, .. } = &mut self.state {
                    *selected = move_prev(*selected, rows.len());
                }
            }
            InputCommand::MoveDown => {
                if let AppState::Settings { selected, .. } = &mut self.state {
                    *selected = move_next(*selected, rows.len());
                }
            }
            InputCommand::MoveLeft | InputCommand::MoveRight => {}
            InputCommand::Confirm => self.confirm_settings_row(&rows)?,
            InputCommand::Back => self.leave_settings(),
        }

        Ok(())
    }

    /// Activates the selected row from D-pad input.
    fn confirm_settings_row(&mut self, rows: &[SettingsRow]) -> Result<()> {
        let AppState::Settings { selected, .. } = &self.state else {
            return Ok(());
        };
        let Some(row) = rows.get(*selected) else {
            self.leave_settings();
            return Ok(());
        };

        match row {
            SettingsRow::LocaleToggle => {
                return self.handle_settings_command(Command::ToggleLocaleExpanded);
            }
            SettingsRow::LocaleOption(locale) => {
                return self.handle_settings_command(Command::SetLocale(*locale));
            }
            SettingsRow::GameSwap { title_id, enabled } => {
                return self.handle_settings_command(Command::SetSwapShouldersAndTriggers {
                    title_id: title_id.clone(),
                    enabled: !enabled,
                });
            }
            SettingsRow::SwapShouldersTriggers(enabled) => {
                return self.handle_settings_command(Command::SetSwapShouldersTriggers(!enabled));
            }
            SettingsRow::GameRearTouch { title_id, enabled } => {
                return self.handle_settings_command(Command::SetRearTouchEnabled {
                    title_id: title_id.clone(),
                    enabled: !enabled,
                });
            }
            SettingsRow::GameFrontTouchAuxiliary { title_id, enabled } => {
                return self.handle_settings_command(Command::SetFrontTouchAuxiliaryButtons {
                    title_id: title_id.clone(),
                    enabled: !enabled,
                });
            }
            SettingsRow::StreamDebug(mode) => {
                return self.handle_settings_command(Command::SetStreamDebugMode(*mode));
            }
            SettingsRow::LatencyTest(enabled) => {
                return self.handle_settings_command(Command::SetLatencyTestMode(!enabled));
            }
            SettingsRow::RegionBypass => {
                let next = self.settings.next_region_bypass_choice();
                return self.handle_settings_command(Command::SetRegionBypassIp(next));
            }
            SettingsRow::RegionBypassEnterIp => {
                return self.handle_settings_command(Command::EnterRegionBypassIp);
            }
            SettingsRow::VideoBitrateCap(current) => {
                return self.handle_settings_command(Command::SetVideoBitrateCap(
                    next_bitrate_preset(*current),
                ));
            }
            SettingsRow::VideoH264Profile(profile) => {
                let next = match profile {
                    H264Profile::Baseline => H264Profile::Main,
                    H264Profile::Main => H264Profile::Baseline,
                };
                return self.handle_settings_command(Command::SetVideoH264Profile(next));
            }
            SettingsRow::DamagePolicy(policy) => {
                let next = match policy {
                    crate::settings::DamagePolicy::LowLatency => {
                        crate::settings::DamagePolicy::Balanced
                    }
                    crate::settings::DamagePolicy::Balanced => {
                        crate::settings::DamagePolicy::Quality
                    }
                    crate::settings::DamagePolicy::Quality => {
                        crate::settings::DamagePolicy::LowLatency
                    }
                };
                return self.handle_settings_command(Command::SetDamagePolicy(next));
            }
            SettingsRow::RepairWindow(policy) => {
                let next = match policy {
                    crate::settings::RepairWindow::Off => {
                        crate::settings::RepairWindow::Short
                    }
                    crate::settings::RepairWindow::Short => {
                        crate::settings::RepairWindow::Balanced
                    }
                    crate::settings::RepairWindow::Balanced => {
                        crate::settings::RepairWindow::Patient
                    }
                    crate::settings::RepairWindow::Patient => {
                        crate::settings::RepairWindow::Off
                    }
                };
                return self.handle_settings_command(Command::SetRepairWindow(next));
            }
            SettingsRow::LatencyProfile(profile) => {
                let next = match profile {
                    crate::settings::LatencyProfile::Fast => {
                        crate::settings::LatencyProfile::Smooth
                    }
                    crate::settings::LatencyProfile::Smooth => {
                        crate::settings::LatencyProfile::Fast
                    }
                };
                return self.handle_settings_command(Command::SetLatencyProfile(next));
            }
            SettingsRow::ColorProfile(profile) => {
                let next = match profile {
                    ColorProfile::Rgb565 => ColorProfile::Rgba8888,
                    ColorProfile::Rgba8888 => ColorProfile::Rgb565,
                };
                return self.handle_settings_command(Command::SetColorProfile(next));
            }
            SettingsRow::DirectPresent(enabled) => {
                return self.handle_settings_command(Command::SetDirectPresent(!enabled));
            }
            SettingsRow::PsButtonCapture(enabled) => {
                return self.handle_settings_command(Command::SetPsButtonCapture(!enabled));
            }
            SettingsRow::Back => {}
        }

        self.leave_settings();
        Ok(())
    }

    fn leave_settings(&mut self) {
        let state = std::mem::replace(&mut self.state, AppState::TitleList { selected: 0 });
        if let AppState::Settings { return_to, .. } = state {
            // streaming tokens are issued at sign-in
            let safe_to_refresh = matches!(
                *return_to,
                AppState::ModeSelect { .. }
                    | AppState::TitleList { .. }
                    | AppState::ConsoleList { .. }
                    | AppState::Error {
                        retry_sign_in: true,
                        ..
                    }
            );
            if self.region_bypass_changed && safe_to_refresh && self.service.auth.has_saved_login()
            {
                self.load_credentials();
            } else {
                self.set_state(*return_to);
            }
        } else {
            self.state = state;
        }
    }

    pub(crate) fn handle_settings_command(&mut self, command: Command) -> Result<()> {
        match command {
            Command::ToggleLocaleExpanded => {
                let expanded = matches!(
                    &self.state,
                    AppState::Settings {
                        locale_expanded: false,
                        ..
                    }
                );
                let selected = if expanded {
                    let current = Locale::ALL
                        .iter()
                        .position(|&locale| locale == self.settings.locale)
                        .unwrap_or(0);
                    1 + current
                } else {
                    0
                };
                if let AppState::Settings {
                    locale_expanded,
                    selected: current,
                    ..
                } = &mut self.state
                {
                    *locale_expanded = expanded;
                    *current = selected;
                }
            }
            Command::SetLocale(locale) => {
                self.set_locale(locale);
                if let AppState::Settings {
                    locale_expanded,
                    selected,
                    ..
                } = &mut self.state
                {
                    *locale_expanded = false;
                    *selected = 0;
                }
            }
            Command::SetSwapShouldersAndTriggers { title_id, enabled } => {
                self.settings
                    .set_swap_shoulders_and_triggers(title_id, enabled);
                self.settings.save();
            }
            Command::SetSwapShouldersTriggers(enabled) => {
                self.settings.swap_shoulders_and_triggers = enabled;
                self.settings.save();
            }
            Command::SetRearTouchEnabled { title_id, enabled } => {
                self.settings.set_rear_touch_enabled(title_id, enabled);
                self.settings.save();
            }
            Command::SetFrontTouchAuxiliaryButtons { title_id, enabled } => {
                self.settings
                    .set_front_touch_auxiliary_buttons(title_id, enabled);
                self.settings.save();
            }
            Command::SetStreamDebugMode(mode) => {
                self.settings.stream_debug_mode = mode;
                self.settings.save();
            }
            Command::SetLatencyTestMode(enabled) => {
                self.settings.latency_test_mode = enabled;
                self.settings.save();
            }
            Command::SetVideoBitrateCap(kbps) => {
                // 0 = no cap: keep it, do not clamp up to a minimum.
                // Clamp to the nearest valid ladder entry for non-zero values.
                self.settings.video_bitrate_kbps = if kbps == 0 {
                    0
                } else {
                    let mut v = kbps;
                    // Find the nearest valid entry: round to the closest preset.
                    if let Some(&nearest) = BITRATE_PRESETS_KBPS.iter().min_by_key(|&&p| (p as i32 - kbps as i32).abs()) {
                        v = nearest;
                    };
                    v
                };
                self.settings.save();
            }
            Command::SetVideoH264Profile(profile) => {
                self.settings.video_h264_profile = profile;
                self.settings.save();
            }
            Command::SetDamagePolicy(policy) => {
                self.settings.damage_policy = policy;
                self.settings.save();
                // Live: the RTP Damaged arm reads this per event — applies
                // mid-stream without reconnecting (item 11).
                crate::api::streaming::rtc::rtp::DAMAGE_POLICY.store(
                    match policy {
                        crate::settings::DamagePolicy::LowLatency => 0,
                        crate::settings::DamagePolicy::Balanced => 1,
                        crate::settings::DamagePolicy::Quality => 2,
                    },
                    std::sync::atomic::Ordering::Relaxed,
                );
            }
            Command::SetRepairWindow(policy) => {
                self.settings.repair_window = policy;
                self.settings.save();
                // Live: the assembler reads this per damage event from the atomic.
            }
            Command::SetLatencyProfile(profile) => {
                self.settings.latency_profile = profile;
                self.settings.save();
                // Live: the decode-bank admission limit reads this per submit
                // (item 15) — applies mid-stream without reconnecting.
                crate::streaming::video::worker::LATENCY_PROFILE.store(
                    match profile {
                        crate::settings::LatencyProfile::Fast => 0,
                        crate::settings::LatencyProfile::Smooth => 1,
                    },
                    std::sync::atomic::Ordering::Relaxed,
                );
            }
            Command::SetDirectPresent(enabled) => {
                self.settings.direct_present = enabled;
                self.settings.save();
                // Applies at the next stream start (connection.rs stores
                // DIRECT_MODE per session).
            }
            Command::SetColorProfile(profile) => {
                // No live store: the decoder output format is fixed at decoder
                // construction, so this applies at the next stream start.
                self.settings.color_profile = profile;
                self.settings.save();
            }
            Command::SetPsButtonCapture(enabled) => {
                self.settings.ps_button_capture = enabled;
                self.settings.save();
            }
            Command::SetRegionBypassIp(ip) => {
                let before = self.settings.active_region_bypass_ip();
                self.settings.region_bypass_ip = ip;
                self.settings.save();
                self.note_region_bypass_change(before);
            }
            Command::EnterRegionBypassIp => {
                self.ime_request = Some(crate::app::ImeTarget::RegionBypassIp);
            }
        }

        Ok(())
    }

    /// Text from the on-screen keyboard for the bypass IP. A valid IP becomes the
    /// active one; empty input turns the bypass off; anything else is ignored.
    pub(crate) fn set_region_bypass_ip_from_input(&mut self, text: &str) {
        let ip = if text.trim().is_empty() {
            None
        } else if let Some(ip) = parse_region_bypass_ip(text) {
            Some(ip)
        } else {
            eprintln!("Settings: ignoring invalid region bypass IP {text:?}");
            return;
        };
        let _ = self.handle_settings_command(Command::SetRegionBypassIp(ip));
    }

    fn note_region_bypass_change(&mut self, before: Option<String>) {
        if self.settings.active_region_bypass_ip() != before {
            self.region_bypass_changed = true;
        }
    }

    pub(crate) fn set_locale(&mut self, locale: Locale) {
        if self.settings.locale == locale {
            return;
        }

        self.settings.locale = locale;
        self.service.api.config.locale = locale.as_str().to_owned();
        self.invalidate_catalog_for_locale_change();
        self.settings.save();
    }
}
