use crate::app::command::{move_next, move_prev};
use crate::app::ui::header::show_header_row;
use crate::app::ui::theme::Theme;
use crate::app::ui::widgets::{draw_title_image, draw_title_image_cover, show_selectable_list};
use crate::app::{AppState, StreamStartTarget, TitleImage, TitleInitialOverlay};
use crate::i18n::I18n;
use crate::{App, AppCommand, InputCommand, StreamKind};
use anyhow::Result;
use std::sync::Arc;
use std::time::{Duration, Instant};

const INITIAL_OVERLAY_DURATION: Duration = Duration::from_millis(800);

fn title_rows(app: &App) -> Vec<(String, Option<Arc<TitleImage>>)> {
    if app.service.titles.is_empty() && matches!(&app.state, AppState::LoadingTitles(_)) {
        return vec![("Requesting /v2/titles".to_owned(), None)];
    }
    if app.service.titles.is_empty() {
        return vec![
            (
                "No titles returned or parsed from /v2/titles".to_owned(),
                None,
            ),
            ("Use xCloud gsToken with xCloud baseUri".to_owned(), None),
        ];
    }
    app.service
        .titles
        .iter()
        .map(|title| (title.display_name().to_owned(), title.icon.clone()))
        .collect()
}

/// Cloud Titles screen: a highlight-only list on the left, details + Play button on the right.
pub(crate) fn show(ctx: &egui::Context, app: &App, commands: &mut Vec<AppCommand>) {
    let selected = match &app.state {
        AppState::TitleList { selected } => *selected,
        AppState::LoadingTitles(_) => 0,
        _ => return,
    };
    let theme = Theme::dark();
    let i18n = I18n::new(app.settings.locale);
    let mut frame = egui::Frame::central_panel(&ctx.style());
    frame.fill = egui::Color32::TRANSPARENT;
    egui::CentralPanel::default().frame(frame).show(ctx, |ui| {
        draw_title_background(ui, app, theme);

        // Opaque strip behind the header so it stays legible over the cover-art backdrop:
        // 8pt panel margin + 36pt logo + 8pt `add_space` below the row.
        // Must stay in sync with `show_header_row` if either changes.
        const HEADER_TINT_HEIGHT: f32 = 8.0 + 36.0 + 8.0;
        let screen_rect = ui.ctx().screen_rect();
        let bg = theme.background;
        let rect = egui::Rect::from_min_size(
            screen_rect.min,
            egui::vec2(screen_rect.width(), HEADER_TINT_HEIGHT),
        );
        ui.painter().rect_filled(
            rect,
            0.0,
            egui::Color32::from_rgba_unmultiplied(bg.r(), bg.g(), bg.b(), 235),
        );
        show_header_row(ui, app, theme, &i18n, Some(commands));
        ui.add_space(8.0);

        let list_width = (ui.available_width() * 0.42).clamp(220.0, 420.0);
        let list_frame = egui::Frame::NONE
            .fill(egui::Color32::from_rgb(0x20, 0x21, 0x24))
            .inner_margin(egui::Margin {
                left: 8,
                right: 8,
                top: 8,
                bottom: 8,
            });
        egui::SidePanel::left("title_list_panel")
            .resizable(false)
            .exact_width(list_width)
            .frame(list_frame)
            .show_inside(ui, |ui| {
                let rows = title_rows(app);
                show_selectable_list(ui, &rows, selected, theme, commands, None);
            });

        egui::Frame::NONE
            .inner_margin(egui::Margin {
                left: 18,
                right: 8,
                top: 0,
                bottom: 0,
            })
            .show(ui, |ui| {
                let title = app.highlighted_title();
                let Some(title) = title else {
                    if matches!(&app.state, AppState::LoadingTitles(_)) {
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.colored_label(theme.text, i18n.text("title-loading"));
                        });
                    } else {
                        ui.colored_label(theme.text, i18n.text("title-select-details"));
                    }
                    return;
                };

                const COVER_WIDTH: f32 = 140.0;
                let catalog = title.details.as_ref();
                let loading = catalog.is_none();
                // While the highlighted title's cover is loading, reserve the last cover's rendered height
                // rather than a generic guess - box art mostly shares the same aspect ratio.
                let last_cover_height_id = egui::Id::new("title_last_cover_height");
                let cover_height = match title.box_art.as_ref() {
                    Some(box_art) => {
                        let height = COVER_WIDTH * box_art.height as f32 / box_art.width as f32;
                        ui.ctx()
                            .data_mut(|d| d.insert_temp(last_cover_height_id, height));
                        height
                    }
                    None => ui
                        .ctx()
                        .data(|d| d.get_temp(last_cover_height_id))
                        .unwrap_or(160.0),
                };

                ui.horizontal(|ui| {
                    let (rect, _) = ui.allocate_exact_size(
                        egui::vec2(COVER_WIDTH, cover_height),
                        egui::Sense::hover(),
                    );
                    if let Some(box_art) = title.box_art.as_ref() {
                        draw_title_image(ui, box_art, rect, "box-art");
                    } else {
                        ui.painter().rect_filled(
                            rect,
                            6.0,
                            egui::Color32::from_rgb(0x18, 0x19, 0x1c),
                        );
                        if loading {
                            ui.put(rect, egui::Spinner::new());
                        }
                    }

                    ui.add_space(12.0);

                    // Fixed to `cover_height` so the bottom_up Play button lands flush with the cover art.
                    ui.allocate_ui_with_layout(
                        egui::vec2(ui.available_width(), cover_height),
                        egui::Layout::top_down(egui::Align::Min),
                        |ui| {
                            ui.heading(
                                egui::RichText::new(title.display_name()).color(theme.text_bright),
                            );
                            ui.add_space(6.0);
                            let mut metadata = catalog
                                .map(|catalog| catalog.genres.clone())
                                .unwrap_or_default();
                            if let Some(year) = catalog
                                .and_then(|catalog| catalog.release_date.as_deref())
                                .and_then(|date| date.get(0..4))
                            {
                                metadata.push(year.to_owned());
                            }
                            if let Some(rating) = catalog.and_then(|catalog| catalog.average_rating)
                            {
                                metadata.push(
                                    match catalog.and_then(|catalog| catalog.rating_count) {
                                        Some(count) => format!("\u{2605} {rating:.1} ({count})"),
                                        None => format!("\u{2605} {rating:.1}"),
                                    },
                                );
                            }
                            if let Some(content_rating) =
                                catalog.and_then(|catalog| catalog.content_rating.as_ref())
                            {
                                metadata.push(content_rating.clone());
                            }
                            if !metadata.is_empty() {
                                ui.colored_label(theme.text_bright, metadata.join(" \u{b7} "));
                                ui.add_space(4.0);
                            }
                            let credits = catalog.and_then(|catalog| {
                                match (&catalog.developer, &catalog.publisher) {
                                    (Some(developer), Some(publisher))
                                        if developer == publisher =>
                                    {
                                        Some(developer.clone())
                                    }
                                    (Some(developer), Some(publisher)) => {
                                        Some(format!("{developer} \u{b7} {publisher}"))
                                    }
                                    (Some(developer), None) => Some(developer.clone()),
                                    (None, Some(publisher)) => Some(publisher.clone()),
                                    (None, None) => None,
                                }
                            });
                            if let Some(credits) = credits {
                                ui.colored_label(theme.text, credits);
                            }

                            ui.with_layout(egui::Layout::bottom_up(egui::Align::Min), |ui| {
                                // `bottom_up` places the first widget at the bottom.
                                ui.add_space(10.0);

                                let play_button = egui::Button::new(
                                    egui::RichText::new(i18n.text("title-play"))
                                        .size(20.0)
                                        .strong()
                                        .color(egui::Color32::WHITE),
                                )
                                .fill(theme.accent)
                                .corner_radius(8.0);
                                let button_width = ui.available_width();
                                if ui
                                    .add_sized(egui::vec2(button_width, 44.0), play_button)
                                    .clicked()
                                {
                                    commands.push(InputCommand::Confirm.into());
                                }
                            });
                        },
                    );
                });

                ui.add_space(10.0);

                egui::ScrollArea::vertical()
                    .id_salt("title_details_description")
                    .show(ui, |ui| {
                        match catalog.and_then(|catalog| catalog.description.as_ref()) {
                            Some(description) => {
                                ui.colored_label(theme.text, description);
                            }
                            None if !loading => {
                                ui.colored_label(theme.text, i18n.text("title-no-description"));
                            }
                            None => {}
                        }
                    });
            });
    });
    draw_initial_overlay(ctx, app);
}

