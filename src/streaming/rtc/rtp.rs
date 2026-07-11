use crate::streaming::video::VideoDecodeWorker;
use bytes::Bytes;
use h264_reader::annexb::AnnexBReader;
use h264_reader::nal::sps::SeqParameterSet;
use h264_reader::nal::{Nal, RefNal, UnitType};
use h264_reader::push::NalInterest;
use rtc::rtp::Packet;
use rtc::rtp::codec::h264::H264Packet;
use rtc::rtp::codec::opus::OpusPacket;
use rtc_media::io::sample_builder::SampleBuilder;

const OPUS_PAYLOAD_TYPE: u8 = 111;
const MAX_PENDING_AUDIO_PACKETS: usize = 32;
const MAX_H264_ACCESS_UNIT_BYTES: usize = 2 * 1024 * 1024;
const VIDEO_RTP_CLOCK_RATE: u32 = 90_000;
const VIDEO_MAX_LATE_PACKETS: u16 = 64;
const AUDIO_MAX_LATE_PACKETS: u16 = 32;

#[derive(Default)]
pub(super) struct VideoSampleStats {
    pub dropped: u32,
    pub source_frame_duration_us: Option<u64>,
}

pub(super) struct AudioRtp {
    samples: SampleBuilder<OpusPacket>,
}

impl AudioRtp {
    pub(super) fn new(sample_rate: u32) -> Self {
        Self {
            samples: SampleBuilder::new(AUDIO_MAX_LATE_PACKETS, OpusPacket, sample_rate)
                .with_max_time_delay(std::time::Duration::from_millis(80)),
        }
    }

    pub(super) fn receive(&mut self, packet: Packet, audio_packets: &mut Vec<Bytes>) {
        if packet.header.payload_type != OPUS_PAYLOAD_TYPE {
            return;
        }

        self.samples.push(packet);
        while audio_packets.len() < MAX_PENDING_AUDIO_PACKETS {
            let Some(sample) = self.samples.pop() else {
                break;
            };
            audio_packets.push(sample.data);
        }
    }
}

pub(super) struct VideoRtp {
    samples: SampleBuilder<H264Packet>,
    stream_too_large: bool,
    waiting_for_keyframe: bool,
}

impl VideoRtp {
    pub(super) fn new() -> Self {
        Self {
            samples: SampleBuilder::new(
                VIDEO_MAX_LATE_PACKETS,
                H264Packet::default(),
                VIDEO_RTP_CLOCK_RATE,
            )
            .with_max_time_delay(std::time::Duration::from_millis(60)),
            stream_too_large: false,
            waiting_for_keyframe: false,
        }
    }

    pub(super) fn waiting_for_keyframe(&self) -> bool {
        self.waiting_for_keyframe
    }

    pub(super) fn wait_for_keyframe(&mut self) {
        self.waiting_for_keyframe = true;
    }

    pub(super) fn receive(
        &mut self,
        worker: &VideoDecodeWorker,
        packet: Packet,
        keyframe_requested: &mut bool,
    ) -> VideoSampleStats {
        self.samples.push(packet);
        let mut stats = VideoSampleStats::default();
        while let Some(sample) = self.samples.pop() {
            stats.source_frame_duration_us =
                Some(sample.duration.as_micros().min(u64::MAX as u128) as u64);
            if sample.data.len() > MAX_H264_ACCESS_UNIT_BYTES {
                eprintln!("Dropping oversized H264 access unit; requesting a keyframe");
                resync_and_drop(
                    worker,
                    keyframe_requested,
                    &mut self.waiting_for_keyframe,
                    &mut stats,
                );
                continue;
            }

            if sample.prev_dropped_packets > sample.prev_padding_packets {
                eprintln!(
                    "H264 sample lost {} RTP packet(s); requesting a keyframe",
                    sample
                        .prev_dropped_packets
                        .saturating_sub(sample.prev_padding_packets)
                );
                *keyframe_requested = true;
                stats.dropped = stats.dropped.saturating_add(1);
                continue;
            }

            let unit = inspect_h264_access_unit(&sample.data);
            let sample_too_large = unit.resolution.is_some_and(|(width, height)| {
                width > crate::HW_DECODE_WIDTH || height > crate::HW_DECODE_HEIGHT
            });
            if sample_too_large {
                eprintln!(
                    "Dropping H264 access unit larger than decoder: {:?} > {}x{}",
                    unit.resolution,
                    crate::HW_DECODE_WIDTH,
                    crate::HW_DECODE_HEIGHT
                );
                self.stream_too_large = true;
                resync_and_drop(
                    worker,
                    keyframe_requested,
                    &mut self.waiting_for_keyframe,
                    &mut stats,
                );
                continue;
            }
            if self.stream_too_large {
                if unit.resolution.is_none() || !unit.has_idr {
                    *keyframe_requested = true;
                    self.waiting_for_keyframe = true;
                    stats.dropped = stats.dropped.saturating_add(1);
                    continue;
                }
                self.stream_too_large = false;
            }
            if self.waiting_for_keyframe {
                // Later keyframes may contain only IDR; AVCDEC retains SPS/PPS across resyncs.
                if !unit.has_idr {
                    *keyframe_requested = true;
                    stats.dropped = stats.dropped.saturating_add(1);
                    continue;
                }
                self.waiting_for_keyframe = false;
            }

            if !worker.submit_access_unit(sample.data.to_vec()) {
                eprintln!("Video decoder queue is full; continuing while requesting a keyframe");
                *keyframe_requested = true;
                stats.dropped = stats.dropped.saturating_add(1);
            }
        }
        stats
    }
}

fn resync_and_drop(
    worker: &VideoDecodeWorker,
    keyframe_requested: &mut bool,
    video_waiting_for_keyframe: &mut bool,
    stats: &mut VideoSampleStats,
) {
    *keyframe_requested = true;
    if !*video_waiting_for_keyframe {
        worker.begin_resync();
    }
    *video_waiting_for_keyframe = true;
    stats.dropped = stats.dropped.saturating_add(1);
}

struct AccessUnitInfo {
    has_idr: bool,
    resolution: Option<(u32, u32)>,
}

fn inspect_h264_access_unit(data: &[u8]) -> AccessUnitInfo {
    let mut info = AccessUnitInfo {
        has_idr: false,
        resolution: None,
    };
    let mut reader = AnnexBReader::accumulate(|nal: RefNal<'_>| {
        let Ok(header) = nal.header() else {
            return NalInterest::Ignore;
        };
        match header.nal_unit_type() {
            UnitType::SliceLayerWithoutPartitioningIdr => {
                info.has_idr = true;
                NalInterest::Ignore
            }
            UnitType::SeqParameterSet => {
                if nal.is_complete() {
                    info.resolution = SeqParameterSet::from_bits(nal.rbsp_bits())
                        .and_then(|sps| sps.pixel_dimensions())
                        .ok();
                }
                NalInterest::Buffer
            }
            _ => NalInterest::Ignore,
        }
    });
    reader.push(data);
    reader.reset();
    info
}
