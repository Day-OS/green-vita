#[link(name = "SDL2", kind = "static")]
#[link(name = "SceAudio_stub", kind = "static")]
unsafe extern "C" {}

pub(crate) mod direct_present;
mod egui_painter;
pub(crate) mod netctl;
pub(crate) mod netprobe;
pub(crate) mod power;
mod surface;

use crate::app::ui::build_ui;
use crate::input::{
    RearTouchButtons, back_button_held, held_menu_direction, map_controller_button_event,
    map_keyboard_event, map_pointer_event, map_stream_pointer_event, open_first_controller,
    read_gamepad_frame,
};
use crate::streaming::audio::AudioRenderer;
use crate::streaming::video::{STREAM_HEIGHT, STREAM_WIDTH};
use crate::{App, AppState, InputCommand, NavigationCommand};
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
const STREAM_INPUT_POLL_INTERVAL: Duration = Duration::from_millis(4);
/// How often the vblank counter is polled while the UI loop waits (perf plan stage 2a).
#[cfg(target_os = "vita")]
const VBLANK_POLL_INTERVAL: Duration = Duration::from_millis(1);

pub(crate) const TARGET_FRAME_TIME: Duration = Duration::from_micros(16_700);

/// Vblank counter of the display (wraps at 16 bits). Used by the UI loop to run
/// exactly once per vblank instead of on a relative timer.
#[cfg(target_os = "vita")]
fn vblank_count() -> u32 {
    unsafe { vitasdk_sys::sceDisplayGetVcount() as u32 }
}

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
    let mut relay_view_until: Option<Instant> = None;
    let mut rear_touch_buttons = RearTouchButtons::default();
    let mut held_direction: Option<InputCommand> = None;
    let mut held_direction_since = Instant::now();
    let mut last_direction_repeat_at = Instant::now();
    let mut vita_ime_active = false;
    // Perf-plan stage 0 probe: start instant of the previous loop iteration.
    let mut previous_loop_start: Option<Instant> = None;
    let mut vita_ime_target: Option<crate::app::ImeTarget> = None;
    let mut vita_ime_pending = None;
    // Perf-plan stage 2b: egui events accumulate across loop iterations; they
    // are consumed on UI frames and capped while the UI is skipped.
    let mut egui_events: Vec<egui::Event> = Vec::new();

    loop {
        let loop_started_at = Instant::now();
        // Perf-plan stage 2a: vblank counter at the start of this iteration.
        #[cfg(target_os = "vita")]
        let vblank_at_start = vblank_count();
        // Perf-plan stage 0 probe (observe-only): loop period (start to start)
        // and, further below, the work time of this iteration.
        if let Some(previous) = previous_loop_start {
            crate::streaming::video::metrics::METRICS
                .ui_loop_us
                .store(
                    loop_started_at.duration_since(previous).as_micros() as u64,
                    std::sync::atomic::Ordering::Relaxed,
                );
        }
        previous_loop_start = Some(loop_started_at);
        // Main-thread liveness for the PS-button watchdog (brainstorm §6), even in menus.
        crate::ps_button::bump_heartbeat();
        let mut direct_commands = Vec::new();
        if vita_ime_target.is_none()
            && let Some(target) = app.ime_request.take()
        {
            vita_ime_pending = None;
            video.text_input().start();
            vita_ime_target = Some(target);
        }

        for event in event_pump.poll_iter() {
            rear_touch_buttons.handle_event(&event);
            let ime_owned_event = vita_ime_target.is_some();
            if ime_owned_event {
                match &event {
                    Event::TextInput { text, .. } => vita_ime_pending = Some(text.clone()),
                    Event::KeyDown {
                        keycode: Some(sdl2::keyboard::Keycode::Return),
                        ..
                    } => {
                        if let Some(text) = vita_ime_pending.take()
                            && let Some(target) = vita_ime_target
                        {
                            app.submit_ime_text(target, text);
                        }
                        video.text_input().stop();
                        vita_ime_target = None;
                    }
                    _ => {}
                }
            }
            // SDL may emit both events for one press.
            if !ime_owned_event
                && let Some(command) = map_keyboard_event(&event)
                && !direct_commands.contains(&command)
            {
                direct_commands.push(command);
            }
            if !ime_owned_event
                && let Some(command) = map_controller_button_event(&event)
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
                && !streaming.front_touch_auxiliary_buttons(&app.settings)
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

        if vita_ime_target.is_some()
            && !video
                .text_input()
                .is_screen_keyboard_shown(surface.canvas.window())
        {
            video.text_input().stop();
            vita_ime_pending = None;
            vita_ime_target = None;
        }

        match vita_ime_target
            .is_none()
            .then(|| held_menu_direction(controller.as_ref()))
            .flatten()
        {
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
        // Back doubles as game input (View) and as the hold-to-pause gesture:
        // withheld while held.  On short release, we replay the hold for
        // exactly its duration so the game sees a proper press→release
        // sequence.
        let mut relay_back_as_view = false;
        // PS-button lock/gesture predicate: on only while actively streaming unpaused —
        // off in menus/lists, the pause overlay and Settings-over-stream (brainstorm §5),
        // where PS works natively.
        let streaming_unpaused =
            matches!(&app.state, AppState::Streaming(streaming) if !streaming.paused);
        if streaming_unpaused {
            // Check if we're still replaying a previous short press
            if let Some(until) = relay_view_until {
                if Instant::now() < until {
                    relay_back_as_view = true;
                } else {
                    relay_view_until = None;
                }
            }
            if back_button_held(controller.as_ref()) {
                let held_since = *back_hold_since.get_or_insert_with(Instant::now);
                // The Select long-press is the sole Select-based menu gesture
                // (hold 1.5 s); a short press is replayed to the game as View
                // on release below.
                let elapsed = held_since.elapsed();
                hold_progress = Some(
                    (elapsed.as_secs_f32() / PAUSE_HOLD_DURATION.as_secs_f32()).clamp(0.0, 1.0),
                );
                if elapsed >= PAUSE_HOLD_DURATION {
                    direct_commands.push(NavigationCommand::OpenPauseOverlay.into());
                    back_hold_since = None;
                    relay_view_until = None; // long press → no relay to game
                }
            } else if let Some(held_since) = back_hold_since.take() {
                // Short press: replay the hold for exactly its duration
                let elapsed = held_since.elapsed();
                if elapsed < PAUSE_HOLD_DURATION {
                    relay_view_until = Some(Instant::now() + elapsed);
                    relay_back_as_view = true;
                }
            }
        } else {
            back_hold_since = None;
        }

        // PS-button gesture machine (brainstorm §5 phase 2): tap = Xbox guide
        // (same nexus pulse as the overlay row; session live is guaranteed since
        // GuidePressed only fires unpaused), hold >= 600 ms = unlock — the
        // shell minimizes on the released press, so the arm event carries no
        // UI state of its own.
        match crate::ps_button::tick(app.settings.ps_button_capture, streaming_unpaused) {
            crate::ps_button::PsButtonTick::GuidePressed => {
                if let Some(streaming) = app.state.streaming_mut() {
                    streaming.press_guide_button();
                }
            }
            crate::ps_button::PsButtonTick::PassthroughArmed { .. } => {}
            crate::ps_button::PsButtonTick::Idle => {}
        }
        // Watchdog guards the main thread while the lock may be held; arming
        // follows session liveness (Settings-over-stream keeps the session alive).
        crate::ps_button::set_watchdog_armed(app.state.streaming().is_some());

        for command in direct_commands {
            app.handle_command(command).await?;
        }

        send_stream_gamepad_state(
            &mut app,
            controller.as_ref(),
            raw_joystick.as_ref(),
            &rear_touch_buttons,
            relay_back_as_view,
        );

        app.tick().await?;
        if let Some(streaming) = app.state.streaming_mut() {
            audio_renderer.submit_packets(streaming.take_audio_packets());
        }
        // HUD hint for as long as the PS button is physically held (capture
        // on, stream live): reflects the last 4 ms poll sample from
        // `ps_button::tick`.
        let ps_hold_hint =
            app.settings.ps_button_capture && streaming_unpaused && crate::ps_button::is_pressed();

        // Perf-plan stage 2b: while streaming unpaused with the video HUD off
        // and no gesture pending, present the frame without running the UI
        // toolkit (its work measured 1.3-1.5 ms per frame on the Vita). egui
        // events are carried over to the next UI frame.
        let ui_needed = hold_progress.is_some()
            || ps_hold_hint
            || vita_ime_active
            || app.settings.latency_test_mode
            || !matches!(&app.state, AppState::Streaming(s) if !s.paused)
            || app.settings.stream_debug_mode != crate::settings::StreamDebugMode::Off
            || !app
                .state
                .streaming()
                .map(|s| s.video_frame().is_some())
                .unwrap_or(false);
        surface.sync_video_frame(app.state.streaming())?;

        // 2b: in direct mode, do not run the egui pass while the hold-to-menu
        // spinner is showing -- the video path keeps presenting the frame, so
        // the screen stays on the game instead of going black. When the hold
        // completes (paused) the menu presents through SDL as before.
        let suppress_ui = crate::shell::direct_present::DIRECT_MODE
            .load(std::sync::atomic::Ordering::Relaxed)
            && hold_progress.is_some()
            && !matches!(&app.state, AppState::Streaming(s) if s.paused);

        if ui_needed && !suppress_ui {
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
                events: std::mem::take(&mut egui_events),
                ..Default::default()
            };
            let mut ui_commands = Vec::new();
            let full_output = egui_ctx.run(raw_input, |ctx| {
                ui_commands = build_ui(ctx, &app, hold_progress, ps_hold_hint);
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
        } else {
            // Carry the events over, capped so a touch storm cannot grow the
            // buffer unboundedly while the UI is skipped.
            if egui_events.len() > 256 {
                egui_events.drain(..egui_events.len() - 256);
            }
            surface.draw_scene(true)?;
            if !surface.present_direct()? {
                surface.canvas.present();
            }
        }
        crate::streaming::video::metrics::METRICS
            .ui_work_us
            .store(
                loop_started_at.elapsed().as_micros() as u64,
                std::sync::atomic::Ordering::Relaxed,
            );
        #[cfg(target_os = "vita")]
        {
            // Perf-plan stage 2a: pace by the display's vblank instead of a relative
            // timer. The relative deadline overshot to 17.8 ms per iteration (56
            // loops/s) for ~59 decoded frames/s. Polling the vblank counter makes
            // the loop run exactly once per vblank whenever the frame's work fits.
            // Input polling keeps its own cadence (STREAM_INPUT_POLL_INTERVAL).
            //
            // 2026-11-26 fix: input is sampled ONCE here, before the wait, so a
            // frame whose work exceeds the remaining vblank can never skip the
            // poll entirely (the `while` would exit on its first check). That
            // regression was invisible at 1.4 ms of work but appears as soon as
            // something forces the egui pass -- latency test mode measured as
            // "slightly less responsive" for exactly this reason.
            event_pump.pump_events();
            send_stream_gamepad_state(
                &mut app,
                controller.as_ref(),
                raw_joystick.as_ref(),
                &rear_touch_buttons,
                false,
            );
            let mut since_input_poll = Duration::ZERO;
            while vblank_count() == vblank_at_start {
                // Latency (2026-11-26): do not sit out the whole vblank when a
                // new frame is already waiting. The pickup is the vblank phase,
                // not vsync (the renderer has no PRESENTVSYNC), so breaking
                // here lets the same iteration sync/draw/present immediately.
                // If no frame is new, the wait still paces the loop at 60 Hz.
                if surface.has_new_frame(app.state.streaming()) {
                    break;
                }
                sleep(VBLANK_POLL_INTERVAL).await;
                since_input_poll += VBLANK_POLL_INTERVAL;
                if since_input_poll >= STREAM_INPUT_POLL_INTERVAL {
                    since_input_poll = Duration::ZERO;

                    // SDL controller state is refreshed independently of rendering. Events remain
                    // queued for the normal UI pass at the start of the next frame.
                    event_pump.pump_events();
                    send_stream_gamepad_state(
                        &mut app,
                        controller.as_ref(),
                        raw_joystick.as_ref(),
                        &rear_touch_buttons,
                        false,
                    );
                }
            }
        }
        #[cfg(not(target_os = "vita"))]
        {
            // Other targets keep the original relative-deadline pacing.
            let frame_deadline = loop_started_at + TARGET_FRAME_TIME;
            if Instant::now() < frame_deadline {
                while Instant::now() < frame_deadline {
                    let remaining = frame_deadline.saturating_duration_since(Instant::now());
                    sleep(remaining.min(STREAM_INPUT_POLL_INTERVAL)).await;
                    if Instant::now() >= frame_deadline {
                        break;
                    }

                    event_pump.pump_events();
                    send_stream_gamepad_state(
                        &mut app,
                        controller.as_ref(),
                        raw_joystick.as_ref(),
                        &rear_touch_buttons,
                        false,
                    );
                }
            } else {
                tokio::task::yield_now().await;
            }
        }
    }
}

/// Pause-menu edge guard: true while the pause menu is open. The first sample
/// after it closes is dropped, because the button that triggered "return to
/// game" (X) is still held and would otherwise leak into the game as a press.
static WAS_PAUSED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Set when the pause menu closes; input stays blocked until the gamepad is
/// neutral (see `GamepadFrame::is_neutral`). A single dropped sample is not
/// enough -- the resume button is held for ~100 ms, so the 4 ms samples after
/// it would still leak.
static BLOCK_UNTIL_NEUTRAL: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

fn send_stream_gamepad_state(
    app: &mut App,
    controller: Option<&sdl2::controller::GameController>,
    raw_joystick: Option<&sdl2::joystick::Joystick>,
    rear_touch_buttons: &RearTouchButtons,
    relay_back_as_view: bool,
) {
    let paused = matches!(&app.state, AppState::Streaming(s) if s.paused);
    let just_resumed =
        WAS_PAUSED.swap(paused, std::sync::atomic::Ordering::Relaxed) && !paused;
    if just_resumed {
        BLOCK_UNTIL_NEUTRAL.store(true, std::sync::atomic::Ordering::Relaxed);
    }
    let settings = &app.settings;
    let (rear_touch_enabled, front_touch_auxiliary_buttons) = match &app.state {
        AppState::Streaming(streaming) if !streaming.paused => (
            streaming.rear_touch_enabled(settings),
            streaming.front_touch_auxiliary_buttons(settings),
        ),
        _ => return,
    };
    let AppState::Streaming(streaming) = &mut app.state else {
        return;
    };
    let Some(mut frame) = read_gamepad_frame(
        controller,
        raw_joystick,
        rear_touch_buttons,
        rear_touch_enabled,
        front_touch_auxiliary_buttons,
    ) else {
        return;
    };

    // After the pause menu closes, block input until the pad is neutral: the
    // resume button is still held and would otherwise leak into the game.
    if BLOCK_UNTIL_NEUTRAL.load(std::sync::atomic::Ordering::Relaxed) {
        if frame.is_neutral() {
            BLOCK_UNTIL_NEUTRAL.store(false, std::sync::atomic::Ordering::Relaxed);
        } else {
            return;
        }
    }

    // Detect button press edges for latency test mode (square/stopwatch).
    // This is intentionally cheap and does not alter the frame contents or timing.
    crate::input::detect_and_note_button_press(&frame);

    // Back is relayed separately after the hold gesture resolves.
    frame.view = f32::from(relay_back_as_view);
    streaming.send_gamepad_frame(frame, settings);
}
