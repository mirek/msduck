use msduck_tds::{
    MAX_MESSAGE,
    framing::{Decoder, Error},
};

fn packet(kind: u8, status: u8, id: u8, body: &[u8]) -> Vec<u8> {
    let len = (body.len() + 8) as u16;
    let mut bytes = vec![kind, status, (len >> 8) as u8, len as u8, 0, 0, id, 0];
    bytes.extend_from_slice(body);
    bytes
}

fn fragmented() -> Vec<u8> {
    [packet(1, 2, 255, &[0x61; 504]), packet(1, 1, 0, &[0x62; 3])].concat()
}

#[test]
fn every_split_and_bytewise_input_preserve_payload_status_and_id_wrap() {
    let wire = fragmented();
    for split in 0..wire.len() {
        let mut d = Decoder::new(512, 507).unwrap();
        let first = d.feed(&wire[..split]).unwrap();
        assert_eq!(first.consumed, split);
        assert!(first.message.is_none());
        let second = d.feed(&wire[split..]).unwrap();
        assert_eq!(second.consumed, wire.len() - split);
        let message = second.message.unwrap();
        assert_eq!(message.kind, 1);
        assert_eq!(message.status, 3); // IGNORE survives from the first fragment.
        assert_eq!(message.payload, [vec![0x61; 504], vec![0x62; 3]].concat());
        d.finish().unwrap();
    }
    let mut d = Decoder::new(512, 507).unwrap();
    for (i, byte) in wire.iter().enumerate() {
        let result = d.feed(&[*byte]).unwrap();
        assert_eq!(result.consumed, 1);
        assert_eq!(result.message.is_some(), i + 1 == wire.len());
    }
    d.finish().unwrap();
}

#[test]
fn input_tail_stays_with_caller_and_attention_is_its_own_message() {
    let first = fragmented();
    let attention = packet(6, 1, 1, &[]);
    let all = [first.clone(), attention].concat();
    let mut d = Decoder::new(512, 507).unwrap();
    let a = d.feed(&all).unwrap();
    assert_eq!(a.consumed, first.len());
    assert_eq!(a.message.unwrap().kind, 1);
    d.set_packet_size(4096).unwrap();
    let b = d.feed(&all[a.consumed..]).unwrap();
    assert_eq!(b.consumed, 8);
    let message = b.message.unwrap();
    assert_eq!(message.kind, 6);
    assert!(message.payload.is_empty());
    d.finish().unwrap();
    assert_eq!(d.feed(&[]).err(), Some(Error::Finished));
}

#[test]
fn repeated_tiberius_ids_are_preserved_without_accepting_unrelated_ids() {
    let first = packet(1, 0, 42, &[0; 504]);
    for (id, expected) in [(42, None), (43, None), (44, Some(Error::PacketOrder))] {
        let mut d = Decoder::new(512, MAX_MESSAGE).unwrap();
        d.feed(&first).unwrap();
        let result = d.feed(&packet(1, 1, id, &[1]));
        assert_eq!(result.err(), expected);
    }
}

#[test]
fn malformed_headers_poison_without_waiting_for_advertised_bodies() {
    for (bytes, error) in [
        (vec![1, 1, 0, 7, 0, 0, 1, 0], Error::PacketLength),
        (vec![1, 1, 2, 1, 0, 0, 1, 0], Error::PacketLength),
        (vec![1, 0, 0, 8, 0, 0, 1, 0], Error::ShortNonFinalPacket),
        (vec![1, 1, 0, 10, 0, 0, 1, 0], Error::MessageLimit),
    ] {
        let mut d = Decoder::new(512, 1).unwrap();
        assert_eq!(d.feed(&bytes).err(), Some(error));
        assert_eq!(d.feed(&packet(6, 1, 1, &[])).err(), Some(Error::Poisoned));
        assert_eq!(d.finish(), Err(Error::Poisoned));
        assert_eq!(d.set_packet_size(1024), Err(Error::Poisoned));
    }
    let mut d = Decoder::new(512, MAX_MESSAGE).unwrap();
    d.feed(&packet(1, 0, 1, &[0; 504])).unwrap();
    assert_eq!(
        d.feed(&packet(3, 1, 2, &[])).err(),
        Some(Error::ChangedType)
    );
}

