use crate::streaming::rtc::{AUDIO_CHANNELS, AUDIO_SAMPLE_RATE};
use anyhow::{Context, Result, bail};
use bytes::Bytes;
use sdl2::audio::{AudioQueue, AudioSpecDesired};
use std::ptr::NonNull;
use std::sync::mpsc::{Receiver, SyncSender, TryRecvError, TrySendError, sync_channel};

const AUDIO_BYTES_PER_SECOND: u32 = AUDIO_SAMPLE_RATE as u32 * AUDIO_CHANNELS as u32 * 2;
const MAX_QUEUED_AUDIO_BYTES: u32 = AUDIO_BYTES_PER_SECOND * 240 / 1_000;
const AUDIO_START_BUFFER_BYTES: u32 = AUDIO_BYTES_PER_SECOND * 80 / 1_000;
const MAX_OPUS_FRAME_SAMPLES_PER_CHANNEL: usize = 5_760;
const MAX_PENDING_OPUS_PACKETS: usize = 32;
const MAX_PENDING_PCM_BUFFERS: usize = 8;

const OPUS_OK: i32 = 0;

#[repr(C)]
struct OpusDecoderState {
    _private: [u8; 0],
}

#[link(name = "opus", kind = "static")]
unsafe extern "C" {
    fn opus_decoder_create(
        sample_rate: i32,
        channels: i32,
        error: *mut i32,
    ) -> *mut OpusDecoderState;
    fn opus_decode(
        decoder: *mut OpusDecoderState,
        data: *const u8,
        length: i32,
        pcm: *mut i16,
        frame_size: i32,
        decode_fec: i32,
    ) -> i32;
    fn opus_decoder_destroy(decoder: *mut OpusDecoderState);
}

struct NativeOpusDecoder {
    state: NonNull<OpusDecoderState>,
}

unsafe impl Send for NativeOpusDecoder {}

impl NativeOpusDecoder {
    fn new() -> Result<Self> {
        let mut error = OPUS_OK;
        // SAFETY: libopus initializes and exclusively owns the returned opaque decoder state.
        let state =
            unsafe { opus_decoder_create(AUDIO_SAMPLE_RATE, AUDIO_CHANNELS as i32, &mut error) };
        if error != OPUS_OK {
            if !state.is_null() {
                // SAFETY: a non-null state returned by libopus must be released with this function.
                unsafe { opus_decoder_destroy(state) };
            }
            bail!("libopus failed to create a decoder: error {error}");
        }
        let state = NonNull::new(state).context("libopus returned a null decoder")?;
        Ok(Self { state })
    }

    fn decode(&mut self, packet: &[u8], pcm: &mut [i16]) -> Result<usize> {
        let packet_len = i32::try_from(packet.len()).context("Opus packet is too large")?;
        // SAFETY: `state` is a live decoder, and `pcm` has room for the maximum Opus frame.
        let decoded = unsafe {
            opus_decode(
                self.state.as_ptr(),
                packet.as_ptr(),
                packet_len,
                pcm.as_mut_ptr(),
                MAX_OPUS_FRAME_SAMPLES_PER_CHANNEL as i32,
                0,
            )
        };
        if decoded < OPUS_OK {
            bail!("libopus decode error {decoded}");
        }
        Ok(decoded as usize)
    }
}

impl Drop for NativeOpusDecoder {
    fn drop(&mut self) {
        // SAFETY: this is the sole owner and the state has not previously been destroyed.
        unsafe { opus_decoder_destroy(self.state.as_ptr()) };
    }
}

pub struct AudioRenderer {
    queue: AudioQueue<i16>,
    packets_tx: SyncSender<Bytes>,
    samples_rx: Receiver<Vec<i16>>,
    started: bool,
}

