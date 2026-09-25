#[path = "../src/for_xml_path.rs"]
mod for_xml_path;

use for_xml_path::{
    Alias, Context, Descriptor, Directive, Error, Field, Plan, RowName, Value, ValueKind, serialize,
};
use serde_json::Value as Json;

fn fixture() -> Json {
    serde_json::from_str(include_str!("../../../reference/for-xml-path.json")).unwrap()
}

fn observed(name: &str) -> Json {
    fixture()["runs"][0]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["name"] == name)
        .unwrap()["result"]
        .clone()
}

fn collation() -> Json {
    observed("ordinary row")["sets"][0]["columns"][0]["collation"].clone()
}

fn plan(
    row_name: RowName,
    fields: Vec<Field>,
    directives: Vec<Directive>,
    context: Context,
) -> Plan<Json> {
    Plan {
        row_name,
        fields,
        directives,
        context,
        collation: collation(),
    }
}

fn field(alias: Alias, kind: ValueKind) -> Field {
    Field { alias, kind }
}

fn element(path: &[&str], kind: ValueKind) -> Field {
    field(
        Alias::Element(path.iter().map(|part| (*part).into()).collect()),
        kind,
    )
}

fn attribute(name: &str, kind: ValueKind) -> Field {
    field(Alias::Attribute(name.into()), kind)
}

fn text(value: &str) -> Option<Value> {
    Some(Value::Text(value.encode_utf16().collect()))
}

fn integer(value: i64) -> Option<Value> {
    Some(Value::Integer(value))
}