fn draw_initial_overlay(ctx: &egui::Context, app: &App) {
    let Some(overlay) = &app.title_initial_overlay else {
        return;
    };
    let elapsed = overlay.shown_at.elapsed();
    if elapsed >= INITIAL_OVERLAY_DURATION {
        return;
    }

    ctx.request_repaint_after(INITIAL_OVERLAY_DURATION.saturating_sub(elapsed));
    let fade = if elapsed < Duration::from_millis(550) {
        1.0
    } else {
        1.0 - (elapsed - Duration::from_millis(550)).as_secs_f32() / 0.25
    };
    let alpha = (220.0 * fade.clamp(0.0, 1.0)).round() as u8;
    let screen = ctx.screen_rect();
    let rect = egui::Rect::from_center_size(screen.center(), egui::vec2(104.0, 104.0));
    let painter = ctx.layer_painter(egui::LayerId::new(
        egui::Order::Foreground,
        egui::Id::new("title_initial_overlay"),
    ));
    painter.rect_filled(
        rect,
        14.0,
        egui::Color32::from_rgba_unmultiplied(20, 21, 24, alpha),
    );
    painter.text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        &overlay.label,
        egui::FontId::proportional(56.0),
        egui::Color32::from_rgba_unmultiplied(255, 255, 255, alpha),
    );
}

