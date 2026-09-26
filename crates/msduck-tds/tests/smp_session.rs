// Temporary path import until the separately owned lib.rs can export this module.
pub use msduck_tds::smp;
#[path = "../src/smp_session.rs"]
mod smp_session;

use smp::{Header, Kind, Limits};
use smp_session::{Action, Config, Phase, Sessions};

fn header(kind: Kind, sid: u16, seq: u32, window: u32) -> Header {
    let bytes = smp::encode(kind, sid, seq, window, &[], Limits::new(8).unwrap()).unwrap();
    Header::decode(bytes[..16].try_into().unwrap(), Limits::new(8).unwrap()).unwrap()
}

fn machine(max_sessions: usize, max_credit: u32) -> Sessions {
    Sessions::new(Config::new(max_sessions, max_credit).unwrap())
}

#[test]
fn four_packet_credit_is_inclusive_and_ack_opens_fifth() {
    let mut sessions = machine(2, 16);
    assert_eq!(
        sessions.receive(header(Kind::Syn, 5, 0, 4)).unwrap(),
        Action::Opened { session_id: 5 }
    );
    for seq in 1..=4 {
        assert_eq!(
            sessions.send_data(5).unwrap(),
            Some(Action::Frame {
                kind: Kind::Data,
                session_id: 5,
                sequence: seq,
                window: 4,
            })
        );
    }
    assert_eq!(
        &smp::encode(Kind::Data, 5, 4, 4, &[], Limits::new(8).unwrap()).unwrap()[..16],
        &[
            0x53, 0x08, 0x05, 0x00, 0x10, 0, 0, 0, 0x04, 0, 0, 0, 0x04, 0, 0, 0
        ]
    );
    assert_eq!(sessions.send_data(5).unwrap(), None);
    assert_eq!(
        sessions.receive(header(Kind::Ack, 5, 0, 5)).unwrap(),
        Action::None
    );
    assert_eq!(
        sessions.send_data(5).unwrap(),
        Some(Action::Frame {
            kind: Kind::Data,
            session_id: 5,
            sequence: 5,
            window: 4,
        })
    );
    assert_eq!(sessions.get(5).unwrap().sent, 5);
}

#[test]
fn receive_credit_advances_only_after_consumer_release() {
    let mut sessions = machine(1, 16);
    sessions.receive(header(Kind::Syn, 2, 0, 4)).unwrap();
    for seq in 1..=4 {
        assert_eq!(
            sessions.receive(header(Kind::Data, 2, seq, 4)).unwrap(),
            Action::DeliverData { session_id: 2 }
        );
    }
    let before = sessions.get(2).unwrap();
    assert_eq!(before.local_window, 4);
    assert!(sessions.receive(header(Kind::Data, 2, 5, 4)).is_err());
    assert_eq!(sessions.get(2), Some(before));
    assert_eq!(
        sessions.release(2, 1).unwrap(),
        Action::Frame {
            kind: Kind::Ack,
            session_id: 2,
            sequence: 0,
            window: 5,
        }
    );
    assert_eq!(
        sessions.receive(header(Kind::Data, 2, 5, 4)).unwrap(),
        Action::DeliverData { session_id: 2 }
    );
    assert!(sessions.release(2, 5).is_err());
    assert_eq!(
        sessions.release(2, 4).unwrap(),
        Action::Frame {
            kind: Kind::Ack,
            session_id: 2,
            sequence: 0,
            window: 9,
        }
    );
}

#[test]
fn invalid_sequences_and_windows_leave_state_unchanged() {
    let mut sessions = machine(1, 8);
    for invalid in [
        header(Kind::Syn, 1, 1, 4),
        header(Kind::Syn, 1, 0, 0),
        header(Kind::Syn, 1, 0, 9),
    ] {
        assert!(sessions.receive(invalid).is_err());
        assert!(sessions.is_empty());
    }
    sessions.receive(header(Kind::Syn, 1, 0, 4)).unwrap();
    let before = sessions.get(1).unwrap();
    for invalid in [
        header(Kind::Syn, 1, 0, 4),
        header(Kind::Data, 1, 2, 4),
        header(Kind::Ack, 1, 1, 5),
        header(Kind::Ack, 1, 0, 3),
        header(Kind::Ack, 1, 0, 9),
        header(Kind::Fin, 1, 1, 4),
    ] {
        assert!(sessions.receive(invalid).is_err());
        assert_eq!(sessions.get(1), Some(before));
    }
    sessions.receive(header(Kind::Data, 1, 1, 4)).unwrap();
    let after = sessions.get(1).unwrap();
    assert!(sessions.receive(header(Kind::Data, 1, 1, 4)).is_err());
    assert_eq!(sessions.get(1), Some(after));
    assert!(sessions.release(1, 2).is_err());
    assert_eq!(sessions.get(1), Some(after));
}

