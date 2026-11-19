//! Data packets of the link: the header, numbering, acks and rates.
//!
//! Every packet after the handshake starts with this header:
//!
//! ```text
//! flag 1 | int1 connect sequence bit | int9 sequence | int9 highest ack
//! | int2 type (0 data, 1 ping, 2 ack) | int3 ack byte count | ack mask
//! ```
//!
//! Data packets then carry two rate flags, each followed when set by a
//! 10-bit update delay (ms) and a 10-bit packet size (bytes), and then the
//! game section, events and ghosts (later modules). Ping and ack packets
//! end after the mask.
//!
//! Each side numbers its data packets 1, 2, 3, ...; pings and acks reuse
//! the last number. The only delivery signal is the ack: when a header's
//! highest ack moves forward, each of our packets up to it is reported
//! delivered or lost ([`Notify`]). The bookkeeping follows the open (MIT)
//! Torque net layer, as the real 5.12 client and the Seihai gserver keep
//! it.

use super::bits::{BitReader, BitWriter, OutOfBits};

/// The most data packets in flight before only acks may be sent.
pub const WINDOW: u32 = 30;

/// How far ahead of the last received sequence a packet may be.
const RECEIVE_WINDOW: u32 = 31;

/// Width of each rate field.
const RATE_BITS: u32 = 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PacketType {
    Data = 0,
    Ping = 1,
    Ack = 2,
}

impl PacketType {
    fn from_bits(v: u32) -> Option<Self> {
        match v {
            0 => Some(PacketType::Data),
            1 => Some(PacketType::Ping),
            2 => Some(PacketType::Ack),
            _ => None,
        }
    }
}

/// A packet rate: how often to send and how big a packet may be.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rate {
    /// Milliseconds between packets.
    pub update_delay: u32,
    /// Bytes per packet.
    pub packet_size: u32,
}

impl Rate {
    /// What the real 5.12 client puts in its first packet as its maximum.
    pub const CLIENT_512_MAX: Rate = Rate {
        update_delay: 32,
        packet_size: 450,
    };

    fn write(rate: Option<Rate>, w: &mut BitWriter) {
        w.flag(rate.is_some());
        if let Some(r) = rate {
            w.int(r.update_delay.min(1023), RATE_BITS);
            w.int(r.packet_size.min(1023), RATE_BITS);
        }
    }

    fn read(r: &mut BitReader<'_>) -> Result<Option<Rate>, OutOfBits> {
        Ok(if r.flag()? {
            Some(Rate {
                update_delay: r.int(RATE_BITS)?,
                packet_size: r.int(RATE_BITS)?,
            })
        } else {
            None
        })
    }
}

/// What became of one of our data packets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Notify {
    pub sequence: u32,
    pub delivered: bool,
}

/// Why an incoming packet was dropped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Dropped {
    /// First bit 0: an out-of-band packet, for the handshake.
    OutOfBand,
    /// From an older link (the connect sequence bit differs).
    OtherLink,
    /// Packet type 3 or more than four ack bytes.
    BadHeader,
    /// More than 31 packets ahead of the last one received.
    OutsideWindow,
    /// Acknowledges a packet not sent yet.
    AckOutsideWindow,
    Short(OutOfBits),
}

impl From<OutOfBits> for Dropped {
    fn from(e: OutOfBits) -> Self {
        Dropped::Short(e)
    }
}

/// A data packet's body, positioned after the rate fields.
#[derive(Debug)]
pub struct DataPacket<'a> {
    pub sequence: u32,
    pub current_rate: Option<Rate>,
    pub max_rate: Option<Rate>,
    /// Reads the game section, events and ghosts that follow.
    pub body: BitReader<'a>,
}

