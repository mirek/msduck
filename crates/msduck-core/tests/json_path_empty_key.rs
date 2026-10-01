use msduck_core::json_path::{diagnostic, extract_detailed, extract_utf16_detailed};
use serde_json::json;

#[test]
fn captured_empty_property_errors_preserve_identity_before_document_validation() {
    let fixture: serde_json::Value = serde_json::from_str(include_str!(
        "../../../reference/prepared-json-path-errors.json"
    ))
    .unwrap();
    assert_eq!(fixture["runs"][0], fixture["runs"][1]);
    let mut compared = 0;
    for run in fixture["runs"].as_array().unwrap() {
        for record in run.as_array().unwrap() {
            let document = match record["name"].as_str() {
                Some("JSON_QUERY malformed path") => "{}",
                Some("JSON_QUERY malformed document and path") => "bad",
                _ => continue,
            };
            let path = "$.[";
            let error = extract_detailed(document, path, true).unwrap_err();
            let utf16 = extract_utf16_detailed(
                &document.encode_utf16().collect::<Vec<_>>(),
                &path.encode_utf16().collect::<Vec<_>>(),
                true,
            )
            .unwrap_err();
            assert_eq!(error, utf16);
            let actual = diagnostic(&error.backend_message()).unwrap();
            assert_eq!(
                json!({"number":actual.number,"state":actual.state,
                    "class":actual.severity,"lineNumber":1,"message":actual.message}),
                record["preparation"]["errors"][0]
            );
            compared += 1;
        }
    }
    assert_eq!(compared, 8);
}

#[test]
fn valid_property_and_array_selection_and_existing_document_errors_survive() {
    assert_eq!(
        extract_detailed("{\"a\":[{\"b\":{}}]}", "$.a[0].b", true).unwrap(),
        Some("{}".into())
    );
    assert_eq!(
        extract_detailed("{\"[\":{}}", "$.\"[\"", true).unwrap(),
        Some("{}".into())
    );
    let error = extract_detailed("bad", "$.a", true).unwrap_err();
    let diagnostic = diagnostic(&error.backend_message()).unwrap();
    assert_eq!((diagnostic.number, diagnostic.state), (13609, 1));
    assert_eq!(
        diagnostic.message,
        "JSON text is not properly formatted. Unexpected character 'b' is found at position 0."
    );
}
