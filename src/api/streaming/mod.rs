//! Provider-neutral streaming backend consumed by the application.

pub(crate) mod rtc;

use crate::Stream;
use crate::api_xbox::streaming::backend::XboxStreamingBackend;
use crate::streaming::input::{GamepadFrame, PointerEvent};
use crate::streaming::video::{DecodedFrame, DirectVideoOutput};
use anyhow::Result;
use bytes::Bytes;
use std::sync::Arc;

pub(crate) enum PlaybackBackendEvent {
    Status(String),
    VideoResolution(u32, u32),
    Closed,
    Error(String),
}

/// Extension point for service-specific signaling, transport and input protocols.
pub(crate) enum PlaybackBackend {
    Xbox(XboxStreamingBackend),
}

impl PlaybackBackend {
    pub(crate) fn start_xbox(stream: Stream) -> Result<Self> {
        Ok(Self::Xbox(XboxStreamingBackend::start(stream)?))
    }

    pub(crate) fn try_recv_event(&mut self) -> Option<PlaybackBackendEvent> {
        match self {
            Self::Xbox(backend) => backend.try_recv_event(),
        }
    }

    pub(crate) fn try_recv_audio_packets(&self) -> Option<Vec<Bytes>> {
        match self {
            Self::Xbox(backend) => backend.try_recv_audio_packets(),
        }
    }

    pub(crate) fn take_latest_frame(&self) -> Option<(u64, DecodedFrame)> {
        match self {
            Self::Xbox(backend) => backend.take_latest_frame(),
        }
    }

    pub(crate) fn direct_video_output(&self) -> Arc<DirectVideoOutput> {
        match self {
            Self::Xbox(backend) => backend.direct_video_output(),
        }
    }

    pub(crate) fn send_gamepad_frame(&self, frame: GamepadFrame) {
        match self {
            Self::Xbox(backend) => backend.send_gamepad_frame(frame),
        }
    }

    pub(crate) fn send_gamepad_pulse(&self, frame: GamepadFrame) {
        match self {
            Self::Xbox(backend) => backend.send_gamepad_pulse(frame),
        }
    }

    pub(crate) fn send_pointer_event(&self, event: PointerEvent) {
        match self {
            Self::Xbox(backend) => backend.send_pointer_event(event),
        }
    }

    pub(crate) async fn maintain(&mut self) -> Option<String> {
        match self {
            Self::Xbox(backend) => backend.maintain().await,
        }
    }

    pub(crate) fn description(&self) -> String {
        match self {
            Self::Xbox(backend) => backend.description(),
        }
    }

    pub(crate) async fn stop(self) -> Result<()> {
        match self {
            Self::Xbox(backend) => backend.stop().await,
        }
    }
}