/// One accepted incoming packet.
#[derive(Debug)]
pub struct Received<'a> {
    pub packet_type: PacketType,
    /// Our packets this one acknowledged or reported lost, oldest first.
    pub notifies: Vec<Notify>,
    /// A ping: send [`Link::ack`] at once.
    pub answer_with_ack: bool,
    /// A new data packet. `None` for pings, acks and duplicates.
    pub data: Option<DataPacket<'a>>,
}

/// The numbering and ack state of one link.
#[derive(Debug, Clone)]
pub struct Link {
    connect_bit: u32,
    last_seq_received: u32,
    highest_acked: u32,
    last_send_seq: u32,
    ack_mask: u32,
    /// The highest of their packets they know we have acknowledged.
    last_ack_acked: u32,
    /// For each of our recent data packets: what we had received then.
    received_at_send: [u32; 32],
    established: bool,
}

impl Link {
    /// A link with the connect sequence from the handshake.
    pub fn new(connect_sequence: u32) -> Self {
        Link {
            connect_bit: connect_sequence & 1,
            last_seq_received: 0,
            highest_acked: 0,
            last_send_seq: 0,
            ack_mask: 0,
            last_ack_acked: 0,
            received_at_send: [0; 32],
            established: false,
        }
    }

    /// Whether the other side has acknowledged any of our packets yet.
    pub fn established(&self) -> bool {
        self.established
    }

    /// The number of our last data packet.
    pub fn last_sent(&self) -> u32 {
        self.last_send_seq
    }

    /// Too many data packets unacknowledged: send only acks for now.
    pub fn window_full(&self) -> bool {
        self.last_send_seq - self.highest_acked >= WINDOW
    }

    fn header(&mut self, ty: PacketType) -> BitWriter {
        let ack_bytes = ((self.last_seq_received - self.last_ack_acked + 7) >> 3).min(4);
        if ty == PacketType::Data {
            self.last_send_seq += 1;
        }
        let mut w = BitWriter::new();
        w.flag(true);
        w.int(self.connect_bit, 1);
        w.int(self.last_send_seq & 0x1ff, 9);
        w.int(self.last_seq_received & 0x1ff, 9);
        w.int(ty as u32, 2);
        w.int(ack_bytes, 3);
        let mask = if ack_bytes == 4 {
            u32::MAX
        } else {
            !(u32::MAX << (ack_bytes * 8))
        };
        w.int(self.ack_mask & mask, ack_bytes * 8);
        if ty == PacketType::Data {
            self.received_at_send[(self.last_send_seq & 31) as usize] = self.last_seq_received;
        }
        w
    }

    /// Starts the next data packet, with the rate fields written. `None`
    /// when the window is full. Its number is [`Link::last_sent`].
    pub fn data(
        &mut self,
        current_rate: Option<Rate>,
        max_rate: Option<Rate>,
    ) -> Option<BitWriter> {
        if self.window_full() {
            return None;
        }
        let mut w = self.header(PacketType::Data);
        Rate::write(current_rate, &mut w);
        Rate::write(max_rate, &mut w);
        Some(w)
    }

    /// A ping, which asks the other side for an ack.
    pub fn ping(&mut self) -> Vec<u8> {
        self.header(PacketType::Ping).into_bytes()
    }

    /// An ack, the answer to a ping.
    pub fn ack(&mut self) -> Vec<u8> {
        self.header(PacketType::Ack).into_bytes()
    }

