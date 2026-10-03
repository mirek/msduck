#[allow(dead_code)]
#[path = "../src/concat_ws.rs"]
mod rules;
use msduck_core::{
    character::{CharacterType, Family, Length},
    collation::Label,
    types::Type,
};
use rules::{Argument, Collation, Encoding, Error, Function};
use serde_json::{Value, json};
const DEFAULT: &str = "SQL_Latin1_General_CP1_CI_AS";
const SC: &str = "Latin1_General_100_CI_AS_SC";
fn catalog() -> Vec<Collation> {
    [
        (DEFAULT, false, false),
        (SC, true, false),
        ("Latin1_General_100_BIN2", false, true),
        ("Latin1_General_100_CS_AS", false, true),
    ]
    .into_iter()
    .map(|(name, supplementary, case_sensitive)| Collation {
        name: name.into(),
        supplementary,
        case_sensitive,
        encoding: Encoding::Cp1252,
    })
    .collect()
}
fn arg(family: Family, length: Length) -> Argument {
    Argument::character(
        CharacterType::new(family, length).unwrap(),
        Label::CoercibleDefault(DEFAULT.into()),
    )
}
fn text(value: &str) -> Option<Vec<u16>> {
    Some(value.encode_utf16().collect())
}
fn literal(value: Option<&str>, unicode: bool) -> (Argument, Option<Vec<u16>>) {
    match value {
        None => (Argument::null_literal(), None),
        Some(s) => (
            arg(
                if unicode {
                    Family::Nvarchar
                } else {
                    Family::Varchar
                },
                Length::Bounded(s.encode_utf16().count().max(1) as u16),
            ),
            text(s),
        ),
    }
}
fn fixture() -> Value {
    // serde_json strings cannot carry an isolated UTF-16 surrogate. Preserve
    // the four exact captured values in a typed unit carrier, not U+FFFD or
    // a dropped record. The raw fixture itself remains unchanged.
    let source = include_str!("../../../reference/concat-ws-translate.json");
    let raw = r#""\ud83d-a""#;
    assert_eq!(source.matches(raw).count(), 4);
    serde_json::from_str(&source.replace(raw, r#"{"utf16":[55357,45,97]}"#)).unwrap()
}

fn observed<'a>(run: &'a Value, name: &str) -> &'a Value {
    &run.as_array()
        .unwrap()
        .iter()
        .find(|r| r["name"] == name)
        .unwrap()["result"]
}
fn default_match(a: &[u16], b: &[u16]) -> Option<bool> {
    if a == b {
        return Some(true);
    }
    if (a == [0xd83d, 0xde00] && b.len() == 1 && b[0] < 128)
        || (b == [0xd83d, 0xde00] && a.len() == 1 && a[0] < 128)
    {
        return Some(false);
    }

    // Explicit comparison observations used by these cases; not a general
    // implementation of linguistic weights. Other pairs remain unknown.
    if a.len() == 1 && b.len() == 1 {
        let (x, y) = (a[0], b[0]);
        if x < 128 && y < 128 {
            return Some((x as u8).eq_ignore_ascii_case(&(y as u8)));
        }
        if [0xd83d, 0xde00].contains(&x) && [0xd83d, 0xde00].contains(&y) {
            return Some(true);
        }
        if [0xe9, 0xd83d, 0xde00].contains(&x) && y < 128
            || [0xe9, 0xd83d, 0xde00].contains(&y) && x < 128
        {
            return Some(false);
        }
    }
    None
}
fn captured_default_key(unit: &[u16]) -> Option<Vec<u16>> {
    // Only equivalence classes established for the retained default-collation
    // probes. These are not general SQL Server linguistic weights.
    match unit {
        [x] if *x < 128 => Some(vec![(*x as u8).to_ascii_lowercase().into()]),
        [0xe9] => Some(vec![0xe9]),
        [0xd83d] | [0xde00] => Some(vec![0xd800]),
        [0xd83d, 0xde00] => Some(vec![0xd83d, 0xde00]),
        _ => None,
    }
}

#[test]
fn ordinary_character_values_and_metadata_match_all_four_captures() {
    let cases: &[(&str, Function, bool, &[Option<&str>])] = &[
        (
            "cws ansi basic",
            Function::ConcatWs,
            false,
            &[Some(","), Some("a"), Some("b"), Some("c")],
        ),
        (
            "cws unicode basic",
            Function::ConcatWs,
            true,
            &[Some(","), Some("a"), Some("b"), Some("c")],
        ),
        (
            "cws null argument skipped",
            Function::ConcatWs,
            false,
            &[Some(","), Some("a"), None, Some("b")],
        ),
        (
            "cws all null arguments",
            Function::ConcatWs,
            false,
            &[Some(","), None, None],
        ),
        (
            "cws null literal separator",
            Function::ConcatWs,
            false,
            &[None, Some("a"), Some("b")],
        ),
        (
            "cws all null with null separator",
            Function::ConcatWs,
            false,
            &[None, None, None],
        ),
        (
            "cws empty separator",
            Function::ConcatWs,
            false,
            &[Some(""), Some("a"), Some("b")],
        ),
        (
            "cws empty string arguments kept",
            Function::ConcatWs,
            false,
            &[Some(","), Some("a"), Some(""), Some("b"), Some("")],
        ),
        (
            "cws multi character separator",
            Function::ConcatWs,
            false,
            &[Some(" - "), Some("a "), Some(" b")],
        ),
        (
            "tr ansi basic",
            Function::Translate,
            false,
            &[Some("2*[3+4]/{7-2}"), Some("[]{}"), Some("()()")],
        ),
        (
            "tr unicode basic",
            Function::Translate,
            true,
            &[Some("2*[3+4]/{7-2}"), Some("[]{}"), Some("()()")],
        ),
        (
            "tr null literal input",
            Function::Translate,
            false,
            &[None, Some("a"), Some("b")],
        ),
        (
            "tr null characters",
            Function::Translate,
            false,
            &[Some("abc"), None, Some("b")],
        ),
        (
            "tr null translations",
            Function::Translate,
            false,
            &[Some("abc"), Some("a"), None],
        ),
        (
            "tr empty characters",
            Function::Translate,
            false,
            &[Some("abc"), Some(""), Some("")],
        ),
        (
            "tr empty input",
            Function::Translate,
            false,
            &[Some(""), Some("a"), Some("b")],
        ),
        (
            "tr duplicate characters",
            Function::Translate,
            false,
            &[Some("aab"), Some("aa"), Some("xy")],
        ),
        (
            "tr no chaining",
            Function::Translate,
            false,
            &[Some("abc"), Some("ab"), Some("bc")],
        ),
        (
            "tr case insensitive default",
            Function::Translate,
            false,
            &[Some("ABCabc"), Some("abc"), Some("xyz")],
        ),
        (
            "tr accent insensitive probe",
            Function::Translate,
            true,
            &[Some("eéE"), Some("e"), Some("x")],
        ),
        (
            "tr trailing space translation",
            Function::Translate,
            false,
            &[Some("a b"), Some("a "), Some("x_")],
        ),
    ];
    let f = fixture();
    let mut comparisons = 0;
    for container in f["containers"].as_array().unwrap() {
        for run in container["runs"].as_array().unwrap() {
            for (name, function, unicode, values) in cases {
                let (args, values): (Vec<_>, Vec<_>) =
                    values.iter().map(|v| literal(*v, *unicode)).unzip();
                let args_before = args.clone();
                let before = values.clone();
                let plan = rules::plan(*function, &args, DEFAULT, &catalog()).unwrap();
                let result = rules::evaluate(&plan, &values, &default_match).unwrap();
                assert_eq!(
                    rules::evaluate_with_keys(&plan, &values, &captured_default_key).unwrap(),
                    result,
                    "indexed {name}"
                );
                let expected = observed(run, name);
                let actual = result.map(|u| String::from_utf16(&u).unwrap());
                assert_eq!(json!([[actual]]), expected["sets"][0]["rows"], "{name}");
                let column = &expected["sets"][0]["columns"][0];
                assert_eq!(
                    plan.flags,
                    column["flags"].as_u64().unwrap() as u16,
                    "{name}"
                );
                assert_eq!(
                    if *unicode { "NVarChar" } else { "VarChar" },
                    column["type"].as_str().unwrap(),
                    "{name}"
                );
                let width = match plan.declaration.length() {
                    Length::Max => 65535,
                    Length::Bounded(n) => u64::from(n) * if *unicode { 2 } else { 1 },
                };
                assert_eq!(width, column["length"].as_u64().unwrap(), "{name}");
                assert_eq!(args, args_before);
                assert_eq!(values, before);
                comparisons += 1;
            }
        }
    }
    assert_eq!(comparisons, 84);
}
#[test]
fn declaration_widths_caps_and_prepared_values_are_independent() {
    let args = vec![
        arg(Family::Nvarchar, Length::Bounded(5)),
        arg(Family::Nvarchar, Length::Bounded(10)),
        arg(Family::Nvarchar, Length::Bounded(20)),
    ];
    let plan = rules::plan(Function::ConcatWs, &args, DEFAULT, &catalog()).unwrap();
    assert_eq!(plan.declaration.length(), Length::Bounded(35));
    let f = fixture();
    for container in f["containers"].as_array().unwrap() {
        for run in container["runs"].as_array().unwrap() {
            let prepared = &run
                .as_array()
                .unwrap()
                .iter()
                .find(|r| r["name"] == "prepared cws")
                .unwrap()["prepared"];
            assert_eq!(prepared["prepare"]["sets"][0]["columns"][0]["length"], 70);
            for execution in prepared["executions"].as_array().unwrap() {
                let values: Vec<_> = ["sep", "a", "b"]
                    .iter()
                    .map(|name| {
                        execution["values"][name]
                            .as_str()
                            .map(|s| s.encode_utf16().collect())
                    })
                    .collect();
                let actual = rules::evaluate(&plan, &values, &default_match)
                    .unwrap()
                    .map(|v| String::from_utf16(&v).unwrap());
                assert_eq!(json!([[actual]]), execution["result"]["sets"][0]["rows"]);
            }
        }
    }
}
#[test]
fn errors_unknowns_and_surrogate_rules_are_explicit() {
    assert!(
        matches!(rules::plan(Function::ConcatWs,&[],DEFAULT,&catalog()),Err(Error::Sql(e))if e.number==189&&e.severity==15)
    );
    assert!(
        matches!(rules::plan(Function::Translate,&[],DEFAULT,&catalog()),Err(Error::Sql(e))if e.number==174&&e.severity==15)
    );
    let unknown = Argument {
        kind: Some(Type::Float),
        converted_width: None,
        collation: None,
    };
    assert_eq!(
        rules::plan(
            Function::ConcatWs,
            &[
                arg(Family::Varchar, Length::Bounded(1)),
                unknown,
                Argument::null_literal()
            ],
            DEFAULT,
            &catalog()
        ),
        Err(Error::UnknownConversionWidth)
    );
    for sc in [false, true] {
        let mut args = vec![
            arg(Family::Nvarchar, Length::Bounded(4)),
            arg(Family::Nvarchar, Length::Bounded(2)),
            arg(Family::Nvarchar, Length::Bounded(1)),
        ];
        if sc {
            args[0].collation = Some(Label::Explicit(SC.into()));
        }
        let p = rules::plan(Function::Translate, &args, DEFAULT, &catalog()).unwrap();
        let values = vec![text("a😀b"), text("😀"), text("x")];
        let result = rules::evaluate(&p, &values, &default_match);
        if sc {
            assert_eq!(result.unwrap(), text("axb"))
        } else {
            assert!(matches!(result,Err(Error::Sql(e))if e.number==9828&&e.state==3));
        }
    }
}

