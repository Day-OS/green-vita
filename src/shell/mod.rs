#[link(name = "SDL2", kind = "static")]
#[link(name = "SceAudio_stub", kind = "static")]
unsafe extern "C" {}

mod egui_painter;
mod surface;

use crate::app::ui::build_ui;
use crate::input::{
    RearTouchButtons, back_button_held, held_menu_direction, map_controller_button_event,
    map_keyboard_event, map_pointer_event, map_stream_pointer_event, open_first_controller,
    read_gamepad_frame,
};
use crate::streaming::audio::AudioRenderer;
use crate::{App, AppState, InputCommand, NavigationCommand, STREAM_HEIGHT, STREAM_WIDTH};
use anyhow::{Context, Result};
use sdl2::event::Event;
use std::time::{Duration, Instant};
use surface::{HEIGHT, VitaSurface, WIDTH};
use tokio::time::sleep;

const PAUSE_HOLD_DURATION: Duration = Duration::from_millis(1500);
/// Scales `pixels_per_point` up so the UI reads legibly on the Vita's small screen.
const UI_SCALE: f32 = 1.3;
// D-Pad/left-stick auto-repeat: immediate on press, then repeating once held past the delay.
const DIRECTION_REPEAT_INITIAL_DELAY: Duration = Duration::from_millis(350);
const DIRECTION_REPEAT_INTERVAL: Duration = Duration::from_millis(90);

pub(crate) const TARGET_FRAME_TIME: Duration = Duration::from_millis(16);

