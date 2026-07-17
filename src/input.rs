use crate::streaming::control::input::{GamepadFrame, PointerEvent};
use crate::{AppCommand, InputCommand};
use sdl2::controller::{Axis, Button, GameController};
use sdl2::event::Event;
use sdl2::joystick::Joystick;
use sdl2::keyboard::Keycode;
use sdl2::mouse::MouseButton;
use std::collections::HashMap;

pub fn map_keyboard_event(event: &Event) -> Option<AppCommand> {
    let Event::KeyDown {
        keycode: Some(key),
        repeat: false,
        ..
    } = event
    else {
        return None;
    };
    let command = match *key {
        Keycode::Escape => InputCommand::Back,
        Keycode::Return => InputCommand::Confirm,
        Keycode::Up => InputCommand::MoveUp,
        Keycode::Down => InputCommand::MoveDown,
        Keycode::Left => InputCommand::MoveLeft,
        Keycode::Right => InputCommand::MoveRight,
        _ => return None,
    };
    Some(command.into())
}

pub fn map_controller_button_event(event: &Event) -> Option<AppCommand> {
    match event {
        Event::ControllerButtonDown {
            button: Button::B, ..
        } => Some(InputCommand::Back.into()),
        Event::ControllerButtonDown {
            button: Button::A, ..
        } => Some(InputCommand::Confirm.into()),
        _ => None,
    }
}

const MENU_STICK_DEADZONE: f32 = 0.5;

pub fn held_menu_direction(controller: Option<&GameController>) -> Option<InputCommand> {
    let controller = controller?;
    if controller.button(Button::DPadUp) {
        return Some(InputCommand::MoveUp);
    }
    if controller.button(Button::DPadDown) {
        return Some(InputCommand::MoveDown);
    }
    if controller.button(Button::DPadLeft) {
        return Some(InputCommand::MoveLeft);
    }
    if controller.button(Button::DPadRight) {
        return Some(InputCommand::MoveRight);
    }
    let x = axis_to_f32(controller.axis(Axis::LeftX));
    let y = axis_to_f32(controller.axis(Axis::LeftY));
    if y.abs() >= x.abs() {
        match y {
            y if y <= -MENU_STICK_DEADZONE => Some(InputCommand::MoveUp),
            y if y >= MENU_STICK_DEADZONE => Some(InputCommand::MoveDown),
            _ => None,
        }
    } else {
        match x {
            x if x <= -MENU_STICK_DEADZONE => Some(InputCommand::MoveLeft),
            x if x >= MENU_STICK_DEADZONE => Some(InputCommand::MoveRight),
            _ => None,
        }
    }
}

pub fn register_vita_controller_mapping(sdl: &sdl2::Sdl) -> Result<(), String> {
    let joystick_subsystem = sdl.joystick()?;
    if joystick_subsystem
        .num_joysticks()
        .map_err(|e| e.to_string())?
        == 0
    {
        return Ok(());
    }
    let guid = joystick_subsystem
        .device_guid(0)
        .map_err(|e| e.to_string())?;
    let mapping = format!(
        "{guid},PSVita Controller,\
         a:b2,b:b1,x:b3,y:b0,\
         back:b10,start:b11,\
         leftshoulder:b4,rightshoulder:b5,\
         leftstick:b14,rightstick:b15,\
         dpup:b8,dpdown:b6,dpleft:b7,dpright:b9,\
         leftx:a0,lefty:a1,rightx:a2,righty:a3,\
         lefttrigger:b12,righttrigger:b13,platform:PS Vita,"
    );
    sdl.game_controller()?
        .add_mapping(&mapping)
        .map_err(|e| e.to_string())?;
    Ok(())
}

pub fn open_first_controller(subsystem: &sdl2::GameControllerSubsystem) -> Option<GameController> {
    let available = subsystem.num_joysticks().ok()?;
    (0..available).find_map(|id| {
        if !subsystem.is_game_controller(id) {
            return None;
        }
        subsystem.open(id).ok()
    })
}

