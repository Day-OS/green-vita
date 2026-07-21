//! Wire format for xCloud's "input" data channel: gamepad + pointer report packets.

use crate::streaming::input::{GamepadFrame, PointerEvent};
use std::time::Instant;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u16)]
pub enum ReportType {
    None = 0,
    Gamepad = 2,
    Pointer = 4,
    ClientMetadata = 8,
    ServerMetadata = 16,
}

#[derive(Debug, Clone, Default)]
pub struct PointerFrame {
    pub events: Vec<PointerEvent>,
}

#[derive(Debug, Clone)]
pub struct InputPacket {
    report_type: u16,
    total_size: usize,
    sequence: u32,
    gamepad_frames: Vec<GamepadFrame>,
    pointer_frames: Vec<PointerFrame>,
    max_touchpoints: u8,
    created_at: Instant,
}

impl InputPacket {
    pub fn new(sequence: u32) -> Self {
        Self {
            report_type: ReportType::None as u16,
            total_size: 14,
            sequence,
            gamepad_frames: Vec::new(),
            pointer_frames: Vec::new(),
            max_touchpoints: 0,
            created_at: Instant::now(),
        }
    }

    pub fn client_metadata(sequence: u32, max_touchpoints: u8) -> Self {
        let mut packet = Self::new(sequence);
        packet.report_type = ReportType::ClientMetadata as u16;
        packet.total_size = 15;
        packet.max_touchpoints = max_touchpoints;
        packet
    }

    pub fn set_data(&mut self, gamepads: Vec<GamepadFrame>, pointers: Vec<PointerFrame>) {
        let mut size = 14;
        if !gamepads.is_empty() {
            self.report_type |= ReportType::Gamepad as u16;
            size += 1 + 23 * gamepads.len();
        }
        if !pointers.is_empty() {
            self.report_type |= ReportType::Pointer as u16;
            size += 1 + pointers
                .iter()
                .map(|frame| 1 + frame.events.len() * 20)
                .sum::<usize>();
        }

        self.total_size = size;
        self.gamepad_frames = gamepads;
        self.pointer_frames = pointers;
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(self.total_size);
        push_le(&mut bytes, self.report_type.to_le_bytes());
        push_le(&mut bytes, self.sequence.to_le_bytes());
        bytes.extend_from_slice(&(self.created_at.elapsed().as_secs_f64() * 1000.0).to_le_bytes());

        if !self.gamepad_frames.is_empty() {
            self.write_gamepads(&mut bytes);
        }
        if !self.pointer_frames.is_empty() {
            self.write_pointers(&mut bytes);
        }
        if self.report_type == ReportType::ClientMetadata as u16 {
            bytes.push(self.max_touchpoints);
        }

        debug_assert_eq!(bytes.len(), self.total_size);
        bytes
    }

    fn write_gamepads(&self, bytes: &mut Vec<u8>) {
        bytes.push(self.gamepad_frames.len() as u8);
        for input in &self.gamepad_frames {
            bytes.push(input.gamepad_index);
            let mut mask = 0u16;
            let buttons = [
                (input.nexus, 2),
                (input.menu, 4),
                (input.view, 8),
                (input.a, 16),
                (input.b, 32),
                (input.x, 64),
                (input.y, 128),
                (input.dpad_up, 256),
                (input.dpad_down, 512),
                (input.dpad_left, 1024),
                (input.dpad_right, 2048),
                (input.left_shoulder, 4096),
                (input.right_shoulder, 8192),
                (input.left_thumb, 16384),
                (input.right_thumb, 32768),
            ];
            for (value, bit) in buttons {
                if value > 0.0 {
                    mask |= bit;
                }
            }
            push_le(bytes, mask.to_le_bytes());
            push_le(bytes, normalize_axis(input.left_thumb_x_axis).to_le_bytes());
            push_le(
                bytes,
                normalize_axis(-input.left_thumb_y_axis).to_le_bytes(),
            );
            push_le(
                bytes,
                normalize_axis(input.right_thumb_x_axis).to_le_bytes(),
            );
            push_le(
                bytes,
                normalize_axis(-input.right_thumb_y_axis).to_le_bytes(),
            );
            push_le(bytes, normalize_trigger(input.left_trigger).to_le_bytes());
            push_le(bytes, normalize_trigger(input.right_trigger).to_le_bytes());
            push_le(bytes, 1u32.to_le_bytes());
            bytes.extend_from_slice(&1u32.to_be_bytes());
        }
    }

    fn write_pointers(&self, bytes: &mut Vec<u8>) {
        bytes.push(1);
        if let Some(frame) = self.pointer_frames.first() {
            bytes.push(frame.events.len() as u8);
            for event in &frame.events {
                push_le(bytes, event.contact_major.to_le_bytes());
                push_le(bytes, event.contact_minor.to_le_bytes());
                bytes.push(event.pressure);
                push_le(bytes, event.twist.to_le_bytes());
                push_le(bytes, 0u32.to_le_bytes());
                push_le(bytes, event.x.to_le_bytes());
                push_le(bytes, event.y.to_le_bytes());
                bytes.push(event.event_type);
            }
        }
    }
}

fn normalize_trigger(value: f32) -> u16 {
    if value < 0.0 {
        0
    } else {
        (65535.0 * value).clamp(0.0, 65535.0) as u16
    }
}

fn normalize_axis(value: f32) -> i16 {
    let max = 32767.0;
    (value * max).clamp(-max, max) as i16
}

fn push_le<const N: usize>(bytes: &mut Vec<u8>, raw: [u8; N]) {
    bytes.extend_from_slice(&raw);
}

/// Batches queued frames into [`InputPacket`]s, one wire packet per send.
#[derive(Debug, Default)]
pub struct InputQueue {
    sequence: u32,
    gamepads: Vec<GamepadFrame>,
    pointers: Vec<PointerFrame>,
}

impl InputQueue {
    pub fn queue_gamepad_frames(
        &mut self,
        frames: impl IntoIterator<Item = GamepadFrame>,
        force_send: bool,
    ) -> Option<Vec<u8>> {
        self.gamepads.extend(frames);
        self.check_queue_and_packet(force_send)
    }

    pub fn queue_pointer_frame(&mut self, frame: PointerFrame) -> Option<Vec<u8>> {
        if let Some(existing) = self.pointers.first_mut() {
            existing.events.extend(frame.events);
        } else {
            self.pointers.push(frame);
        }
        self.check_queue_and_packet(true)
    }

    pub fn client_metadata_packet(&mut self, max_touchpoints: u8) -> Vec<u8> {
        InputPacket::client_metadata(self.next_sequence(), max_touchpoints).to_bytes()
    }

    fn check_queue_and_packet(&mut self, force_send: bool) -> Option<Vec<u8>> {
        let should_send = force_send || !self.gamepads.is_empty() || !self.pointers.is_empty();
        should_send.then(|| self.drain_packet())
    }

    fn drain_packet(&mut self) -> Vec<u8> {
        let mut packet = InputPacket::new(self.next_sequence());
        packet.set_data(
            std::mem::take(&mut self.gamepads),
            std::mem::take(&mut self.pointers),
        );
        packet.to_bytes()
    }

    fn next_sequence(&mut self) -> u32 {
        self.sequence = self.sequence.wrapping_add(1);
        self.sequence
    }
}