#[test]
fn wrap_is_valid_for_data_and_window_not_for_backward_credit() {
    // A real connection would take 2^32 packets to reach this state. Lifted
    // counters let this pure test verify the boundary without allocating them.
    let mut sessions = machine(1, 8);
    sessions.receive(header(Kind::Syn, 7, 0, 4)).unwrap();
    let state = sessions.sessions_for_test_mut(7);
    state.sent = u64::from(u32::MAX) - 1;
    state.peer_window = u64::from(u32::MAX);
    state.received = u64::from(u32::MAX) - 1;
    state.consumed = state.received;
    state.local_window = u64::from(u32::MAX) + 3;
    // WNDW zero is valid at rollover: it means logical sequence 2^32.
    assert_eq!(
        sessions
            .receive(header(Kind::Ack, 7, u32::MAX - 1, 0))
            .unwrap(),
        Action::None
    );
    assert_eq!(
        sessions.send_data(7).unwrap().unwrap(),
        Action::Frame {
            kind: Kind::Data,
            session_id: 7,
            sequence: u32::MAX,
            window: 2,
        }
    );
    assert_eq!(
        sessions.send_data(7).unwrap().unwrap(),
        Action::Frame {
            kind: Kind::Data,
            session_id: 7,
            sequence: 0,
            window: 2,
        }
    );
    assert_eq!(
        sessions
            .receive(header(Kind::Data, 7, u32::MAX, 1))
            .unwrap(),
        Action::DeliverData { session_id: 7 }
    );
    assert_eq!(
        sessions.receive(header(Kind::Data, 7, 0, 1)).unwrap(),
        Action::DeliverData { session_id: 7 }
    );
    let before = sessions.get(7).unwrap();
    assert!(sessions.receive(header(Kind::Ack, 7, 0, u32::MAX)).is_err());
    assert_eq!(sessions.get(7), Some(before));
}

#[test]
fn fin_in_either_order_and_data_in_fin_sent_follow_spec() {
    let mut sessions = machine(2, 8);
    sessions.receive(header(Kind::Syn, 1, 0, 4)).unwrap();
    sessions.receive(header(Kind::Syn, 2, 0, 4)).unwrap();
    assert_eq!(
        sessions.send_fin(1).unwrap(),
        Action::Frame {
            kind: Kind::Fin,
            session_id: 1,
            sequence: 0,
            window: 4,
        }
    );
    let before = sessions.get(1).unwrap();
    assert_eq!(before.phase, Phase::FinSent);
    assert_eq!(
        sessions.receive(header(Kind::Data, 1, 1, 5)).unwrap(),
        Action::IgnoreData { session_id: 1 }
    );
    assert_eq!(sessions.get(1), Some(before));
    assert!(sessions.send_fin(1).is_err());
    assert_eq!(
        sessions.receive(header(Kind::Fin, 1, 0, 4)).unwrap(),
        Action::Closed { session_id: 1 }
    );
    assert!(sessions.get(1).is_none());
    assert!(sessions.receive(header(Kind::Fin, 1, 0, 4)).is_err());

    assert_eq!(
        sessions.receive(header(Kind::Fin, 2, 0, 4)).unwrap(),
        Action::PeerFin { session_id: 2 }
    );
    assert_eq!(sessions.get(2).unwrap().phase, Phase::FinReceived);
    let before = sessions.get(2).unwrap();
    assert!(sessions.receive(header(Kind::Data, 2, 1, 4)).is_err());
    assert!(sessions.receive(header(Kind::Fin, 2, 0, 4)).is_err());
    assert_eq!(sessions.get(2), Some(before));
    assert_eq!(
        sessions.send_fin(2).unwrap(),
        Action::Frame {
            kind: Kind::Fin,
            session_id: 2,
            sequence: 0,
            window: 4,
        }
    );
    assert!(sessions.is_empty());
}

#[test]
fn session_limit_and_interleaved_credit_are_isolated() {
    let mut sessions = machine(2, 8);
    sessions.receive(header(Kind::Syn, 0, 0, 4)).unwrap();
    sessions.receive(header(Kind::Syn, 1, 0, 4)).unwrap();
    assert!(sessions.receive(header(Kind::Syn, 2, 0, 4)).is_err());
    assert_eq!(sessions.len(), 2);
    sessions.receive(header(Kind::Data, 0, 1, 4)).unwrap();
    sessions.send_data(1).unwrap();
    sessions.receive(header(Kind::Ack, 1, 0, 5)).unwrap();
    assert_eq!(sessions.get(0).unwrap().received, 1);
    assert_eq!(sessions.get(0).unwrap().sent, 0);
    assert_eq!(sessions.get(1).unwrap().received, 0);
    assert_eq!(sessions.get(1).unwrap().sent, 1);
    assert_eq!(sessions.get(0).unwrap().peer_window, 4);
    assert_eq!(sessions.get(1).unwrap().peer_window, 5);
}
