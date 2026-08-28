use anyhow::{Context, Result, bail};
use bytes::Bytes;
use sdl2::audio::{AudioQueue, AudioSpecDesired};
use std::collections::VecDeque;
use std::ptr::NonNull;
use std::sync::Mutex;
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, TryRecvError, sync_channel};
use std::time::{Duration, Instant};

pub const AUDIO_SAMPLE_RATE: i32 = 48_000;
const AUDIO_CHANNELS: usize = 2;

const AUDIO_BYTES_PER_SECOND: u32 = AUDIO_SAMPLE_RATE as u32 * AUDIO_CHANNELS as u32 * 2;
/// Ceiling that also walks the queue back down to the operating point. Ordinary drift (the
/// device clock and the server's audio clock differ) is corrected by dropping a single 10 ms
/// frame here rather than clearing the whole queue, which would cause an audible cut.
const MAX_QUEUED_AUDIO_BYTES: u32 = AUDIO_BYTES_PER_SECOND * 100 / 1_000;
/// The operating point: the queue settles here, so this *is* the audio latency. Lower than it
/// used to be because the jitter buffer now absorbs reordering upstream - stacking both full-size
/// buffers would just add latency for no extra smoothing.
const AUDIO_START_BUFFER_BYTES: u32 = AUDIO_BYTES_PER_SECOND * 40 / 1_000;
/// Hard reset, kept well above the ceiling so it only fires when something is genuinely wrong -
/// ordinary drift is handled by dropping single frames at `MAX_QUEUED_AUDIO_BYTES`.
const AUDIO_RESYNC_BYTES: u32 = AUDIO_BYTES_PER_SECOND * 150 / 1_000;
/// How long the queue must stay empty before playback is treated as genuinely over rather than
/// momentarily starved.
const STREAM_IDLE_PAUSE: Duration = Duration::from_millis(250);

const MAX_OPUS_FRAME_SAMPLES_PER_CHANNEL: usize = 5_760;
/// Hand-off to the decode thread, in 10 ms packets. The jitter buffer below does the real
/// smoothing now, so packets do not need to accumulate here the way they used to.
const MAX_PENDING_OPUS_PACKETS: usize = 6;
const MAX_PENDING_PCM_BUFFERS: usize = 8;

/// xCloud advertises `ptime=10`, so one packet is 10 ms - 480 samples per channel.
const AUDIO_FRAME_SAMPLES_PER_CHANNEL: i32 = 480;
/// `a=rtpmap:63 red/48000/2` - RFC 2198 redundant coding. Not currently negotiated by xCloud's
/// SDP, but the jitter buffer recognizes it so recovery keeps working if that ever changes.
pub const RED_PAYLOAD_TYPE: u8 = 63;
const OPUS_PAYLOAD_TYPE: u8 = 111;

/// How much audio the jitter buffer holds before releasing the first packet. Clamped the same way
/// OpenNOW-Switch's `AudioLatencyPolicy` clamps it, and for the same reason: under 30 ms stops
/// absorbing anything, over 100 ms is audible lag.
const TARGET_BUFFER_MS: u32 = 40;
const MAX_JITTER_PACKETS: usize = 12;
/// A gap this long means the transport stalled rather than a packet being late; the sequence
/// numbers on the far side of it are not worth waiting for.
const EPOCH_RESET_GAP: Duration = Duration::from_millis(500);

const OPUS_OK: i32 = 0;