fn assert_case(name: &str, plan: Plan<Json>, rows: Vec<Vec<Option<Value>>>) {
    let observation = observed(name);
    assert_eq!(observation["errors"], serde_json::json!([]), "{name}");
    let output = serialize(plan, &rows).unwrap_or_else(|error| panic!("{name}: {error:?}"));
    let set = &observation["sets"][0];
    let expected = set["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row[0].as_str().unwrap().to_owned())
        .collect::<Vec<_>>();
    let actual = output
        .rows
        .iter()
        .map(|units| String::from_utf16(units).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(actual, expected, "{name}");
    assert_eq!(
        observation["done"].as_array().unwrap().last().unwrap()["rowCount"],
        output.source_row_count,
        "{name}"
    );
    let column = &set["columns"][0];
    match output.descriptor {
        Descriptor::NText {
            name: label,
            collation,
        } => {
            assert_eq!(column["name"], label, "{name}");
            assert_eq!(column["type"], "NText", "{name}");
            assert_eq!(column["length"], 2_147_483_646_u64, "{name}");
            assert_eq!(column["flags"], 1, "{name}");
            assert_eq!(column["collation"], collation, "{name}");
        }
        Descriptor::Xml {
            name: label,
            nested,
        } => {
            assert_eq!(column["name"], label, "{name}");
            assert_eq!(column["type"], "Xml", "{name}");
            assert!(column["length"].is_null(), "{name}");
            assert!(column["collation"].is_null(), "{name}");
            assert_eq!(column["flags"], if nested { 35 } else { 3 }, "{name}");
        }
    }
}

#[test]
fn path_rows_aliases_order_and_xml_escaping_match_reference() {
    assert_case(
        "ordinary row",
        plan(
            RowName::Named("row".into()),
            vec![element(&["value"], ValueKind::Integer)],
            vec![],
            Context::Direct,
        ),
        vec![vec![integer(1)]],
    );
    assert_case(
        "empty row name",
        plan(
            RowName::Named(String::new()),
            vec![element(&["value"], ValueKind::Integer)],
            vec![],
            Context::Direct,
        ),
        vec![vec![integer(1)]],
    );
    assert_case(
        "default row name",
        plan(
            RowName::Default,
            vec![element(&["value"], ValueKind::Integer)],
            vec![],
            Context::Direct,
        ),
        vec![vec![integer(1)]],
    );
    assert_case(
        "attribute and escaped element",
        plan(
            RowName::Named("item".into()),
            vec![
                attribute("id", ValueKind::Integer),
                element(&["name"], ValueKind::Text),
            ],
            vec![],
            Context::Direct,
        ),
        vec![vec![integer(7), text("A&B <C> \"quoted\"")]],
    );
    for (name, alias) in [
        ("unnamed text", Alias::Unnamed),
        ("text node alias", Alias::Text),
    ] {
        assert_case(
            name,
            plan(
                RowName::Named("row".into()),
                vec![field(alias, ValueKind::Text)],
                vec![],
                Context::Direct,
            ),
            vec![vec![text("A&B <C>")]],
        );
    }
    assert_case(
        "nested element alias",
        plan(
            RowName::Named("row".into()),
            vec![
                element(&["meta", "id"], ValueKind::Integer),
                element(&["meta", "name"], ValueKind::Text),
            ],
            vec![],
            Context::Direct,
        ),
        vec![vec![integer(1), text("X")]],
    );
    assert_case(
        "ordered rows",
        plan(
            RowName::Named("item".into()),
            vec![
                attribute("id", ValueKind::Integer),
                element(&["name"], ValueKind::Text),
            ],
            vec![],
            Context::Direct,
        ),
        vec![vec![integer(1), text("one")], vec![integer(2), text("two")]],
    );
}

#[test]
fn null_empty_root_and_type_requirements_match_reference() {
    assert_case(
        "null element omitted",
        plan(
            RowName::Named("row".into()),
            vec![element(&["value"], ValueKind::Text)],
            vec![],
            Context::Direct,
        ),
        vec![vec![None]],
    );
    assert_case(
        "null attribute omitted",
        plan(
            RowName::Named("row".into()),
            vec![attribute("code", ValueKind::Text)],
            vec![],
            Context::Direct,
        ),
        vec![vec![None]],
    );
    assert_case(
        "null element xinil",
        plan(
            RowName::Named("row".into()),
            vec![element(&["value"], ValueKind::Text)],
            vec![Directive::ElementsXsiNil],
            Context::Direct,
        ),
        vec![vec![None]],
    );
    assert_case(
        "zero source rows",
        plan(
            RowName::Named("row".into()),
            vec![element(&["value"], ValueKind::Integer)],
            vec![],
            Context::Direct,
        ),
        vec![],
    );
    assert_case(
        "root wrapper",
        plan(
            RowName::Named("row".into()),
            vec![element(&["value"], ValueKind::Integer)],
            vec![Directive::Root("root".into())],
            Context::Direct,
        ),
        vec![vec![integer(1)]],
    );
    assert_case(
        "xml type",
        plan(
            RowName::Named("row".into()),
            vec![element(&["value"], ValueKind::Integer)],
            vec![Directive::Type],
            Context::Direct,
        ),
        vec![vec![integer(1)]],
    );
    assert_case(
        "xml type with root",
        plan(
            RowName::Named("row".into()),
            vec![element(&["value"], ValueKind::Integer)],
            vec![Directive::Type, Directive::Root("root".into())],
            Context::Direct,
        ),
        vec![vec![integer(1)]],
    );
    assert_case(
        "typed nested xml",
        plan(
            RowName::Named("item".into()),
            vec![element(&["value"], ValueKind::Integer)],
            vec![Directive::Type],
            Context::Nested("payload".into()),
        ),
        vec![vec![integer(1)]],
    );
}

#[test]
fn binary_base64_matches_captured_bytes() {
    assert_case(
        "binary base64",
        plan(
            RowName::Named("row".into()),
            vec![element(&["data"], ValueKind::Binary)],
            vec![Directive::BinaryBase64],
            Context::Direct,
        ),
        vec![vec![Some(Value::Binary(vec![0, 255, 60]))]],
    );
}

#[test]
fn binary_tail_sizes_and_utf16_supplementary_units_are_not_truncated() {
    for (bytes, expected) in [
        (vec![0], "AA=="),
        (vec![0, 255], "AP8="),
        (vec![0, 255, 60], "AP88"),
        (vec![0, 255, 60, 125], "AP88fQ=="),
    ] {
        let output = serialize(
            plan(
                RowName::Named("row".into()),
                vec![element(&["data"], ValueKind::Binary)],
                vec![Directive::BinaryBase64],
                Context::Direct,
            ),
            &[vec![Some(Value::Binary(bytes))]],
        )
        .unwrap();
        assert_eq!(
            String::from_utf16(&output.rows[0]).unwrap(),
            format!("<row><data>{expected}</data></row>")
        );
    }
    let output = serialize(
        plan(
            RowName::Named("row".into()),
            vec![element(&["value"], ValueKind::Text)],
            vec![],
            Context::Direct,
        ),
        &[vec![text("🦆 &")]],
    )
    .unwrap();
    assert_eq!(
        output.rows[0],
        "<row><value>🦆 &amp;</value></row>"
            .encode_utf16()
            .collect::<Vec<_>>()
    );
}

#[test]
fn captured_invalid_plans_fail_before_emitting_xml() {
    let invalid = [
        (
            "attribute after element",
            plan(
                RowName::Named("row".into()),
                vec![
                    attribute("id", ValueKind::Integer),
                    element(&["value"], ValueKind::Integer),
                    attribute("late", ValueKind::Integer),
                ],
                vec![],
                Context::Direct,
            ),
            Error::AttributeAfterElement("late".into()),
        ),
        (
            "duplicate root directive",
            plan(
                RowName::Named("row".into()),
                vec![element(&["value"], ValueKind::Integer)],
                vec![Directive::Root("a".into()), Directive::Root("b".into())],
                Context::Direct,
            ),
            Error::DuplicateRoot,
        ),
        (
            "duplicate type directive",
            plan(
                RowName::Named("row".into()),
                vec![element(&["value"], ValueKind::Integer)],
                vec![Directive::Type, Directive::Type],
                Context::Direct,
            ),
            Error::DuplicateType,
        ),
    ];
    for (name, plan, expected) in invalid {
        let error = match serialize::<Json>(plan, &[]) {
            Err(error) => error,
            Ok(_) => panic!("{name}: expected an error"),
        };
        assert_eq!(error, expected, "{name}");
        let diagnostic = error.sql_diagnostic().unwrap();
        let captured = &observed(name)["errors"][0];
        assert_eq!(diagnostic.0, captured["number"], "{name}");
        assert_eq!(diagnostic.1, captured["state"], "{name}");
        assert_eq!(diagnostic.2, captured["class"], "{name}");
        assert_eq!(diagnostic.3, captured["message"], "{name}");
    }
}

#[test]
fn unsupported_values_fail_explicitly_without_lossy_unicode_replacement() {
    let text_plan = || {
        plan(
            RowName::Named("row".into()),
            vec![element(&["value"], ValueKind::Text)],
            vec![],
            Context::Direct,
        )
    };
    assert!(matches!(
        serialize(text_plan(), &[vec![Some(Value::Text(vec![0xD800]))]]),
        Err(Error::InvalidUtf16 { row: 0, column: 0 })
    ));
    assert!(matches!(
        serialize(text_plan(), &[vec![Some(Value::Text(vec![0]))]]),
        Err(Error::UnsupportedXmlCharacter { row: 0, column: 0 })
    ));
    assert!(matches!(
        serialize(text_plan(), &[vec![]]),
        Err(Error::RowArity {
            row: 0,
            expected: 1,
            actual: 0
        })
    ));
    assert!(matches!(
        serialize(text_plan(), &[vec![integer(1)]]),
        Err(Error::ValueKind { row: 0, column: 0 })
    ));
    assert!(matches!(
        serialize(
            plan(
                RowName::Named("row".into()),
                vec![element(&["data"], ValueKind::Binary)],
                vec![],
                Context::Direct,
            ),
            &[vec![Some(Value::Binary(vec![0]))]],
        ),
        Err(Error::BinaryBase64Required { row: 0, column: 0 })
    ));
}