#[test]
fn eof_at_every_incomplete_prefix_is_fatal_including_between_packets() {
    let bytes = fragmented();
    for end in 1..bytes.len() {
        let mut d = Decoder::new(512, MAX_MESSAGE).unwrap();
        d.feed(&bytes[..end]).unwrap();
        assert_eq!(d.finish(), Err(Error::IncompleteEof), "prefix {end}");
        assert_eq!(d.feed(&bytes[end..]).err(), Some(Error::Poisoned));
    }
    let mut empty = Decoder::new(512, 0).unwrap();
    assert_eq!(empty.feed(&[]).unwrap().consumed, 0);
    empty.finish().unwrap();
}

#[test]
fn limits_and_packet_size_changes_are_checked_at_message_boundaries() {
    assert_eq!(
        Decoder::new(511, 1).err(),
        Some(Error::InvalidConfiguration)
    );
    assert_eq!(
        Decoder::new(32768, 1).err(),
        Some(Error::InvalidConfiguration)
    );
    assert_eq!(
        Decoder::new(512, MAX_MESSAGE + 1).err(),
        Some(Error::InvalidConfiguration)
    );
    let mut d = Decoder::new(512, 506).unwrap();
    d.feed(&packet(1, 0, 1, &[0; 504])).unwrap();
    assert_eq!(d.set_packet_size(4096), Err(Error::Busy));
    assert_eq!(
        d.feed(&packet(1, 1, 2, &[0; 3])).err(),
        Some(Error::MessageLimit)
    );
    let mut zero = Decoder::new(512, 0).unwrap();
    assert!(zero.feed(&packet(6, 1, 1, &[])).unwrap().message.is_some());
    assert_eq!(zero.set_packet_size(511), Err(Error::InvalidConfiguration));
    zero.feed(&packet(6, 1, 1, &[])[..1]).unwrap();
    assert_eq!(zero.set_packet_size(4096), Err(Error::Busy));
}

#[test]
fn captured_attention_packets_decode_at_every_boundary() {
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("../../../reference/attention.json")).unwrap();
    let mut count = 0;
    for run in fixture["rawCaptures"].as_array().unwrap() {
        for case in run.as_array().unwrap() {
            for p in case["packets"].as_array().unwrap() {
                let hex = p["hex"].as_str().unwrap();
                let bytes: Vec<u8> = (0..hex.len())
                    .step_by(2)
                    .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
                    .collect();
                if p["direction"] != "out" || bytes[0] != 6 {
                    continue;
                }
                count += 1;
                for split in 0..bytes.len() {
                    let mut d = Decoder::new(4096, MAX_MESSAGE).unwrap();
                    assert!(d.feed(&bytes[..split]).unwrap().message.is_none());
                    let msg = d.feed(&bytes[split..]).unwrap().message.unwrap();
                    assert_eq!((msg.kind, msg.status), (6, 1));
                    assert!(msg.payload.is_empty());
                    d.finish().unwrap();
                }
            }
        }
    }
    assert_eq!(count, 48);
}

#[test]
fn read_hint_never_crosses_message_boundary_for_any_short_read_size() {
    let first = fragmented();
    let bytes = [first.clone(), packet(6, 1, 1, &[])].concat();
    for width in [1, 3, 8, 17, 504, 512, 4096] {
        let mut decoder = Decoder::new(512, MAX_MESSAGE).unwrap();
        let mut at = 0;
        loop {
            let hint = decoder.read_size().unwrap();
            assert!(hint > 0);
            let n = hint.min(width).min(bytes.len() - at);
            let progress = decoder.feed(&bytes[at..at + n]).unwrap();
            assert_eq!(progress.consumed, n);
            at += n;
            if progress.message.is_some() {
                break;
            }
        }
        assert_eq!(at, first.len());
        assert_eq!(decoder.read_size().unwrap(), 8);
    }
}
