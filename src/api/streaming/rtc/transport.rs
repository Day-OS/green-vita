use super::ice;
use crate::api::streaming::rtc::peer::FeedbackPeer as RTCPeerConnection;
use crate::streaming::video::metrics::METRICS;
use anyhow::{Context, Result};
use bytes::BytesMut;
use rtc::sansio::Protocol;
use rtc::shared::{TaggedBytesMut, TransportContext, TransportProtocol};
use std::net::{SocketAddr, UdpSocket as StdUdpSocket};
use std::sync::atomic::Ordering;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;
use std::time::{Duration, Instant};
use tokio::net::UdpSocket;

/// A datagram captured by the dedicated receive thread together with its true
/// wire-arrival instant.
struct CapturedDatagram {
    data: Vec<u8>,
    peer_addr: SocketAddr,
    at: Instant,
    /// µs since RX_CLOCK at capture (true wire arrival) — feeds TWCC stamps.
    at_us: u64,
}

/// UDP/ICE transport shared by WebRTC streaming providers.
pub(crate) struct RtcTransport {
    pub(crate) socket: UdpSocket,
    local_addr: SocketAddr,
    datagrams: Receiver<CapturedDatagram>,
}

impl RtcTransport {
    pub(crate) async fn bind(
        peer: &mut RTCPeerConnection,
        stun_server: &str,
        route_probe: &str,
    ) -> Result<Self> {
        let std_socket = StdUdpSocket::bind("0.0.0.0:0")
            .context("failed to bind UDP socket for WebRTC transport")?;
        std_socket
            .set_nonblocking(true)
            .context("failed to set UDP socket non-blocking")?;
        // Perf-plan stage 0 probe + stage 4 re-test (buffer only). The probe
        // reports the kernel's default UDP receive buffer; stage 4 then raises
        // it to 512 packets with a step-down ladder.
        //
        // History: stage 4 (2026-11-25) raised the buffer to 294 912 B AND
        // lowered the RX thread priority to 64. Device A/B on 2026-11-26 showed
        // that combination adds standing latency the CSV cannot see (0.3.2 with
        // the 124 800 B default: no accumulation; 0.3.3 with 294 912 B: ~1 s
        // within a minute), so both were reverted (4ba69db). That A/B only
        // disproved the buffer as a cause of ACCUMULATION; the stage's actual
        // target -- damage in busy sessions -- was never tested. This re-test
        // changes the buffer alone and keeps the RX thread at its default
        // priority, so the two effects stay separable.
        #[cfg(target_os = "vita")]
        {
            use std::os::fd::AsRawFd;
            unsafe extern "C" {
                fn getsockopt(
                    fd: i32,
                    level: i32,
                    optname: i32,
                    optval: *mut std::ffi::c_void,
                    optlen: *mut u32,
                ) -> i32;
                // C signature: socklen_t is passed BY VALUE for setsockopt
                // (getsockopt takes a pointer).
                fn setsockopt(
                    fd: i32,
                    level: i32,
                    optname: i32,
                    optval: *const std::ffi::c_void,
                    optlen: u32,
                ) -> i32;
            }
            let level = vitasdk_sys::SCE_NET_SOL_SOCKET as i32;
            let optname = vitasdk_sys::SCE_NET_SO_RCVBUF as i32;
            let fd = std_socket.as_raw_fd();
            let mut size: i32 = 0;
            let mut length: u32 = std::mem::size_of::<i32>() as u32;
            let probe = unsafe {
                getsockopt(fd, level, optname, (&mut size as *mut i32).cast(), &mut length)
            };
            if probe == 0 {
                // Session-constant CSV column (perf plan 2026-11-24): stderr is
                // not retrievable from the device, the CSV is.
                METRICS
                    .rcvbuf_def_bytes
                    .store(size.max(0) as u64, Ordering::Relaxed);
                eprintln!("WebRTC UDP receive buffer (kernel default): {size} bytes");
            } else {
                eprintln!(
                    "WebRTC UDP receive buffer size unavailable (getsockopt returned {probe})"
                );
            }

            // Perf-plan stage 4 (buffer-only re-test): target 512 packets
            // (~1.1 KB each = 589 824 B), stepping down to 256 then 128 packets
            // if the OS refuses, instead of the 124 800-byte default (~113
            // packets). Measured pile-ups reached 95 packets = 77 % of that
            // default while the RX thread was starved. The RX thread priority is
            // deliberately left at its default in this build.
            let ladder = [589_824i32, 294_912, 147_456];
            let mut requested: i32 = 0;
            for candidate in ladder {
                let result = unsafe {
                    setsockopt(
                        fd,
                        level,
                        optname,
                        (&candidate as *const i32).cast(),
                        std::mem::size_of::<i32>() as u32,
                    )
                };
                if result == 0 {
                    requested = candidate;
                    break;
                }
            }
            let mut effective: i32 = 0;
            let mut effective_len: u32 = std::mem::size_of::<i32>() as u32;
            let queried = unsafe {
                getsockopt(
                    fd,
                    level,
                    optname,
                    (&mut effective as *mut i32).cast(),
                    &mut effective_len,
                )
            };
            if queried != 0 {
                // Fall back to what we know: our request, else the default.
                effective = if requested != 0 { requested } else { size };
            }

            // stderr is not retrievable from the device, so keep one line on
            // disk next to metrics.csv / rtp-dump.bin for the FTP pull.
            // `requested_bytes=0` would mean the ladder did not run.
            let _ = std::fs::create_dir_all("ux0:data/xcloud-rust");
            let _ = std::fs::write(
                "ux0:data/xcloud-rust/rcvbuf.txt",
                format!(
                    "default_bytes={} requested_bytes={} effective_bytes={}\n",
                    size.max(0),
                    requested.max(0),
                    effective.max(0)
                ),
            );
            eprintln!(
                "SO_RCVBUF: default {} requested {} effective {}",
                size.max(0),
                requested.max(0),
                effective.max(0)
            );
        }
        let socket_addr = std_socket
            .local_addr()
            .context("failed to read local UDP socket address")?;
        let local_addr = ice::add_host_candidate(peer, socket_addr.port(), route_probe)
            .context("failed to add local host ICE candidate")?;

        // Dedicated RX thread: drains the kernel buffer within ~1 ms regardless of
        // render/presentation load on the UI thread (vita-moonlight-relay's fix for
        // "image lags behind while controls responsive"). The cloned fd shares the
        // O_NONBLOCK flag with the async twin on POSIX, so the thread polls with a
        // 1 ms sleep instead of blocking.
        let rx_socket = std_socket
            .try_clone()
            .context("failed to clone UDP socket for receive thread")?;
        let socket = UdpSocket::from_std(std_socket)
            .context("failed to register UDP socket with the async runtime")?;
        match ice::discover_server_reflexive_candidate(&socket, local_addr, stun_server).await {
            Ok(Some(public_addr)) => {
                if let Err(error) =
                    ice::add_srflx_candidate(peer, public_addr, local_addr, stun_server)
                {
                    eprintln!("Failed to add server-reflexive ICE candidate: {error:#}");
                }
            }
            Ok(None) => eprintln!("STUN request produced no usable response"),
            Err(error) => eprintln!("STUN discovery failed: {error:#}"),
        }

        ice::local_candidate(peer, String::new(), None)
            .context("failed to signal end-of-candidates")?;

        let (tx, datagrams) = mpsc::sync_channel::<CapturedDatagram>(2048);
        thread::Builder::new()
            .name("rtc-recv".into())
            .spawn(move || {
                // Pin RX to its own core so render/GUI load on the main thread
                // cannot delay datagram pickup; main thread pins itself to core 0.
                unsafe {
                    let result = vitasdk_sys::sceKernelChangeThreadCpuAffinityMask(
                        vitasdk_sys::SCE_KERNEL_THREAD_ID_SELF as _,
                        0b0010,
                    );
                    if result != 0 {
                        eprintln!("rtc-recv affinity pin failed: {result}");
                    }
                }
                // Stage 4 (2026-11-25) also lowered this thread's priority to 64
                // (Vita: 0 = highest, 0x7F = lowest, default 32). The 2026-11-26
                // A/B could not separate the buffer from the priority, so this
                // buffer-only re-test keeps the default priority: the thread
                // keeps the priority it had in 0.3.2 and only the socket buffer
                // changes.
                let mut buf = vec![0u8; 2048];
                // Arm the arrival clock BEFORE the first recv so every
                // capture instant is >= the anchor (no duration underflow).
                std::sync::LazyLock::force(&crate::api::streaming::rtc::rtp::RX_CLOCK);
                // Perf-plan stage 0 probe: datagrams drained since the last
                // idle moment; its maximum is reported as rx_drain_max.
                let mut drained_since_idle: u64 = 0;
                // Pause probe (perf plan 2026-11-25): time of the previous poll
                // of the socket; the largest poll-to-poll interval is reported
                // as rx_gap_max_us (how long this thread went unscheduled).
                let mut last_poll_at = Instant::now();
                loop {
                    {
                        let now = Instant::now();
                        METRICS.rx_gap_max_us.fetch_max(
                            now.duration_since(last_poll_at).as_micros() as u64,
                            Ordering::Relaxed,
                        );
                        last_poll_at = now;
                    }
                    match rx_socket.recv_from(&mut buf) {
                        Ok((n, peer_addr)) => {
                            drained_since_idle += 1;
                            // Raw SR census at the true wire layer (RFC 7983:
                            // V=2 + PT=200; SRTCP keeps the 8-byte header in
                            // clear; STUN/DTLS have V!=2 and RTP payload types
                            // never equal 200 under mux rules). Splits "console
                            // never SRs us" from "fork drops inbound RTCP after
                            // this point" — compare vs CSV rtcp_sr_count (the
                            // crate's post-read stats).
                            if n >= 2 && buf[0] >> 6 == 2 && buf[1] == 200 {
                                METRICS.rtcp_sr_raw.fetch_add(1, Ordering::Relaxed);
                                // The SR body is SRTCP-encrypted: only the first 8 bytes
                                // (V/P/RC, PT, sender SSRC) are in clear, bytes 8-11
                                // are the SRTCP index and 12+ is ciphertext. Parsing
                                // `buf[8..20]` therefore yielded the index plus
                                // ciphertext, not NTP/RTP timestamps — the 2026-11-26
                                // lobby session showed a non-monotone NTP "anchor"
                                // (1.05-2.75e12) and a structurally dead
                                // capture_to_arrival metric. The SRTCP key lives inside
                                // the rtc crate, so the body stays unread here; only the
                                // LOCAL arrival instant and the census are recorded.
                                if n >= 28 {
                                    let sr_anchor_local_us = crate::api::streaming::rtc::media::PROCESS_START.elapsed().as_micros() as u64;
                                    // Wall clock at SR arrival: UNIX epoch microseconds.
                                    let sr_anchor_local_wall_us = std::time::SystemTime::now()
                                        .duration_since(std::time::UNIX_EPOCH)
                                        .map(|d| d.as_micros() as u64)
                                        .unwrap_or(0);
                                    METRICS.sr_anchor_local_us.store(sr_anchor_local_us, Ordering::Relaxed);
                                    METRICS.sr_anchor_local_wall_us.store(sr_anchor_local_wall_us, Ordering::Relaxed);
                                }
                            }
                            let at = Instant::now();
                            let datagram = CapturedDatagram {
                                data: buf[..n].to_vec(),
                                peer_addr,
                                at,
                                at_us: at
                                    .duration_since(*crate::api::streaming::rtc::rtp::RX_CLOCK)
                                    .as_micros() as u64,
                            };
                            if tx.send(datagram).is_err() {
                                break;
                            }
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            if drained_since_idle > 0 {
                                METRICS
                                    .rx_drain_max
                                    .fetch_max(drained_since_idle, Ordering::Relaxed);
                                drained_since_idle = 0;
                            }
                            thread::sleep(Duration::from_millis(1));
                        }
                        Err(error) => {
                            eprintln!("WebRTC receive thread stopped: {error}");
                            break;
                        }
                    }
                }
            })
            .context("failed to spawn WebRTC receive thread")?;

        Ok(Self {
            socket,
            local_addr,
            datagrams,
        })
    }

