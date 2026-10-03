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
        ("cws 255 arguments", Function::ConcatWs, 255),
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
        cases.push((name, rules::plan(function, &arguments, DEFAULT, &catalog())));
    }
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
