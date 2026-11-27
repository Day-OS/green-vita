//! Service-neutral input state produced by the Vita UI.
//!
//! Each streaming backend is responsible for translating these values to its
//! own wire protocol.

#[derive(Debug, Clone, Default, PartialEq)]
pub struct GamepadFrame {
    pub gamepad_index: u8,
    pub nexus: f32,
    pub menu: f32,
    pub view: f32,
    pub a: f32,
    pub b: f32,
    pub x: f32,
    pub y: f32,
    pub dpad_up: f32,
    pub dpad_down: f32,
    pub dpad_left: f32,
    pub dpad_right: f32,
    pub left_shoulder: f32,
    pub right_shoulder: f32,
    pub left_thumb: f32,
    pub right_thumb: f32,
    pub left_thumb_x_axis: f32,
    pub left_thumb_y_axis: f32,
    pub right_thumb_x_axis: f32,
    pub right_thumb_y_axis: f32,
    pub left_trigger: f32,
    pub right_trigger: f32,
}

impl GamepadFrame {
    /// True when no button is pressed and every axis is inside a small
    /// deadzone. Used to re-arm input after the pause menu closes: the resume
    /// button is still held, so input stays blocked until the pad is neutral.
    pub fn is_neutral(&self) -> bool {
        let buttons = [
            self.nexus,
            self.menu,
            self.view,
            self.a,
            self.b,
            self.x,
            self.y,
            self.dpad_up,
            self.dpad_down,
            self.dpad_left,
            self.dpad_right,
            self.left_shoulder,
            self.right_shoulder,
            self.left_thumb,
            self.right_thumb,
            self.left_trigger,
            self.right_trigger,
        ];
        buttons.iter().all(|v| *v == 0.0)
            && self.left_thumb_x_axis.abs() < 0.05
            && self.left_thumb_y_axis.abs() < 0.05
            && self.right_thumb_x_axis.abs() < 0.05
            && self.right_thumb_y_axis.abs() < 0.05
    }
}

#[derive(Debug, Clone, Default)]
pub struct PointerEvent {
    pub contact_major: u16,
    pub contact_minor: u16,
    pub pressure: u8,
    pub twist: u16,
    pub x: u32,
    pub y: u32,
    pub event_type: u8,
}
