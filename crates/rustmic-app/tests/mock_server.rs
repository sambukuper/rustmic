//! End to end test against a mock WO Mic server.
//!
//! The mock speaks the control channel on TCP (the same bytes the original
//! server sends: the `e`, `f` and `g` answers) and pushes Wi-Fi framed Opus
//! packets over UDP. The client pipeline must decode them back into the
//! silence the test encoded.

use std::io::{Read, Write};
use std::net::{TcpListener, UdpSocket};
use std::sync::atomic::Ordering;
use std::time::Duration;

use opus::Encoder;
use rustmic_app::session::{PipelineOptions, pump};
use rustmic_app::sink::SampleSink;
use rustmic_core::media::Framing;
use rustmic_core::protocol::ControlClient;
use rustmic_core::settings::TransportType;
use rustmic_core::stats::Statistics;
use rustmic_transport::Session;

const FRAME_SAMPLES: usize = 960;

/// Collects every sample the pipeline produced.
#[derive(Default)]
struct CollectSink {
    frames: usize,
    samples: Vec<i16>,
}

impl SampleSink for CollectSink {
    fn write(&mut self, samples: &[i16]) -> rustmic_core::error::Result<()> {
        self.frames += 1;
        self.samples.extend_from_slice(samples);
        Ok(())
    }

    fn name(&self) -> &'static str {
        "collect"
    }
}

/// Builds one Wi-Fi framed datagram: `04 00 | len | seq | ts | flag | body`.
fn wifi_datagram(seq: u16, timestamp: u32, payload: &[u8]) -> Vec<u8> {
    let mut datagram = Vec::with_capacity(11 + payload.len());
    datagram.extend_from_slice(&[0x04, 0x00]);
    datagram.extend_from_slice(&((7 + payload.len()) as u16).to_be_bytes());
    datagram.extend_from_slice(&seq.to_be_bytes());
    datagram.extend_from_slice(&timestamp.to_be_bytes());
    datagram.push(0x00);
    datagram.extend_from_slice(payload);
    datagram
}

/// A control channel answer: `[tag][len u32 BE][payload]`.
fn answer(tag: u8, payload: &[u8]) -> Vec<u8> {
    let mut message = Vec::with_capacity(5 + payload.len());
    message.push(tag);
    message.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    message.extend_from_slice(payload);
    message
}

/// Serves one control connection exactly like the original server.
fn spawn_control_server(listener: TcpListener) {
    std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("control connection");
        let mut command = [0u8; 11];
        stream.read_exact(&mut command).expect("connect command");
        assert_eq!(command[0], b'e');
        assert_eq!(&command[5..], &[0x04, 0x04, 0x00, 0x00, 0x00, 0x00]);
        stream
            .write_all(&answer(b'e', &[0x00, 0x04]))
            .expect("connect answer");

        stream.read_exact(&mut command).expect("version command");
        assert_eq!(command[0], b'f');
        assert_eq!(&command[7..], &3u32.to_be_bytes(), "wifi transport on the wire");
        stream.write_all(&answer(b'f', &[0x00])).expect("version answer");

        let mut start = [0u8; 5];
        stream.read_exact(&mut start).expect("start command");
        assert_eq!(start[0], b'g');
        stream.write_all(&answer(b'g', &[0x00])).expect("start answer");
    });
}

/// Grabs an ephemeral UDP port for the client's media socket.
fn free_udp_port() -> u16 {
    UdpSocket::bind("127.0.0.1:0")
        .expect("probe socket")
        .local_addr()
        .expect("probe address")
        .port()
}

#[test]
fn the_client_pipeline_survives_a_full_server_round_trip() {
    let control_listener = TcpListener::bind("127.0.0.1:0").expect("control bind");
    let control_port = control_listener.local_addr().expect("control address").port();
    spawn_control_server(control_listener);

    let media_port = free_udp_port();
    let sender = UdpSocket::bind("127.0.0.1:0").expect("sender bind");

    // Bind the client's media socket first: datagrams sent before the bind
    // would be answered with ICMP port unreachable and silently dropped.
    let mut session = rustmic_transport::open(
        TransportType::Wifi,
        "127.0.0.1",
        control_port,
        media_port,
        "",
        None,
        None,
    )
    .expect("open session");

    let mut control = ControlClient::new(&mut *session.control);
    let reply = control.connect().expect("connect handshake");
    assert_eq!(reply.framing, Framing::Wifi as u8);
    control.check_version(TransportType::Wifi).expect("version handshake");
    control.start().expect("start handshake");

    let mut encoder =
        Encoder::new(48000, opus::Channels::Mono, opus::Application::Voip).expect("encoder");
    let silence = [0i16; FRAME_SAMPLES];
    let mut packet = [0u8; 400];
    let packet_len = encoder.encode(&silence, &mut packet).expect("encode");

    // The datagrams queue in the client's media socket buffer. A
    // deliberately broken datagram at the end of the queue wakes the pump
    // with a protocol error once the five real frames are consumed, which
    // this test treats as the end of the stream.
    for seq in 0..5u16 {
        let datagram = wifi_datagram(seq, seq as u32 * 20, &packet[..packet_len]);
        sender.send_to(&datagram, ("127.0.0.1", media_port)).expect("media datagram");
    }
    sender.send_to(&[0xFF, 0xFF, 0x00, 0x00], ("127.0.0.1", media_port)).expect("wake datagram");

    let options = PipelineOptions {
        buffer_size: 3,
        reconnect: false,
        reconnect_delay: Duration::from_secs(1),
        stop: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
    };
    let stats = Statistics::new();
    let mut sink = CollectSink::default();
    let mut session: Session = session;

    // The pump drains the five queued frames and stops on the trailing
    // broken datagram; either way the collected samples are the point.
    let pump_result = pump(&mut session, Framing::Wifi, &options, &mut sink, &stats);
    let _ = pump_result;

    assert_eq!(sink.frames, 5, "every sent packet must reach the sink");
    assert_eq!(sink.samples.len(), 5 * FRAME_SAMPLES);
    assert!(
        sink.samples.iter().all(|&s| s.abs() < 64),
        "decoded silence must stay silent"
    );
    assert_eq!(stats.packets_received.load(Ordering::Relaxed), 5);
}