#[test]
fn isolated_surrogate_evidence_is_preserved_as_exact_units() {
    let arguments = vec![
        arg(Family::Nvarchar, Length::Bounded(1)),
        arg(Family::Nchar, Length::Bounded(1)),
        arg(Family::Nvarchar, Length::Bounded(1)),
    ];
    let p = rules::plan(Function::ConcatWs, &arguments, DEFAULT, &catalog()).unwrap();
    let result = rules::evaluate(
        &p,
        &[text("-"), Some(vec![0xd83d]), text("a")],
        &default_match,
    )
    .unwrap()
    .unwrap();
    let f = fixture();
    for container in f["containers"].as_array().unwrap() {
        for run in container["runs"].as_array().unwrap() {
            let captured = observed(run, "cws lone surrogate");
            assert_eq!(json!(result), captured["sets"][0]["rows"][0][0]["utf16"]);
            assert_eq!(json!(result.len() * 2), captured["sets"][0]["rows"][0][1]);
        }
    }
}

#[test]
fn captured_bounded_truncation_and_max_first_rules() {
    let f = fixture();
    for (name, family, width) in [
        ("cws varchar long value", Family::Varchar, 5000),
        ("cws nvarchar long value", Family::Nvarchar, 3000),
    ] {
        let arguments = vec![
            arg(family, Length::Bounded(1)),
            arg(family, Length::Bounded(width)),
            arg(family, Length::Bounded(width)),
        ];
        let p = rules::plan(Function::ConcatWs, &arguments, DEFAULT, &catalog()).unwrap();
        let values = vec![
            text("-"),
            text(&"x".repeat(width as usize)),
            text(&"y".repeat(width as usize)),
        ];
        let output = rules::evaluate(&p, &values, &default_match)
            .unwrap()
            .unwrap();
        for c in f["containers"].as_array().unwrap() {
            for run in c["runs"].as_array().unwrap() {
                let row = &observed(run, name)["sets"][0]["rows"][0];
                assert_eq!(json!(output.len()), row[0]);
                assert_eq!(
                    json!(output.len() * if family == Family::Nvarchar { 2 } else { 1 }),
                    row[1]
                );
            }
        }
    }
    for (name, family) in [
        ("tr long varchar max value", Family::Varchar),
        ("tr long nvarchar max value", Family::Nvarchar),
    ] {
        let arguments = vec![
            arg(family, Length::Max),
            arg(Family::Varchar, Length::Bounded(1)),
            arg(Family::Varchar, Length::Bounded(1)),
        ];
        let p = rules::plan(Function::Translate, &arguments, DEFAULT, &catalog()).unwrap();
        assert_eq!(p.declaration.length(), Length::Max);
        let result = rules::evaluate(
            &p,
            &[text(&"a".repeat(9000)), text("a"), text("b")],
            &default_match,
        )
        .unwrap()
        .unwrap();
        assert_eq!(result, vec![u16::from(b'b'); 9000]);
        for c in f["containers"].as_array().unwrap() {
            for run in c["runs"].as_array().unwrap() {
                assert_eq!(observed(run, name)["sets"][0]["columns"][0]["length"], 8);
                assert_eq!(observed(run, name)["sets"][0]["columns"][0]["type"], "IntN");
                assert_eq!(
                    json!(
                        (result.len() * if family == Family::Nvarchar { 2 } else { 1 }).to_string()
                    ),
                    observed(run, name)["sets"][0]["rows"][0][0]
                );
            }
        }
    }
    let bounded = rules::plan(
        Function::Translate,
        &[
            arg(Family::Varchar, Length::Bounded(10)),
            arg(Family::Varchar, Length::Max),
            arg(Family::Varchar, Length::Bounded(1)),
        ],
        DEFAULT,
        &catalog(),
    )
    .unwrap();
    assert_eq!(bounded.declaration.length(), Length::Bounded(8000));
}
#[test]
fn compile_diagnostics_retain_all_captured_identity_fields() {
    let f = fixture();
    let mut cases = vec![];
    for (name, function, count) in [
        ("cws one argument", Function::ConcatWs, 2),
        ("cws no arguments", Function::ConcatWs, 0),
        ("cws separator only null", Function::ConcatWs, 1),
        ("cws 255 arguments", Function::ConcatWs, 255),
        ("cws 256 arguments", Function::ConcatWs, 256),
        ("tr two arguments", Function::Translate, 2),
        ("tr four arguments", Function::Translate, 4),
    ] {
        cases.push((
            name,
            rules::plan(
                function,
                &vec![arg(Family::Varchar, Length::Bounded(1)); count],
                DEFAULT,
                &catalog(),
            ),
        ));
    }
    for (name, kind) in [
        ("cws sql_variant argument", Type::Variant),
        ("cws xml argument", Type::Xml),
    ] {
        let arguments = vec![
            arg(Family::Varchar, Length::Bounded(1)),
            Argument {
                kind: Some(kind),
                converted_width: None,
                collation: None,
            },
            arg(Family::Varchar, Length::Bounded(1)),
        ];
        cases.push((
            name,
            rules::plan(Function::ConcatWs, &arguments, DEFAULT, &catalog()),
        ));
    }
    for (name, function, explicit) in [
        (
            "cws conflicting explicit collations",
            Function::ConcatWs,
            true,
        ),
        ("cws implicit column collations", Function::ConcatWs, false),
        (
            "tr conflicting explicit collations",
            Function::Translate,
            true,
        ),
    ] {
        let mut arguments = vec![arg(Family::Varchar, Length::Bounded(1)); 3];
        let label = |name: &str| {
            if explicit {
                Label::Explicit(name.into())
            } else {
                Label::Implicit(name.into())
            }
        };
        arguments[1].collation = Some(label("Latin1_General_100_BIN2"));
        arguments[2].collation = Some(label("Latin1_General_100_CS_AS"));
        cases.push((
            name,
            rules::plan_with_context(
                function,
                &arguments,
                DEFAULT,
                &catalog(),
                Some(rules::DiagnosticContext::SelectColumn(
                    std::num::NonZeroUsize::new(1).unwrap(),
                )),
            ),
        ));
    }
    cases.push((
        "tr integer characters",
        rules::plan(
            Function::Translate,
            &[
                literal(Some("a1b"), false).0,
                literal(Some("1"), false).0,
                Argument {
                    kind: Some(Type::Int),
                    converted_width: None,
                    collation: None,
                },
            ],
            DEFAULT,
            &catalog(),
        ),
    ));
    for (name, result) in cases {
        let Err(Error::Sql(actual)) = result else {
            panic!("missing SQL error: {name}")
        };
        for c in f["containers"].as_array().unwrap() {
            for run in c["runs"].as_array().unwrap() {
                let expected = &observed(run, name)["errors"][0];
                assert_eq!(json!(actual.number), expected["number"], "{name}");
                assert_eq!(json!(actual.state), expected["state"], "{name}");
                assert_eq!(json!(actual.severity), expected["class"], "{name}");
                assert_eq!(json!(actual.message), expected["message"], "{name}");
            }
        }
    }
}