pub async fn run(mut app: App) -> Result<()> {
    crate::streaming::video::reserve_decoder_cdram();

    let sdl = sdl2::init().map_err(anyhow::Error::msg)?;
    let video = sdl.video().map_err(anyhow::Error::msg)?;
    let audio = sdl.audio().map_err(anyhow::Error::msg)?;
    crate::input::register_vita_controller_mapping(&sdl).map_err(anyhow::Error::msg)?;
    let game_controller_subsystem = sdl.game_controller().map_err(anyhow::Error::msg)?;
    let mut controller = open_first_controller(&game_controller_subsystem);
    let joystick_subsystem = sdl.joystick().map_err(anyhow::Error::msg)?;
    let raw_joystick = joystick_subsystem.open(0).ok();
    let mut event_pump = sdl.event_pump().map_err(anyhow::Error::msg)?;
    let mut surface = VitaSurface::new(&video)?;
    let mut audio_renderer =
        AudioRenderer::new(&audio).context("failed to set up audio renderer")?;
    let egui_ctx = egui::Context::default();
    crate::app::ui::fonts::configure(&egui_ctx);
    let start_time = Instant::now();
    let mut pointer_pos = egui::Pos2::ZERO;
    let mut back_hold_since: Option<Instant> = None;
    let mut rear_touch_buttons = RearTouchButtons::default();
    let mut held_direction: Option<InputCommand> = None;
    let mut held_direction_since = Instant::now();
    let mut last_direction_repeat_at = Instant::now();

    loop {
        let loop_started_at = Instant::now();
        let mut egui_events = Vec::new();
        let mut direct_commands = Vec::new();

        for event in event_pump.poll_iter() {
            rear_touch_buttons.handle_event(&event);
            // SDL may emit both events for one press.
            if let Some(command) = map_keyboard_event(&event)
                && !direct_commands.contains(&command)
            {
                direct_commands.push(command);
            }
            if let Some(command) = map_controller_button_event(&event)
                && !direct_commands.contains(&command)
            {
                direct_commands.push(command);
            }
            if let Some(egui_event) = map_pointer_event(
                &event,
                (WIDTH as f32 / UI_SCALE, HEIGHT as f32 / UI_SCALE),
                UI_SCALE,
                &mut pointer_pos,
            ) {
                egui_events.push(egui_event);
            }
            if let AppState::Streaming(streaming) = &app.state
                && !streaming.paused
                && let Some(pointer_event) = map_stream_pointer_event(
                    &event,
                    (WIDTH as f32, HEIGHT as f32),
                    surface.video_rect(),
                    // Scale input into the active stream resolution.
                    streaming
                        .video_size()
                        .unwrap_or((STREAM_WIDTH, STREAM_HEIGHT)),
                )
            {
                streaming.send_pointer_event(pointer_event);
            }
            if let Event::ControllerDeviceAdded { .. } = event
                && controller.is_none()
            {
                controller = open_first_controller(&game_controller_subsystem);
            }
            if let Event::ControllerDeviceRemoved { .. } = event {
                controller = None;
            }
        }

        match held_menu_direction(controller.as_ref()) {
            Some(direction) if held_direction == Some(direction) => {
                if held_direction_since.elapsed() >= DIRECTION_REPEAT_INITIAL_DELAY
                    && last_direction_repeat_at.elapsed() >= DIRECTION_REPEAT_INTERVAL
                {
                    direct_commands.push(direction.into());
                    last_direction_repeat_at = Instant::now();
                }
            }
            Some(direction) => {
                direct_commands.push(direction.into());
                held_direction = Some(direction);
                held_direction_since = Instant::now();
                last_direction_repeat_at = Instant::now();
            }
            None => held_direction = None,
        }

        // Drives the top-left hold-progress ring in `build_ui`.
        let mut hold_progress: Option<f32> = None;
        // Back doubles as game input (View) and as the hold-to-pause gesture: withheld while
        // held, replayed as a single View tap only if released before the pause fired.
        let mut relay_back_as_view = false;
        if matches!(&app.state, AppState::Streaming(streaming) if !streaming.paused) {
            if back_button_held(controller.as_ref()) {
                let held_since = *back_hold_since.get_or_insert_with(Instant::now);
                let elapsed = held_since.elapsed();
                hold_progress = Some(
                    (elapsed.as_secs_f32() / PAUSE_HOLD_DURATION.as_secs_f32()).clamp(0.0, 1.0),
                );
                if elapsed >= PAUSE_HOLD_DURATION {
                    direct_commands.push(NavigationCommand::OpenPauseOverlay.into());
                    back_hold_since = None;
                }
            } else if let Some(held_since) = back_hold_since.take() {
                relay_back_as_view = held_since.elapsed() < PAUSE_HOLD_DURATION;
            }
        } else {
            back_hold_since = None;
        }

        for command in direct_commands {
            app.handle_command(command).await?;
        }

        let settings = &app.settings;
        if let AppState::Streaming(streaming) = &mut app.state
            && !streaming.paused
            && let Some(mut gamepad_frame) = read_gamepad_frame(
                controller.as_ref(),
                raw_joystick.as_ref(),
                &rear_touch_buttons,
            )
        {
            // Back is relayed separately after the hold gesture resolves.
            gamepad_frame.view = f32::from(relay_back_as_view);
            streaming.send_gamepad_frame(gamepad_frame, settings);
        }

        app.tick().await?;
        if let Some(streaming) = app.state.streaming_mut() {
            audio_renderer.submit_packets(streaming.take_audio_packets());
        }
        surface.upload_video_frame(app.state.streaming())?;

        let raw_input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(WIDTH as f32 / UI_SCALE, HEIGHT as f32 / UI_SCALE),
            )),
            viewport_id: egui::ViewportId::ROOT,
            viewports: std::iter::once((
                egui::ViewportId::ROOT,
                egui::ViewportInfo {
                    native_pixels_per_point: Some(UI_SCALE),
                    ..Default::default()
                },
            ))
            .collect(),
            time: Some(start_time.elapsed().as_secs_f64()),
            predicted_dt: TARGET_FRAME_TIME.as_secs_f32(),
            events: egui_events,
            ..Default::default()
        };

        let mut ui_commands = Vec::new();
        let full_output = egui_ctx.run(raw_input, |ctx| {
            ui_commands = build_ui(ctx, &app, hold_progress);
        });

        for command in ui_commands {
            app.handle_command(command).await?;
        }

        surface.draw_scene(matches!(&app.state, AppState::Streaming(_)))?;
        let clipped_primitives =
            egui_ctx.tessellate(full_output.shapes, full_output.pixels_per_point);
        surface.paint_egui(
            full_output.pixels_per_point,
            &clipped_primitives,
            &full_output.textures_delta,
        )?;

        let remaining_frame_time = TARGET_FRAME_TIME.saturating_sub(loop_started_at.elapsed());
        if !remaining_frame_time.is_zero() {
            sleep(remaining_frame_time).await;
        } else {
            tokio::task::yield_now().await;
        }
    }
}