    pub(crate) async fn flush(&self, peer: &mut RTCPeerConnection) {
        while let Some(outgoing) = peer.poll_write() {
            // Counts TWCC feedback (RTPFB, PT=205, FMT=15) that actually
            // leaves the socket. VERIFIED 2026-10-30: has counted ZERO — the
            // registry is silent and the manual sender is stubbed; this
            // counter goes live with 2c (flag-gated TWCC-alone send).
            let bytes = &outgoing.message;
            // 2c diagnostic: ANY RTPFB (PT=205) leaving, any FMT — splits
            // "interceptor emits nothing" from "emits but mis-tagged".
            if bytes.len() >= 2 && bytes[1] == 205 {
                crate::streaming::video::metrics::METRICS
                    .rtcp_rtpfb_any
                    .fetch_add(1, Ordering::Relaxed);
            }
            if bytes.len() >= 2 && bytes[1] == 205 && bytes[0] & 0x1F == 15 {
                crate::streaming::video::metrics::METRICS
                    .twcc_sent
                    .fetch_add(1, Ordering::Relaxed);
            }
            // S5b: NACK (RTPFB FMT=1) leaving the socket — socket-truth
            // count (packets, not seqs) for CSV rtcp_nack_sent. Also feeds
            // the Safe-Ceiling REMB ladder via its own accumulator (kept
            // separate from the metrics drain so neither steals counts).
            if bytes.len() >= 2 && bytes[1] == 205 && bytes[0] & 0x1F == 1 {
                crate::streaming::video::metrics::METRICS
                    .rtcp_nack_sent
                    .fetch_add(1, Ordering::Relaxed);
                crate::api::streaming::rtc::media::REMB_NACK_EGRESS
                    .fetch_add(1, Ordering::Relaxed);
            }
            if let Err(error) = self
                .socket
                .send_to(&outgoing.message, outgoing.transport.peer_addr)
                .await
            {
                eprintln!(
                    "Failed to send WebRTC UDP packet to {}: {error}",
                    outgoing.transport.peer_addr
                );
            }
        }
    }