pub fn back_button_held(controller: Option<&GameController>) -> bool {
    controller.is_some_and(|controller| controller.button(Button::Back))
}

/// Wire-format gamepad frame sent over the "input" data channel each tick.
pub fn read_gamepad_frame(
    controller: Option<&GameController>,
    raw_joystick: Option<&Joystick>,
    touch_buttons: &RearTouchButtons,
    front_touch_auxiliary_buttons: bool,
) -> Option<GamepadFrame> {
    let controller = controller?;
    let button = |b: Button| f32::from(controller.button(b));
    Some(GamepadFrame {
        gamepad_index: 0,
        nexus: button(Button::Guide),
        menu: button(Button::Start),
        view: button(Button::Back),
        a: button(Button::A),
        b: button(Button::B),
        x: button(Button::X),
        y: button(Button::Y),
        dpad_up: button(Button::DPadUp),
        dpad_down: button(Button::DPadDown),
        dpad_left: button(Button::DPadLeft),
        dpad_right: button(Button::DPadRight),
        left_shoulder: button(Button::LeftShoulder),
        right_shoulder: button(Button::RightShoulder),
        left_thumb: f32::from(
            controller.button(Button::LeftStick)
                || touch_buttons.pressed(RearTouchButton::L3, front_touch_auxiliary_buttons),
        ),
        right_thumb: f32::from(
            controller.button(Button::RightStick)
                || touch_buttons.pressed(RearTouchButton::R3, front_touch_auxiliary_buttons),
        ),
        left_thumb_x_axis: axis_to_f32(controller.axis(Axis::LeftX)),
        left_thumb_y_axis: axis_to_f32(controller.axis(Axis::LeftY)),
        right_thumb_x_axis: axis_to_f32(controller.axis(Axis::RightX)),
        right_thumb_y_axis: axis_to_f32(controller.axis(Axis::RightY)),
        left_trigger: trigger_value(
            controller,
            raw_joystick,
            Axis::TriggerLeft,
            4,
            12,
            touch_buttons.pressed(RearTouchButton::L2, front_touch_auxiliary_buttons),
        ),
        right_trigger: trigger_value(
            controller,
            raw_joystick,
            Axis::TriggerRight,
            5,
            13,
            touch_buttons.pressed(RearTouchButton::R2, front_touch_auxiliary_buttons),
        ),
    })
}

/// Combines the SDL gamepad mapping, the Vita driver's raw analog/button values, and rear touch.
fn trigger_value(
    controller: &GameController,
    raw_joystick: Option<&Joystick>,
    mapped_axis: Axis,
    raw_axis_index: u32,
    raw_button_index: u32,
    rear_touch_pressed: bool,
) -> f32 {
    let mapped_value = controller.axis(mapped_axis).max(0) as f32 / i16::MAX as f32;
    let raw_axis_value = raw_joystick
        .and_then(|joystick| joystick.axis(raw_axis_index).ok())
        .map(normalize_raw_trigger_axis)
        .unwrap_or(0.0);
    let raw_button_pressed = raw_joystick
        .and_then(|joystick| joystick.button(raw_button_index).ok())
        .unwrap_or(false);

    mapped_value
        .max(raw_axis_value)
        .max(f32::from(raw_button_pressed || rear_touch_pressed))
}

/// SDL's Vita driver maps the native 0..=255 trigger byte over the signed joystick range.
fn normalize_raw_trigger_axis(raw: i16) -> f32 {
    let value = (raw as f32 - i16::MIN as f32) / u16::MAX as f32;
    if value < 0.02 { 0.0 } else { value }
}

fn axis_to_f32(raw: i16) -> f32 {
    raw as f32 / i16::MAX as f32
}

/// The front touchscreen's own SDL touch device id (registered second on the Vita's touch
/// backend, `SDL_vitatouch.c`: `SDL_AddTouch(1, ..., "Front")`, `SDL_AddTouch(2, ..., "Back")`).
const FRONT_TOUCH_DEVICE_ID: i64 = 1;
const REAR_TOUCH_DEVICE_ID: i64 = 2;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum RearTouchButton {
    L2,
    R2,
    L3,
    R3,
}