fn title_initial(title_id: &str) -> String {
    match title_id
        .chars()
        .find(|character| character.is_alphanumeric())
    {
        Some(character) if character.is_alphabetic() => character.to_uppercase().collect(),
        Some(_) | None => "#".to_owned(),
    }
}

fn initial_groups(app: &App) -> Vec<(usize, String)> {
    let mut groups = Vec::new();
    for (index, title) in app.service.titles.iter().enumerate() {
        let initial = title_initial(&title.title_id);
        if groups
            .last()
            .is_none_or(|(_, previous): &(usize, String)| *previous != initial)
        {
            groups.push((index, initial));
        }
    }
    groups
}

fn adjacent_initial_group(
    groups: &[(usize, String)],
    selected: usize,
    move_right: bool,
) -> Option<(usize, String)> {
    if groups.is_empty() {
        return None;
    }
    let current_group = groups
        .iter()
        .rposition(|(start, _)| *start <= selected)
        .unwrap_or(0);
    let target_group = if move_right {
        (current_group + 1) % groups.len()
    } else {
        current_group.checked_sub(1).unwrap_or(groups.len() - 1)
    };
    groups.get(target_group).cloned()
}

#[derive(Clone)]
struct BackgroundFade {
    current_key: usize,
    current: Arc<TitleImage>,
    previous: Option<Arc<TitleImage>>,
    pending: Option<(usize, Arc<TitleImage>)>,
    changed_at: f64,
}