    pub(crate) fn receive(&mut self, peer: &mut RTCPeerConnection) {
        loop {
            match self.datagrams.try_recv() {
                Ok(datagram) => {
                    // TWCC socket-arrival stapling: tag video-RTP datagrams
                    // (our negotiated PTs 102/112 — deterministic offer,
                    // echoed in every archived answer) with their capture
                    // instant. Popped by rtp::take_twcc_arrival at the
                    // recorder site.
                    if datagram.data.len() >= 4
                        && datagram.data[0] >> 6 == 2
                        && matches!(datagram.data[1] & 0x7F, 102 | 112)
                    {
                        let seq = u16::from_be_bytes([datagram.data[2], datagram.data[3]]);
                        crate::api::streaming::rtc::rtp::offer_twcc_arrival(seq, datagram.at_us);
                    }
                    let handle_started = Instant::now();
                    if let Err(error) = peer.handle_read(TaggedBytesMut {
                        now: datagram.at,
                        transport: TransportContext {
                            local_addr: self.local_addr,
                            peer_addr: datagram.peer_addr,
                            ecn: None,
                            transport_protocol: TransportProtocol::UDP,
                        },
                        message: BytesMut::from(datagram.data.as_slice()),
                    }) {
                        eprintln!(
                            "Failed to handle WebRTC UDP packet from {}: {error}",
                            datagram.peer_addr
                        );
                    }
                    // Perf-plan stage 0 probe: RTC-thread CPU spent in the
                    // WebRTC read path (SRTP decrypt, RTP demux) per second.
                    METRICS.rtc_cpu_us.fetch_add(
                        handle_started.elapsed().as_micros() as u64,
                        Ordering::Relaxed,
                    );
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    eprintln!("WebRTC receive channel closed");
                    break;
                }
            }
        }
    }
}
