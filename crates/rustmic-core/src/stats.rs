//! Statistics shown by the client.
//!
//! The original client displays a small window with the number of received
//! packets, the number of lost packets and the average latency. The counters
//! below cover the same ground and are updated from the media thread.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// Live counters of a connection.
#[derive(Debug, Default)]
pub struct Statistics {
    /// Packets received from the media channel.
    pub packets_received: AtomicU64,
    /// Packets handed to the audio output.
    pub packets_played: AtomicU64,
    /// Packets that were dropped because they arrived too late.
    pub packets_dropped: AtomicU64,
    /// Sequence numbers that never arrived.
    pub packets_lost: AtomicU64,
    /// Opus packets that could not be decoded.
    pub decode_errors: AtomicU64,
    /// Bytes received from the media channel.
    pub bytes_received: AtomicU64,
    /// Number of reconnects since the client started.
    pub reconnects: AtomicU64,
    /// Sum of the latencies of all received packets, in microseconds.
    latency_sum_us: AtomicU64,
    /// Largest latency seen so far, in microseconds.
    latency_max_us: AtomicU64,
    /// Point in time the connection was opened.
    started: std::sync::Mutex<Option<Instant>>,
}

impl Statistics {
    /// Creates a fresh set of counters.
    pub fn new() -> Statistics {
        Statistics::default()
    }

    /// Marks the beginning of a connection.
    pub fn reset(&self) {
        self.packets_received.store(0, Ordering::Relaxed);
        self.packets_played.store(0, Ordering::Relaxed);
        self.packets_dropped.store(0, Ordering::Relaxed);
        self.packets_lost.store(0, Ordering::Relaxed);
        self.decode_errors.store(0, Ordering::Relaxed);
        self.bytes_received.store(0, Ordering::Relaxed);
        self.latency_sum_us.store(0, Ordering::Relaxed);
        self.latency_max_us.store(0, Ordering::Relaxed);
        *self.started.lock().expect("statistics mutex") = Some(Instant::now());
    }

    /// Records a received packet.
    pub fn on_packet(&self, bytes: usize, latency: Duration) {
        self.packets_received.fetch_add(1, Ordering::Relaxed);
        self.bytes_received.fetch_add(bytes as u64, Ordering::Relaxed);
        let micros = latency.as_micros() as u64;
        self.latency_sum_us.fetch_add(micros, Ordering::Relaxed);
        self.latency_max_us.fetch_max(micros, Ordering::Relaxed);
    }

    /// Records how long a packet waited in the jitter buffer, without
    /// counting it as received again.
    pub fn on_latency(&self, latency: Duration) {
        let micros = latency.as_micros() as u64;
        self.latency_sum_us.fetch_add(micros, Ordering::Relaxed);
        self.latency_max_us.fetch_max(micros, Ordering::Relaxed);
    }

    /// Records a played packet.
    pub fn on_played(&self) {
        self.packets_played.fetch_add(1, Ordering::Relaxed);
    }

    /// Records a dropped packet.
    pub fn on_dropped(&self, count: u64) {
        self.packets_dropped.fetch_add(count, Ordering::Relaxed);
    }

    /// Records lost packets.
    pub fn on_lost(&self, count: u64) {
        self.packets_lost.fetch_add(count, Ordering::Relaxed);
    }

    /// Records a decoding failure.
    pub fn on_decode_error(&self) {
        self.decode_errors.fetch_add(1, Ordering::Relaxed);
    }

    /// Records a reconnect.
    pub fn on_reconnect(&self) {
        self.reconnects.fetch_add(1, Ordering::Relaxed);
    }

    /// Average latency of the received packets.
    pub fn average_latency(&self) -> Duration {
        let count = self.packets_received.load(Ordering::Relaxed);
        if count == 0 {
            return Duration::ZERO;
        }
        let sum = self.latency_sum_us.load(Ordering::Relaxed);
        Duration::from_micros(sum / count)
    }

    /// Largest latency seen so far.
    pub fn max_latency(&self) -> Duration {
        Duration::from_micros(self.latency_max_us.load(Ordering::Relaxed))
    }

    /// Time since the connection was opened.
    pub fn uptime(&self) -> Duration {
        self.started
            .lock()
            .expect("statistics mutex")
            .map(|t| t.elapsed())
            .unwrap_or_default()
    }

    /// A snapshot of the counters.
    pub fn snapshot(&self) -> StatsSnapshot {
        StatsSnapshot {
            packets_received: self.packets_received.load(Ordering::Relaxed),
            packets_played: self.packets_played.load(Ordering::Relaxed),
            packets_dropped: self.packets_dropped.load(Ordering::Relaxed),
            packets_lost: self.packets_lost.load(Ordering::Relaxed),
            decode_errors: self.decode_errors.load(Ordering::Relaxed),
            bytes_received: self.bytes_received.load(Ordering::Relaxed),
            reconnects: self.reconnects.load(Ordering::Relaxed),
            average_latency: self.average_latency(),
            max_latency: self.max_latency(),
            uptime: self.uptime(),
        }
    }
}

/// An immutable copy of the counters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StatsSnapshot {
    pub packets_received: u64,
    pub packets_played: u64,
    pub packets_dropped: u64,
    pub packets_lost: u64,
    pub decode_errors: u64,
    pub bytes_received: u64,
    pub reconnects: u64,
    pub average_latency: Duration,
    pub max_latency: Duration,
    pub uptime: Duration,
}

impl std::fmt::Display for StatsSnapshot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "received {} packets ({} KiB), played {}, dropped {}, lost {}, decode errors {}, \
             average latency {} ms, maximum latency {} ms, uptime {} s",
            self.packets_received,
            self.bytes_received / 1024,
            self.packets_played,
            self.packets_dropped,
            self.packets_lost,
            self.decode_errors,
            self.average_latency.as_millis(),
            self.max_latency.as_millis(),
            self.uptime.as_secs()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counters_are_updated() {
        let stats = Statistics::new();
        stats.reset();
        stats.on_packet(100, Duration::from_millis(20));
        stats.on_packet(100, Duration::from_millis(40));
        stats.on_played();
        stats.on_dropped(2);
        stats.on_lost(1);
        stats.on_decode_error();

        let snapshot = stats.snapshot();
        assert_eq!(snapshot.packets_received, 2);
        assert_eq!(snapshot.packets_played, 1);
        assert_eq!(snapshot.packets_dropped, 2);
        assert_eq!(snapshot.packets_lost, 1);
        assert_eq!(snapshot.decode_errors, 1);
        assert_eq!(snapshot.bytes_received, 200);
        assert_eq!(snapshot.average_latency, Duration::from_millis(30));
        assert_eq!(snapshot.max_latency, Duration::from_millis(40));
    }

    #[test]
    fn the_average_latency_is_zero_without_packets() {
        let stats = Statistics::new();
        assert_eq!(stats.average_latency(), Duration::ZERO);
    }
}
