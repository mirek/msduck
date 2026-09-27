#[path = "../src/soundex.rs"]
mod soundex;

use msduck_core::{
    character::{CastInput, CharacterType, Family, Length},
    encoding::{decode_cp1252, encode_cp1252},
};
use serde_json::Value;
use soundex::{Mode, cp1252};

fn fixture() -> Value {
    serde_json::from_str(include_str!("../../../reference/soundex-difference.json")).unwrap()
}

fn record<'a>(run: &'a [Value], name: &str) -> &'a [Value] {
    run.iter()
        .find(|entry| entry["name"] == name)
        .unwrap_or_else(|| panic!("missing {name}"))["result"]["sets"][0]["rows"]
        .as_array()
        .unwrap()
}

fn decoded(input: &[u8], mode: Mode) -> String {
    decode_cp1252(&cp1252(input, mode))
}

#[test]
fn every_captured_varchar_byte_and_context_matches_both_modes() {
    let fixture = fixture();
    let containers = fixture["containers"].as_array().unwrap();
    assert_eq!(containers.len(), 2);
    for container in containers {
        let runs = container["runs"].as_array().unwrap();
        assert_eq!(runs.len(), 2);
        for run in runs {
            let run = run.as_array().unwrap();
            for (name, mode) in [
                ("soundex per character varchar", Mode::Current),
                ("compatibility 100 per character varchar", Mode::Legacy),
            ] {
                let rows = record(run, name);
                assert_eq!(rows.len(), 256);
                for (number, row) in rows.iter().enumerate() {
                    assert_eq!(row[0].as_u64(), Some(number as u64));
                    let byte = number as u8;
                    let cases: [&[u8]; 5] = [
                        &[byte, b'B', b'D'],
                        &[b'B', byte, b'B'],
                        &[b'B', byte, b'D'],
                        &[b'B', b'D', byte],
                        &[byte],
                    ];
                    for (case, input) in cases.into_iter().enumerate() {
                        assert_eq!(
                            decoded(input, mode),
                            row[case + 1].as_str().unwrap(),
                            "{name}: byte {byte:#04x}, case {case}"
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn every_captured_source_word_matches_varchar_conversion() {
    let fixture = fixture();
    let target = CharacterType::new(Family::Varchar, Length::Bounded(40)).unwrap();
    for container in fixture["containers"].as_array().unwrap() {
        for run in container["runs"].as_array().unwrap() {
            let run = run.as_array().unwrap();
            let modern = record(run, "soundex source words");
            let legacy = record(run, "compatibility 100 source words");
            assert_eq!(modern.len(), 173);
            assert_eq!(legacy.len(), modern.len());
            for (index, (now, old)) in modern.iter().zip(legacy).enumerate() {
                assert_eq!(now[0].as_u64(), Some((index + 1) as u64));
                assert_eq!(old[0].as_u64(), now[0].as_u64());
                let Some(word) = now[1].as_str() else {
                    assert!(now[2].is_null() && old[1].is_null());
                    continue;
                };
                let converted = target.cast(word, CastInput::Text).unwrap();
                let bytes = encode_cp1252(&converted).unwrap();
                assert_eq!(
                    decoded(&bytes, Mode::Current),
                    now[2].as_str().unwrap(),
                    "word {index}: {word:?}"
                );
                assert_eq!(
                    decoded(&bytes, Mode::Legacy),
                    old[1].as_str().unwrap(),
                    "word {index}: {word:?}"
                );
            }
        }
    }
}

#[test]
fn boundaries_and_mode_specific_runs() {
    assert_eq!(cp1252(b"", Mode::Current), *b"0000");
    assert_eq!(cp1252(b"1Robert", Mode::Current), *b"0000");
    assert_eq!(cp1252(b"Robert-Other", Mode::Current), *b"R163");
    assert_eq!(cp1252(b"BHB", Mode::Current), *b"B000");
    assert_eq!(cp1252(b"BHB", Mode::Legacy), *b"B100");
    assert_eq!(cp1252(b"Pfister", Mode::Current), *b"P236");
    assert_eq!(cp1252(b"Pfister", Mode::Legacy), *b"P123");
    assert_eq!(cp1252(&vec![b'b'; 12_000], Mode::Current), *b"B000");
    assert_eq!(
        cp1252(&[0xff, b'R'], Mode::Current),
        [0x9f, b'6', b'0', b'0']
    );
}
