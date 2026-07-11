use crate::app::TitleImage;
use crate::app::ui::theme::Theme;
use crate::{AppCommand, InputCommand};
use std::sync::Arc;

pub fn draw_title_image(
    ui: &mut egui::Ui,
    image: &Arc<TitleImage>,
    rect: egui::Rect,
    label: &'static str,
) {
    let texture = image.texture(ui.ctx(), label);
    ui.painter().image(
        texture.id(),
        rect,
        egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0)),
        egui::Color32::WHITE,
    );
}

pub fn draw_title_image_cover(
    ui: &mut egui::Ui,
    image: &Arc<TitleImage>,
    rect: egui::Rect,
    label: &'static str,
    tint: egui::Color32,
) {
    let texture = image.texture(ui.ctx(), label);
    let image_aspect = image.width as f32 / image.height as f32;
    let rect_aspect = rect.width() / rect.height();
    let uv = if image_aspect > rect_aspect {
        let visible_width = rect_aspect / image_aspect;
        let inset = (1.0 - visible_width) * 0.5;
        egui::Rect::from_min_max(egui::pos2(inset, 0.0), egui::pos2(1.0 - inset, 1.0))
    } else {
        let visible_height = image_aspect / rect_aspect;
        let inset = (1.0 - visible_height) * 0.5;
        egui::Rect::from_min_max(egui::pos2(0.0, inset), egui::pos2(1.0, 1.0 - inset))
    };
    ui.painter().image(texture.id(), rect, uv, tint);
}

/// Hand-rolled 1:1 drag-scroll (no momentum/fling), shared by every scroll area in this UI.
/// `id_salt` must be unique per call site.
struct DragScroll {
    offset_id: egui::Id,
    max_offset_id: egui::Id,
    offset_y: f32,
}

impl DragScroll {
    pub fn begin(ui: &mut egui::Ui, id_salt: &str) -> Self {
        let drag_id = ui.make_persistent_id((id_salt, "drag"));
        let offset_id = ui.make_persistent_id((id_salt, "offset"));
        let max_offset_id = ui.make_persistent_id((id_salt, "max_offset"));

        let mut offset_y = ui.data(|d| d.get_temp::<f32>(offset_id)).unwrap_or(0.0);
        let max_offset_y = ui.data(|d| d.get_temp::<f32>(max_offset_id)).unwrap_or(0.0);
        // `click_and_drag` needs real movement before dragging, so it doesn't fight taps.
        let drag_response = ui.interact(
            ui.available_rect_before_wrap(),
            drag_id,
            egui::Sense::click_and_drag(),
        );
        offset_y = (offset_y - drag_response.drag_delta().y).clamp(0.0, max_offset_y);

        Self {
            offset_id,
            max_offset_id,
            offset_y,
        }
    }

    /// Persists the clamped scroll offset.
    pub fn end<R>(
        self,
        ui: &mut egui::Ui,
        output: &egui::containers::scroll_area::ScrollAreaOutput<R>,
    ) {
        let offset_y = output.state.offset.y;
        let max_offset_y = (output.content_size.y - output.inner_rect.height()).max(0.0);
        ui.data_mut(|d| {
            d.insert_temp(self.offset_id, offset_y);
            d.insert_temp(self.max_offset_id, max_offset_y);
        });
    }
}

/// Same `DragScroll` behavior as `show_selectable_list`, for plain interactive widgets.
pub fn smooth_scroll_area(
    ui: &mut egui::Ui,
    id_salt: &str,
    add_contents: impl FnOnce(&mut egui::Ui),
) {
    let drag = DragScroll::begin(ui, id_salt);
    let output = egui::ScrollArea::vertical()
        .id_salt(id_salt)
        .drag_to_scroll(false)
        .vertical_scroll_offset(drag.offset_y)
        .auto_shrink(false)
        .show(ui, add_contents);
    drag.end(ui, &output);
}

