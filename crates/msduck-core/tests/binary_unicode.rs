use msduck_core::{
    binary_unicode::{Error, convert},
    character::{CharacterType, Family, Length},
};

fn target(s: &str) -> CharacterType {
    let family = if s.starts_with("NCHAR") {
        Family::Nchar
    } else {
        Family::Nvarchar
    };
    let length = match s.split_once('(').map(|(_, s)| s.trim_end_matches(')')) {
        Some("MAX") => Length::Max,
        Some(n) => Length::Bounded(n.parse().unwrap()),
        None => Length::Bounded(30),
    };
    CharacterType::new(family, length).unwrap()
}
fn hex(s: &str) -> Vec<u8> {
    s.as_bytes()
        .chunks_exact(2)
        .map(|b| u8::from_str_radix(std::str::from_utf8(b).unwrap(), 16).unwrap())
        .collect()
}
#[test]
fn conversion_matches_raw_bytes_in_every_non_null_reference_result() {
    // serde_json requires Unicode scalar strings; SQL text may contain lone
    // surrogates. Escape only their display spelling for this byte-rule test.
    // Expected raw hex stays untouched; the JS replay compares original text.
    let mut json = include_str!("../../../tests/reference/binary-unicode.json").to_owned();
    for unit in 0xd800..=0xdfff {
        let spelling = format!("\\u{unit:04x}");
        json = json.replace(&spelling, &format!("\\{spelling}"));
    }
    let fixture: serde_json::Value = serde_json::from_str(&json).unwrap();
    let mut checked = 0;
    for case in fixture["cases"].as_array().unwrap() {
        let raw = &case["result"]["sets"][0]["rows"][0][1];
        if raw.is_null() {
            continue;
        }
        let source = case["source"].as_str().unwrap_or("0x410042");
        let style = case["style"]
            .as_i64()
            .or_else(|| case["style"].as_str().and_then(|s| s.parse().ok()))
            .unwrap_or(0) as i32;
        let value = convert(
            &hex(source.strip_prefix("0x").unwrap()),
            target(case["type"].as_str().unwrap_or("NVARCHAR(5)")),
            style,
            10000,
        )
        .unwrap();
        let actual: Vec<_> = value.iter().flat_map(|u| u.to_le_bytes()).collect();
        assert_eq!(
            actual,
            hex(raw["value"].as_str().unwrap()),
            "{}",
            case["sql"]
        );
        checked += 1;
    }
    assert_eq!(checked, 124);
}
#[test]
fn output_limits_apply_after_truncation_and_before_padding_allocation() {
    assert_eq!(
        convert(&[0x41, 0, 0x42], target("NVARCHAR(1)"), 0, 1).unwrap(),
        [65]
    );
    assert_eq!(
        convert(&[0x41], target("NVARCHAR(MAX)"), 0, 0),
        Err(Error::OutputLimit)
    );
    assert_eq!(
        convert(&[], target("NCHAR(3)"), 0, 2),
        Err(Error::OutputLimit)
    );
    assert_eq!(
        convert(&[0xab], target("NVARCHAR(MAX)"), 1, 3),
        Err(Error::OutputLimit)
    );
    assert_eq!(
        convert(&[], target("NVARCHAR(MAX)"), 3, 100),
        Err(Error::UnsupportedStyle(3))
    );
}
