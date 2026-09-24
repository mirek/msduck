use msduck_tds::request_lifecycle::{Action as A, Error, Event as E, Lifecycle, RequestId};

fn admit(s: &mut Lifecycle) -> RequestId {
    let actions = s.advance(E::Admit).unwrap();
    let id = s.active().unwrap();
    assert_eq!(actions, [A::Dispatch(id)]);
    id
}

#[test]
fn cancellation_latches_before_worker_entry() {
    let mut s = Lifecycle::default();
    let id = admit(&mut s);
    assert_eq!(s.advance(E::Attention).unwrap(), [A::CancelWorker(id)]);
    assert_eq!(
        s.advance(E::WorkerStarted(id)).unwrap(),
        [A::SkipExecution(id)]
    );
    assert_eq!(s.advance(E::Admit), Err(Error::Busy));
    assert_eq!(
        s.advance(E::WorkerQuiesced(id)).unwrap(),
        [A::QueueResponse(id)]
    );
    assert_eq!(
        s.advance(E::AttentionEomWritten(id)),
        Err(Error::InvalidTransition)
    );
    assert_eq!(
        s.advance(E::ResponseEomWritten(id)).unwrap(),
        [A::QueueAttentionAck(id)]
    );
    assert_eq!(s.advance(E::Admit), Err(Error::Busy));
    assert_eq!(
        s.advance(E::AttentionEomWritten(id)).unwrap(),
        [A::Retire(id)]
    );
}

#[test]
fn both_worker_completion_and_attention_orders_preserve_response_boundaries() {
    for completion_first in [false, true] {
        let mut s = Lifecycle::default();
        let id = admit(&mut s);
        assert_eq!(s.advance(E::WorkerStarted(id)).unwrap(), [A::Execute(id)]);
        if completion_first {
            assert_eq!(
                s.advance(E::WorkerQuiesced(id)).unwrap(),
                [A::QueueResponse(id)]
            );
            assert!(s.advance(E::Attention).unwrap().is_empty());
        } else {
            assert_eq!(s.advance(E::Attention).unwrap(), [A::CancelWorker(id)]);
            assert_eq!(
                s.advance(E::WorkerQuiesced(id)).unwrap(),
                [A::QueueResponse(id)]
            );
        }
        assert_eq!(
            s.advance(E::ResponseEomWritten(id)).unwrap(),
            [A::QueueAttentionAck(id)]
        );
        assert_eq!(
            s.advance(E::AttentionEomWritten(id)).unwrap(),
            [A::Retire(id)]
        );
        let next = admit(&mut s);
        assert!(next.get() > id.get());
        for stale in [
            E::Cancel(id),
            E::WorkerStarted(id),
            E::WorkerQuiesced(id),
            E::ResponseEomWritten(id),
            E::AttentionEomWritten(id),
        ] {
            let before = s.clone();
            assert!(s.advance(stale).unwrap().is_empty());
            assert_eq!(s, before);
        }
    }
}

#[test]
fn disconnect_cannot_retire_a_live_worker_or_emit_responses() {
    for started in [false, true] {
        let mut s = Lifecycle::default();
        let id = admit(&mut s);
        if started {
            s.advance(E::WorkerStarted(id)).unwrap();
        }
        assert_eq!(
            s.advance(E::Disconnect).unwrap(),
            [A::CloseTransport, A::DiscardOutput, A::CancelWorker(id)]
        );
        assert_eq!(s.active(), Some(id));
        assert!(s.advance(E::Disconnect).unwrap().is_empty());
        assert_eq!(s.advance(E::Admit), Err(Error::Closed));
        if !started {
            assert_eq!(
                s.advance(E::WorkerStarted(id)).unwrap(),
                [A::SkipExecution(id)]
            );
        }
        assert_eq!(s.advance(E::WorkerQuiesced(id)).unwrap(), [A::Retire(id)]);
        assert_eq!(s.active(), None);
    }
}

