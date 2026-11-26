//! Unit tests of the state machines' rules (protocol document, "State machines").

use alloc::vec;
use alloc::vec::Vec;

use crate::*;

const L: Limits = Limits::new(DEFAULT_MAX_FRAME);

fn open_sender(chan: Channel, credit: u64) -> ChannelSender {
    let mut s = ChannelSender::new(chan);
    let r = s.connect(0);
    assert_eq!((r.kind, r.seq), (Kind::Resume, 0));
    s.on_resume_ok(0).unwrap();
    s.grant(credit);
    s
}

fn sent(o: Outcome<Frame>) -> Frame {
    match o {
        Outcome::Done(f) => f,
        Outcome::WouldBlock(w) => panic!("blocked: {:?}", w.reason),
    }
}

fn blocked<T: core::fmt::Debug>(o: Outcome<T>) -> WouldBlock {
    match o {
        Outcome::WouldBlock(w) => w,
        Outcome::Done(t) => panic!("sent: {t:?}"),
    }
}

#[test]
fn sender_numbers_from_one_and_spends_credit() {
    let mut s = open_sender(Channel::Capture, 10);
    let f = sent(s.try_send(vec![1; 4], Ext::NONE, 5, &L).unwrap());
    assert_eq!((f.seq, f.sent_us, s.credit()), (1, 5, 6));
    let f = sent(s.try_send(vec![2; 6], Ext::NONE, 6, &L).unwrap());
    assert_eq!((f.seq, s.credit(), s.unacked().len()), (2, 0, 2));
    let w = blocked(s.try_send(vec![3], Ext::NONE, 7, &L).unwrap());
    assert_eq!(w.payload, vec![3]);
    assert_eq!(
        w.reason,
        Blocked::Credit {
            need: 1,
            available: 0
        }
    );
    // An empty payload needs no credit.
    assert_eq!(
        sent(s.try_send(Vec::new(), Ext::NONE, 8, &L).unwrap()).seq,
        3
    );
}

#[test]
fn sender_blocks_until_resumed_and_rejects_oversize() {
    let mut s = ChannelSender::new(Channel::Results);
    let w = blocked(s.try_send(vec![1], Ext::NONE, 0, &L).unwrap());
    assert_eq!(w.reason, Blocked::Disconnected);
    s.connect(0);
    s.grant(100);
    let w = blocked(s.try_send(vec![1], Ext::NONE, 0, &L).unwrap());
    assert_eq!(w.reason, Blocked::Resuming);
    let small = Limits::new(30);
    let e = s.try_send(vec![0; 7], Ext::NONE, 0, &small).unwrap_err();
    assert_eq!(e, Error::Oversize { len: 31, max: 30 });
}

#[test]
fn sender_acks_are_cumulative_and_checked() {
    let mut s = open_sender(Channel::Capture, 100);
    for _ in 0..3 {
        sent(s.try_send(vec![0], Ext::NONE, 0, &L).unwrap());
    }
    assert_eq!(s.on_ack(2), Ok(2));
    assert_eq!(s.on_ack(1), Ok(0)); // stale
    assert_eq!(
        s.on_ack(4),
        Err(Error::AckAhead {
            chan: Channel::Capture,
            ack: 4,
            last_sent: 3
        })
    );
    assert_eq!(s.unacked().map(|f| f.seq).collect::<Vec<_>>(), [3]);
}