/// Tracks the Vita's rear auxiliary zones and, optionally, the same zones on front touch.
#[derive(Default)]
pub struct RearTouchButtons {
    rear_fingers: HashMap<i64, RearTouchButton>,
    front_fingers: HashMap<i64, RearTouchButton>,
}

impl RearTouchButtons {
    pub fn handle_event(&mut self, event: &Event) {
        match *event {
            Event::FingerDown {
                touch_id,
                finger_id,
                x,
                y,
                ..
            }
            | Event::FingerMotion {
                touch_id,
                finger_id,
                x,
                y,
                ..
            } if touch_id == REAR_TOUCH_DEVICE_ID || touch_id == FRONT_TOUCH_DEVICE_ID => {
                let button = match (x < 0.5, y < 0.5) {
                    (true, true) => RearTouchButton::L2,
                    (false, true) => RearTouchButton::R2,
                    (true, false) => RearTouchButton::L3,
                    (false, false) => RearTouchButton::R3,
                };
                self.fingers_mut(touch_id).insert(finger_id, button);
            }
            Event::FingerUp {
                touch_id,
                finger_id,
                ..
            } if touch_id == REAR_TOUCH_DEVICE_ID || touch_id == FRONT_TOUCH_DEVICE_ID => {
                self.fingers_mut(touch_id).remove(&finger_id);
            }
            _ => {}
        }
    }

    fn fingers_mut(&mut self, touch_id: i64) -> &mut HashMap<i64, RearTouchButton> {
        if touch_id == FRONT_TOUCH_DEVICE_ID {
            &mut self.front_fingers
        } else {
            &mut self.rear_fingers
        }
    }

    fn pressed(&self, button: RearTouchButton, include_front: bool) -> bool {
        self.rear_fingers.values().any(|pressed| *pressed == button)
            || (include_front
                && self
                    .front_fingers
                    .values()
                    .any(|pressed| *pressed == button))
    }
}

/// The Vita's SDL2 port has no touch-to-mouse emulation, so taps arrive as Finger* events, not
/// Mouse* translated into pointer events here. Rear touch is reserved for L2/R2/L3/R3.
pub fn map_pointer_event(
    event: &Event,
    screen_size: (f32, f32),
    pixels_per_point: f32,
    pointer_pos: &mut egui::Pos2,
) -> Option<egui::Event> {
    match *event {
        // Mouse coords are real screen pixels, so divide by pixels_per_point to get egui points.
        Event::MouseMotion { x, y, .. } => {
            *pointer_pos = mouse_to_screen_pos(x, y, pixels_per_point);
            Some(egui::Event::PointerMoved(*pointer_pos))
        }
        Event::MouseButtonDown {
            mouse_btn, x, y, ..
        } => Some(pointer_button_at(
            pointer_pos,
            mouse_to_screen_pos(x, y, pixels_per_point),
            map_mouse_button(mouse_btn),
            true,
        )),
        Event::MouseButtonUp {
            mouse_btn, x, y, ..
        } => Some(pointer_button_at(
            pointer_pos,
            mouse_to_screen_pos(x, y, pixels_per_point),
            map_mouse_button(mouse_btn),
            false,
        )),
        Event::MouseWheel { x, y, .. } => Some(egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Line,
            delta: egui::vec2(x as f32, y as f32),
            modifiers: egui::Modifiers::default(),
        }),
        Event::FingerDown { touch_id, x, y, .. } if touch_id == FRONT_TOUCH_DEVICE_ID => {
            Some(pointer_button_at(
                pointer_pos,
                touch_to_screen_pos(x, y, screen_size),
                egui::PointerButton::Primary,
                true,
            ))
        }
        Event::FingerMotion { touch_id, x, y, .. } if touch_id == FRONT_TOUCH_DEVICE_ID => {
            *pointer_pos = touch_to_screen_pos(x, y, screen_size);
            Some(egui::Event::PointerMoved(*pointer_pos))
        }
        Event::FingerUp { touch_id, x, y, .. } if touch_id == FRONT_TOUCH_DEVICE_ID => {
            Some(pointer_button_at(
                pointer_pos,
                touch_to_screen_pos(x, y, screen_size),
                egui::PointerButton::Primary,
                false,
            ))
        }
        _ => None,
    }
}