#[test]
fn converted_numeric_payloads_cannot_escape_width_or_encoding_contracts() {
    let integer = Argument {
        kind: Some(Type::Int),
        converted_width: None,
        collation: None,
    };
    let arguments = vec![
        arg(Family::Varchar, Length::Bounded(1)),
        integer,
        arg(Family::Varchar, Length::Bounded(1)),
    ];
    let p = rules::plan(Function::ConcatWs, &arguments, DEFAULT, &catalog()).unwrap();
    for invalid in [vec![u16::from(b'1'); 13], vec![0xd83d, 0xde00]] {
        assert_eq!(
            rules::evaluate(&p, &[text(","), Some(invalid), text("a")], &default_match),
            Err(Error::InvalidPayload)
        );
    }
    assert_eq!(
        rules::evaluate(&p, &[text(","), text("42"), text("a")], &default_match).unwrap(),
        text("42,a")
    );
    let max_arguments = vec![
        arg(Family::Varchar, Length::Bounded(1)),
        arg(Family::Varchar, Length::Max),
        arg(Family::Varchar, Length::Bounded(1)),
    ];
    let max_plan = rules::plan(Function::ConcatWs, &max_arguments, DEFAULT, &catalog()).unwrap();
    assert_eq!(
        rules::evaluate(
            &max_plan,
            &[text(","), text("😀"), text("a")],
            &default_match
        ),
        Err(Error::InvalidPayload)
    );
}

#[test]
fn declared_character_families_padding_max_and_collations_match_captures() {
    use Family::{Char, Nchar, Nvarchar, Varchar};
    let bounded = |family, width| arg(family, Length::Bounded(width));
    let max = |family| arg(family, Length::Max);
    let lit = |s: &str| literal(Some(s), false).0;
    let nlit = |s: &str| literal(Some(s), true).0;
    let explicit = |s: &str, name: &str| {
        let mut argument = lit(s);
        argument.collation = Some(Label::Explicit(name.into()));
        argument
    };
    let int = Argument {
        kind: Some(Type::Int),
        converted_width: None,
        collation: None,
    };
    let cases = vec![
        (
            "cws typed null arguments skipped",
            Function::ConcatWs,
            vec![
                lit(","),
                bounded(Varchar, 5),
                lit("a"),
                bounded(Nvarchar, 5),
                lit("b"),
                int,
            ],
            vec![text(","), None, text("a"), None, text("b"), None],
        ),
        (
            "cws all typed null arguments",
            Function::ConcatWs,
            vec![lit(","), bounded(Varchar, 5), bounded(Varchar, 5)],
            vec![text(","), None, None],
        ),
        (
            "cws typed null separator",
            Function::ConcatWs,
            vec![bounded(Varchar, 3), lit("a"), lit("b")],
            vec![None, text("a"), text("b")],
        ),
        (
            "cws char padding",
            Function::ConcatWs,
            vec![lit("|"), bounded(Char, 3), bounded(Nchar, 2)],
            vec![text("|"), text("a  "), text("b ")],
        ),
        (
            "cws varchar widths",
            Function::ConcatWs,
            vec![lit("-"), bounded(Varchar, 10), bounded(Varchar, 20)],
            vec![text("-"), text("a"), text("b")],
        ),
        (
            "cws varchar widths with long separator",
            Function::ConcatWs,
            vec![
                bounded(Varchar, 7),
                bounded(Varchar, 10),
                bounded(Varchar, 20),
                bounded(Varchar, 30),
            ],
            vec![text("--"), text("a"), text("b"), text("c")],
        ),
        (
            "cws nvarchar widths",
            Function::ConcatWs,
            vec![nlit("-"), bounded(Nvarchar, 10), bounded(Nvarchar, 20)],
            vec![text("-"), text("a"), text("b")],
        ),
        (
            "cws mixed varchar nvarchar",
            Function::ConcatWs,
            vec![lit("-"), bounded(Varchar, 10), bounded(Nvarchar, 20)],
            vec![text("-"), text("a"), text("b")],
        ),
        (
            "cws unicode separator ansi arguments",
            Function::ConcatWs,
            vec![nlit("-"), bounded(Varchar, 10), bounded(Varchar, 20)],
            vec![text("-"), text("a"), text("b")],
        ),
        (
            "cws varchar width over 8000",
            Function::ConcatWs,
            vec![lit("-"), bounded(Varchar, 5000), bounded(Varchar, 5000)],
            vec![text("-"), text("a"), text("b")],
        ),
        (
            "cws nvarchar width over 4000",
            Function::ConcatWs,
            vec![nlit("-"), bounded(Nvarchar, 3000), bounded(Nvarchar, 3000)],
            vec![text("-"), text("a"), text("b")],
        ),
        (
            "cws varchar max argument",
            Function::ConcatWs,
            vec![lit("-"), max(Varchar), lit("b")],
            vec![text("-"), text("a"), text("b")],
        ),
        (
            "cws nvarchar max argument",
            Function::ConcatWs,
            vec![lit("-"), max(Nvarchar), lit("b")],
            vec![text("-"), text("a"), text("b")],
        ),
        (
            "cws varchar max separator",
            Function::ConcatWs,
            vec![max(Varchar), lit("a"), lit("b")],
            vec![text("-"), text("a"), text("b")],
        ),
        (
            "cws explicit collation",
            Function::ConcatWs,
            vec![lit(","), explicit("a", "Latin1_General_100_BIN2"), lit("b")],
            vec![text(","), text("a"), text("b")],
        ),
        (
            "tr null input",
            Function::Translate,
            vec![bounded(Varchar, 10), lit("a"), lit("b")],
            vec![None, text("a"), text("b")],
        ),
        (
            "tr varchar width",
            Function::Translate,
            vec![
                bounded(Varchar, 10),
                bounded(Varchar, 30),
                bounded(Varchar, 40),
            ],
            vec![text("abc"), text("a"), text("x")],
        ),
        (
            "tr nvarchar width",
            Function::Translate,
            vec![bounded(Nvarchar, 10), nlit("a"), nlit("x")],
            vec![text("abc"), text("a"), text("x")],
        ),
        (
            "tr varchar input unicode characters",
            Function::Translate,
            vec![bounded(Varchar, 10), nlit("a"), nlit("x")],
            vec![text("abc"), text("a"), text("x")],
        ),
        (
            "tr nvarchar input ansi characters",
            Function::Translate,
            vec![bounded(Nvarchar, 10), lit("a"), lit("x")],
            vec![text("abc"), text("a"), text("x")],
        ),
        (
            "tr char input",
            Function::Translate,
            vec![bounded(Char, 5), lit("b "), lit("x_")],
            vec![text("ab   "), text("b "), text("x_")],
        ),
        (
            "tr varchar max input",
            Function::Translate,
            vec![max(Varchar), lit("a"), lit("x")],
            vec![text("abc"), text("a"), text("x")],
        ),
        (
            "tr nvarchar max input",
            Function::Translate,
            vec![max(Nvarchar), nlit("a"), nlit("x")],
            vec![text("abc"), text("a"), text("x")],
        ),
        (
            "tr varchar max characters",
            Function::Translate,
            vec![bounded(Varchar, 10), max(Varchar), lit("x")],
            vec![text("abc"), text("a"), text("x")],
        ),
        (
            "tr binary collation",
            Function::Translate,
            vec![
                explicit("ABCabc", "Latin1_General_100_BIN2"),
                lit("abc"),
                lit("xyz"),
            ],
            vec![text("ABCabc"), text("abc"), text("xyz")],
        ),
        (
            "tr case sensitive collation",
            Function::Translate,
            vec![
                explicit("ABCabc", "Latin1_General_100_CS_AS"),
                lit("abc"),
                lit("xyz"),
            ],
            vec![text("ABCabc"), text("abc"), text("xyz")],
        ),
    ];
    let f = fixture();
    let mut comparisons = 0;
    for container in f["containers"].as_array().unwrap() {
        for run in container["runs"].as_array().unwrap() {
            for (name, function, arguments, values) in &cases {
                let p = rules::plan(*function, arguments, DEFAULT, &catalog()).unwrap();
                let before = values.clone();
                let result = rules::evaluate(&p, values, &|a, b| {
                    if p.flags & 2 != 0 {
                        Some(a == b)
                    } else {
                        default_match(a, b)
                    }
                })
                .unwrap()
                .map(|v| String::from_utf16(&v).unwrap());
                let expected = observed(run, name);
                assert!(expected["errors"].as_array().unwrap().is_empty(), "{name}");
                assert_eq!(json!([[result]]), expected["sets"][0]["rows"], "{name}");
                let column = &expected["sets"][0]["columns"][0];
                let unicode = p.declaration.family() == Nvarchar;
                assert_eq!(
                    column["type"],
                    if unicode { "NVarChar" } else { "VarChar" },
                    "{name}"
                );
                assert_eq!(column["flags"], p.flags, "{name}");
                let bytes = match p.declaration.length() {
                    Length::Max => 65535,
                    Length::Bounded(n) => u64::from(n) * if unicode { 2 } else { 1 },
                };
                assert_eq!(column["length"], bytes, "{name}");
                let binary = p.collation.name() == Some("Latin1_General_100_BIN2");
                let sensitive = p.collation.name() == Some("Latin1_General_100_CS_AS");
                assert_eq!(
                    column["collation"]["flags"],
                    if binary {
                        32
                    } else if sensitive {
                        12
                    } else {
                        13
                    },
                    "{name}"
                );
                assert_eq!(
                    column["collation"]["version"],
                    if binary || sensitive { 2 } else { 0 },
                    "{name}"
                );
                assert_eq!(
                    column["collation"]["sortId"],
                    if binary || sensitive { 0 } else { 52 },
                    "{name}"
                );
                assert_eq!(column["collation"]["lcid"], 1033, "{name}");
                assert_eq!(column["collation"]["codepage"], "CP1252", "{name}");
                assert_eq!(column["precision"], Value::Null, "{name}");
                assert_eq!(column["scale"], Value::Null, "{name}");
                assert_eq!(values, &before);
                comparisons += 1;
            }
        }
    }
    assert_eq!(comparisons, 104);
}