#[test]
fn sender_resumes_from_hwm_in_order_under_credit() {
    let mut s = open_sender(Channel::Commands, 100);
    for i in 0..4u8 {
        sent(s.try_send(vec![i; 10], Ext::NONE, 0, &L).unwrap());
    }
    s.on_ack(1).unwrap();
    s.disconnect();
    assert_eq!(s.credit(), 0);
    let r = s.connect(50);
    assert_eq!((r.kind, r.seq, r.sent_us), (Kind::Resume, 1, 50));
    // The peer had committed 2 (its ack was lost): resend 3 and 4.
    assert_eq!(s.on_resume_ok(2), Ok(1));
    assert_eq!(
        s.poll_resend(60),
        Resend::Blocked {
            need: 10,
            available: 0
        }
    );
    s.grant(15);
    let w = blocked(s.try_send(vec![9], Ext::NONE, 60, &L).unwrap());
    assert_eq!(w.reason, Blocked::Resending);
    let Resend::Frame(f) = s.poll_resend(61) else {
        panic!()
    };
    assert_eq!((f.seq, f.sent_us, f.payload[0]), (3, 61, 2));
    assert_eq!(
        s.poll_resend(62),
        Resend::Blocked {
            need: 10,
            available: 5
        }
    );
    s.on_ack(4).unwrap(); // an ack covering the rest ends the resend
    assert_eq!(s.poll_resend(63), Resend::Idle);
    assert_eq!(sent(s.try_send(vec![9], Ext::NONE, 64, &L).unwrap()).seq, 5);
}

#[test]
fn sender_resume_ok_errors() {
    let mut s = open_sender(Channel::Capture, 100);
    sent(s.try_send(vec![0], Ext::NONE, 0, &L).unwrap());
    sent(s.try_send(vec![0], Ext::NONE, 0, &L).unwrap());
    let unexpected = Error::Unexpected {
        chan: Channel::Capture,
        kind: Kind::ResumeOk,
    };
    assert_eq!(s.on_resume_ok(1), Err(unexpected));
    s.on_ack(1).unwrap();
    s.connect(0);
    assert_eq!(
        s.on_resume_ok(3),
        Err(Error::ResumeAhead {
            chan: Channel::Capture,
            hwm: 3,
            last_sent: 2
        })
    );
    assert_eq!(
        s.on_resume_ok(0),
        Err(Error::ResumeBehind {
            chan: Channel::Capture,
            hwm: 0,
            acked: 1
        })
    );
    assert_eq!(s.on_resume_ok(2), Ok(1));
}

#[test]
fn sender_restore_checks_order() {
    let f = |seq| Frame::data(Channel::Bulk, seq, 0, Ext::NONE, vec![seq as u8]);
    let s = ChannelSender::restore(Channel::Bulk, 4, vec![f(5), f(6)]).unwrap();
    assert_eq!((s.acked(), s.next_seq(), s.unacked().len()), (4, 7, 2));
    let e = ChannelSender::restore(Channel::Bulk, 4, vec![f(5), f(7)]).unwrap_err();
    assert_eq!(
        e,
        Error::Restore {
            chan: Channel::Bulk,
            expected_seq: 6
        }
    );
    let e = ChannelSender::restore(Channel::Capture, 4, vec![f(5)]).unwrap_err();
    assert_eq!(e.name(), "restore");
}

#[test]
fn receiver_dedupes_detects_gaps_and_acks_on_commit() {
    let mut r = ChannelReceiver::new(Channel::Capture);
    let d = |seq| Frame::data(Channel::Capture, seq, 0, Ext::NONE, vec![0; 2]);
    let c = r.grant(7, 1);
    assert_eq!((c.kind, c.credit_bytes()), (Kind::Credit, Some(7)));
    assert_eq!(r.on_data(&d(1)), Ok(Delivery::Fresh));
    assert_eq!(r.on_data(&d(1)), Ok(Delivery::Duplicate));
    assert_eq!(r.on_data(&d(3)), Ok(Delivery::Gap { expected: 2 }));
    assert_eq!(
        r.on_data(&d(2)),
        Err(Error::CreditExceeded {
            chan: Channel::Capture,
            need: 2,
            available: 1
        })
    );
    assert_eq!(
        r.commit(2, 0),
        Err(Error::CommitAhead {
            chan: Channel::Capture,
            seq: 2,
            received: 1
        })
    );
    let ack = r.commit(1, 9).unwrap().unwrap();
    assert_eq!((ack.kind, ack.seq, ack.sent_us), (Kind::Ack, 1, 9));
    assert_eq!(r.commit(1, 9), Ok(None));
}