/// One RTP audio packet, with the header fields the jitter buffer needs to order and repair the
/// stream.
#[derive(Debug, Clone)]
pub struct AudioPacket {
    pub payload: Bytes,
    pub sequence: u16,
    pub payload_type: u8,
}

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

    /// Packet loss concealment: libopus interpolates a replacement frame when handed a null
    /// buffer, which sounds considerably better than the silence a naive gap-filler would insert.
    fn conceal(&mut self, pcm: &mut [i16]) -> Result<usize> {
        // SAFETY: libopus documents a null/zero-length input as the PLC path; `pcm` is sized for
        // one frame at `AUDIO_FRAME_SAMPLES_PER_CHANNEL`.
        let decoded = unsafe {
            opus_decode(
                self.state.as_ptr(),
                std::ptr::null(),
                0,
                pcm.as_mut_ptr(),
                AUDIO_FRAME_SAMPLES_PER_CHANNEL,
                0,
            )
        };
        if decoded < OPUS_OK {
            bail!("libopus concealment error {decoded}");
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

/// The primary (most recent) Opus payload carried by an RTP packet, unwrapped from its RED
/// envelope if it has one.
fn red_primary(data: &[u8], payload_type: u8) -> Option<&[u8]> {
    if data.is_empty() {
        return None;
    }
    if payload_type != RED_PAYLOAD_TYPE {
        return Some(data);
    }

    // RED block headers are 4 bytes each while the top bit is set, then a 1-byte header for the
    // primary block. The redundant payloads sit between the headers and the primary one.
    let mut header = 0usize;
    let mut redundant_bytes = 0usize;
    while header < data.len() && data[header] & 0x80 != 0 {
        if header + 4 > data.len() {
            return None;
        }
        redundant_bytes += usize::from(data[header + 2] & 0x03) << 8 | usize::from(data[header + 3]);
        header += 4;
    }
    if header >= data.len() {
        return None;
    }
    header += 1;
    if header + redundant_bytes >= data.len() {
        return None;
    }
    Some(&data[header + redundant_bytes..])
}

/// The first redundant payload in a RED packet - a copy of an *earlier* packet's audio. This is
/// what makes recovery possible: when packet N is lost, packet N+1 usually still carries it.
fn red_first_redundant(data: &[u8]) -> Option<&[u8]> {
    if data.len() < 5 || data[0] & 0x80 == 0 {
        return None;
    }
    let first_length = usize::from(data[2] & 0x03) << 8 | usize::from(data[3]);
    let mut header = 0usize;
    while header < data.len() && data[header] & 0x80 != 0 {
        if header + 4 > data.len() {
            return None;
        }
        header += 4;
    }
    if header >= data.len() || first_length == 0 {
        return None;
    }
    header += 1;
    if header + first_length > data.len() {
        return None;
    }
    Some(&data[header..header + first_length])
}

/// What the jitter buffer decided to do with the next slot in the sequence.
enum Release {
    /// Decode this payload. The payload type travels with it because it decides whether the bytes
    /// are RED-wrapped: assuming either way mangles the other.
    Payload { payload: Bytes, payload_type: u8 },
    /// The packet is gone and could not be recovered - run concealment for its slot.
    Conceal,
}

/// Reorders RTP audio by sequence number, recovers losses from RED redundancy, and reports
/// unrecoverable gaps so the decoder can conceal them.
struct JitterBuffer {
    packets: VecDeque<AudioPacket>,
    expected_sequence: u16,
    have_expected: bool,
    primed: bool,
    prime_packets: usize,
    initial_hold: Duration,
    resync_hold: Duration,
    hold_until: Option<Instant>,
    last_arrival: Option<Instant>,
}

impl JitterBuffer {
    fn new(target_buffer_ms: u32) -> Self {
        let target = target_buffer_ms.clamp(30, 100);
        Self {
            packets: VecDeque::with_capacity(MAX_JITTER_PACKETS),
            expected_sequence: 0,
            have_expected: false,
            primed: false,
            // One packet is 10 ms, so the target buffer converts straight to a packet count.
            prime_packets: (target / 10).max(1) as usize,
            initial_hold: Duration::from_millis(u64::from(target)),
            resync_hold: Duration::from_millis(u64::from(target / 2).max(30)),
            hold_until: None,
            last_arrival: None,
        }
    }

    fn reset(&mut self, hold: Duration, now: Instant) {
        self.packets.clear();
        self.primed = false;
        self.have_expected = false;
        self.hold_until = Some(now + hold);
    }

    fn push(&mut self, packet: AudioPacket) {
        let now = Instant::now();
        let stalled = self
            .last_arrival
            .is_some_and(|previous| now.duration_since(previous) > EPOCH_RESET_GAP);
        self.last_arrival = Some(now);

        // A huge forward jump means the sender restarted its sequence rather than that we are
        // missing thousands of packets, so waiting for the gap to fill would stall forever.
        let jumped = self.have_expected
            && packet.sequence.wrapping_sub(self.expected_sequence) as i16
                > (MAX_JITTER_PACKETS * 4) as i16;
        if stalled || jumped {
            self.reset(self.resync_hold, now);
        }

        if self.hold_until.is_none() && !self.primed {
            self.hold_until = Some(now + self.initial_hold);
        }

        // Already played past this one - decoding it now would put it out of order.
        if self.have_expected
            && (packet.sequence.wrapping_sub(self.expected_sequence) as i16) < 0
        {
            return;
        }
        if self.packets.iter().any(|held| held.sequence == packet.sequence) {
            return;
        }
        if self.packets.len() >= MAX_JITTER_PACKETS {
            self.packets.pop_front();
        }
        let position = self
            .packets
            .iter()
            .position(|held| (packet.sequence.wrapping_sub(held.sequence) as i16) < 0)
            .unwrap_or(self.packets.len());
        self.packets.insert(position, packet);
    }

    fn next(&mut self) -> Option<Release> {
        if self.hold_until.is_some_and(|until| Instant::now() < until) {
            return None;
        }
        if !self.primed {
            if self.packets.len() < self.prime_packets {
                return None;
            }
            self.primed = true;
            self.hold_until = None;
            self.expected_sequence = self.packets.front()?.sequence;
            self.have_expected = true;
        }
        if self.packets.is_empty() {
            return None;
        }

        if let Some(index) = self
            .packets
            .iter()
            .position(|held| held.sequence == self.expected_sequence)
        {
            let packet = self.packets.remove(index)?;
            self.expected_sequence = self.expected_sequence.wrapping_add(1);
            return Some(Release::Payload {
                payload: packet.payload,
                payload_type: packet.payload_type,
            });
        }

        // The expected packet is missing. If the next one is RED, it carries a copy of exactly
        // this payload, so the loss is repairable without waiting for a retransmit.
        let successor = self.expected_sequence.wrapping_add(1);
        if let Some(next) = self
            .packets
            .iter()
            .find(|held| held.sequence == successor && held.payload_type == RED_PAYLOAD_TYPE)
            && let Some(recovered) = red_first_redundant(&next.payload)
        {
            let recovered = Bytes::copy_from_slice(recovered);
            self.expected_sequence = self.expected_sequence.wrapping_add(1);
            return Some(Release::Payload {
                payload: recovered,
                // Already unwrapped from its RED envelope - it is bare Opus now, and running it
                // through the RED parser again would eat the front of the frame.
                payload_type: OPUS_PAYLOAD_TYPE,
            });
        }

        self.expected_sequence = self.expected_sequence.wrapping_add(1);
        Some(Release::Conceal)
    }
}

/// The live decode worker's inbox, published so the RTP-receiving thread can hand packets
/// straight over the moment they arrive.
///
/// Audio used to reach the worker by riding along with the shell's render loop: RTP packets
/// piled up in a `Vec` on the RTC thread, and the shell only drained it once per ~16 ms render
/// frame. That hop was harmless with the old 80 ms jitter tolerance, but the jitter buffer's
/// actual per-packet reordering window is much thinner than its 40 ms target suggests (see
/// `JitterBuffer::next`), so the extra 16 ms was enough on its own to make a merely-late packet
/// look lost by the time it reached the buffer. Publishing the sender here lets `submit_packet`
/// be called directly from wherever a packet is received, with no render-frame hop in between.
static WORKER_INBOX: Mutex<Option<SyncSender<AudioPacket>>> = Mutex::new(None);

/// Hands one freshly-received RTP audio packet to the decode thread. Safe to call from any
/// thread, and a no-op when no session is running.
pub fn submit_packet(packet: AudioPacket) {
    let Ok(inbox) = WORKER_INBOX.lock() else {
        return;
    };
    let Some(sender) = inbox.as_ref() else {
        return;
    };
    let _ = sender.try_send(packet);
}

fn publish_inbox(sender: Option<SyncSender<AudioPacket>>) {
    if let Ok(mut inbox) = WORKER_INBOX.lock() {
        *inbox = sender;
    }
}

pub struct AudioRenderer {
    queue: AudioQueue<i16>,
    samples_rx: Receiver<Vec<i16>>,
    started: bool,
    empty_since: Option<Instant>,
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
        publish_inbox(Some(packets_tx));

        Ok(Self {
            queue,
            samples_rx,
            started: false,
            empty_since: None,
        })
    }

    /// Drains decoded PCM into the SDL queue and manages playback start/pause/resync. Audio
    /// packets themselves no longer pass through here - they reach the decode worker directly
    /// from the RTP-receiving thread via `submit_packet`. Call once per render frame; the
    /// buffering thresholds here operate on a much coarser scale (tens of ms) than the
    /// render-frame cadence, so polling this only that often is fine.
    pub fn pump(&mut self) {
        if self.queue.size() == 0 {
            if self.started {
                let since = *self.empty_since.get_or_insert_with(Instant::now);
                if since.elapsed() >= STREAM_IDLE_PAUSE {
                    self.queue.pause();
                    self.started = false;
                }
            }
        } else {
            self.empty_since = None;
        }

        loop {
            match self.samples_rx.try_recv() {
                Ok(samples) => {
                    let sample_bytes = (samples.len() * size_of::<i16>()) as u32;
                    if self.queue.size() >= AUDIO_RESYNC_BYTES {
                        // Genuinely drifting: nothing short of a reset brings latency back.
                        self.queue.pause();
                        self.queue.clear();
                        self.started = false;
                    } else if self.queue.size().saturating_add(sample_bytes) > MAX_QUEUED_AUDIO_BYTES
                    {
                        // A burst. Dropping this frame keeps playback continuous, where clearing
                        // would throw away good buffered audio and force a re-buffer.
                        continue;
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
        self.empty_since = None;
        match spawn_decode_worker() {
            Ok((packets_tx, samples_rx)) => {
                publish_inbox(Some(packets_tx));
                self.samples_rx = samples_rx;
            }
            Err(error) => {
                publish_inbox(None);
                eprintln!("Failed to restart audio decode worker: {error:#}");
            }
        }
    }
}

impl Drop for AudioRenderer {
    fn drop(&mut self) {
        publish_inbox(None);
    }
}

fn spawn_decode_worker() -> Result<(SyncSender<AudioPacket>, Receiver<Vec<i16>>)> {
    let (packets_tx, packets_rx) = sync_channel::<AudioPacket>(MAX_PENDING_OPUS_PACKETS);
    let (samples_tx, samples_rx) = sync_channel::<Vec<i16>>(MAX_PENDING_PCM_BUFFERS);

    let mut decoder = NativeOpusDecoder::new().context("failed to create Opus decoder")?;

    std::thread::Builder::new()
        .name("green-vita-audio-decode".to_owned())
        .spawn(move || {
            let mut jitter = JitterBuffer::new(TARGET_BUFFER_MS);
            let mut decode_buf = vec![0i16; MAX_OPUS_FRAME_SAMPLES_PER_CHANNEL * AUDIO_CHANNELS];
            // Long enough to notice a stalled stream, short enough that the jitter buffer's hold
            // deadline is honoured with roughly one packet of slack.
            let poll_interval = Duration::from_millis(12);

            loop {
                match packets_rx.recv_timeout(poll_interval) {
                    Ok(packet) => jitter.push(packet),
                    Err(RecvTimeoutError::Timeout) => {}
                    Err(RecvTimeoutError::Disconnected) => break,
                }
                // Anything else already waiting goes in before we release, so a burst is
                // reordered as a group rather than one packet per poll.
                while let Ok(packet) = packets_rx.try_recv() {
                    jitter.push(packet);
                }

                while let Some(release) = jitter.next() {
                    let samples_per_channel = match release {
                        Release::Payload {
                            payload,
                            payload_type,
                        } => match red_primary(&payload, payload_type)
                            .filter(|primary| !primary.is_empty())
                        {
                            Some(primary) => match decoder.decode(primary, &mut decode_buf) {
                                Ok(samples) => samples,
                                Err(error) => {
                                    eprintln!(
                                        "Failed to decode Opus audio packet: {error}; resetting decoder"
                                    );
                                    if let Ok(new_decoder) = NativeOpusDecoder::new() {
                                        decoder = new_decoder;
                                    }
                                    decoder.conceal(&mut decode_buf).unwrap_or(0)
                                }
                            },
                            None => decoder.conceal(&mut decode_buf).unwrap_or(0),
                        },
                        Release::Conceal => match decoder.conceal(&mut decode_buf) {
                            Ok(samples) => samples,
                            Err(error) => {
                                eprintln!("Opus concealment failed: {error}");
                                0
                            }
                        },
                    };
                    if samples_per_channel == 0 {
                        continue;
                    }

                    let sample_count = samples_per_channel * AUDIO_CHANNELS;
                    if samples_tx
                        .send(decode_buf[..sample_count].to_vec())
                        .is_err()
                    {
                        // The renderer dropped `samples_rx` (it's being replaced or torn down) -
                        // nothing further to do on this thread.
                        return;
                    }
                }
            }
        })
        .context("failed to spawn audio decode worker")?;

    Ok((packets_tx, samples_rx))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A RED packet: one 4-byte redundant block header (top bit set) describing a 3-byte redundant
    /// payload, then the 1-byte primary header, then both payloads.
    fn red_packet() -> Vec<u8> {
        let mut packet = vec![0x80 | 111, 0x00, 0x00, 0x03];
        packet.push(111);
        packet.extend_from_slice(&[0xAA, 0xBB, 0xCC]);
        packet.extend_from_slice(&[0x11, 0x22]);
        packet
    }

    #[test]
    fn red_primary_skips_redundant_blocks() {
        let packet = red_packet();
        assert_eq!(
            red_primary(&packet, RED_PAYLOAD_TYPE),
            Some(&[0x11u8, 0x22][..])
        );
    }

    #[test]
    fn red_primary_passes_plain_opus_through() {
        let packet = [0x11u8, 0x22, 0x33];
        assert_eq!(red_primary(&packet, 111), Some(&packet[..]));
    }

    #[test]
    fn red_first_redundant_recovers_previous_payload() {
        let packet = red_packet();
        assert_eq!(
            red_first_redundant(&packet),
            Some(&[0xAAu8, 0xBB, 0xCC][..])
        );
    }

    #[test]
    fn red_helpers_reject_truncated_packets() {
        assert_eq!(red_primary(&[0x80, 0x00], RED_PAYLOAD_TYPE), None);
        assert_eq!(red_first_redundant(&[0x80, 0x00]), None);
        assert_eq!(red_first_redundant(&[0x01, 0x02, 0x03, 0x04, 0x05]), None);
    }

    fn packet(sequence: u16, payload: &[u8], payload_type: u8) -> AudioPacket {
        AudioPacket {
            payload: Bytes::copy_from_slice(payload),
            sequence,
            payload_type,
        }
    }

    fn primed_buffer() -> JitterBuffer {
        // A 30 ms target primes on 3 packets, keeping these tests short.
        JitterBuffer::new(30)
    }

    fn release_payload(release: Option<Release>) -> Option<Bytes> {
        match release {
            Some(Release::Payload { payload, .. }) => Some(payload),
            _ => None,
        }
    }

    fn release_type(release: Option<Release>) -> Option<u8> {
        match release {
            Some(Release::Payload { payload_type, .. }) => Some(payload_type),
            _ => None,
        }
    }

    #[test]
    fn buffer_holds_until_primed() {
        let mut buffer = primed_buffer();
        buffer.push(packet(1, &[1], 111));
        buffer.hold_until = None;
        assert!(buffer.next().is_none(), "released before priming");
        buffer.push(packet(2, &[2], 111));
        buffer.push(packet(3, &[3], 111));
        buffer.hold_until = None;
        assert_eq!(release_payload(buffer.next()).as_deref(), Some(&[1u8][..]));
    }

    #[test]
    fn buffer_reorders_by_sequence() {
        let mut buffer = primed_buffer();
        buffer.push(packet(3, &[3], 111));
        buffer.push(packet(1, &[1], 111));
        buffer.push(packet(2, &[2], 111));
        buffer.hold_until = None;

        let released: Vec<u8> = std::iter::from_fn(|| release_payload(buffer.next()))
            .map(|payload| payload[0])
            .collect();
        assert_eq!(released, vec![1, 2, 3]);
    }

    #[test]
    fn buffer_recovers_loss_from_red_redundancy() {
        let mut buffer = primed_buffer();
        buffer.push(packet(10, &[10], 111));
        // 11 never arrives; 12 is RED and carries a copy of it.
        buffer.push(packet(12, &red_packet(), RED_PAYLOAD_TYPE));
        buffer.push(packet(13, &[13], 111));
        buffer.hold_until = None;

        assert_eq!(release_payload(buffer.next()).as_deref(), Some(&[10u8][..]));
        assert_eq!(
            release_payload(buffer.next()).as_deref(),
            Some(&[0xAAu8, 0xBB, 0xCC][..]),
            "lost packet was not recovered from RED redundancy"
        );
    }

    /// Regression: the payload type has to survive the jitter buffer. Releasing it without one
    /// forced the caller to guess, and guessing RED for a plain Opus packet made the parser walk
    /// headers that were not there and hand back a mangled frame - every packet, all session.
    #[test]
    fn released_packets_keep_their_payload_type() {
        let mut buffer = primed_buffer();
        buffer.push(packet(1, &[1], 111));
        buffer.push(packet(2, &red_packet(), RED_PAYLOAD_TYPE));
        buffer.push(packet(3, &[3], 111));
        buffer.hold_until = None;

        assert_eq!(release_type(buffer.next()), Some(111), "plain Opus stayed plain");
        assert_eq!(
            release_type(buffer.next()),
            Some(RED_PAYLOAD_TYPE),
            "a RED packet must be reported as RED so its envelope gets unwrapped"
        );
    }

    /// A payload recovered *out of* a RED envelope is already bare Opus. Re-parsing it as RED
    /// would eat the front of the frame.
    #[test]
    fn red_recovered_payloads_are_reported_as_plain_opus() {
        let mut buffer = primed_buffer();
        buffer.push(packet(10, &[10], 111));
        buffer.push(packet(12, &red_packet(), RED_PAYLOAD_TYPE));
        buffer.push(packet(13, &[13], 111));
        buffer.hold_until = None;

        buffer.next();
        let recovered = buffer.next();
        assert_eq!(
            release_type(recovered),
            Some(OPUS_PAYLOAD_TYPE),
            "recovered redundancy must not be run through the RED parser twice"
        );
    }

    /// The end-to-end shape of the bug: what the decoder actually receives for each packet type.
    #[test]
    fn plain_opus_survives_the_red_unwrap_step() {
        let opus = [0x11u8, 0x22, 0x33];
        assert_eq!(
            red_primary(&opus, 111),
            Some(&opus[..]),
            "plain Opus must pass through untouched"
        );
        assert_ne!(
            red_primary(&opus, RED_PAYLOAD_TYPE),
            Some(&opus[..]),
            "guessing RED for plain Opus corrupts it - this is what made audio unintelligible"
        );
    }

    #[test]
    fn buffer_conceals_unrecoverable_gap() {
        let mut buffer = primed_buffer();
        buffer.push(packet(20, &[20], 111));
        // 21 is lost and 22 is plain Opus, so there is no redundant copy to fall back on.
        buffer.push(packet(22, &[22], 111));
        buffer.push(packet(23, &[23], 111));
        buffer.hold_until = None;

        assert_eq!(release_payload(buffer.next()).as_deref(), Some(&[20u8][..]));
        assert!(
            matches!(buffer.next(), Some(Release::Conceal)),
            "gap should have asked for concealment"
        );
        assert_eq!(release_payload(buffer.next()).as_deref(), Some(&[22u8][..]));
    }

    #[test]
    fn buffer_drops_packets_already_played() {
        let mut buffer = primed_buffer();
        buffer.push(packet(5, &[5], 111));
        buffer.push(packet(6, &[6], 111));
        buffer.push(packet(7, &[7], 111));
        buffer.hold_until = None;
        assert_eq!(release_payload(buffer.next()).as_deref(), Some(&[5u8][..]));

        buffer.push(packet(4, &[4], 111));
        assert_eq!(
            release_payload(buffer.next()).as_deref(),
            Some(&[6u8][..]),
            "a late packet was played out of order"
        );
    }
}
