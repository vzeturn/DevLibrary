//! Property-style tests without third-party crates: random interleavings of send, credit,
//! delivery, commit/ack, dropped connections, resume and resends between two sessions must
//! end with every payload applied exactly once and in order on each channel, both ways; and
//! decoding random or mutated bytes must never panic.

use std::collections::VecDeque;

use dl_agent_rt::{Channel, Config, Event, Ext, Frame, Limits, Outcome, Session, SessionState};

/// xorshift64* (Vigna, "An experimental exploration of Marsaglia's xorshift generators").
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Rng {
        Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
    fn chance(&mut self, percent: u64) -> bool {
        self.below(100) < percent
    }
}

const MAX_PAYLOAD: usize = 300;

struct Peer {
    s: Session,
    made: u32,
    backlog: [VecDeque<Vec<u8>>; 5],
    generated: [Vec<Vec<u8>>; 5],
    pending: [Vec<(u64, Vec<u8>)>; 5],
    applied: [Vec<Vec<u8>>; 5],
}

#[derive(Default)]
struct Stats {
    duplicates: u64,
    resent: u64,
    blocked_on_credit: u64,
    pongs: u64,
}

impl Peer {
    fn new(config: Config) -> Peer {
        Peer {
            s: Session::new(config),
            made: 0,
            backlog: Default::default(),
            generated: Default::default(),
            pending: Default::default(),
            applied: Default::default(),
        }
    }

    fn generate(&mut self, rng: &mut Rng, chan: Channel) {
        self.made += 1;
        let mut p = vec![chan.as_u8()];
        p.extend_from_slice(&self.made.to_be_bytes());
        let filler = rng.below((MAX_PAYLOAD - p.len()) as u64) as usize;
        p.extend((0..filler).map(|_| rng.next() as u8));
        self.generated[chan.index()].push(p.clone());
        self.backlog[chan.index()].push_back(p);
    }

    /// Sends the backlog of a channel in order until something blocks.
    fn pump_backlog(&mut self, chan: Channel, now: u64, stats: &mut Stats) {
        while let Some(p) = self.backlog[chan.index()].pop_front() {
            let ext = Ext {
                t_detected_us: Some(now),
                t_read_us: None,
            };
            match self.s.send(chan, p, ext, now).expect("send") {
                Outcome::Done(_) => {}
                Outcome::WouldBlock(w) => {
                    if matches!(w.reason, dl_agent_rt::Blocked::Credit { .. }) {
                        stats.blocked_on_credit += 1;
                    }
                    self.backlog[chan.index()].push_front(w.payload);
                    break;
                }
            }
        }
    }

    fn flush(&mut self, wire: &mut VecDeque<Vec<u8>>) {
        let limits = self.s.config().limits;
        while let Some(f) = self.s.poll_outgoing() {
            wire.push_back(f.encode(&limits).expect("encode"));
        }
    }

    fn receive(&mut self, bytes: &[u8], now: u64, stats: &mut Stats) {
        let f = Frame::decode(bytes, &self.s.config().limits).expect("decode");
        match self.s.handle(f, now).expect("handle") {
            Event::Data(f) => self.pending[f.chan.index()].push((f.seq, f.payload)),
            Event::Duplicate { .. } => stats.duplicates += 1,
            Event::Gap {
                chan,
                expected,
                got,
            } => panic!("gap on {chan}: {expected} vs {got}"),
            Event::Resumed { resending, .. } => stats.resent += resending,
            Event::Pong { .. } => stats.pongs += 1,
            _ => {}
        }
    }

    /// The application stores everything pending durably, then commits.
    fn commit(&mut self, chan: Channel, now: u64) {
        let pending = std::mem::take(&mut self.pending[chan.index()]);
        if let Some(&(last, _)) = pending.last() {
            self.applied[chan.index()].extend(pending.into_iter().map(|(_, p)| p));
            self.s.commit(chan, last, now).expect("commit");
        }
    }

    fn idle(&self) -> bool {
        self.backlog.iter().all(VecDeque::is_empty)
            && self.pending.iter().all(Vec::is_empty)
            && Channel::ALL
                .iter()
                .all(|&c| self.s.sender(c).unacked().len() == 0)
    }
}