pub(crate) fn draw_title_background(ui: &mut egui::Ui, app: &App, theme: Theme) {
    let Some(title) = app.highlighted_title() else {
        return;
    };
    if let Some(background) = title.background.as_ref() {
        background.texture(ui.ctx(), "title-background");
    }
    let now = ui.input(|input| input.time);
    let fade_id = egui::Id::new("title_background_fade");
    let mut needs_repaint = false;
    let fade = ui.ctx().data_mut(|data| {
        let existing = data.get_temp::<BackgroundFade>(fade_id);
        let fade = match (title.background.as_ref(), existing) {
            (Some(background), Some(mut fade)) => {
                let key = Arc::as_ptr(background) as usize;
                if fade.current_key == key {
                    fade.pending = None;
                } else if fade
                    .pending
                    .as_ref()
                    .is_some_and(|(pending_key, _)| *pending_key == key)
                {
                    let (_, next) = fade
                        .pending
                        .take()
                        .expect("pending background checked above");
                    fade.previous = Some(std::mem::replace(&mut fade.current, next));
                    fade.current_key = key;
                    fade.changed_at = now;
                } else {
                    fade.pending = Some((key, Arc::clone(background)));
                    needs_repaint = true;
                }
                Some(fade)
            }
            (Some(background), None) => Some(BackgroundFade {
                current_key: Arc::as_ptr(background) as usize,
                current: Arc::clone(background),
                previous: None,
                pending: None,
                changed_at: now,
            }),
            (None, Some(mut fade)) => {
                fade.pending = None;
                Some(fade)
            }
            (None, None) => None,
        };
        if let Some(fade) = &fade {
            data.insert_temp(fade_id, fade.clone());
        }
        fade
    });
    if needs_repaint {
        ui.ctx().request_repaint();
    }
    let Some(mut fade) = fade else {
        return;
    };

    let rect = ui.ctx().screen_rect();
    const FADE_SECONDS: f64 = 0.28;
    const IMAGE_ALPHA: u8 = 150;
    let fade_t = ((now - fade.changed_at) / FADE_SECONDS).clamp(0.0, 1.0) as f32;

    if fade_t >= 1.0 && fade.previous.take().is_some() {
        ui.ctx()
            .data_mut(|data| data.insert_temp(fade_id, fade.clone()));
    }

    let current_alpha = if fade.previous.is_some() {
        (fade_t * IMAGE_ALPHA as f32).round() as u8
    } else {
        IMAGE_ALPHA
    };

    if let Some(previous) = &fade.previous {
        let alpha = ((1.0 - fade_t) * IMAGE_ALPHA as f32).round() as u8;
        if alpha > 0 {
            draw_title_image_cover(
                ui,
                previous,
                rect,
                "title-background-previous",
                egui::Color32::from_rgba_unmultiplied(255, 255, 255, alpha),
            );
            ui.ctx().request_repaint();
        }
    }
    draw_title_image_cover(
        ui,
        &fade.current,
        rect,
        "title-background",
        egui::Color32::from_rgba_unmultiplied(255, 255, 255, current_alpha),
    );
    ui.painter().rect_filled(
        rect,
        0.0,
        egui::Color32::from_rgba_unmultiplied(
            theme.background.r(),
            theme.background.g(),
            theme.background.b(),
            120,
        ),
    );
}

impl App {
    pub(crate) async fn handle_title_list_input(&mut self, command: InputCommand) -> Result<()> {
        let item_count = self.service.titles.len();
        match command {
            InputCommand::MoveUp => {
                if let AppState::TitleList { selected } = &mut self.state {
                    *selected = move_prev(*selected, item_count);
                }
            }
            InputCommand::MoveDown => {
                if let AppState::TitleList { selected } = &mut self.state {
                    *selected = move_next(*selected, item_count);
                }
            }
            InputCommand::MoveLeft | InputCommand::MoveRight => {
                let selected = match &self.state {
                    AppState::TitleList { selected } => *selected,
                    _ => return Ok(()),
                };
                let groups = initial_groups(self);
                if let Some((target, label)) =
                    adjacent_initial_group(&groups, selected, command == InputCommand::MoveRight)
                {
                    if let AppState::TitleList { selected } = &mut self.state {
                        *selected = target;
                    }
                    self.title_initial_overlay = Some(TitleInitialOverlay {
                        label,
                        shown_at: Instant::now(),
                    });
                }
            }
            InputCommand::Confirm => {
                let AppState::TitleList { selected } = &self.state else {
                    return Ok(());
                };
                let selected = *selected;
                let Some(title) = self.service.titles.get(selected).cloned() else {
                    return Ok(());
                };
                let target_id = title.title_id;
                self.start_stream_for_target(StreamStartTarget {
                    kind: StreamKind::Cloud,
                    label: format!("cloud title {target_id}"),
                    target_id,
                    return_selected: selected,
                });
            }
            InputCommand::Back => {
                self.set_state(AppState::ModeSelect { selected: 0 });
            }
        }

        Ok(())
    }
}