fn pointer_button_at(
    pointer_pos: &mut egui::Pos2,
    pos: egui::Pos2,
    button: egui::PointerButton,
    pressed: bool,
) -> egui::Event {
    *pointer_pos = pos;
    egui::Event::PointerButton {
        pos,
        button,
        pressed,
        modifiers: egui::Modifiers::default(),
    }
}

fn mouse_to_screen_pos(x: i32, y: i32, pixels_per_point: f32) -> egui::Pos2 {
    egui::pos2(x as f32 / pixels_per_point, y as f32 / pixels_per_point)
}

fn touch_to_screen_pos(x: f32, y: f32, (width, height): (f32, f32)) -> egui::Pos2 {
    egui::pos2(x * width, y * height)
}

fn map_mouse_button(button: MouseButton) -> egui::PointerButton {
    match button {
        MouseButton::Right => egui::PointerButton::Secondary,
        MouseButton::Middle => egui::PointerButton::Middle,
        _ => egui::PointerButton::Primary,
    }
}

/// Same taps as `map_pointer_event`, but translated into an xCloud pointer event scaled from the
/// Vita's screen into the streamed video's resolution - `None` outside the video area or letterbox.
pub fn map_stream_pointer_event(
    event: &Event,
    window_size: (f32, f32),
    video_rect: sdl2::rect::Rect,
    stream_size: (u32, u32),
) -> Option<PointerEvent> {
    let (event_type, window_pos) = match *event {
        Event::MouseButtonDown { x, y, .. } => (1u8, (x as f32, y as f32)),
        Event::MouseButtonUp { x, y, .. } => (2u8, (x as f32, y as f32)),
        Event::MouseMotion { x, y, .. } => (3u8, (x as f32, y as f32)),
        Event::FingerDown { touch_id, x, y, .. } if touch_id == FRONT_TOUCH_DEVICE_ID => {
            (1u8, (x * window_size.0, y * window_size.1))
        }
        Event::FingerUp { touch_id, x, y, .. } if touch_id == FRONT_TOUCH_DEVICE_ID => {
            (2u8, (x * window_size.0, y * window_size.1))
        }
        Event::FingerMotion { touch_id, x, y, .. } if touch_id == FRONT_TOUCH_DEVICE_ID => {
            (3u8, (x * window_size.0, y * window_size.1))
        }
        _ => return None,
    };

    // xCloud expects a zeroed contact/position on release, wherever the finger actually lifted.
    if event_type == 2 {
        return Some(PointerEvent {
            event_type,
            ..Default::default()
        });
    }

    let (x, y) = map_touch_to_stream_pos(window_pos, video_rect, stream_size)?;
    Some(PointerEvent {
        contact_major: 1,
        contact_minor: 1,
        pressure: 255,
        twist: 0,
        x,
        y,
        event_type,
    })
}

fn map_touch_to_stream_pos(
    (x, y): (f32, f32),
    video_rect: sdl2::rect::Rect,
    (stream_width, stream_height): (u32, u32),
) -> Option<(u32, u32)> {
    let local_x = x - video_rect.x() as f32;
    let local_y = y - video_rect.y() as f32;
    if video_rect.width() == 0
        || video_rect.height() == 0
        || local_x < 0.0
        || local_y < 0.0
        || local_x > video_rect.width() as f32
        || local_y > video_rect.height() as f32
    {
        return None;
    }
    Some((
        (local_x / video_rect.width() as f32 * stream_width as f32).round() as u32,
        (local_y / video_rect.height() as f32 * stream_height as f32).round() as u32,
    ))
}