#[test]
fn receiver_resume_answers_hwm_and_rewind_forgets_uncommitted() {
    let mut r = ChannelReceiver::with_hwm(Channel::Results, 10);
    r.grant(100, 0);
    let d = |seq| Frame::data(Channel::Results, seq, 0, Ext::NONE, vec![1]);
    assert_eq!(r.on_data(&d(11)), Ok(Delivery::Fresh));
    let ok = r.on_resume(10, 5).unwrap();
    assert_eq!((ok.kind, ok.seq), (Kind::ResumeOk, 10));
    assert_eq!(
        r.on_resume(11, 5),
        Err(Error::ResumeConflict {
            chan: Channel::Results,
            last_acked: 11,
            hwm: 10
        })
    );
    assert_eq!(r.on_data(&d(11)), Ok(Delivery::Duplicate));
    r.rewind();
    assert_eq!(r.on_data(&d(11)), Ok(Delivery::Fresh));
    let other = Frame::data(Channel::Bulk, 1, 0, Ext::NONE, Vec::new());
    assert_eq!(r.on_data(&other).unwrap_err().name(), "wrong_channel");
    r.disconnect();
    assert_eq!(r.credit(), 0);
}

fn handshake(a: &mut Session, b: &mut Session) {
    a.connect(0);
    b.connect(0);
    for _ in 0..2 {
        while let Some(f) = a.poll_outgoing() {
            b.handle(f, 0).unwrap();
        }
        while let Some(f) = b.poll_outgoing() {
            a.handle(f, 0).unwrap();
        }
    }
}

#[test]
fn session_writes_control_before_capture_before_bulk() {
    let (mut a, mut b) = (
        Session::new(Config::default()),
        Session::new(Config::default()),
    );
    handshake(&mut a, &mut b);
    assert_eq!(a.pending_outgoing(), 0);
    let send = |s: &mut Session, c: Channel| match s.send(c, vec![c.as_u8()], Ext::NONE, 1) {
        Ok(Outcome::Done(seq)) => seq,
        other => panic!("{other:?}"),
    };
    send(&mut a, Channel::Bulk);
    send(&mut a, Channel::Capture);
    send(&mut a, Channel::Results);
    send(&mut a, Channel::Control);
    a.ping(Vec::new(), 1).unwrap();
    let order: Vec<(Kind, Channel)> = core::iter::from_fn(|| a.poll_outgoing())
        .map(|f| (f.kind, f.chan))
        .collect();
    assert_eq!(
        order,
        [
            (Kind::Data, Channel::Control),
            (Kind::Ping, Channel::Control),
            (Kind::Data, Channel::Capture),
            (Kind::Data, Channel::Results),
            (Kind::Data, Channel::Bulk),
        ]
    );
}

#[test]
fn session_ping_pong_and_close() {
    let (mut a, mut b) = (
        Session::new(Config::default()),
        Session::new(Config::default()),
    );
    handshake(&mut a, &mut b);
    let seq = a.ping(vec![7], 100).unwrap();
    let ping = a.poll_outgoing().unwrap();
    assert_eq!(b.handle(ping, 120), Ok(Event::Ping { seq }));
    let pong = b.poll_outgoing().unwrap();
    assert_eq!(
        (pong.kind, pong.payload.as_slice()),
        (Kind::Pong, &[7u8][..])
    );
    assert_eq!(a.handle(pong, 140), Ok(Event::Pong { seq, rtt_us: 40 }));

    a.close(CloseCode::GOING_AWAY, "restart", 150).unwrap();
    let w = blocked(a.send(Channel::Capture, vec![1], Ext::NONE, 151).unwrap());
    assert_eq!(w.reason, Blocked::Closing);
    let close = a.poll_outgoing().unwrap();
    let ev = b.handle(close, 160).unwrap();
    assert_eq!(
        ev,
        Event::Closed {
            code: CloseCode::GOING_AWAY,
            reason: "restart".into()
        }
    );
    assert_eq!(b.state(), SessionState::Closing);
    b.disconnect();
    let stray = Frame::ack(Channel::Capture, 0, 0);
    assert_eq!(b.handle(stray, 170), Err(Error::NotConnected));
}

