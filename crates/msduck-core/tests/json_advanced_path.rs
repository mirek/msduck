use msduck_core::json_path::{
    PATH, diagnostic, exists, exists_utf16, extract, extract_utf16, path,
};

#[test]
fn captured_range_results_and_diagnostic_identities_match_both_runs() {
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("../../../reference/json-advanced-path.json")).unwrap();
    let runs = fixture["containers"][0]["runs"].as_array().unwrap();
    let mut compared = 0;
    for run in runs {
        for record in run.as_array().unwrap() {
            let name = record["name"].as_str().unwrap();
            let source = record["source"].as_str().unwrap();
            let path_text = record["path"].as_str().unwrap();
            let query = record["fn"] == "JSON_QUERY";
            let actual = extract(source, path_text, query);
            let source_units = source.encode_utf16().collect::<Vec<_>>();
            let path_units = path_text.encode_utf16().collect::<Vec<_>>();
            let utf16 = extract_utf16(&source_units, &path_units, query)
                .map(|value| value.map(|units| String::from_utf16(&units).unwrap()));
            assert_eq!(actual, utf16, "UTF-16 parity: {name}/query={query}");
            let errors = record["result"]["errors"].as_array().unwrap();
            if let Some(reference) = errors.first() {
                let error = actual.unwrap_err();
                let identity = diagnostic(error).unwrap();
                assert_eq!(identity.number, reference["number"], "{name}/query={query}");
                if identity.number != 13607 {
                    assert_eq!(identity.state, reference["state"], "{name}/query={query}");
                }
                if !matches!(identity.number, 13607 | 13609 | 13659) {
                    assert_eq!(
                        identity.message, reference["message"],
                        "{name}/query={query}"
                    );
                }
            } else {
                let value = &record["result"]["sets"][0]["rows"][0][0];
                let expected = value.as_str().map(str::to_owned);
                assert_eq!(actual, Ok(expected), "{name}/query={query}");
            }
            compared += 1;
        }
    }
    assert_eq!(compared, 284);
}

#[test]
fn ordinary_and_existence_paths_still_reject_ranges() {
    let source = "[1,2]";
    let selector = "$[0 to 1]";
    assert_eq!(path(selector), Err(PATH));
    assert!(!exists(source, selector));
    assert!(!exists_utf16(
        &source.encode_utf16().collect::<Vec<_>>(),
        &selector.encode_utf16().collect::<Vec<_>>()
    ));
    assert_eq!(extract(source, selector, false), Ok(None));

    let many = format!("[{}]", "1,".repeat(10_000).trim_end_matches(','));
    assert_eq!(
        extract(&many, "$[9999 to 9999]", false),
        Ok(Some("1".into()))
    );
    assert_eq!(extract(&many, "$[0 to 9999]", false), Ok(None));
}