#[test]
fn prepared_translate_rebindings_preserve_metadata_and_exact_runtime_errors() {
    let f = fixture();
    let mut executions = 0;
    for (name, family) in [
        ("prepared tr", Family::Nvarchar),
        ("prepared tr ansi", Family::Varchar),
    ] {
        // Declarations are fixed before any execution values are inspected.
        let arguments = vec![arg(family, Length::Bounded(10)); 3];
        let p = rules::plan(Function::Translate, &arguments, DEFAULT, &catalog()).unwrap();
        for c in f["containers"].as_array().unwrap() {
            for run in c["runs"].as_array().unwrap() {
                let prepared = &run
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|r| r["name"] == name)
                    .unwrap()["prepared"];
                let declaration = &prepared["prepare"]["sets"][0]["columns"][0];
                assert_eq!(declaration["length"], 8000);
                assert_eq!(declaration["flags"], p.flags);
                assert_eq!(
                    declaration["type"],
                    if family == Family::Nvarchar {
                        "NVarChar"
                    } else {
                        "VarChar"
                    }
                );
                assert!(
                    prepared["prepare"]["sets"][0]["rows"]
                        .as_array()
                        .unwrap()
                        .is_empty()
                );
                for execution in prepared["executions"].as_array().unwrap() {
                    let values: Vec<_> = ["input", "from", "to"]
                        .iter()
                        .map(|name| execution["values"][name].as_str().and_then(text))
                        .collect();
                    let before = values.clone();
                    let result = rules::evaluate(&p, &values, &default_match);
                    let captured = &execution["result"];
                    assert_eq!(&captured["sets"][0]["columns"][0], declaration);
                    if captured["errors"].as_array().unwrap().is_empty() {
                        let value = result.unwrap().map(|v| String::from_utf16(&v).unwrap());
                        assert_eq!(json!([[value]]), captured["sets"][0]["rows"]);
                    } else {
                        let Err(Error::Sql(actual)) = result else {
                            panic!("missing runtime diagnostic: {name}")
                        };
                        assert_eq!(captured["errors"].as_array().unwrap().len(), 1);
                        let error = &captured["errors"][0];
                        assert_eq!(json!(actual.number), error["number"]);
                        assert_eq!(json!(actual.state), error["state"]);
                        assert_eq!(json!(actual.severity), error["class"]);
                        assert_eq!(json!(actual.message), error["message"]);
                        assert!(captured["sets"][0]["rows"].as_array().unwrap().is_empty());
                    }
                    assert_eq!(values, before);
                    executions += 1;
                }
            }
        }
    }
    assert_eq!(executions, 32);
}

#[test]
fn largest_valid_concat_arity_matches_captured_empty_separator_width() {
    let mut arguments = vec![literal(Some(""), false).0];
    arguments.extend(vec![literal(Some("x"), false).0; 253]);
    let mut values = vec![text("")];
    values.extend(vec![text("x"); 253]);
    let p = rules::plan(Function::ConcatWs, &arguments, DEFAULT, &catalog()).unwrap();
    let output = rules::evaluate(&p, &values, &default_match)
        .unwrap()
        .unwrap();
    assert_eq!(p.declaration.length(), Length::Bounded(505));
    let f = fixture();
    for c in f["containers"].as_array().unwrap() {
        for run in c["runs"].as_array().unwrap() {
            let observed = observed(run, "cws 254 arguments");
            assert_eq!(observed["sets"][0]["columns"][0]["length"], 505);
            assert_eq!(
                json!([[String::from_utf16(&output).unwrap()]]),
                observed["sets"][0]["rows"]
            );
        }
    }
}

#[test]
fn implicit_collation_diagnostics_require_explicit_statement_context() {
    let mut arguments = vec![arg(Family::Varchar, Length::Bounded(1)); 3];
    arguments[1].collation = Some(Label::Implicit("Latin1_General_100_BIN2".into()));
    arguments[2].collation = Some(Label::Implicit("Latin1_General_100_CS_AS".into()));
    assert_eq!(
        rules::plan(Function::ConcatWs, &arguments, DEFAULT, &catalog()),
        Err(Error::UnknownDiagnosticContext)
    );
    // Context rendering is deterministic. SQL Server evidence for column 1 is
    // checked in the full captured-diagnostic test; these positions additionally
    // verify that the caller's explicit position survives without defaulting.
    for column in [1, 2, 17] {
        let result = rules::plan_with_context(
            Function::ConcatWs,
            &arguments,
            DEFAULT,
            &catalog(),
            Some(rules::DiagnosticContext::SelectColumn(
                std::num::NonZeroUsize::new(column).unwrap(),
            )),
        );
        let Err(Error::Sql(error)) = result else {
            panic!("missing contextual error")
        };
        assert_eq!(error.number, 451);
        assert_eq!(error.state, 1);
        assert_eq!(error.severity, 16);
        assert_eq!(
            error.message,
            format!(
                "Cannot resolve collation conflict between \"Latin1_General_100_CS_AS\" and \"Latin1_General_100_BIN2\" in concat_ws operator occurring in SELECT statement column {column}."
            )
        );
    }
    assert!(std::num::NonZeroUsize::new(0).is_none());
}