/// A vertical list of clickable rows, one of which is `selected_index` - shared by every
/// list-shaped screen (`TitleList`, `ConsoleList`).
pub fn show_selectable_list(
    ui: &mut egui::Ui,
    rows: &[(String, Option<Arc<TitleImage>>)],
    selected_index: usize,
    theme: Theme,
    commands: &mut Vec<AppCommand>,
    confirm_on_click: Option<AppCommand>,
) {
    let drag = DragScroll::begin(ui, "selectable_list");

    let last_selected_id = ui.make_persistent_id("selectable_list_last_selected");
    // `scroll_to_me` must only fire the frame the selection actually changes.
    let last_selected: Option<usize> = ui.data(|d| d.get_temp(last_selected_id));
    let selection_changed = last_selected != Some(selected_index);
    ui.data_mut(|d| d.insert_temp(last_selected_id, selected_index));

    // Rows are hand-painted (not `SelectableLabel`) so icons and selected backgrounds stay fixed
    // size while D-Pad movement scrolls the list.
    let row_height = 34.0;
    let icon_size = 22.0;
    let font_id = egui::TextStyle::Body.resolve(ui.style());

    let output = egui::ScrollArea::vertical()
        .id_salt("selectable_list")
        .drag_to_scroll(false)
        .vertical_scroll_offset(drag.offset_y)
        .show(ui, |ui| {
            for (index, (row, icon)) in rows.iter().enumerate() {
                let selected = index == selected_index;
                let (rect, response) = ui.allocate_exact_size(
                    egui::vec2(ui.available_width(), row_height),
                    egui::Sense::click(),
                );

                if ui.is_rect_visible(rect) {
                    let row_rect = rect.shrink2(egui::vec2(4.0, 3.0));
                    if selected {
                        ui.painter().rect_filled(
                            row_rect,
                            5.0,
                            egui::Color32::from_rgb(0x31, 0x32, 0x37),
                        );
                        let accent_rect = egui::Rect::from_min_max(
                            row_rect.min,
                            egui::pos2(row_rect.min.x + 3.0, row_rect.max.y),
                        );
                        ui.painter().rect_filled(accent_rect, 2.0, theme.accent);
                    } else if response.hovered() {
                        ui.painter().rect_filled(
                            row_rect,
                            5.0,
                            egui::Color32::from_rgb(0x29, 0x2a, 0x2f),
                        );
                    }

                    let text_x = if let Some(image) = icon {
                        let icon_rect = egui::Rect::from_min_size(
                            row_rect.min + egui::vec2(10.0, (row_rect.height() - icon_size) / 2.0),
                            egui::vec2(icon_size, icon_size),
                        );
                        ui.painter().rect_filled(
                            icon_rect.expand(2.0),
                            4.0,
                            egui::Color32::from_rgb(0x18, 0x19, 0x1c),
                        );
                        draw_title_image(ui, image, icon_rect, "row-icon");
                        icon_rect.max.x + 10.0
                    } else {
                        row_rect.min.x + 12.0
                    };

                    ui.painter().text(
                        egui::pos2(text_x, row_rect.center().y),
                        egui::Align2::LEFT_CENTER,
                        row,
                        font_id.clone(),
                        if selected {
                            theme.text_bright
                        } else {
                            theme.text
                        },
                    );
                }

                if response.clicked() {
                    if index > selected_index {
                        commands.extend(std::iter::repeat_n(
                            AppCommand::from(InputCommand::MoveDown),
                            index - selected_index,
                        ));
                    } else if index < selected_index {
                        commands.extend(std::iter::repeat_n(
                            AppCommand::from(InputCommand::MoveUp),
                            selected_index - index,
                        ));
                    }
                    if let Some(command) = &confirm_on_click {
                        commands.push(command.clone());
                    }
                }
                // D-Pad Up/Down can move the highlight off-screen with no pointer interaction.
                if selected && selection_changed {
                    response.scroll_to_me(Some(egui::Align::Center));
                }
            }
        });

    drag.end(ui, &output);
}

/// A full-width rounded tile, filled solid with `theme.accent` when `selected`, like the Xbox
/// dashboard. Shared by the hamburger menu and the pause overlay.
pub(super) fn menu_item(
    ui: &mut egui::Ui,
    theme: Theme,
    icon: &str,
    label: &str,
    selected: bool,
) -> bool {
    const HEIGHT: f32 = 44.0;
    const CORNER_RADIUS: f32 = 8.0;
    const ICON_COLUMN_WIDTH: f32 = 40.0;

    let width = ui.available_width();
    let (rect, response) = ui.allocate_exact_size(egui::vec2(width, HEIGHT), egui::Sense::click());

    if ui.is_rect_visible(rect) {
        let (fill, text_color) = if selected {
            (Some(theme.accent), egui::Color32::WHITE)
        } else if response.hovered() {
            (
                Some(egui::Color32::from_rgb(0x2c, 0x2d, 0x33)),
                theme.text_bright,
            )
        } else {
            (None, theme.text_bright)
        };
        if let Some(fill) = fill {
            ui.painter().rect_filled(rect, CORNER_RADIUS, fill);
        }

        let font_id = egui::FontId::proportional(16.0);
        ui.painter().text(
            egui::pos2(rect.min.x + ICON_COLUMN_WIDTH / 2.0, rect.center().y),
            egui::Align2::CENTER_CENTER,
            icon,
            font_id.clone(),
            text_color,
        );
        ui.painter().text(
            egui::pos2(rect.min.x + ICON_COLUMN_WIDTH, rect.center().y),
            egui::Align2::LEFT_CENTER,
            label,
            font_id,
            text_color,
        );
    }

    response.clicked()
}

/// A "hold to pause" ring: dim full circle plus a bright wedge that sweeps in with `progress`.
pub fn draw_hold_progress_ring(ui: &mut egui::Ui, progress: f32) {
    const SIZE: f32 = 56.0;
    const RADIUS: f32 = 20.0;
    const RING_SEGMENTS: usize = 48;

    let (rect, _) = ui.allocate_exact_size(egui::vec2(SIZE, SIZE), egui::Sense::hover());
    let center = rect.center();
    let painter = ui.painter();

    painter.circle_filled(center, RADIUS + 4.0, egui::Color32::from_black_alpha(140));
    painter.circle_stroke(
        center,
        RADIUS,
        egui::Stroke::new(3.0_f32, egui::Color32::from_white_alpha(60)),
    );

    let progress = progress.clamp(0.0, 1.0);
    if progress <= 0.0 {
        return;
    }
    // Starts at 12 o'clock (`-FRAC_PI_2`) and sweeps clockwise.
    let start_angle = -std::f32::consts::FRAC_PI_2;
    let steps = (RING_SEGMENTS as f32 * progress).ceil().max(1.0) as usize;
    let mut points = Vec::with_capacity(steps + 2);
    points.push(center);
    for step in 0..=steps {
        let t = (step as f32 / RING_SEGMENTS as f32).min(progress);
        let angle = start_angle + t * std::f32::consts::TAU;
        points.push(center + RADIUS * egui::vec2(angle.cos(), angle.sin()));
    }
    painter.add(egui::Shape::convex_polygon(
        points,
        egui::Color32::WHITE,
        egui::Stroke::NONE,
    ));
}
