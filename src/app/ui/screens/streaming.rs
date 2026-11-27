use crate::App;
use crate::app::ui::theme::Theme;
use crate::app::ui::widgets::draw_hold_progress_ring;
use crate::i18n::I18n;
use crate::settings::StreamDebugMode;
use egui::FontId;

/// Extract a value after `key` in the status string (e.g. `srcfps:60` → "60").
fn extract_field(status: &str, key: &str) -> String {
    if let Some(pos) = status.find(key) {
        let start = pos + key.len();
        let mut val = String::new();
        for ch in status[start..].chars() {
            if ch.is_ascii_alphanumeric() || ch == '/' || ch == '.' || ch == '%' {
                val.push(ch);
            } else {
                break;
            }
        }
        val
    } else {
        String::new()
    }
}

/// Build cached debug rows (rebuild ≤ every 250 ms to avoid per-frame overhead).
/// Uses static cached rows; the matrix is recomputed no more than once per 250 ms window.
/// Each row is a fixed-width cell: padded label + right-aligned value, so when a value's
/// width changes only that cell's interior shifts and nothing layout-shifts.
fn build_debug_rows(status: &str, settings: &crate::settings::Settings) -> Vec<String> {
    static LAST_REBUILD: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    static CACHED_ROWS: std::sync::atomic::AtomicPtr<Vec<String>> = std::sync::atomic::AtomicPtr::new(core::ptr::null_mut());

    let now_ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    let last = LAST_REBUILD.load(std::sync::atomic::Ordering::Relaxed);
    let cached_ptr = CACHED_ROWS.load(std::sync::atomic::Ordering::Relaxed);

    // Rebuild if no cached rows or if 250 ms has elapsed since last rebuild.
    if cached_ptr.is_null() || now_ms.saturating_sub(last) > 250 {
        let f = |key: &str| extract_field(status, key);
        let dp = f("d/p:");
        let da = f("d/a:");
        let repl = f("repl:");
        let skip = f("skip:");
        let hwb = f("hwb:");
        let lag = f("lag:");
        let bw = f("bw:");
        let lagmax = f("lagMax:");
        let cta = f("cta:");
        let cta_max = f("ctaMax:");
        let twcd = f("twcd:");
        let twcdmx = f("twcdMx:");
        let srage = f("srAge:");
        let srcoff = f("srCoff:");
        let fl = f("fl:");
        let rs = f("rs:");
        let tw_s = f("tw:");
        let st = f("stride:");
        let pick = f("pick:");
        let enc = f("enc:");
        let err = f("err:");
        let cs = f("cs:");
        let rxd = f("rxd:");
        let rcpu = f("rcpu:");
        let ui = f("ui:");
        let uw = f("uw:");
        let kf = f("kf:");
        let idr = f("idr:");
        let scr = f("scr:");

        const LABEL_WIDTH: usize = 16;
        const VALUE_WIDTH: usize = 8;

        let res = if enc.is_empty() { "?".to_string() } else { enc };
        let err_val = if err.is_empty() { "0".to_string() } else { err };
        let color_val = if cs.is_empty() { "BT.709".to_string() } else { cs };

        let new_rows: Vec<String> = vec![
            format!("{:<LABEL_WIDTH$}{:>VALUE_WIDTH$}", format!("Dec/Pres:{}", dp), format!("Dec/Pip:{}ms", da)),
            format!("{:<LABEL_WIDTH$}{:>VALUE_WIDTH$}", format!("Repl:{}", repl), format!("Skip:{}", skip)),
            format!("{:<LABEL_WIDTH$}{:>VALUE_WIDTH$}", format!("HWbuf:{}", hwb), format!("Lag:{}ms", lag)),
            format!("{:<LABEL_WIDTH$}{:>VALUE_WIDTH$}", format!("BW:{}kbps", bw), format!("LagMx:{}ms", lagmax)),
            format!("{:<LABEL_WIDTH$}{:>VALUE_WIDTH$}", format!("CTA:{}ms", cta), format!("CTAMx:{}ms", cta_max)),
            format!("{:<LABEL_WIDTH$}{:>VALUE_WIDTH$}", format!("TcD:{}us", twcd), format!("TcDMx:{}us", twcdmx)),
            format!("{:<LABEL_WIDTH$}{:>VALUE_WIDTH$}", format!("SRage:{}ms", srage), format!("SRcoff:{}ms", srcoff)),
            format!("{:<LABEL_WIDTH$}{:>VALUE_WIDTH$}", format!("Resync:{}", rs), format!("Flush:{}", fl)),
            format!("{:<LABEL_WIDTH$}{:>VALUE_WIDTH$}", format!("TWCC:{}", tw_s), format!("Strd:{}", st)),
            format!("{:<LABEL_WIDTH$}{:>VALUE_WIDTH$}", format!("Pick:{}us", pick), format!("RxCPU:{}ms", rcpu)),
            format!("{:<LABEL_WIDTH$}{:>VALUE_WIDTH$}", "Decoder:avcdec", format!("Res:{}", res)),
            format!("{:<LABEL_WIDTH$}{:>VALUE_WIDTH$}", format!("Color:{}", color_val), format!("Err:{}", err_val)),
            format!("{:<LABEL_WIDTH$}{:>VALUE_WIDTH$}", format!("UIloop:{}ms", ui), format!("UIwork:{}ms", uw)),
            format!("{:<LABEL_WIDTH$}{:>VALUE_WIDTH$}", format!("KfReq:{}", kf), format!("IDR:{}KB", idr)),
            format!("{:<LABEL_WIDTH$}{:>VALUE_WIDTH$}", format!("RxdMax:{}", rxd), format!("Scratch:{}", scr)),
        ];

        // Free any previously cached rows and store the new ones.
        let _ = CACHED_ROWS.store(std::ptr::null_mut(), std::sync::atomic::Ordering::Relaxed);
        let _ = LAST_REBUILD.store(now_ms, std::sync::atomic::Ordering::Relaxed);
        let _boxed = Box::into_raw(Box::new(new_rows.clone()));
        CACHED_ROWS.store(_boxed, std::sync::atomic::Ordering::Relaxed);

        new_rows
    } else {
        // Return a clone of the cached rows.
        if !cached_ptr.is_null() {
            unsafe { (*cached_ptr).clone() }
        } else {
            build_debug_rows(status, settings)
        }
    }
}

