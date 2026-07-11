use crate::streaming::rtc::rtp;
use crate::streaming::video::{DecodedFrame, DecoderConfig, VideoDecodeWorker};
use anyhow::Result;
use bytes::Bytes;
use rtc::media_stream::MediaStreamTrackId;
use rtc::rtp::Packet;
use rtc::rtp_transceiver::RTCRtpReceiverId;
use std::time::{Duration, Instant};

const STREAM_STATS_INTERVAL: Duration = Duration::from_secs(1);

#[derive(Default)]
struct VideoStats {
    dropped: u64,
    decode_errors: u64,
    last_sample_duration_us: Option<u64>,
}

pub(super) struct VideoReceiver {
    track_id: Option<MediaStreamTrackId>,
    receiver_id: Option<RTCRtpReceiverId>,
    ssrc: Option<u32>,
    rtp: rtp::VideoRtp,
    decoder: VideoDecodeWorker,
    current_frame: Option<DecodedFrame>,
    latest_frame: Option<u64>,
    next_frame_id: u64,
    last_stats_report: Instant,
    stats: VideoStats,
}

impl VideoReceiver {
    pub(super) fn new(config: DecoderConfig) -> Result<Self> {
        Ok(Self {
            track_id: None,
            receiver_id: None,
            ssrc: None,
            rtp: rtp::VideoRtp::new(),
            decoder: VideoDecodeWorker::spawn(config)?,
            current_frame: None,
            latest_frame: None,
            next_frame_id: 0,
            last_stats_report: Instant::now(),
            stats: VideoStats::default(),
        })
    }

    pub(super) fn open(
        &mut self,
        track_id: MediaStreamTrackId,
        receiver_id: RTCRtpReceiverId,
        ssrc: u32,
    ) {
        self.track_id = Some(track_id);
        self.receiver_id = Some(receiver_id);
        self.ssrc = Some(ssrc);
    }

    pub(super) fn handles(&self, track_id: &MediaStreamTrackId) -> bool {
        self.track_id.as_ref() == Some(track_id)
    }

    pub(super) fn receive(&mut self, packet: Packet, keyframe_requested: &mut bool) {
        let sample_stats = self.rtp.receive(&self.decoder, packet, keyframe_requested);
        self.stats.dropped = self
            .stats
            .dropped
            .saturating_add(sample_stats.dropped as u64);
        if sample_stats.source_frame_duration_us.is_some() {
            self.stats.last_sample_duration_us = sample_stats.source_frame_duration_us;
        }
    }

    pub(super) fn drain_decoder(&mut self, keyframe_requested: &mut bool) {
        let mut decode_errors = 0u64;
        while let Some(result) = self.decoder.try_recv() {
            match result {
                Ok(frame) => {
                    self.next_frame_id = self.next_frame_id.wrapping_add(1);
                    self.latest_frame = Some(self.next_frame_id);
                    self.current_frame = Some(frame);
                }
                Err(error) => {
                    eprintln!("Failed to decode H264 video frame: {error}");
                    decode_errors = decode_errors.saturating_add(1);
                    *keyframe_requested = true;
                }
            }
        }

        if decode_errors > 0 {
            self.stats.decode_errors = self.stats.decode_errors.saturating_add(decode_errors);
            self.decoder.reset_decoder();
            self.rtp.wait_for_keyframe();
        }
    }

    pub(super) fn take_new_frame(
        &mut self,
        last_sent_frame: &mut Option<u64>,
    ) -> Option<(u64, DecodedFrame)> {
        let frame_id = self.latest_frame?;
        if Some(frame_id) == *last_sent_frame {
            return None;
        }
        let frame = self.current_frame.take()?;
        *last_sent_frame = Some(frame_id);
        Some((frame_id, frame))
    }

    pub(super) fn rtcp_target(&self) -> Option<(RTCRtpReceiverId, u32)> {
        Some((self.receiver_id?, self.ssrc?))
    }

    pub(super) fn status(&mut self, now: Instant) -> Option<String> {
        if now.duration_since(self.last_stats_report) < STREAM_STATS_INTERVAL {
            return None;
        }
        self.last_stats_report = now;

        let performance = crate::streaming::video::video_performance_summary();
        let memory = crate::streaming::video::decoder_memory_summary();
        let source_fps = self
            .stats
            .last_sample_duration_us
            .filter(|duration| *duration > 0)
            .map(|duration| 1_000_000 / duration)
            .unwrap_or(0);
        Some(format!(
            "srcfps:{source_fps} {performance} {memory} wait:{} drop:{} err:{}",
            u8::from(self.rtp.waiting_for_keyframe()),
            self.stats.dropped,
            self.stats.decode_errors,
        ))
    }
}

pub(super) struct AudioReceiver {
    track_id: Option<MediaStreamTrackId>,
    rtp: rtp::AudioRtp,
    packets: Vec<Bytes>,
}

impl AudioReceiver {
    pub(super) fn new(sample_rate: u32) -> Self {
        Self {
            track_id: None,
            rtp: rtp::AudioRtp::new(sample_rate),
            packets: Vec::new(),
        }
    }

    pub(super) fn open(&mut self, track_id: MediaStreamTrackId) {
        self.track_id = Some(track_id);
    }

    pub(super) fn handles(&self, track_id: &MediaStreamTrackId) -> bool {
        self.track_id.as_ref() == Some(track_id)
    }

    pub(super) fn receive(&mut self, packet: Packet) {
        self.rtp.receive(packet, &mut self.packets);
    }

    pub(super) fn drain(&mut self) -> Vec<Bytes> {
        std::mem::take(&mut self.packets)
    }
}