#[test]
fn session_initial_credit_and_resume_report() {
    let cfg = Config {
        initial_credit: [0, 64, 0, 0, 0],
        ..Config::default()
    };
    let (mut a, mut b) = (Session::new(cfg), Session::new(cfg));
    handshake(&mut a, &mut b);
    assert_eq!(a.sender(Channel::Capture).credit(), 64);
    assert_eq!(a.sender(Channel::Bulk).credit(), 0);
    let w = blocked(a.send(Channel::Bulk, vec![1], Ext::NONE, 1).unwrap());
    assert_eq!(
        w.reason,
        Blocked::Credit {
            need: 1,
            available: 0
        }
    );
    assert!(matches!(
        a.send(Channel::Capture, vec![1; 8], Ext::NONE, 1),
        Ok(Outcome::Done(1))
    ));
    // Reconnect before the frame is delivered: it is resent after resume_ok.
    a.connect(5);
    b.connect(5);
    let mut resumed = None;
    while let Some(f) = a.poll_outgoing() {
        b.handle(f, 5).unwrap();
    }
    while let Some(f) = b.poll_outgoing() {
        if let Event::Resumed {
            chan: Channel::Capture,
            hwm,
            resending,
        } = a.handle(f, 6).unwrap()
        {
            resumed = Some((hwm, resending));
        }
    }
    assert_eq!(resumed, Some((0, 1)));
    // a also queued its own resume_ok answers (control class, so first).
    let again = core::iter::from_fn(|| a.poll_outgoing())
        .find(|f| f.kind == Kind::Data)
        .unwrap();
    assert_eq!(
        (again.chan, again.seq, again.sent_us),
        (Channel::Capture, 1, 6)
    );
}

#[test]
fn session_from_parts_checks_channel_order() {
    let mut senders = Channel::ALL.map(ChannelSender::new);
    senders.swap(1, 2);
    let receivers = Channel::ALL.map(ChannelReceiver::new);
    let e = Session::from_parts(Config::default(), senders, receivers).unwrap_err();
    assert_eq!(
        e,
        Error::WrongChannel {
            expected: Channel::Capture,
            got: Channel::Commands
        }
    );
}

#[test]
fn encode_refuses_invalid_and_oversize_frames() {
    let mut f = Frame::ack(Channel::Capture, 1, 0);
    f.payload.push(1);
    assert_eq!(
        f.encode(&L),
        Err(Error::Field {
            kind: Kind::Ack,
            field: "payload"
        })
    );
    let big = Frame::data(Channel::Bulk, 1, 0, Ext::NONE, vec![0; 100]);
    assert_eq!(
        big.encode(&Limits::new(123)).unwrap_err().name(),
        "oversize"
    );
    assert_eq!(big.encode(&Limits::new(124)).unwrap().len(), 124);
    let long = "x".repeat(MAX_CLOSE_REASON + 1);
    assert_eq!(
        Frame::close(CloseCode::NORMAL, &long, 0)
            .unwrap_err()
            .name(),
        "field"
    );
}

#[test]
fn errors_display_their_facts() {
    let e = Error::Truncated { need: 24, have: 3 };
    assert_eq!(
        alloc::format!("{e}"),
        "frame truncated: need 24 bytes, have 3"
    );
    let e = Error::CreditExceeded {
        chan: Channel::Bulk,
        need: 5,
        available: 2,
    };
    assert_eq!(
        alloc::format!("{e}"),
        "bulk: data frame of 5 payload bytes exceeds the 2-byte credit"
    );
}
