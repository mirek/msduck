use msduck_core::json_path::{PATH, diagnostic, exists, exists_utf16, extract, extract_utf16};

#[test]
fn captured_wildcard_results_and_core_diagnostics_match_both_reference_runs() {
    let fixture: serde_json::Value = serde_json::from_str(include_str!(
        "../../../reference/json-extraction-wildcard.json"
    ))
    .unwrap();
    let runs = fixture["containers"][0]["runs"].as_array().unwrap();
    let mut compared = 0;
    let mut advanced = 0;
    for run in runs {
        for record in run.as_array().unwrap() {
            let name = record["name"].as_str().unwrap();
            let source = record["source"].as_str().unwrap();
            let path = record["path"].as_str().unwrap();
            let query = record["fn"] == "JSON_QUERY";
            if matches!(
                name,
                "range path" | "single range path" | "last path" | "list path"
            ) {
                assert_eq!(msduck_core::json_path::path(path), Err(PATH));
                advanced += 1;
            }
            let actual = extract(source, path, query);
            let source_units = source.encode_utf16().collect::<Vec<_>>();
            let path_units = path.encode_utf16().collect::<Vec<_>>();
            let utf16 = extract_utf16(&source_units, &path_units, query)
                .map(|value| value.map(|units| String::from_utf16(&units).unwrap()));
            assert_eq!(actual, utf16, "UTF-16 parity: {name}/query={query}");
            let errors = record["result"]["errors"].as_array().unwrap();
            if let Some(reference) = errors.first() {
                let error = actual.unwrap_err();
                let identity = diagnostic(error).unwrap();
                assert_eq!(identity.number, reference["number"], "{name}/query={query}");
                if name != "wildcard invalid suffix" {
                    assert_eq!(identity.state, reference["state"], "{name}/query={query}");
                }
                if identity.number != 13609 && identity.number != 13607 {
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
    assert_eq!(compared, 212);
    assert_eq!(advanced, 16);
}

#[test]
fn wildcard_existence_is_unchanged_and_large_arrays_are_iterative() {
    let source = "[1,2]";
    let path = "$[*]";
    assert!(exists(source, path));
    assert!(exists_utf16(
        &source.encode_utf16().collect::<Vec<_>>(),
        &path.encode_utf16().collect::<Vec<_>>()
    ));
    let many = format!("[{}]", "1,".repeat(20_000).trim_end_matches(','));
    assert_eq!(extract(&many, path, false), Ok(None));
    assert_eq!(extract(&many, path, true), Ok(None));
}