#[test]
fn translate_supplementary_utf8_and_mismatch_cases_retain_captured_contracts() {
    let cases = [
        (
            "tr length mismatch longer",
            false,
            false,
            ["abc", "ab", "x"],
        ),
        (
            "tr length mismatch shorter",
            false,
            false,
            ["abc", "a", "xy"],
        ),
        (
            "tr trailing space mismatch",
            false,
            false,
            ["a b", "a ", "x"],
        ),
        (
            "tr supplementary default collation",
            true,
            false,
            ["a😀b", "😀", "xy"],
        ),
        (
            "tr supplementary default collation mismatch",
            true,
            false,
            ["a😀b", "😀", "x"],
        ),
        (
            "tr supplementary sc collation",
            true,
            true,
            ["a😀b", "😀", "x"],
        ),
        (
            "tr supplementary sc collation pair",
            true,
            true,
            ["a😀b", "😀", "xy"],
        ),
        (
            "tr supplementary replacement",
            true,
            true,
            ["abc", "b", "😀"],
        ),
    ];
    let mut declared_cases: Vec<_> = cases
        .into_iter()
        .map(|(name, unicode, sc, strings)| {
            let (mut arguments, values): (Vec<_>, Vec<_>) = strings
                .into_iter()
                .map(|s| literal(Some(s), unicode))
                .unzip();
            if sc {
                arguments[0].collation = Some(Label::Explicit(SC.into()));
            }
            (name, arguments, values)
        })
        .collect();
    declared_cases.push((
        "tr lone surrogate",
        vec![
            arg(Family::Nvarchar, Length::Bounded(3)),
            arg(Family::Nchar, Length::Bounded(1)),
            literal(Some("x"), true).0,
        ],
        vec![Some(vec![97, 0xd83d, 98]), Some(vec![0xd83d]), text("x")],
    ));
    let utf8 = "Latin1_General_100_CI_AS_SC_UTF8";
    let mut input = arg(Family::Varchar, Length::Bounded(10));
    input.collation = Some(Label::Explicit(utf8.into()));
    declared_cases.push((
        "tr utf8 collation",
        vec![
            input,
            literal(Some("é"), true).0,
            literal(Some("e"), true).0,
        ],
        vec![text("aéb"), text("é"), text("e")],
    ));
    let mut catalog = catalog();
    catalog.push(Collation {
        name: utf8.into(),
        supplementary: true,
        case_sensitive: false,
        encoding: Encoding::Utf8,
    });
    let f = fixture();
    let mut comparisons = 0;
    for c in f["containers"].as_array().unwrap() {
        for run in c["runs"].as_array().unwrap() {
            for (name, arguments, values) in &declared_cases {
                let p = rules::plan(Function::Translate, arguments, DEFAULT, &catalog).unwrap();
                let result = rules::evaluate(&p, values, &default_match);
                assert_eq!(
                    rules::evaluate_with_keys(&p, values, &captured_default_key),
                    result,
                    "indexed {name}"
                );
                let captured = observed(run, name);
                let column = &captured["sets"][0]["columns"][0];
                assert_eq!(column["flags"], p.flags, "{name}");
                assert_eq!(column["length"], 8000, "{name}");
                assert_eq!(
                    column["type"],
                    if p.declaration.family() == Family::Nvarchar {
                        "NVarChar"
                    } else {
                        "VarChar"
                    },
                    "{name}"
                );
                assert_eq!(
                    column["collation"]["version"],
                    if p.supplementary { 2 } else { 0 },
                    "{name}"
                );
                assert_eq!(
                    column["collation"]["flags"],
                    if p.encoding == Encoding::Utf8 { 77 } else { 13 },
                    "{name}"
                );
                assert_eq!(
                    column["collation"]["sortId"],
                    if p.supplementary { 0 } else { 52 },
                    "{name}"
                );
                assert_eq!(
                    column["collation"]["codepage"],
                    if p.encoding == Encoding::Utf8 {
                        "utf-8"
                    } else {
                        "CP1252"
                    },
                    "{name}"
                );
                if captured["errors"].as_array().unwrap().is_empty() {
                    let actual = result.unwrap().map(|v| String::from_utf16(&v).unwrap());
                    assert_eq!(json!([[actual]]), captured["sets"][0]["rows"], "{name}");
                } else {
                    let Err(Error::Sql(actual)) = result else {
                        panic!("missing error: {name}")
                    };
                    let expected = &captured["errors"][0];
                    assert_eq!(json!(actual.number), expected["number"], "{name}");
                    assert_eq!(json!(actual.state), expected["state"], "{name}");
                    assert_eq!(json!(actual.severity), expected["class"], "{name}");
                    assert_eq!(json!(actual.message), expected["message"], "{name}");
                    assert!(captured["sets"][0]["rows"].as_array().unwrap().is_empty());
                }
                comparisons += 1;
            }
        }
    }
    assert_eq!(comparisons, 40);
}

fn assert_character_descriptor(p: &rules::Plan, column: &Value, name: &str) {
    let unicode = p.declaration.family() == Family::Nvarchar;
    assert_eq!(
        column["type"],
        if unicode { "NVarChar" } else { "VarChar" },
        "{name}"
    );
    assert_eq!(column["flags"], p.flags, "{name}");
    let bytes = match p.declaration.length() {
        Length::Max => 65535,
        Length::Bounded(n) => u64::from(n) * if unicode { 2 } else { 1 },
    };
    assert_eq!(column["length"], bytes, "{name}");
    assert_eq!(column["precision"], Value::Null, "{name}");
    assert_eq!(column["scale"], Value::Null, "{name}");
    assert_eq!(column["collation"]["lcid"], 1033, "{name}");
    assert_eq!(column["collation"]["flags"], 13, "{name}");
    assert_eq!(column["collation"]["version"], 0, "{name}");
    assert_eq!(column["collation"]["sortId"], 52, "{name}");
    assert_eq!(column["collation"]["codepage"], "CP1252", "{name}");
}

fn assert_runtime_error(actual: &Error, expected: &Value) {
    let Error::Sql(actual) = actual else {
        panic!("missing SQL runtime error: {actual:?}")
    };
    assert_eq!(json!(actual.number), expected["number"]);
    assert_eq!(json!(actual.state), expected["state"]);
    assert_eq!(json!(actual.severity), expected["class"]);
    assert_eq!(json!(actual.message), expected["message"]);
}

#[test]
fn character_rpc_declarations_and_values_match_all_captured_requests() {
    let names = [
        "rpc cws unicode",
        "rpc cws ansi",
        "rpc cws null separator",
        "rpc cws null argument",
        "rpc cws inferred length argument",
        "rpc cws max argument",
        "rpc cws unicode replay",
        "rpc tr unicode",
        "rpc tr ansi",
        "rpc tr max input",
        "rpc tr null input",
        "rpc tr length mismatch",
        "rpc tr mismatch then select",
        "rpc tr unicode replay",
    ];
    let f = fixture();
    let mut comparisons = 0;
    for c in f["containers"].as_array().unwrap() {
        for run in c["runs"].as_array().unwrap() {
            for name in names {
                let request = run
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|r| r["name"] == name)
                    .unwrap();
                let parameters = request["parameters"].as_array().unwrap();
                // Bind declarations before reading execution payloads. The one
                // inferred tedious parameter is supplied as NVARCHAR(1),
                // consistent with the captured 52-byte result descriptor.
                // Input RPC declaration bytes are not retained in the fixture;
                // the core never infers a width from a current value.
                let arguments: Vec<_> = parameters
                    .iter()
                    .map(|parameter| {
                        let family = match parameter["type"].as_str().unwrap() {
                            "NVarChar" => Family::Nvarchar,
                            "VarChar" => Family::Varchar,
                            other => panic!("unexpected character request type: {other}"),
                        };
                        let width = parameter["options"]["length"].as_u64().unwrap_or_else(|| {
                            assert_eq!(name, "rpc cws inferred length argument");
                            assert_eq!(parameter["name"], "a");
                            1
                        });
                        let cap = if family == Family::Nvarchar {
                            4000
                        } else {
                            8000
                        };
                        arg(
                            family,
                            if width > cap {
                                Length::Max
                            } else {
                                Length::Bounded(width as u16)
                            },
                        )
                    })
                    .collect();
                let function = if name.starts_with("rpc cws") {
                    Function::ConcatWs
                } else {
                    Function::Translate
                };
                let p = rules::plan(function, &arguments, DEFAULT, &catalog()).unwrap();
                let values: Vec<_> = parameters
                    .iter()
                    .map(|parameter| parameter["value"].as_str().and_then(text))
                    .collect();
                let before = values.clone();
                let result = rules::evaluate(&p, &values, &default_match);
                let captured = &request["result"];
                assert_character_descriptor(&p, &captured["sets"][0]["columns"][0], name);
                if captured["errors"].as_array().unwrap().is_empty() {
                    let value = result.unwrap().map(|v| String::from_utf16(&v).unwrap());
                    assert_eq!(json!([[value]]), captured["sets"][0]["rows"], "{name}");
                } else {
                    assert_eq!(captured["errors"].as_array().unwrap().len(), 1);
                    assert_runtime_error(&result.unwrap_err(), &captured["errors"][0]);
                    assert!(captured["sets"][0]["rows"].as_array().unwrap().is_empty());
                }
                if name == "rpc tr mismatch then select" {
                    // Retain evidence for later shell integration: the pure
                    // expression core does not execute subsequent statements.
                    assert_eq!(captured["sets"].as_array().unwrap().len(), 2);
                    assert_eq!(captured["sets"][1]["rows"], json!([[1]]));
                }
                assert_eq!(values, before);
                comparisons += 1;
            }
        }
    }
    assert_eq!(comparisons, 56);
}

