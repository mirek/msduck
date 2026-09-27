use msduck_core::json_path::{DOCUMENT, extract, extract_utf16};

#[test]
fn captured_scalar_document_roots_are_invalid_for_both_extractors() {
    let fixture: serde_json::Value = serde_json::from_str(include_str!(
        "../../../reference/json-extraction-boundaries.json"
    ))
    .unwrap();
    let runs = fixture["containers"][0]["runs"].as_array().unwrap();
    let cases = [
        ("valid root scalar", "1"),
        ("valid root string", "\"text\""),
        ("valid root null", "null"),
    ];
    for run in runs {
        let records = run.as_array().unwrap();
        for (name, source) in cases {
            for (function, query) in [("JSON_VALUE", false), ("JSON_QUERY", true)] {
                let record = records
                    .iter()
                    .find(|record| record["name"] == name && record["fn"] == function)
                    .unwrap();
                assert_eq!(
                    record["result"]["errors"][0]["number"], 13609,
                    "{name}/{function}"
                );
                assert_eq!(
                    record["result"]["errors"][0]["state"], 1,
                    "{name}/{function}"
                );
                assert_eq!(
                    extract(source, "$", query),
                    Err(DOCUMENT),
                    "{name}/{function}"
                );
                let units = source.encode_utf16().collect::<Vec<_>>();
                assert_eq!(
                    extract_utf16(&units, &[b'$' as u16], query),
                    Err(DOCUMENT),
                    "UTF-16 {name}/{function}"
                );
            }
        }
    }
}

#[test]
fn object_and_array_roots_keep_early_source_preserving_selection() {
    assert_eq!(extract("{\"a\":1}", "$", false), Ok(None));
    assert_eq!(
        extract("{\"a\":1}", "$", true),
        Ok(Some("{\"a\":1}".into()))
    );
    assert_eq!(extract("[1,2]", "$", false), Ok(None));
    assert_eq!(extract("[1,2]", "$", true), Ok(Some("[1,2]".into())));
    assert_eq!(
        extract("{\"a\": [ 1, 2 ],\"b\":x}", "$.a", true),
        Ok(Some("[ 1, 2 ]".into()))
    );
    assert_eq!(
        extract("{\"a\":1,\"b\":x}", "$.a", false),
        Ok(Some("1".into()))
    );
}
