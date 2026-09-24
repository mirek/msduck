use msduck_core::{
    character::{CastInput, CharacterType, ConvertedUnicode, Error, Family, Length},
    encoding::encode_cp1252,
};
use serde_json::Value;

fn fixture() -> Value {
    serde_json::from_str(include_str!(
        "../../../reference/windows-1252-best-fit.json"
    ))
    .unwrap()
}
fn bytes(hex: &str) -> Vec<u8> {
    hex.as_bytes()
        .chunks_exact(2)
        .map(|p| u8::from_str_radix(std::str::from_utf8(p).unwrap(), 16).unwrap())
        .collect()
}
fn encoded(value: ConvertedUnicode) -> Vec<u8> {
    let ConvertedUnicode::Ansi(value) = value else {
        panic!("ANSI target")
    };
    encode_cp1252(&value).unwrap()
}

#[test]
fn every_utf16_unit_matches_all_captured_collations() {
    let data = fixture();
    let maps = data["maps"].as_array().unwrap();
    assert_eq!(maps.len(), 6);
    let expected = bytes(maps[0]["hex"].as_str().unwrap());
    assert_eq!(expected.len(), 65536);
    for map in maps {
        assert_eq!(bytes(map["hex"].as_str().unwrap()), expected);
    }
    let target = CharacterType::new(Family::Varchar, Length::Bounded(1)).unwrap();
    for unit in 0..=u16::MAX {
        let wanted = [expected[usize::from(unit)]];
        assert_eq!(
            encoded(target.cast_utf16(&[unit]).unwrap()),
            wanted,
            "cast U+{unit:04X}"
        );
        assert_eq!(
            encoded(target.store_utf16(&[unit]).unwrap()),
            wanted,
            "store U+{unit:04X}"
        );
        if let Some(ch) = char::from_u32(u32::from(unit)) {
            let text = ch.to_string();
            assert_eq!(
                encode_cp1252(&target.cast(&text, CastInput::Text).unwrap()).unwrap(),
                wanted,
                "UTF8 cast U+{unit:04X}"
            );
            assert_eq!(
                encode_cp1252(&target.store(&text).unwrap()).unwrap(),
                wanted,
                "UTF8 store U+{unit:04X}"
            );
        }
    }
}

#[test]
fn cast_padding_and_storage_overflow_match_captured_source_boundaries() {
    for probe in fixture()["probes"].as_array().unwrap() {
        let declaration = probe["declaration"].as_str().unwrap();
        let family = if declaration.starts_with("VARCHAR") {
            Family::Varchar
        } else {
            Family::Char
        };
        let length = declaration.split(['(', ')']).nth(1).unwrap();
        let length = if length == "MAX" {
            Length::Max
        } else {
            Length::Bounded(length.parse().unwrap())
        };
        let target = CharacterType::new(family, length).unwrap();
        let raw = bytes(probe["sourceHex"].as_str().unwrap());
        let units = raw
            .chunks_exact(2)
            .map(|b| u16::from_le_bytes([b[0], b[1]]))
            .collect::<Vec<_>>();
        let expected = bytes(
            probe["cast"]["result"]["sets"][0]["rows"][0][0]["value"]
                .as_str()
                .unwrap(),
        );
        assert_eq!(
            encoded(target.cast_utf16(&units).unwrap()),
            expected,
            "{declaration} {units:04X?}"
        );
        let storage = target.store_utf16(&units);
        if probe["storage"]["result"]["errors"]
            .as_array()
            .unwrap()
            .is_empty()
        {
            let expected = bytes(
                probe["storage"]["result"]["sets"][0]["rows"][0][0]["value"]
                    .as_str()
                    .unwrap(),
            );
            assert_eq!(
                encoded(storage.unwrap()),
                expected,
                "store {declaration} {units:04X?}"
            );
            if let Ok(text) = String::from_utf16(&units) {
                assert_eq!(
                    encode_cp1252(&target.store(&text).unwrap()).unwrap(),
                    expected
                );
            }
        } else {
            assert_eq!(storage, Err(Error::Truncated), "{declaration} {units:04X?}");
            if let Ok(text) = String::from_utf16(&units) {
                assert_eq!(target.store(&text), Err(Error::Truncated));
            }
        }
    }
}

#[test]
fn sql_best_fit_does_not_relax_the_wire_encoder() {
    assert!(encode_cp1252("Ā🦆").is_err());
    let target = CharacterType::new(Family::Varchar, Length::Max).unwrap();
    assert_eq!(target.cast("Ā🦆", CastInput::Text).unwrap(), "A??");
}