#[test]
fn column_batches_preserve_nulls_and_prefix_rows_before_runtime_error() {
    let implicit = |family, width| {
        Argument::character(
            CharacterType::new(family, Length::Bounded(width)).unwrap(),
            Label::Implicit(DEFAULT.into()),
        )
    };
    let integer = Argument {
        kind: Some(Type::Int),
        converted_width: None,
        collation: None,
    };
    let rows = [
        [Some("-"), Some("a"), Some("u"), Some("1")],
        [None, Some("a"), None, None],
        [Some("+"), None, Some("u"), Some("3")],
        [Some(""), None, None, None],
    ];
    let cases = [
        (
            "cws column rows",
            Function::ConcatWs,
            vec![
                literal(Some(","), true).0,
                implicit(Family::Varchar, 10),
                implicit(Family::Nvarchar, 10),
                integer,
            ],
        ),
        (
            "cws column null separator",
            Function::ConcatWs,
            vec![
                implicit(Family::Nvarchar, 3),
                implicit(Family::Varchar, 10),
                implicit(Family::Nvarchar, 10),
            ],
        ),
        (
            "tr column rows",
            Function::Translate,
            vec![
                implicit(Family::Varchar, 10),
                literal(Some("au"), false).0,
                literal(Some("AU"), false).0,
            ],
        ),
        (
            "tr column mismatch row",
            Function::Translate,
            vec![
                literal(Some("a-b+c"), true).0,
                implicit(Family::Nvarchar, 3),
                literal(Some("x"), true).0,
            ],
        ),
    ];
    let f = fixture();
    let mut comparisons = 0;
    for (name, function, arguments) in cases {
        let p = rules::plan(function, &arguments, DEFAULT, &catalog()).unwrap();
        let mut results = vec![];
        let mut error = None;
        for (index, [separator, ansi, unicode, integer]) in rows.iter().copied().enumerate() {
            let values: Vec<_> = match name {
                "cws column rows" => [Some(","), ansi, unicode, integer]
                    .into_iter()
                    .map(|v| v.and_then(text))
                    .collect(),
                "cws column null separator" => [separator, ansi, unicode]
                    .into_iter()
                    .map(|v| v.and_then(text))
                    .collect(),
                "tr column rows" => [ansi, Some("au"), Some("AU")]
                    .into_iter()
                    .map(|v| v.and_then(text))
                    .collect(),
                "tr column mismatch row" => [Some("a-b+c"), separator, Some("x")]
                    .into_iter()
                    .map(|v| v.and_then(text))
                    .collect(),
                _ => unreachable!(),
            };
            let before = values.clone();
            let result = rules::evaluate(&p, &values, &default_match);
            assert_eq!(values, before);
            match result {
                Ok(output) => results.push(json!([
                    index + 1,
                    output.map(|v| String::from_utf16(&v).unwrap())
                ])),
                Err(actual) => {
                    assert_eq!(name, "tr column mismatch row");
                    assert_eq!(index, 3);
                    error = Some(actual);
                    break;
                }
            }
        }
        for c in f["containers"].as_array().unwrap() {
            for run in c["runs"].as_array().unwrap() {
                let captured = observed(run, name);
                assert_eq!(json!(results), captured["sets"][0]["rows"], "{name}");
                assert_character_descriptor(&p, &captured["sets"][0]["columns"][1], name);
                match &error {
                    Some(actual) => {
                        assert_eq!(captured["errors"].as_array().unwrap().len(), 1);
                        assert_runtime_error(actual, &captured["errors"][0]);
                    }
                    None => assert!(captured["errors"].as_array().unwrap().is_empty()),
                }
                comparisons += 1;
            }
        }
    }
    assert_eq!(comparisons, 16);
}

#[test]
fn remaining_character_and_explicit_integer_text_cases_match_captures() {
    let integer = Argument {
        kind: Some(Type::Int),
        converted_width: None,
        collation: None,
    };
    let lit = |s: &str, unicode| literal(Some(s), unicode).0;
    let cases = vec![
        (
            "cws supplementary unicode",
            Function::ConcatWs,
            vec![lit("😀", true), lit("a", true), lit("𝄞", true)],
            vec![text("😀"), text("a"), text("𝄞")],
        ),
        (
            "cws supplementary to varchar",
            Function::ConcatWs,
            vec![lit("-", false), lit("😀", true), lit("a", false)],
            vec![text("-"), text("😀"), text("a")],
        ),
        (
            "cws integer separator",
            Function::ConcatWs,
            vec![integer.clone(), lit("a", false), lit("b", false)],
            vec![text("0"), text("a"), text("b")],
        ),
        (
            "tr integer input",
            Function::Translate,
            vec![integer, lit("1", false), lit("9", false)],
            vec![text("12321"), text("1"), text("9")],
        ),
    ];
    let f = fixture();
    let mut comparisons = 0;
    for (name, function, arguments, values) in cases {
        let p = rules::plan(function, &arguments, DEFAULT, &catalog()).unwrap();
        let output = rules::evaluate(&p, &values, &default_match)
            .unwrap()
            .unwrap();
        for c in f["containers"].as_array().unwrap() {
            for run in c["runs"].as_array().unwrap() {
                let captured = observed(run, name);
                assert_character_descriptor(&p, &captured["sets"][0]["columns"][0], name);
                assert_eq!(
                    json!(String::from_utf16(&output).unwrap()),
                    captured["sets"][0]["rows"][0][0],
                    "{name}"
                );
                assert!(captured["errors"].as_array().unwrap().is_empty());
                if name == "cws supplementary unicode" {
                    assert_eq!(json!(output.len()), captured["sets"][0]["rows"][0][1]);
                }
                comparisons += 1;
            }
        }
    }
    assert_eq!(comparisons, 16);
}

#[test]
fn concat_max_output_and_predicate_inputs_match_captured_values() {
    let f = fixture();
    let p = rules::plan(
        Function::ConcatWs,
        &[
            literal(Some("-"), false).0,
            arg(Family::Varchar, Length::Max),
            arg(Family::Varchar, Length::Bounded(8000)),
        ],
        DEFAULT,
        &catalog(),
    )
    .unwrap();
    assert_eq!(p.declaration.length(), Length::Max);
    let output = rules::evaluate(
        &p,
        &[text("-"), text(&"x".repeat(5000)), text(&"y".repeat(5000))],
        &default_match,
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        output,
        "x".repeat(5000)
            .chars()
            .chain(['-'])
            .chain("y".repeat(5000).chars())
            .map(|c| c as u16)
            .collect::<Vec<_>>()
    );
    let predicate = rules::plan(
        Function::ConcatWs,
        &[
            literal(Some(","), false).0,
            arg(Family::Varchar, Length::Bounded(10)),
            arg(Family::Nvarchar, Length::Bounded(10)),
        ],
        DEFAULT,
        &catalog(),
    )
    .unwrap();
    let inputs = [
        [Some("a"), Some("u")],
        [Some("a"), None],
        [None, Some("u")],
        [None, None],
    ];
    let matching: Vec<_> = inputs
        .into_iter()
        .enumerate()
        .filter_map(|(index, [a, u])| {
            let value = rules::evaluate(
                &predicate,
                &[text(","), a.and_then(text), u.and_then(text)],
                &default_match,
            )
            .unwrap();
            (value == text("a,u")).then(|| json!([index + 1]))
        })
        .collect();
    for c in f["containers"].as_array().unwrap() {
        for run in c["runs"].as_array().unwrap() {
            let max = observed(run, "cws varchar max long value");
            assert_eq!(max["sets"][0]["columns"][0]["type"], "IntN");
            assert_eq!(max["sets"][0]["columns"][0]["length"], 8);
            assert_eq!(
                json!(output.len().to_string()),
                max["sets"][0]["rows"][0][0]
            );
            assert_eq!(
                json!(matching),
                observed(run, "cws in predicate")["sets"][0]["rows"]
            );
        }
    }
}

