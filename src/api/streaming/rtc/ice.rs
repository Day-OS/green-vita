use anyhow::{Context, Result};
use bytes::BytesMut;
use rtc::peer_connection::RTCPeerConnection;
use rtc::peer_connection::transport::RTCIceCandidateInit;
use rtc::sansio::Protocol;
use rtc::shared::{TaggedBytesMut, TransportContext, TransportProtocol};
use std::net::SocketAddr;
use std::time::{Duration, Instant};
use tokio::net::UdpSocket;

pub fn local_candidate(
    pc: &mut RTCPeerConnection,
    candidate: String,
    url: Option<String>,
) -> Result<()> {
    pc.add_local_candidate(RTCIceCandidateInit {
        candidate,
        sdp_mid: Some("0".to_owned()),
        sdp_mline_index: Some(0),
        username_fragment: None,
        url,
    })
    .context("failed to register local ICE candidate")
}

pub fn add_host_candidate(
    pc: &mut RTCPeerConnection,
    local_port: u16,
    route_probe: &str,
) -> Result<SocketAddr> {
    let probe = std::net::UdpSocket::bind("0.0.0.0:0")
        .context("failed to open a socket to discover the local IP")?;
    probe
        .connect(route_probe)
        .context("failed to determine local network route")?;
    let local_addr = probe
        .local_addr()
        .context("failed to read discovered local address")?;

    let priority = (126u32 << 24) | (65535u32 << 8) | 255;
    let candidate = format!(
        "candidate:1 1 udp {priority} {} {local_port} typ host",
        local_addr.ip()
    );
    local_candidate(pc, candidate, None)?;

    Ok(SocketAddr::new(local_addr.ip(), local_port))
}

pub async fn discover_server_reflexive_candidate(
    socket: &UdpSocket,
    local_addr: SocketAddr,
    stun_server: &str,
) -> Result<Option<SocketAddr>> {
    let stun_addr = tokio::net::lookup_host(stun_server)
        .await
        .context("failed to resolve STUN server")?
        .next()
        .context("STUN server resolved to no addresses")?;

    let mut client = rtc::stun::client::ClientBuilder::new()
        .build(local_addr, stun_addr, TransportProtocol::UDP)
        .map_err(|error| anyhow::anyhow!("failed to build STUN client: {error}"))?;

    let mut request = rtc::stun::message::Message::new();
    request
        .build(&[
            Box::<rtc::stun::message::TransactionId>::default(),
            Box::new(rtc::stun::message::BINDING_REQUEST),
        ])
        .map_err(|error| anyhow::anyhow!("failed to build STUN request: {error}"))?;
    client
        .handle_write(request)
        .map_err(|error| anyhow::anyhow!("failed to queue STUN request: {error}"))?;

    while let Some(transmit) = client.poll_write() {
        socket
            .send_to(&transmit.message, stun_addr)
            .await
            .context("failed to send STUN request")?;
    }

    let mut buf = vec![0u8; 1500];
    let (n, peer_addr) = tokio::time::timeout(Duration::from_secs(2), socket.recv_from(&mut buf))
        .await
        .context("STUN request timed out")?
        .context("failed to receive STUN response")?;

    client
        .handle_read(TaggedBytesMut {
            now: Instant::now(),
            transport: TransportContext {
                local_addr,
                peer_addr,
                ecn: None,
                transport_protocol: TransportProtocol::UDP,
            },
            message: BytesMut::from(&buf[..n]),
        })
        .map_err(|error| anyhow::anyhow!("failed to parse STUN response: {error}"))?;

    let Some(rtc::stun::agent::StunEvent::Message(response)) = client.poll_event() else {
        return Ok(None);
    };
    let mut xor_addr = rtc::stun::xoraddr::XorMappedAddress::default();
    <rtc::stun::xoraddr::XorMappedAddress as rtc::stun::message::Getter>::get_from(
        &mut xor_addr,
        &response,
    )
    .map_err(|error| anyhow::anyhow!("failed to decode STUN response: {error}"))?;

    Ok(Some(SocketAddr::new(xor_addr.ip, xor_addr.port)))
}

pub fn add_srflx_candidate(
    pc: &mut RTCPeerConnection,
    public_addr: SocketAddr,
    host_addr: SocketAddr,
    stun_server: &str,
) -> Result<()> {
    let priority = (100u32 << 24) | (65535u32 << 8) | 255;
    let candidate = format!(
        "candidate:2 1 udp {priority} {} {} typ srflx raddr {} rport {}",
        public_addr.ip(),
        public_addr.port(),
        host_addr.ip(),
        host_addr.port(),
    );
    local_candidate(pc, candidate, Some(format!("stun:{stun_server}")))
}