#[test]
fn idle_and_duplicate_wire_attention_are_not_guessed_from_active_captures() {
    let mut s = Lifecycle::default();
    assert_eq!(s.advance(E::Attention), Err(Error::IdleAttentionUncaptured));
    let id = admit(&mut s);
    s.advance(E::Attention).unwrap();
    let before = s.clone();
    assert_eq!(
        s.advance(E::Attention),
        Err(Error::RepeatedAttentionUncaptured)
    );
    assert_eq!(s, before);
    assert!(s.advance(E::Cancel(id)).unwrap().is_empty());
}

#[test]
fn retained_active_captures_require_two_messages_before_reuse() {
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("../../../reference/attention.json")).unwrap();
    assert_eq!(fixture["results"].as_array().unwrap().len(), 24);
    for case in fixture["results"].as_array().unwrap() {
        let responses = case["responses"].as_array().unwrap();
        assert_eq!(responses.len(), 2);
        assert_eq!(
            responses[1],
            serde_json::json!([{"token":253,"status":32,"command":253,"count":"0"}])
        );
        let mut s = Lifecycle::default();
        let id = admit(&mut s);
        s.advance(E::WorkerStarted(id)).unwrap();
        s.advance(E::Attention).unwrap();
        let original = s.advance(E::WorkerQuiesced(id)).unwrap();
        assert_eq!(original, [A::QueueResponse(id)]);
        assert_eq!(s.advance(E::Admit), Err(Error::Busy));
        let ack = s.advance(E::ResponseEomWritten(id)).unwrap();
        assert_eq!(ack, [A::QueueAttentionAck(id)]);
        assert_eq!(s.advance(E::Admit), Err(Error::Busy));
        s.advance(E::AttentionEomWritten(id)).unwrap();
        admit(&mut s);
    }
}

#[test]
fn bounded_event_sequences_preserve_quiescence_and_ack_ordering() {
    // Independent effect-history invariants, including invalid/repeated events.
    fn visit(
        s: Lifecycle,
        id: RequestId,
        depth: usize,
        started: bool,
        quiesced: bool,
        response_written: bool,
        ack_queued: bool,
    ) {
        if depth == 0 {
            return;
        }
        for event in [
            E::Admit,
            E::WorkerStarted(id),
            E::WorkerQuiesced(id),
            E::Attention,
            E::Cancel(id),
            E::ResponseEomWritten(id),
            E::AttentionEomWritten(id),
            E::Disconnect,
        ] {
            let mut next = s.clone();
            let Ok(actions) = next.advance(event) else {
                assert_eq!(next, s);
                continue;
            };
            let became_started = started || (event == E::WorkerStarted(id) && !actions.is_empty());
            let became_quiesced =
                quiesced || (event == E::WorkerQuiesced(id) && !actions.is_empty());
            let wrote_response =
                response_written || (event == E::ResponseEomWritten(id) && !actions.is_empty());
            let mut queued_ack = ack_queued;
            for action in &actions {
                match action {
                    A::QueueResponse(_) => {
                        assert!(became_started && became_quiesced && !next.is_closed())
                    }
                    A::QueueAttentionAck(_) => {
                        assert!(became_quiesced && wrote_response && !next.is_closed());
                        queued_ack = true;
                    }
                    A::Retire(_) => assert!(
                        became_quiesced
                            && (next.is_closed()
                                || (wrote_response
                                    && (!ack_queued || event == E::AttentionEomWritten(id))))
                    ),
                    A::Execute(_) => assert!(!next.is_closed()),
                    _ => {}
                }
            }
            // A later admitted request has separate focused stale-event tests.
            if next.active().is_some_and(|current| current != id) {
                continue;
            }
            visit(
                next,
                id,
                depth - 1,
                became_started,
                became_quiesced,
                wrote_response,
                queued_ack,
            );
        }
    }
    let mut s = Lifecycle::default();
    let id = admit(&mut s);
    visit(s, id, 7, false, false, false, false);
}