fn run(seed: u64, steps: usize, stats: &mut Stats) {
    let mut rng = Rng::new(seed);
    let config = Config {
        limits: Limits::new(1024),
        initial_credit: [MAX_PAYLOAD as u64 + rng.below(700); 5],
    };
    let mut peers = [Peer::new(config), Peer::new(config)];
    let mut wires: [VecDeque<Vec<u8>>; 2] = Default::default(); // wires[i]: from peer i
    let mut now = 1u64;
    let mut connected = false;
    for _ in 0..steps {
        now += 1 + rng.below(50);
        let p = rng.below(2) as usize;
        let chan = Channel::ALL[rng.below(5) as usize];
        match rng.below(100) {
            0..=19 => peers[p].generate(&mut rng, chan),
            20..=34 => peers[p].pump_backlog(chan, now, stats),
            35..=44 => peers[p].flush(&mut wires[p]),
            45..=69 => {
                // Deliver a few frames from p's wire to the other peer, in order.
                for _ in 0..rng.below(6) {
                    let Some(bytes) = wires[p].pop_front() else {
                        break;
                    };
                    peers[1 - p].receive(&bytes, now, stats);
                }
            }
            70..=79 => peers[p].commit(chan, now),
            80..=86 if peers[p].s.state() == SessionState::Open => {
                let bytes = rng.below(2 * MAX_PAYLOAD as u64);
                peers[p].s.grant(chan, bytes, now).expect("grant");
            }
            87..=89 if connected => {
                // The connection drops: frames in flight are lost.
                for (peer, wire) in peers.iter_mut().zip(wires.iter_mut()) {
                    wire.clear();
                    peer.s.disconnect();
                    if rng.chance(50) {
                        // The application lost what it had not stored yet.
                        for c in Channel::ALL {
                            peer.pending[c.index()].clear();
                            peer.s.rewind(c);
                        }
                    }
                }
                connected = false;
            }
            90..=95 if !connected => {
                peers.iter_mut().for_each(|peer| peer.s.connect(now));
                connected = true;
            }
            96..=99 if connected => {
                peers[p].s.ping(vec![1, 2, 3], now).expect("ping");
            }
            _ => {}
        }
    }
    // Drain: reconnect, then keep granting, sending, delivering and committing until idle.
    if !connected {
        peers.iter_mut().for_each(|peer| peer.s.connect(now));
    }
    let exchange =
        |peers: &mut [Peer; 2], wires: &mut [VecDeque<Vec<u8>>; 2], stats: &mut Stats| loop {
            for (peer, wire) in peers.iter_mut().zip(wires.iter_mut()) {
                peer.flush(wire);
            }
            if wires.iter().all(VecDeque::is_empty) {
                break;
            }
            for p in 0..2 {
                while let Some(bytes) = wires[p].pop_front() {
                    peers[1 - p].receive(&bytes, now, stats);
                }
            }
        };
    for round in 0.. {
        assert!(round < 10_000, "seed {seed}: no progress");
        for peer in &mut peers {
            for c in Channel::ALL {
                peer.pump_backlog(c, now, stats);
            }
        }
        exchange(&mut peers, &mut wires, stats);
        for peer in &mut peers {
            for c in Channel::ALL {
                peer.commit(c, now);
            }
        }
        exchange(&mut peers, &mut wires, stats);
        if peers.iter().all(Peer::idle) {
            break;
        }
        for peer in &mut peers {
            for c in Channel::ALL {
                peer.s.grant(c, MAX_PAYLOAD as u64 * 4, now).expect("grant");
            }
        }
    }
    for p in 0..2 {
        for c in Channel::ALL {
            assert_eq!(
                peers[1 - p].applied[c.index()],
                peers[p].generated[c.index()],
                "seed {seed}: {c} from peer {p} not delivered exactly once in order"
            );
        }
    }
}

#[test]
fn random_interleavings_deliver_exactly_once_in_order() {
    let mut stats = Stats::default();
    for seed in 1..=300 {
        run(seed, 600, &mut stats);
    }
    // The interesting paths really ran.
    assert!(stats.duplicates > 0, "no duplicate delivery exercised");
    assert!(stats.resent > 0, "no resend exercised");
    assert!(stats.blocked_on_credit > 0, "credit never blocked");
    assert!(stats.pongs > 0, "no ping answered");
}

#[test]
fn decoding_random_bytes_never_panics() {
    let mut rng = Rng::new(7);
    let limits = Limits::new(256);
    for _ in 0..200_000 {
        let len = rng.below(80) as usize;
        let mut b: Vec<u8> = (0..len).map(|_| rng.next() as u8).collect();
        // Bias toward plausible headers so the deeper checks run too.
        if len > 3 && rng.chance(80) {
            b[0] = 1;
            b[1] = rng.below(6) as u8;
            b[2] = rng.below(10) as u8;
            b[3] = rng.below(3) as u8;
        }
        if len >= 24 && rng.chance(70) {
            let rest = (len - 24) as u32;
            let pl = if rng.chance(50) {
                rest
            } else {
                rng.below(64) as u32
            };
            b[20..24].copy_from_slice(&pl.to_be_bytes());
        }
        if let Ok(f) = Frame::decode(&b, &limits) {
            let again = f.encode(&limits).expect("a decoded frame encodes");
            assert_eq!(Frame::decode(&again, &limits), Ok(f));
        }
    }
}

#[test]
fn mutated_valid_frames_never_panic() {
    let mut rng = Rng::new(11);
    let limits = Limits::default();
    let seeds = [
        Frame::data(
            Channel::Capture,
            9,
            100,
            Ext {
                t_detected_us: Some(1),
                t_read_us: Some(2),
            },
            b"payload".to_vec(),
        ),
        Frame::credit(Channel::Bulk, 4096, 3),
        Frame::close(dl_agent_rt::CloseCode::NORMAL, "bye", 4).expect("close"),
        Frame::ping(5, 6, vec![0; 16]),
    ];
    for _ in 0..100_000 {
        let base = &seeds[rng.below(seeds.len() as u64) as usize];
        let mut b = base.encode(&limits).expect("encode");
        for _ in 0..1 + rng.below(4) {
            match rng.below(3) {
                0 => {
                    let i = rng.below(b.len() as u64) as usize;
                    b[i] ^= 1 << rng.below(8);
                }
                1 => {
                    let cut = rng.below(b.len() as u64 + 1) as usize;
                    b.truncate(cut);
                }
                _ => b.push(rng.next() as u8),
            }
            if b.is_empty() {
                break;
            }
        }
        if let Ok(f) = Frame::decode(&b, &limits) {
            assert!(f.validate().is_ok());
        }
    }
}