impl AudioRenderer {
    pub fn new(audio: &sdl2::AudioSubsystem) -> Result<Self> {
        let desired = AudioSpecDesired {
            freq: Some(AUDIO_SAMPLE_RATE),
            channels: Some(AUDIO_CHANNELS as u8),
            samples: Some(1024),
        };
        let queue = audio
            .open_queue(None, &desired)
            .map_err(anyhow::Error::msg)
            .context("failed to open SDL audio queue")?;
        let spec = queue.spec();
        if spec.freq != AUDIO_SAMPLE_RATE || spec.channels != AUDIO_CHANNELS as u8 {
            eprintln!(
                "SDL audio opened as {} Hz / {} channel(s), requested {} Hz / {} channel(s)",
                spec.freq, spec.channels, AUDIO_SAMPLE_RATE, AUDIO_CHANNELS
            );
        }
        let (packets_tx, samples_rx) = spawn_decode_worker()?;

        Ok(Self {
            queue,
            packets_tx,
            samples_rx,
            started: false,
        })
    }

    pub fn submit_packets(&mut self, packets: Vec<Bytes>) {
        if self.started && self.queue.size() == 0 {
            self.queue.pause();
            self.started = false;
        }

        for packet in packets {
            match self.packets_tx.try_send(packet) {
                Ok(()) | Err(TrySendError::Full(_)) => {}
                Err(TrySendError::Disconnected(packet)) => {
                    eprintln!("Audio decode worker stopped; restarting");
                    self.restart_decode_worker();
                    let _ = self.packets_tx.try_send(packet);
                }
            }
        }

        loop {
            match self.samples_rx.try_recv() {
                Ok(samples) => {
                    let sample_bytes = (samples.len() * size_of::<i16>()) as u32;
                    if self.queue.size().saturating_add(sample_bytes) > MAX_QUEUED_AUDIO_BYTES {
                        self.queue.pause();
                        self.queue.clear();
                        self.started = false;
                    }
                    if let Err(error) = self.queue.queue_audio(&samples) {
                        eprintln!("Failed to queue SDL audio: {error}");
                    }
                    if !self.started && self.queue.size() >= AUDIO_START_BUFFER_BYTES {
                        self.queue.resume();
                        self.started = true;
                    }
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    eprintln!("Audio decode worker output disconnected; restarting");
                    self.restart_decode_worker();
                    break;
                }
            }
        }
    }

    fn restart_decode_worker(&mut self) {
        self.queue.pause();
        self.queue.clear();
        self.started = false;
        match spawn_decode_worker() {
            Ok((packets_tx, samples_rx)) => {
                self.packets_tx = packets_tx;
                self.samples_rx = samples_rx;
            }
            Err(error) => eprintln!("Failed to restart audio decode worker: {error:#}"),
        }
    }
}

fn spawn_decode_worker() -> Result<(SyncSender<Bytes>, Receiver<Vec<i16>>)> {
    let (packets_tx, packets_rx) = sync_channel::<Bytes>(MAX_PENDING_OPUS_PACKETS);
    let (samples_tx, samples_rx) = sync_channel::<Vec<i16>>(MAX_PENDING_PCM_BUFFERS);

    let mut decoder = NativeOpusDecoder::new().context("failed to create Opus decoder")?;

    std::thread::Builder::new()
        .name("green-vita-audio-decode".to_owned())
        .spawn(move || {
            let mut decode_buf = vec![0i16; MAX_OPUS_FRAME_SAMPLES_PER_CHANNEL * AUDIO_CHANNELS];
            while let Ok(packet) = packets_rx.recv() {
                let samples_per_channel = match decoder.decode(&packet, &mut decode_buf) {
                    Ok(samples_per_channel) => samples_per_channel,
                    Err(error) => {
                        eprintln!("Failed to decode Opus audio packet: {error}");
                        continue;
                    }
                };

                let sample_count = samples_per_channel * AUDIO_CHANNELS;
                if samples_tx
                    .send(decode_buf[..sample_count].to_vec())
                    .is_err()
                {
                    break;
                }
            }
        })
        .context("failed to spawn audio decode worker")?;

    Ok((packets_tx, samples_rx))
}