#[test]
fn incomplete_catalogs_and_conversion_contracts_remain_explicit_barriers() {
    let base = vec![arg(Family::Varchar, Length::Bounded(1)); 3];
    for function in [Function::ConcatWs, Function::Translate] {
        let mut missing = base.clone();
        missing[1].collation = None;
        assert_eq!(
            rules::plan(function, &missing, DEFAULT, &catalog()),
            Err(Error::UnknownCollation)
        );
        for position in 0..3 {
            let mut unknown = base.clone();
            unknown[position].collation = Some(Label::Implicit("unavailable_catalog_entry".into()));
            assert_eq!(
                rules::plan(function, &unknown, DEFAULT, &catalog()),
                Err(Error::UnknownCollation)
            );
        }
        let mut ambiguous = catalog();
        let mut duplicate = ambiguous[0].clone();
        duplicate.name = DEFAULT.to_ascii_lowercase();
        ambiguous.push(duplicate);
        assert_eq!(
            rules::plan(function, &base, DEFAULT, &ambiguous),
            Err(Error::UnknownCollation)
        );
        let mut unresolved = base.clone();
        unresolved[0].collation = Some(Label::NoCollation {
            left: DEFAULT.into(),
            right: "missing".into(),
        });
        assert_eq!(
            rules::plan(function, &unresolved, DEFAULT, &catalog()),
            Err(Error::UnknownCollation)
        );
    }
    for kind in [Type::Text, Type::Ntext, Type::Image] {
        let mut arguments = base.clone();
        arguments[1] = Argument {
            kind: Some(kind),
            converted_width: None,
            collation: None,
        };
        assert_eq!(
            rules::plan(Function::ConcatWs, &arguments, DEFAULT, &catalog()),
            Err(Error::UnknownConversion)
        );
    }
    let mut values = vec![text("a"), text("b"), text("c")];
    let p = rules::plan(Function::Translate, &base, DEFAULT, &catalog()).unwrap();
    assert_eq!(
        rules::evaluate(&p, &values, &|_, _| None),
        Err(Error::UnknownComparison)
    );
    values.pop();
    assert_eq!(
        rules::evaluate(&p, &values, &default_match),
        Err(Error::InvalidPayload)
    );
    let arguments = vec![Argument::null_literal(), base[1].clone(), base[2].clone()];
    let p = rules::plan(Function::ConcatWs, &arguments, DEFAULT, &catalog()).unwrap();
    assert_eq!(
        rules::evaluate(&p, &[text("x"), text("b"), text("c")], &default_match),
        Err(Error::InvalidPayload)
    );
}

#[test]
fn translate_noncharacter_max_conversion_never_fabricates_bounded_metadata() {
    let arguments = vec![
        Argument {
            kind: Some(Type::Binary(
                msduck_core::types::BinaryType::new(false, Length::Max).unwrap(),
            )),
            converted_width: Some(Length::Max),
            collation: None,
        },
        literal(Some("a"), false).0,
        literal(Some("b"), false).0,
    ];
    assert_eq!(
        rules::plan(Function::Translate, &arguments, DEFAULT, &catalog()),
        Err(Error::UnknownConversion)
    );
    // Character MAX remains established by the retained four captures.
    let mut character = arguments.clone();
    character[0] = arg(Family::Varchar, Length::Max);
    let p = rules::plan(Function::Translate, &character, DEFAULT, &catalog()).unwrap();
    assert_eq!(p.declaration.length(), Length::Max);
    assert_eq!(
        rules::evaluate(
            &p,
            &[text(&"a".repeat(9000)), text("a"), text("b")],
            &default_match
        )
        .unwrap(),
        text(&"b".repeat(9000))
    );
}

#[test]
fn max_repeated_characters_do_not_repeat_mapping_scans() {
    let p = rules::plan(
        Function::Translate,
        &vec![arg(Family::Nvarchar, Length::Max); 3],
        DEFAULT,
        &catalog(),
    )
    .unwrap();
    let values = vec![
        Some(vec![100; 1_000_000]),
        Some(vec![97; 8000]),
        Some(vec![98; 8000]),
    ];
    let calls = std::cell::Cell::new(0usize);
    let result = rules::evaluate(&p, &values, &|a, b| {
        calls.set(calls.get() + 1);
        // The old implementation fails immediately on the second input
        // character, without allowing the regression to run billions of calls.
        assert!(
            calls.get() <= 8000,
            "repeated input character rescanned its mapping"
        );
        Some(a == b)
    })
    .unwrap()
    .unwrap();
    assert_eq!(result, vec![100; 1_000_000]);
    assert_eq!(calls.get(), 8000);
}

#[test]
fn opaque_matching_work_is_bounded_and_keys_handle_the_same_large_input() {
    let p = rules::plan(
        Function::Translate,
        &vec![arg(Family::Nvarchar, Length::Max); 3],
        DEFAULT,
        &catalog(),
    )
    .unwrap();
    let input: Vec<u16> = (0x1000..0x2000).collect();
    let values = vec![
        Some(input.clone()),
        Some(vec![97; 8000]),
        Some(vec![98; 8000]),
    ];
    let comparisons = std::cell::Cell::new(0usize);
    assert_eq!(
        rules::evaluate(&p, &values, &|a, b| {
            comparisons.set(comparisons.get() + 1);
            Some(a == b)
        }),
        Err(Error::ComparisonLimit)
    );
    assert_eq!(comparisons.get(), rules::MAX_MATCH_COMPARISONS);
    let keys = std::cell::Cell::new(0usize);
    let result = rules::evaluate_with_keys(&p, &values, &|unit| {
        keys.set(keys.get() + 1);
        Some(unit.to_vec())
    })
    .unwrap();
    assert_eq!(result, Some(input));
    assert_eq!(keys.get(), 8000 + 4096);
}

#[test]
fn indexed_keys_keep_first_mapping_no_chaining_and_empty_mapping_semantics() {
    let args = vec![arg(Family::Nvarchar, Length::Max); 3];
    let p = rules::plan(Function::Translate, &args, DEFAULT, &catalog()).unwrap();
    // Explicit ASCII case-insensitive equivalence classes. These are not an
    // implementation of SQL Server weights for uncaptured character families.
    let key = |unit: &[u16]| {
        (unit.len() == 1 && unit[0] < 128).then(|| (unit[0] as u8).to_ascii_lowercase())
    };
    for (input, from, to, expected) in [("AaB", "aa", "xy", "xxB"), ("abc", "ab", "bc", "bcc")] {
        assert_eq!(
            rules::evaluate_with_keys(&p, &[text(input), text(from), text(to)], &key).unwrap(),
            text(expected)
        );
    }
    assert_eq!(
        rules::evaluate_with_keys(&p, &[text("abc"), text(""), text("")], &|_| None::<u8>).unwrap(),
        text("abc")
    );
    assert_eq!(
        rules::evaluate_with_keys(&p, &[None, text("a"), text("b")], &|_| None::<u8>).unwrap(),
        None
    );
    assert_eq!(
        rules::evaluate_with_keys(&p, &[text("abc"), text("a"), text("b")], &|_| None::<u8>),
        Err(Error::UnknownComparison)
    );
    let values = [text("abc"), text("ab"), text("x")];
    let Err(Error::Sql(error)) = rules::evaluate_with_keys(&p, &values, &|_| None::<u8>) else {
        panic!("mismatch diagnostic must precede unknown weights")
    };
    assert_eq!(error.number, 9828);
    assert_eq!(error.state, 3);
}

