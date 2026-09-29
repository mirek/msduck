// Keep this pure module isolated until a separately claimed lib.rs export is free.
#[path = "../src/decompress.rs"]
mod decompress;

use decompress::{DecodeError, decompress};
use serde_json::Value;

fn unhex(text: &str) -> Vec<u8> {
    text.as_bytes()
        .chunks_exact(2)
        .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
        .collect()
}

#[test]
fn retained_foreign_members() {
    let fixture: Value =
        serde_json::from_str(include_str!("../../../reference/compress-decompress.json")).unwrap();
    let mut checked = 0;
    for container in fixture["containers"].as_array().unwrap() {
        for run in container["runs"].as_array().unwrap() {
            for case in run.as_array().unwrap() {
                let name = case["name"].as_str().unwrap();
                if !name.starts_with("decompress foreign ") {
                    continue;
                }
                let sql = case["sql"].as_str().unwrap();
                let start = sql.find("DECOMPRESS(0x").unwrap() + "DECOMPRESS(0x".len();
                let hex: String = sql[start..]
                    .chars()
                    .take_while(char::is_ascii_hexdigit)
                    .collect();
                let bytes = unhex(&hex);
                let actual = decompress(Some(&bytes), 1 << 20);
                let errors = case["result"]["errors"].as_array().unwrap();
                if !errors.is_empty() {
                    assert_eq!(errors[0]["number"], 9826, "{name}");
                    assert_eq!(actual, Err(DecodeError::Corrupt), "{name}");
                } else {
                    let value = &case["result"]["sets"][0]["rows"][0][0];
                    let expected = if value.is_null() {
                        None
                    } else {
                        Some(unhex(value["value"].as_str().unwrap()))
                    };
                    assert_eq!(actual, Ok(expected), "{name}");
                }
                checked += 1;
            }
        }
    }
    assert_eq!(checked, 30 * 4);
}

#[test]
fn null_empty_and_bound() {
    assert_eq!(decompress(None, 0), Ok(None));
    assert_eq!(decompress(Some(b""), 0), Ok(Some(vec![])));
    let hello = unhex("1f8b0800000000000400cb48cdc9c9070086a6103605000000");
    assert_eq!(decompress(Some(&hello), 5), Ok(Some(b"hello".to_vec())));
    assert_eq!(
        decompress(Some(&hello), 4),
        Err(DecodeError::OutputLimitExceeded)
    );
}

#[test]
fn server_created_members_preserve_source_bytes() {
    let varchar = unhex("1f8b08000000000004004b4c4a0600c241243503000000");
    let nvarchar = unhex("1f8b08000000000004004b64486248660000b07a95ad06000000");
    assert_eq!(decompress(Some(&varchar), 3), Ok(Some(b"abc".to_vec())));
    assert_eq!(
        decompress(Some(&nvarchar), 6),
        Ok(Some(b"a\0b\0c\0".to_vec()))
    );
    // This compact captured member expands to 8,000 UTF-16LE source bytes.
    let large = unhex(
        "1f8b0800000000000400edc3010d00000c022063fbf606396c3497aaaaaaaaaaaaaaeaf3031fa936e3401f0000",
    );
    assert_eq!(
        decompress(Some(&large), 7999),
        Err(DecodeError::OutputLimitExceeded)
    );
    let output = decompress(Some(&large), 8000).unwrap().unwrap();
    assert_eq!(output.len(), 8000);
    assert!(output.chunks_exact(4).all(|unit| unit == b"x\0y\0"));
}