/// Fullscreen video view with debug overlays.
pub(crate) fn show(
    ctx: &egui::Context,
    app: &App,
    hold_progress: Option<f32>,
    ps_hold_hint: bool,
) {
    const HINT_VISIBLE: std::time::Duration = std::time::Duration::from_secs(2);
    const HINT_FADE: std::time::Duration = std::time::Duration::from_secs(1);

    let theme = Theme::dark();
    let i18n = I18n::new(app.settings.locale);
    let streaming = match &app.state {
        crate::AppState::Streaming(streaming) => streaming,
        _ => return,
    };
    let mut frame = egui::Frame::central_panel(&ctx.style());
    frame.fill = egui::Color32::TRANSPARENT;
    egui::CentralPanel::default().frame(frame).show(ctx, |ui| {
        let elapsed = streaming.hint_started_at.elapsed();
        let alpha = if elapsed < HINT_VISIBLE {
            1.0
        } else if elapsed < HINT_VISIBLE + HINT_FADE {
            1.0 - (elapsed - HINT_VISIBLE).as_secs_f32() / HINT_FADE.as_secs_f32()
        } else {
            0.0
        };

        if alpha > 0.0 {
            ui.vertical_centered(|ui| {
                ui.add_space(16.0);
                ui.colored_label(
                    theme.text.gamma_multiply(alpha),
                    i18n.text("streaming-hold-back"),
                );
            });
        }

        // Latency test mode: blinking square + stopwatch on button press.
        // The square flips colour on every button-press edge and the readout shows
        // ms from that edge to this frame (client-side input -> present delay).
        if app.settings.latency_test_mode {
            let blink = crate::latency_test::BLINK.load(std::sync::atomic::Ordering::Relaxed);
            let age_ms = crate::latency_test::last_press_age_ms();

            egui::Area::new(egui::Id::new("latency_test_marker"))
                .order(egui::Order::Foreground)
                .anchor(egui::Align2::LEFT_TOP, egui::vec2(8.0, 8.0))
                .show(ctx, |ui| {
                    let painter = ui.painter();
                    let square = egui::Rect::from_min_size(
                        egui::pos2(0.0, 0.0),
                        egui::vec2(160.0, 160.0),
                    );
                    if blink {
                        painter.rect_filled(square, 0.0, egui::Color32::WHITE);
                        painter.rect_stroke(
                            square,
                            0.0,
                            egui::Stroke::new(2.0, egui::Color32::BLACK),
                            egui::StrokeKind::Inside,
                        );
                    } else {
                        painter.rect_filled(square, 0.0, egui::Color32::BLACK);
                        painter.rect_stroke(
                            square,
                            0.0,
                            egui::Stroke::new(2.0, egui::Color32::WHITE),
                            egui::StrokeKind::Inside,
                        );
                    }

                    let readout = match age_ms {
                        Some(ms) => format!("+{:.0} ms", ms),
                        None => "+--- ms".to_owned(),
                    };
                    let readout_rect = egui::Rect::from_min_size(
                        egui::pos2(168.0, 0.0),
                        egui::vec2(120.0, 40.0),
                    );
                    painter.rect_filled(readout_rect, 0.0, egui::Color32::from_black_alpha(200));
                    painter.text(
                        readout_rect.center(),
                        egui::Align2::CENTER_CENTER,
                        readout,
                        FontId::monospace(16.0),
                        egui::Color32::from_rgb(144, 238, 144),
                    );

                    let _ = ui.allocate_space(egui::vec2(296.0, 160.0));
                });
        }

// Debug overlay modes: Full, Minimal (TOP-RIGHT), or Off.
match app.settings.stream_debug_mode {
    StreamDebugMode::Full if !streaming.status.is_empty() => {
        let rows = build_debug_rows(&streaming.status, &app.settings);

        if !rows.is_empty() {
            const ROW_HEIGHT: f32 = 13.0_f32;
            const LABEL_WIDTH: usize = 16;
            const PAD: f32 = 8.0;
            const GAP: f32 = 12.0;
            const COLUMN_GAP: f32 = 32.0;
            let n = rows.len();

            // Two-column layout when many rows (>10), otherwise single column
            let two_columns = n > 10;
            let n_left = if two_columns { (n + 1) / 2 } else { n };
            let n_right = if two_columns { n - n_left } else { 0 };

            // 2026-11-01 fix: the panel is as tall as the TALLER column
            // (left), not all n rows stacked — right-column rows are
            // y-indexed within their own column (the old global-index
            // Y made the right column trail below the left = "broken
            // columns").
            let rows_tall = if two_columns { n_left } else { n };
            let panel_h = rows_tall as f32 * ROW_HEIGHT + 16.0;

            egui::Area::new(egui::Id::new("debug_overlay"))
                .order(egui::Order::Foreground)
                .anchor(egui::Align2::LEFT_TOP, egui::vec2(8.0, 8.0))
                .show(ctx, |ui| {
                    let painter = ui.painter();

                    // Measure real text widths across all rows (needs painter for font metrics)
                    let mut max_label_w = 0.0;
                    let mut max_value_w = 0.0;
                    for cell_text in &rows {
                        let label_part = &cell_text[..std::cmp::min(LABEL_WIDTH, cell_text.len())];
                        let value_part = if cell_text.len() > LABEL_WIDTH {
                            &cell_text[LABEL_WIDTH..]
                        } else {
                            ""
                        };
                        let g = painter.layout_no_wrap(label_part.to_owned(), FontId::monospace(10.0), theme.text.gamma_multiply(0.85));
                        let label_w = g.size().x;
                        let g2 = painter.layout_no_wrap(value_part.to_owned(), FontId::monospace(10.0), theme.text.gamma_multiply(0.85));
                        let value_w = g2.size().x;
                        if label_w > max_label_w { max_label_w = label_w; }
                        if value_w > max_value_w { max_value_w = value_w; }
                    }

                    let x2 = PAD + max_label_w + GAP + max_value_w + COLUMN_GAP;
                    let panel_w = if two_columns {
                        x2 + max_label_w + GAP + max_value_w + PAD
                    } else {
                        PAD + max_label_w + GAP + max_value_w + PAD
                    };

                    let rect = egui::Rect::from_min_size(
                        egui::pos2(0.0, 0.0),
                        egui::vec2(panel_w, panel_h),
                    );
                    painter.rect_filled(rect, 4.0, egui::Color32::from_black_alpha(200));

                    // Draw rows from build_debug_rows (measured-width columns)
                    for (i, cell_text) in rows.iter().enumerate() {
                        // Right-column rows are indexed within their own
                        // column so both columns start at the same top.
                        let row_in_col = if two_columns && i >= n_left {
                            i - n_left
                        } else {
                            i
                        };
                        let y = 8.0 + row_in_col as f32 * ROW_HEIGHT;

                        if two_columns && i >= n_left {
                            // Right column: split cell text into label and value
                            let label_part = &cell_text[..std::cmp::min(LABEL_WIDTH, cell_text.len())];
                            let value_part = if cell_text.len() > LABEL_WIDTH {
                                &cell_text[LABEL_WIDTH..]
                            } else {
                                ""
                            };
                            // Right label: LEFT-aligned at X2
                            painter.text(
                                egui::pos2(x2, y), egui::Align2::LEFT_TOP,
                                label_part,
                                FontId::monospace(10.0),
                                theme.text.gamma_multiply(0.85),
                            );
                            // Right value: RIGHT-aligned at X2 + max_label_w + GAP + max_value_w
                            let value_x = x2 + max_label_w + GAP + max_value_w;
                            painter.text(
                                egui::pos2(value_x, y), egui::Align2::RIGHT_TOP,
                                value_part,
                                FontId::monospace(10.0),
                                theme.text.gamma_multiply(0.85),
                            );
                            continue;
                        }

                        // Left column (or single column)
                        let label_part = &cell_text[..std::cmp::min(LABEL_WIDTH, cell_text.len())];
                        let value_part = if cell_text.len() > LABEL_WIDTH {
                            &cell_text[LABEL_WIDTH..]
                        } else {
                            ""
                        };

                        let x = PAD; // LEFT-aligned label at start of cell
                        painter.text(
                            egui::pos2(x, y), egui::Align2::LEFT_TOP,
                            label_part,
                            FontId::monospace(10.0),
                            theme.text.gamma_multiply(0.85),
                        );
                        // Value right-aligned at fixed right edge
                        let value_x = PAD + max_label_w + GAP + max_value_w;
                        painter.text(
                            egui::pos2(value_x, y), egui::Align2::RIGHT_TOP,
                            value_part,
                            FontId::monospace(10.0),
                            theme.text.gamma_multiply(0.85),
                        );
                    }
                });
        }
    }
    StreamDebugMode::Minimal if !streaming.status.is_empty() => {
        // Mini overlay: TOP-RIGHT corner, semi-transparent dark rect, white text.
        // Two lines: bw: {mbps:.2} Mbps and fps: {fps:.1}
        let bandwidth_kbps = crate::streaming::video::metrics::METRICS
            .video_bandwidth_kbps
            .load(std::sync::atomic::Ordering::Relaxed);
        let bandwidth_mbps = bandwidth_kbps as f64 / 1000.0;
        // Reuse the same FPS source the full overlay uses: the DISPLAYED
        // fps from the status "fps d/p" pair (decoded/presented) — TODO 6
        // requires displayed FPS, not the console's srcfps.
        let status = &streaming.status;
        let dp = extract_field(status, "d/p:");
        let fps = dp
            .split('/')
            .nth(1)
            .and_then(|p| p.trim().parse::<f64>().ok())
            .unwrap_or(0.0);

        egui::Area::new(egui::Id::new("minimal_debug_overlay"))
            .order(egui::Order::Foreground)
            .anchor(egui::Align2::RIGHT_TOP, egui::vec2(8.0, 8.0))
            .show(ctx, |ui| {
                let painter = ui.painter();

                // Semi-transparent dark rect
                let rect = egui::Rect::from_min_size(
                    egui::pos2(0.0, 0.0),
                    egui::vec2(200.0, 50.0),
                );
                painter.rect_filled(rect, 4.0, egui::Color32::from_black_alpha(128));

                // Bandwidth line
                let bw_text = format!("bw: {:.2} Mbps", bandwidth_mbps);
                let g = painter.layout_no_wrap(bw_text.clone(), FontId::monospace(14.0), egui::Color32::WHITE);
                painter.text(
                    egui::pos2(8.0 + g.size().x / 2.0, 8.0 + g.size().y / 2.0),
                    egui::Align2::CENTER_TOP,
                    bw_text,
                    FontId::monospace(14.0),
                    egui::Color32::WHITE,
                );

                // FPS line
                let fps_text = format!("fps: {:.1}", fps);
                let g2 = painter.layout_no_wrap(fps_text.clone(), FontId::monospace(14.0), egui::Color32::WHITE);
                painter.text(
                    egui::pos2(8.0 + g2.size().x / 2.0, 28.0 + g2.size().y / 2.0),
                    egui::Align2::CENTER_TOP,
                    fps_text,
                    FontId::monospace(14.0),
                    egui::Color32::WHITE,
                );
            });
    }
    _ => {}
}
    // 2b: in direct mode the SDL present overwrites the framebuffer, so the
    // hold spinner is not drawn at all (the screen stays on the video). The
    // present still happens, so the pause menu transition is unaffected.
    if let Some(progress) = hold_progress
        && !crate::shell::direct_present::DIRECT_MODE
            .load(std::sync::atomic::Ordering::Relaxed)
    {
        egui::Area::new(egui::Id::new("pause_hold_indicator"))
            .order(egui::Order::Foreground)
            .anchor(egui::Align2::LEFT_TOP, egui::vec2(16.0, 16.0))
            .show(ctx, |ui| {
                draw_hold_progress_ring(ui, progress);
            });
    }

    // PS button held (capture on, stream live): show a small hint for as
    // long as the button is down — once the hold-arm has unlocked the shell
    // (>= 600 ms), releasing the button alone reaches the Vita home screen.
    if ps_hold_hint {
        egui::Area::new(egui::Id::new("ps_hold_hint"))
            .order(egui::Order::Foreground)
            .anchor(egui::Align2::CENTER_TOP, egui::vec2(0.0, 8.0))
            .show(ctx, |ui| {
                let painter = ui.painter();
                let text = i18n.text("streaming-ps-hold-hint");
                let font_id = FontId::proportional(14.0);
                let text_size = painter
                    .layout_no_wrap(text.clone(), font_id.clone(), egui::Color32::WHITE)
                    .size();
                let rect = egui::Rect::from_min_size(
                    egui::pos2(0.0, 0.0),
                    text_size + egui::vec2(16.0, 10.0),
                );
                painter.rect_filled(rect, 4.0, egui::Color32::from_black_alpha(128));
                painter.text(
                    rect.center(),
                    egui::Align2::CENTER_CENTER,
                    text,
                    font_id,
                    egui::Color32::WHITE,
                );
            });
    }
    });
}
