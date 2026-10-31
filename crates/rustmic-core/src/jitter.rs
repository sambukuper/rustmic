//! Jitter buffer and reordering for the media channel.
//!
//! The original client keeps a small packet queue per connection. For the
//! Wi-Fi transport it also tracks a play time and drops packets that arrive
//! too late (`FUN_00404580`).
//!
//! The reimplementation below keeps the same observable behaviour: packets
//! are handed to the decoder in sequence order, packets that arrive after
//! their slot has been played out are dropped and counted, and the number of
//! packets kept in flight is taken from `bufferSize` in `settings.dat`.

use std::collections::BTreeMap;

use crate::media::MediaFrame;

/// A packet that waits in the jitter buffer.
#[derive(Debug, Clone)]
struct Slot {
    frame: MediaFrame,
}

/// Reorders packets and reports late ones.
#[derive(Debug)]
pub struct JitterBuffer {
    /// Packets that have not been played out yet, keyed by sequence number.
    slots: BTreeMap<u16, Slot>,
    /// Sequence number of the next packet to be played out.
    next_seq: u16,
    /// Number of packets kept in flight.
    capacity: usize,
    /// Whether a sequence number was already seen, used to seed the play
    /// out position.
    started: bool,
    /// Whether a packet was already played out; after the first play out a
    /// packet behind the position is truly late, before it the position can
    /// still be rewound for an earlier packet.
    played_any: bool,
    /// Statistics of the buffer.
    stats: JitterStats,
}

/// Counters maintained by the jitter buffer.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct JitterStats {
    /// Packets accepted by the buffer.
    pub received: u64,
    /// Packets handed to the player.
    pub played: u64,
    /// Packets that arrived too late and were dropped.
    pub dropped: u64,
    /// Packets that were received twice.
    pub duplicates: u64,
    /// Sequence numbers that were skipped because a packet never arrived.
    pub lost: u64,
}

impl JitterBuffer {
    /// Creates a buffer that keeps `capacity` packets in flight.
    pub fn new(capacity: usize) -> JitterBuffer {
        JitterBuffer {
            slots: BTreeMap::new(),
            next_seq: 0,
            capacity: capacity.max(1),
            started: false,
            played_any: false,
            stats: JitterStats::default(),
        }
    }

    /// Changes the number of packets kept in flight.
    pub fn set_capacity(&mut self, capacity: usize) {
        self.capacity = capacity.max(1);
    }

    /// Adds a packet to the buffer.
    ///
    /// Returns `false` when the packet was dropped because its slot had
    /// already been played out, or because it was a duplicate.
    pub fn push(&mut self, frame: MediaFrame) -> bool {
        self.stats.received += 1;

        if !self.started {
            self.started = true;
            self.next_seq = frame.seq;
        }

        // A packet for a slot that was already played out is too late.
        if self.slots.contains_key(&frame.seq) {
            self.stats.duplicates += 1;
            return false;
        }
        if frame.seq.wrapping_sub(self.next_seq) > 0x8000 {
            if self.played_any {
                self.stats.dropped += 1;
                return false;
            }
            // Nothing has been played out yet, so the position can be
            // rewound to accept the earlier packet.
            self.next_seq = frame.seq;
        }

        self.slots.insert(frame.seq, Slot { frame });
        true
    }

    /// Returns the next packet that can be played out.
    ///
    /// A packet is released as soon as its sequence number is the next one
    /// in line, as soon as the buffer holds `capacity` packets, or as soon
    /// as the play out position sits in front of a gap that can no longer
    /// be filled. The capacity case keeps the latency low when the network
    /// is quiet, the gap case keeps the stream running when a packet was
    /// lost.
    pub fn pop(&mut self) -> Option<MediaFrame> {
        if self.slots.is_empty() {
            return None;
        }

        let ready = self.slots.contains_key(&self.next_seq)
            || self.slots.len() >= self.capacity
            || (self.played_any
                && self
                    .slots
                    .keys()
                    .next()
                    .is_some_and(|&lowest| lowest.wrapping_sub(self.next_seq) <= 0x8000
                        && lowest != self.next_seq));

        if !ready {
            return None;
        }

        // If the expected packet is missing, skip over the gap so that a
        // single lost packet does not stall the stream.
        if !self.slots.contains_key(&self.next_seq) {
            let (&lowest, _) = self.slots.iter().next().expect("buffer is not empty");
            let gap = lowest.wrapping_sub(self.next_seq) as u64;
            self.stats.lost += gap;
            self.next_seq = lowest;
        }

        let seq = self.next_seq;
        let slot = self.slots.remove(&seq).expect("slot was checked above");
        self.next_seq = self.next_seq.wrapping_add(1);
        self.played_any = true;
        self.stats.played += 1;
        Some(slot.frame)
    }

    /// Number of packets currently held in the buffer.
    pub fn len(&self) -> usize {
        self.slots.len()
    }

    /// True when the buffer holds no packets.
    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }

    /// Statistics of the buffer.
    pub fn stats(&self) -> JitterStats {
        self.stats
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(seq: u16) -> MediaFrame {
        MediaFrame { seq, timestamp: 0, flags: 0, payload: vec![seq as u8] }
    }

    #[test]
    fn packets_are_released_in_order() {
        let mut buffer = JitterBuffer::new(4);
        assert!(buffer.push(frame(10)));
        assert!(buffer.push(frame(11)));
        assert!(buffer.push(frame(12)));
        assert_eq!(buffer.pop().unwrap().seq, 10);
        assert_eq!(buffer.pop().unwrap().seq, 11);
        assert_eq!(buffer.pop().unwrap().seq, 12);
        assert!(buffer.pop().is_none());
    }

    #[test]
    fn out_of_order_packets_are_reordered() {
        let mut buffer = JitterBuffer::new(4);
        assert!(buffer.push(frame(11)));
        assert!(buffer.push(frame(10)));
        assert_eq!(buffer.pop().unwrap().seq, 10);
        assert_eq!(buffer.pop().unwrap().seq, 11);
    }

    #[test]
    fn duplicates_are_dropped() {
        let mut buffer = JitterBuffer::new(4);
        assert!(buffer.push(frame(1)));
        assert!(!buffer.push(frame(1)));
        assert_eq!(buffer.stats().duplicates, 1);
    }

    #[test]
    fn a_missing_packet_is_skipped_and_counted() {
        let mut buffer = JitterBuffer::new(4);
        assert!(buffer.push(frame(1)));
        assert!(buffer.push(frame(3)));
        assert_eq!(buffer.pop().unwrap().seq, 1);
        assert_eq!(buffer.pop().unwrap().seq, 3);
        assert_eq!(buffer.stats().lost, 1);
    }

    #[test]
    fn a_full_buffer_releases_the_oldest_packet() {
        let mut buffer = JitterBuffer::new(2);
        assert!(buffer.push(frame(1)));
        assert!(buffer.push(frame(2)));
        // the buffer is full now, so the oldest packet is released even
        // though the expected sequence number did not arrive
        assert_eq!(buffer.pop().unwrap().seq, 1);
    }

    #[test]
    fn late_packets_are_dropped() {
        let mut buffer = JitterBuffer::new(4);
        assert!(buffer.push(frame(100)));
        assert_eq!(buffer.pop().unwrap().seq, 100);
        // sequence 99 is behind the play out position
        assert!(!buffer.push(frame(99)));
        assert_eq!(buffer.stats().dropped, 1);
    }
}