    /// Reads one incoming packet's header and updates the numbering.
    pub fn receive<'a>(&mut self, packet: &'a [u8]) -> Result<Received<'a>, Dropped> {
        let mut r = BitReader::new(packet);
        if !r.flag()? {
            return Err(Dropped::OutOfBand);
        }
        let bit = r.int(1)?;
        let mut seq = r.int(9)?;
        let mut highest = r.int(9)?;
        let ty = r.int(2)?;
        let ack_bytes = r.int(3)?;
        if bit != self.connect_bit {
            return Err(Dropped::OtherLink);
        }
        let (Some(ty), true) = (PacketType::from_bits(ty), ack_bytes <= 4) else {
            return Err(Dropped::BadHeader);
        };
        let their_mask = r.int(ack_bytes * 8)?;

        seq |= self.last_seq_received & !0x1ff;
        if seq < self.last_seq_received {
            seq += 0x200;
        }
        if seq > self.last_seq_received + RECEIVE_WINDOW {
            return Err(Dropped::OutsideWindow);
        }
        highest |= self.highest_acked & !0x1ff;
        if highest < self.highest_acked {
            highest += 0x200;
        }
        if highest > self.last_send_seq {
            return Err(Dropped::AckOutsideWindow);
        }

        let shift = seq - self.last_seq_received;
        self.ack_mask = if shift >= 32 {
            0
        } else {
            self.ack_mask << shift
        };
        if ty == PacketType::Data {
            self.ack_mask |= 1;
        }

        let mut notifies = Vec::new();
        for s in self.highest_acked + 1..=highest {
            let back = highest - s;
            let delivered = back < 32 && their_mask >> back & 1 == 1;
            notifies.push(Notify {
                sequence: s,
                delivered,
            });
            if delivered {
                self.last_ack_acked = self.received_at_send[(s & 31) as usize];
                self.established = true;
            }
        }
        if seq - self.last_ack_acked > 32 {
            self.last_ack_acked = seq - 32;
        }
        self.highest_acked = highest;

        let mut data = None;
        if self.last_seq_received != seq {
            self.last_seq_received = seq;
            if ty == PacketType::Data {
                let current_rate = Rate::read(&mut r)?;
                let max_rate = Rate::read(&mut r)?;
                data = Some(DataPacket {
                    sequence: seq,
                    current_rate,
                    max_rate,
                    body: r,
                });
            }
        }
        Ok(Received {
            packet_type: ty,
            notifies,
            answer_with_ack: ty == PacketType::Ping,
            data,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    #[test]
    fn the_first_two_packets_match_the_512_client() {
        // The real client's first two data packets: the max rate in the
        // first, then the game section, which starts with "first person" 1.
        let mut link = Link::new(0x400);
        let mut w = link.data(None, Some(Rate::CLIENT_512_MAX)).unwrap();
        assert_eq!(link.last_sent(), 1);
        assert_eq!(
            w.bit_len(),
            25 + 1 + 1 + 20,
            "header, two rate flags, one rate"
        );
        w.flag(true); // the game section's "first person"
        let first = hex("0500000441b800000000000000000000000000");
        assert_eq!(w.bytes(), &first[..6]);
        let mut w = link.data(None, None).unwrap();
        w.flag(true);
        assert_eq!(w.bytes(), &hex("09000008")[..]);
        assert_eq!(link.last_sent(), 2);
    }

    #[test]
    fn reads_the_captured_first_packet_as_a_server_would() {
        let mut server = Link::new(0x400);
        let first = hex("0500000441b800000000000000000000000000");
        let got = server.receive(&first).unwrap();
        assert_eq!(got.packet_type, PacketType::Data);
        assert!(got.notifies.is_empty());
        let data = got.data.unwrap();
        assert_eq!(data.sequence, 1);
        assert_eq!(data.current_rate, None);
        assert_eq!(data.max_rate, Some(Rate::CLIENT_512_MAX));
    }

    /// Sends `n` data packets from `a`, delivering those `deliver` says.
    fn exchange(a: &mut Link, b: &mut Link, deliver: &[bool]) {
        for &d in deliver {
            let p = a.data(None, None).unwrap().into_bytes();
            if d {
                assert!(b.receive(&p).unwrap().data.is_some());
            }
        }
    }

    #[test]
    fn acks_report_each_packet_delivered_or_lost_in_order() {
        let (mut client, mut server) = (Link::new(0x400), Link::new(0x400));
        exchange(&mut client, &mut server, &[true, false, true, true]);
        let reply = server.data(None, None).unwrap().into_bytes();
        let got = client.receive(&reply).unwrap();
        assert_eq!(
            got.notifies,
            [
                Notify {
                    sequence: 1,
                    delivered: true
                },
                Notify {
                    sequence: 2,
                    delivered: false
                },
                Notify {
                    sequence: 3,
                    delivered: true
                },
                Notify {
                    sequence: 4,
                    delivered: true
                },
            ]
        );
        assert!(client.established());
        // Nothing new to report the second time.
        let reply = server.data(None, None).unwrap().into_bytes();
        assert!(client.receive(&reply).unwrap().notifies.is_empty());
    }

    #[test]
    fn duplicates_are_read_for_acks_only() {
        let (mut client, mut server) = (Link::new(0x400), Link::new(0x400));
        let p = client.data(None, None).unwrap().into_bytes();
        assert!(server.receive(&p).unwrap().data.is_some());
        assert!(server.receive(&p).unwrap().data.is_none());
    }

    #[test]
    fn a_ping_asks_for_an_ack_and_takes_no_number() {
        let (mut client, mut server) = (Link::new(0x400), Link::new(0x400));
        exchange(&mut client, &mut server, &[true]);
        let ping = client.ping();
        assert_eq!(ping.len(), 4, "the real client's ping is 4 bytes");
        assert_eq!(client.last_sent(), 1);
        let got = server.receive(&ping).unwrap();
        assert!(got.answer_with_ack && got.data.is_none());
        let ack = server.ack();
        let got = client.receive(&ack).unwrap();
        assert_eq!(got.packet_type, PacketType::Ack);
        assert_eq!(
            got.notifies,
            [Notify {
                sequence: 1,
                delivered: true
            }]
        );
    }

    #[test]
    fn the_window_closes_at_thirty_unacknowledged() {
        let (mut client, mut server) = (Link::new(0x400), Link::new(0x400));
        exchange(&mut client, &mut server, &[true; 30]);
        assert!(client.window_full());
        assert!(client.data(None, None).is_none());
        let reply = server.data(None, None).unwrap().into_bytes();
        assert_eq!(client.receive(&reply).unwrap().notifies.len(), 30);
        assert!(!client.window_full());
    }

    #[test]
    fn numbering_wraps_past_511() {
        let (mut client, mut server) = (Link::new(0x400), Link::new(0x400));
        for _ in 0..40 {
            exchange(&mut client, &mut server, &[true; 20]);
            let reply = server.data(None, None).unwrap().into_bytes();
            let got = client.receive(&reply).unwrap();
            assert_eq!(got.notifies.len(), 20);
            assert!(got.notifies.iter().all(|n| n.delivered));
        }
        assert_eq!(client.last_sent(), 800);
        assert_eq!(server.last_sent(), 40);
    }

    #[test]
    fn strays_are_dropped() {
        let mut link = Link::new(0x400);
        assert_eq!(
            link.receive(&[0x24, 0, 4, 0, 0]).unwrap_err(),
            Dropped::OutOfBand
        );
        let mut other = Link::new(0x401);
        let p = other.data(None, None).unwrap().into_bytes();
        assert_eq!(link.receive(&p).unwrap_err(), Dropped::OtherLink);
        // Sequence 40 when nothing has arrived yet.
        let mut far = Link::new(0x400);
        for _ in 0..39 {
            far.data(None, None);
            far.highest_acked = far.last_send_seq;
        }
        let p = far.data(None, None).unwrap().into_bytes();
        assert_eq!(link.receive(&p).unwrap_err(), Dropped::OutsideWindow);
        // An ack for a packet never sent.
        let (mut a, mut b) = (Link::new(0x400), Link::new(0x400));
        exchange(&mut b, &mut a, &[true]);
        let p = a.data(None, None).unwrap().into_bytes();
        assert_eq!(
            Link::new(0x400).receive(&p).unwrap_err(),
            Dropped::AckOutsideWindow
        );
    }
}
