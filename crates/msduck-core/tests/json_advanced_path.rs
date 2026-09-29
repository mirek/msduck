use msduck_core::json_path::{
    PATH, diagnostic, exists, exists_utf16, extract, extract_detailed, extract_utf16,
    extract_utf16_detailed, path,
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
            let detailed = extract_detailed(source, path_text, query);
            let detailed_utf16 = extract_utf16_detailed(&source_units, &path_units, query)
                .map(|value| value.map(|units| String::from_utf16(&units).unwrap()));
            assert_eq!(detailed, detailed_utf16, "detailed UTF-16 parity: {name}");
            assert_eq!(
                actual,
                detailed.clone().map_err(|error| error.marker()),
                "legacy extraction parity: {name}"
            );
            let errors = record["result"]["errors"].as_array().unwrap();
            if let Some(reference) = errors.first() {
                let error = actual.unwrap_err();
                let identity = diagnostic(error).unwrap();
                assert_eq!(identity.number, reference["number"], "{name}/query={query}");
                if identity.number != 13607 {
                    assert_eq!(identity.state, reference["state"], "{name}/query={query}");
                }
                if identity.number == 13607 {
                    let error = detailed.as_ref().unwrap_err();
                    let exact = diagnostic(&error.backend_message()).unwrap();
                    assert_eq!(exact.number, 13607, "{name}/query={query}");
                    assert_eq!(exact.state, reference["state"], "{name}/query={query}");
                    assert_eq!(exact.message, reference["message"], "{name}/query={query}");
                }
                if identity.number == 13609 {
                    let error = detailed.as_ref().unwrap_err();
                    let exact = diagnostic(&error.backend_message()).unwrap();
                    assert_eq!(exact.number, 13609, "{name}/query={query}");
                    assert_eq!(exact.state, reference["state"], "{name}/query={query}");
                    assert_eq!(exact.message, reference["message"], "{name}/query={query}");
                }
                if identity.number == 13659 {
                    let message = detailed.unwrap_err().to_string();
                    assert_eq!(message, reference["message"], "{name}/query={query}");
                    let exact = diagnostic(&message).unwrap();
                    assert_eq!(exact.number, 13659);
                    assert_eq!(exact.state, 1);
                    assert_eq!(exact.message, message);
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
fn dynamic_range_identity_requires_a_complete_canonical_message() {
    for message in [
        "Index 0 provided at position 3 is not within the array of size 2.",
        "Index 2 provided at position 7 is not within the array of size 4.",
    ] {
        assert_eq!(diagnostic(message).unwrap().message, message);
        for altered in [
            format!("prefix {message}"),
            format!("{message} trailing"),
            message.replace("Index ", "Index 0"),
            message.replace("size ", "size 0"),
            message.trim_end_matches('.').to_owned(),
        ] {
            assert!(diagnostic(&altered).is_none(), "{altered}");
        }
    }
}

#[test]
fn syntax_identity_requires_a_complete_canonical_backend_marker() {
    let canonical = "__msduck_json_path_syntax_v1:21:4:84";
    let identity = diagnostic(canonical).unwrap();
    assert_eq!(identity.number, 13607);
    assert_eq!(identity.state, 21);
    assert_eq!(
        identity.message,
        "JSON path is not properly formatted. Unexpected character 'T' is found at position 4."
    );
    for malformed in [
        "__msduck_json_path_syntax_v1:21:04:84",
        "__msduck_json_path_syntax_v1:1:4:84",
        "__msduck_json_path_syntax_v1:21:0:84",
        "__msduck_json_path_syntax_v1:21:4:84:1",
        "prefix __msduck_json_path_syntax_v1:21:4:84",
    ] {
        assert!(diagnostic(malformed).is_none(), "{malformed}");
    }
}

#[test]
fn document_identity_requires_a_complete_canonical_backend_marker() {
    let canonical = "__msduck_json_document_syntax_v1:3:120";
    let identity = diagnostic(canonical).unwrap();
    assert_eq!(identity.number, 13609);
    assert_eq!(identity.state, 1);
    assert_eq!(
        identity.message,
        "JSON text is not properly formatted. Unexpected character 'x' is found at position 3."
    );
    for malformed in [
        "__msduck_json_document_syntax_v1:03:120",
        "__msduck_json_document_syntax_v1:3:0120",
        "__msduck_json_document_syntax_v1:3:120:1",
        "prefix __msduck_json_document_syntax_v1:3:120",
    ] {
        assert!(diagnostic(malformed).is_none(), "{malformed}");
    }
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