#[test]
fn max_mappings_stream_duplicates_and_preserve_mismatch() {
    let p = rules::plan(
        Function::Translate,
        &vec![arg(Family::Nvarchar, Length::Max); 3],
        DEFAULT,
        &catalog(),
    )
    .unwrap();
    let values = [
        text("a"),
        Some(vec![97; 1_000_000]),
        Some(vec![98; 1_000_000]),
    ];
    let comparisons = std::cell::Cell::new(0);
    assert_eq!(
        rules::evaluate(&p, &values, &|a, b| {
            comparisons.set(comparisons.get() + 1);
            Some(a == b)
        })
        .unwrap(),
        text("b")
    );
    assert_eq!(comparisons.get(), 1);
    assert_eq!(
        rules::evaluate_with_keys(&p, &values, &|unit| Some(unit[0])).unwrap(),
        text("b")
    );
    let mismatch = [text("a"), Some(vec![97; 1_000_000]), text("b")];
    let Err(Error::Sql(error)) = rules::evaluate_with_keys(&p, &mismatch, &|_| None::<u16>) else {
        panic!("large mismatch must retain SQL diagnostic before key evaluation")
    };
    assert_eq!(error.number, 9828);
    assert_eq!(error.state, 3);
}

#[test]
fn max_output_limits_reject_separator_amplification_and_translation_growth() {
    let p = rules::plan(
        Function::ConcatWs,
        &vec![arg(Family::Nvarchar, Length::Max); 254],
        DEFAULT,
        &catalog(),
    )
    .unwrap();
    let mut values = vec![Some(Vec::new()); 254];
    values[0] = Some(vec![97; 5 * 1024 * 1024]);
    assert_eq!(
        rules::evaluate(&p, &values, &default_match),
        Err(Error::OutputLimit)
    );
    // NULL values contribute no separator gaps; their presence alone cannot
    // cause the allocation limit to reject the empty result.
    values[1..].fill(None);
    assert_eq!(
        rules::evaluate(&p, &values, &default_match).unwrap(),
        text("")
    );

    let p = rules::plan(
        Function::Translate,
        &vec![arg(Family::Nvarchar, Length::Max); 3],
        DEFAULT,
        &catalog(),
    )
    .unwrap();
    let values = [
        Some(vec![97; rules::MAX_OUTPUT_UNITS + 1]),
        text(""),
        text(""),
    ];
    assert_eq!(
        rules::evaluate(&p, &values, &default_match),
        Err(Error::InputLimit)
    );
    assert_eq!(
        rules::evaluate_with_keys(&p, &values, &|unit| Some(unit[0])),
        Err(Error::InputLimit)
    );
}

#[test]
fn translate_replacement_growth_stops_at_output_allocation_limit() {
    let mut arguments = vec![arg(Family::Nvarchar, Length::Max); 3];
    arguments[0].collation = Some(Label::Explicit(SC.into()));
    let p = rules::plan(Function::Translate, &arguments, DEFAULT, &catalog()).unwrap();
    let values = [
        Some(vec![97; rules::MAX_OUTPUT_UNITS / 2 + 1]),
        text("a"),
        text("🦆"),
    ];
    assert_eq!(
        rules::evaluate(&p, &values, &default_match),
        Err(Error::OutputLimit)
    );
    assert_eq!(
        rules::evaluate_with_keys(&p, &values, &|unit| Some(unit.to_vec())),
        Err(Error::OutputLimit)
    );
}

#[test]
fn captured_function_width_boundary_drops_crossing_pairs() {
    let mut arguments = vec![
        Argument::null_literal(),
        arg(Family::Nvarchar, Length::Bounded(4000)),
        arg(Family::Nvarchar, Length::Bounded(2)),
    ];
    arguments[1].collation = Some(Label::Explicit(SC.into()));
    let p = rules::plan(Function::ConcatWs, &arguments, DEFAULT, &catalog()).unwrap();
    assert!(p.supplementary);
    assert_eq!(p.declaration.length(), Length::Bounded(4000));
    assert_eq!(
        rules::evaluate(
            &p,
            &[None, text(&"a".repeat(3999)), text("🦆")],
            &default_match
        )
        .unwrap(),
        text(&"a".repeat(3999))
    );
    // A complete pair ending exactly at the cap remains valid.
    let result = rules::evaluate(
        &p,
        &[None, text(&"a".repeat(3998)), text("🦆")],
        &default_match,
    )
    .unwrap()
    .unwrap();
    assert_eq!(result.len(), 4000);
    assert_eq!(&result[3998..], &[0xd83e, 0xdd86]);

    arguments = vec![
        arg(Family::Nvarchar, Length::Bounded(4000)),
        arg(Family::Nvarchar, Length::Bounded(1)),
        arg(Family::Nvarchar, Length::Bounded(2)),
    ];
    arguments[0].collation = Some(Label::Explicit(SC.into()));
    let p = rules::plan(Function::Translate, &arguments, DEFAULT, &catalog()).unwrap();
    let values = [
        text(&format!("{}b", "a".repeat(3999))),
        text("b"),
        text("🦆"),
    ];
    assert_eq!(
        rules::evaluate(&p, &values, &default_match).unwrap(),
        text(&"a".repeat(3999))
    );
    assert_eq!(
        rules::evaluate_with_keys(&p, &values, &|unit| Some(unit.to_vec())).unwrap(),
        text(&"a".repeat(3999))
    );
}

#[test]
fn captured_concat_leaf_boundaries_preserve_cross_argument_surrogates_without_refill() {
    // Four identical boundary captures: f0f0e55238001a905f96eb3de8c5607573ef7732f4bc9f2f11ced4bae0cbbdc6.
    // Keep raw UTF-16 units; String::from_utf16 would lose these distinctions.
    for collation in [DEFAULT, SC] {
        let prefix = vec![97; 3999];
        let mut high_prefix = prefix.clone();
        high_prefix.push(0xd83d);
        let cases = [
            (
                vec![prefix.clone(), vec![0xd83d, 0xde00], vec![90]],
                prefix.clone(),
            ),
            (
                vec![prefix.clone(), vec![0xd83d], vec![0xde00, 90]],
                high_prefix.clone(),
            ),
            (
                vec![high_prefix.clone(), vec![0xde00, 90]],
                high_prefix.clone(),
            ),
            (vec![prefix.clone(), vec![0xd83d], vec![90]], high_prefix),
            (vec![vec![97; 3998], vec![0xd83d, 0xde00], vec![90]], {
                let mut value = vec![97; 3998];
                value.extend([0xd83d, 0xde00]);
                value
            }),
        ];
        for (leaves, expected) in cases {
            let mut arguments = vec![Argument::null_literal()];
            arguments.extend(
                leaves
                    .iter()
                    .map(|leaf| arg(Family::Nvarchar, Length::Bounded(leaf.len() as u16))),
            );
            arguments[1].collation = Some(Label::Explicit(collation.into()));
            let p = rules::plan(Function::ConcatWs, &arguments, DEFAULT, &catalog()).unwrap();
            assert_eq!(p.declaration.length(), Length::Bounded(4000));
            let mut values = vec![None];
            values.extend(leaves.into_iter().map(Some));
            assert_eq!(
                rules::evaluate(&p, &values, &default_match).unwrap(),
                Some(expected)
            );
        }
    }
}

#[test]
fn captured_concat_separator_truncation_retains_split_high_surrogate() {
    // Four identical 44-record captures:
    // a9aacc82bdfd34f6fb854cf95d2a8fda9a2a3b9eb20b46e55fb6d78c13219ecf.
    for collation in [DEFAULT, SC] {
        for prefix_length in [3999, 3998] {
            let mut arguments = vec![
                arg(Family::Nvarchar, Length::Bounded(2)),
                arg(Family::Nvarchar, Length::Bounded(prefix_length)),
                arg(Family::Nvarchar, Length::Bounded(1)),
            ];
            arguments[1].collation = Some(Label::Explicit(collation.into()));
            let p = rules::plan(Function::ConcatWs, &arguments, DEFAULT, &catalog()).unwrap();
            let values = [
                text("😀"),
                Some(vec![97; usize::from(prefix_length)]),
                text("Z"),
            ];
            let mut expected = vec![97; usize::from(prefix_length)];
            expected.push(0xd83d);
            if prefix_length == 3998 {
                expected.push(0xde00);
            }
            assert_eq!(
                rules::evaluate(&p, &values, &default_match).unwrap(),
                Some(expected)
            );
        }
    }
}
