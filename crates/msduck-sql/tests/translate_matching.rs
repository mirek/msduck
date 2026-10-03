use msduck_core::{
    character::{CharacterType, Family, Length},
    collation::Label,
    encoding::decode_cp1252,
};
use msduck_sql::{
    concat_ws::{self, Argument, Collation, Encoding, Function},
    translate_matching::{self as matching, Context, Domain, Error, Properties},
};
use serde_json::{Value, json};
use std::{fs, path::Path};

fn properties(name: &str, wire: &Value) -> Properties {
    let supplementary = matches!(
        name,
        "Latin1_General_100_CI_AS_SC" | "Latin1_General_100_CI_AS_SC_UTF8"
    );
    let flags = wire["flags"].as_u64().unwrap() as u8;
    Properties {
        collation: Collation {
            name: name.into(),
            supplementary,
            case_sensitive: matches!(flags, 12 | 14 | 32),
            encoding: match wire["codepage"].as_str().unwrap() {
                "CP1252" => Encoding::Cp1252,
                "utf-8" => Encoding::Utf8,
                other => panic!("{other}"),
            },
        },
        lcid: wire["lcid"].as_u64().unwrap() as u32,
        flags,
        version: wire["version"].as_u64().unwrap() as u8,
        sort_id: wire["sortId"].as_u64().unwrap() as u8,
    }
}
fn default_context() -> Context {
    Context::new(
        Domain::Nvarchar,
        properties(
            "SQL_Latin1_General_CP1_CI_AS",
            &json!({"lcid":1033,"flags":13,"version":0,"sortId":52,"codepage":"CP1252"}),
        ),
    )
    .unwrap()
}

#[test]
fn collation_identity_accepts_ascii_case_variants() {
    let mut props = context_properties();
    props.collation.name.make_ascii_lowercase();
    let context = Context::new(Domain::Nvarchar, props.clone()).unwrap();
    let canonical = core_plan(&context, &context_properties());
    context.validate_plan(&canonical).unwrap();
    let lowercase = core_plan(&default_context(), &props);
    default_context().validate_plan(&lowercase).unwrap();
    let certificate = context.certificate(&[97], &[65]).unwrap();
    assert_eq!(certificate.key(&[97]), certificate.key(&[65]));
}

#[test]
fn matching_context_rejects_concat_ws_operation() {
    let props = context_properties();
    let arg = Argument::character(
        CharacterType::new(Family::Nvarchar, Length::Max).unwrap(),
        Label::Explicit(props.collation.name.clone()),
    );
    let mut plan = concat_ws::plan(
        Function::ConcatWs,
        &[arg.clone(), arg.clone(), arg],
        &props.collation.name,
        std::slice::from_ref(&props.collation),
    )
    .unwrap();
    assert_eq!(
        default_context().validate_plan(&plan),
        Err(Error::ContradictoryContext)
    );
    // Public result metadata cannot turn another operation into TRANSLATE.
    plan.flags |= 1;
    assert_eq!(
        default_context().validate_plan(&plan),
        Err(Error::ContradictoryContext)
    );
}

#[test]
fn matching_context_rejects_contradictory_plan_case_metadata() {
    let mut props = context_properties();
    props.collation.case_sensitive = true;
    let plan = core_plan(&default_context(), &props);
    assert_eq!(
        default_context().validate_plan(&plan),
        Err(Error::ContradictoryContext)
    );
}
fn hex(value: &str) -> Vec<u8> {
    assert_eq!(value.len() % 2, 0);
    value
        .as_bytes()
        .chunks_exact(2)
        .map(|b| u8::from_str_radix(std::str::from_utf8(b).unwrap(), 16).unwrap())
        .collect()
}
fn units(value: &Value) -> Option<Vec<u16>> {
    if value.is_null() {
        return None;
    }
    if let Some(text) = value.as_str() {
        return Some(text.encode_utf16().collect());
    }
    Some(
        value["utf16"]
            .as_array()
            .unwrap()
            .iter()
            .map(|u| u.as_u64().unwrap() as u16)
            .collect(),
    )
}
fn decode(bytes: &[u8], domain: Domain, encoding: Encoding) -> Vec<u16> {
    match domain {
        Domain::Nvarchar => {
            assert_eq!(bytes.len() % 2, 0);
            bytes
                .chunks_exact(2)
                .map(|x| u16::from_le_bytes([x[0], x[1]]))
                .collect()
        }
        Domain::Varchar => match encoding {
            Encoding::Cp1252 => decode_cp1252(bytes).encode_utf16().collect(),
            Encoding::Utf8 => std::str::from_utf8(bytes).unwrap().encode_utf16().collect(),
        },
    }
}
fn domain(input: &Value) -> Domain {
    if input["declaration"].as_str().unwrap().starts_with('N') {
        Domain::Nvarchar
    } else {
        Domain::Varchar
    }
}
fn native(value: &Value, domain: Domain, encoding: Encoding) -> Option<Vec<u16>> {
    if value.is_null() {
        None
    } else {
        assert_eq!(value["kind"], "binary");
        Some(decode(
            &hex(value["value"].as_str().unwrap()),
            domain,
            encoding,
        ))
    }
}

// Reversible carrier for JSON strings containing isolated UTF16 units. No
// replacement character, dropped record, output normalization or fixture edit.
fn fixture(name: &str) -> Value {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../reference")
        .join(name);
    let raw = fs::read_to_string(path).unwrap();
    let mut output = String::with_capacity(raw.len());
    let mut offset = 0;
    while let Some(relative) = raw[offset..].find('"') {
        let start = offset + relative;
        output.push_str(&raw[offset..start]);
        let mut end = start + 1;
        let bytes = raw.as_bytes();
        while end < bytes.len() {
            if bytes[end] == b'\\' {
                end += 2;
            } else if bytes[end] == b'"' {
                break;
            } else {
                end += 1;
            }
        }
        let token = &raw[start..=end];
        if serde_json::from_str::<String>(token).is_ok() {
            output.push_str(token);
        } else {
            let mut value = Vec::new();
            let mut chars = token[1..token.len() - 1].chars();
            while let Some(ch) = chars.next() {
                if ch == '\\' {
                    match chars.next().unwrap() {
                        'u' => {
                            let h: String = chars.by_ref().take(4).collect();
                            value.push(u16::from_str_radix(&h, 16).unwrap());
                        }
                        '"' => value.push(34),
                        '\\' => value.push(92),
                        '/' => value.push(47),
                        'b' => value.push(8),
                        'f' => value.push(12),
                        'n' => value.push(10),
                        'r' => value.push(13),
                        't' => value.push(9),
                        _ => panic!("invalid JSON escape"),
                    }
                } else {
                    value.extend(ch.encode_utf16(&mut [0; 2]).iter().copied());
                }
            }
            assert!(String::from_utf16(&value).is_err());
            output.push_str(&json!({"utf16":value}).to_string());
        }
        offset = end + 1;
    }
    output.push_str(&raw[offset..]);
    serde_json::from_str(&output).unwrap()
}

// Independent bounded decoding of actual same-evaluation ROW/NBCROW cells.
// Binary projections are checked separately, never substituted for text bytes.
fn raw_rows(result: &Value) -> Vec<Vec<Option<Vec<u8>>>> {
    let mut columns = &Value::Null;
    let mut rows = Vec::new();
    let mut set = 0;
    let mut row_index = 0;
    for token in result["tokens"].as_array().unwrap() {
        match token["token"].as_str().unwrap() {
            "COLMETADATA" => {
                if set > 0 {
                    assert_eq!(
                        row_index,
                        result["sets"][set - 1]["rows"].as_array().unwrap().len()
                    );
                }
                row_index = 0;
                columns = &token["descriptors"];
                assert_eq!(columns, &result["sets"][set]["columns"]);
                set += 1;
            }
            "ROW" | "NBCROW" => {
                let raw = hex(token["raw"]["hex"].as_str().unwrap());
                let mut at = 0;
                let n = columns.as_array().unwrap().len();
                let bitmap = if token["token"] == "NBCROW" {
                    let size = n.div_ceil(8);
                    at = size;
                    Some(&raw[..size])
                } else {
                    None
                };
                let mut row = Vec::new();
                for (index, c) in columns.as_array().unwrap().iter().enumerate() {
                    if bitmap.is_some_and(|b| b[index / 8] & (1 << (index % 8)) != 0) {
                        row.push(None);
                        continue;
                    }
                    let kind = c["type"].as_str().unwrap();
                    let value = match kind {
                        "Int" => {
                            let b = raw[at..at + 4].to_vec();
                            at += 4;
                            Some(b)
                        }
                        "IntN" => {
                            let len = usize::from(raw[at]);
                            at += 1;
                            if len == 0 {
                                None
                            } else {
                                let b = raw[at..at + len].to_vec();
                                at += len;
                                Some(b)
                            }
                        }
                        "VarBinary" | "VarChar" | "NVarChar" => {
                            if c["length"] == 65535 {
                                let total = u64::from_le_bytes(raw[at..at + 8].try_into().unwrap());
                                at += 8;
                                if total == u64::MAX {
                                    None
                                } else {
                                    let mut b = Vec::new();
                                    loop {
                                        let len =
                                            u32::from_le_bytes(raw[at..at + 4].try_into().unwrap())
                                                as usize;
                                        at += 4;
                                        if len == 0 {
                                            break;
                                        }
                                        assert!(len <= raw.len() - at);
                                        b.extend_from_slice(&raw[at..at + len]);
                                        at += len;
                                    }
                                    assert!(total == u64::MAX - 1 || total == b.len() as u64);
                                    Some(b)
                                }
                            } else {
                                let len = usize::from(u16::from_le_bytes(
                                    raw[at..at + 2].try_into().unwrap(),
                                ));
                                at += 2;
                                if len == 65535 {
                                    None
                                } else {
                                    let b = raw[at..at + len].to_vec();
                                    at += len;
                                    Some(b)
                                }
                            }
                        }
                        other => panic!("unsupported raw cell {other}"),
                    };
                    row.push(value);
                }
                assert_eq!(at, raw.len());
                let decoded = &result["sets"][set - 1]["rows"][row_index];
                for (index, c) in columns.as_array().unwrap().iter().enumerate() {
                    match &row[index] {
                        None => assert!(decoded[index].is_null()),
                        Some(bytes) => match c["type"].as_str().unwrap() {
                            "VarBinary" => {
                                assert_eq!(decoded[index]["kind"], "binary");
                                assert_eq!(hex(decoded[index]["value"].as_str().unwrap()), *bytes);
                            }
                            "Int" | "IntN" => {
                                assert_eq!(bytes.len(), 4);
                                assert_eq!(
                                    decoded[index].as_i64().unwrap(),
                                    i64::from(i32::from_le_bytes(
                                        bytes.as_slice().try_into().unwrap()
                                    ))
                                );
                            }
                            "NVarChar" => assert_eq!(
                                units(&decoded[index]),
                                Some(decode(bytes, Domain::Nvarchar, Encoding::Cp1252))
                            ),
                            "VarChar" => {
                                let client = if c["collation"]["codepage"] == "utf-8" {
                                    String::from_utf8_lossy(bytes).encode_utf16().collect()
                                } else {
                                    decode(bytes, Domain::Varchar, Encoding::Cp1252)
                                        .into_iter()
                                        .map(|u| {
                                            if [129, 141, 143, 144, 157].contains(&u) {
                                                0xfffd
                                            } else {
                                                u
                                            }
                                        })
                                        .collect()
                                };
                                assert_eq!(units(&decoded[index]), Some(client));
                            }
                            other => panic!("unknown decoder {other}"),
                        },
                    }
                }
                row_index += 1;
                rows.push(row);
            }
            _ => {}
        }
    }
    if set > 0 {
        assert_eq!(
            row_index,
            result["sets"][set - 1]["rows"].as_array().unwrap().len()
        );
    }
    rows
}
fn core_plan(context: &Context, properties: &Properties) -> concat_ws::Plan {
    let family = if context.domain() == Domain::Nvarchar {
        Family::Nvarchar
    } else {
        Family::Varchar
    };
    let arg = Argument::character(
        CharacterType::new(family, Length::Max).unwrap(),
        Label::Explicit(properties.collation.name.clone()),
    );
    concat_ws::plan(
        Function::Translate,
        &[arg.clone(), arg.clone(), arg],
        &properties.collation.name,
        std::slice::from_ref(&properties.collation),
    )
    .unwrap()
}
fn check_translation(
    context: &Context,
    properties: &Properties,
    source: &[u16],
    mapping: &[u16],
    replacement: &[u16],
    expected: &[u16],
) {
    let certificate = context.certificate(source, mapping).unwrap();
    let plan = core_plan(context, properties);
    context.validate_plan(&plan).unwrap();
    let saved = plan.clone();
    let values = [
        Some(source.to_vec()),
        Some(mapping.to_vec()),
        Some(replacement.to_vec()),
    ];
    let actual = concat_ws::evaluate_with_keys(&plan, &values, &|unit| certificate.key(unit));
    // Independently apply operation keys to the admitted character alphabet.
    let chars = |value: &[u16]| -> Vec<Vec<u16>> {
        let mut chars = Vec::new();
        let mut at = 0;
        while at < value.len() {
            let size = if properties.collation.supplementary
                && (0xd800..=0xdbff).contains(&value[at])
                && value
                    .get(at + 1)
                    .is_some_and(|u| (0xdc00..=0xdfff).contains(u))
            {
                2
            } else {
                1
            };
            chars.push(value[at..at + size].to_vec());
            at += size;
        }
        chars
    };
    let candidates = chars(mapping);
    let replacements = chars(replacement);
    assert_eq!(candidates.len(), replacements.len());
    let mut predicted = Vec::new();
    for unit in chars(source) {
        let key = certificate.key(&unit).unwrap();
        let matched = candidates
            .iter()
            .position(|m| certificate.key(m) == Some(key));
        predicted.extend_from_slice(matched.map(|i| replacements[i].as_slice()).unwrap_or(&unit));
    }
    assert_eq!(predicted, expected);
    if context.domain() == Domain::Varchar && properties.collation.encoding == Encoding::Utf8 {
        assert_eq!(actual, Err(concat_ws::Error::UnknownEncoding));
    } else if context.domain() == Domain::Varchar
        && [source, mapping, replacement]
            .iter()
            .any(|v| msduck_core::encoding::encode_cp1252(&String::from_utf16(v).unwrap()).is_err())
    {
        // Native CP1252 undefined bytes round-trip. This branch concerns
        // genuinely unrepresentable Unicode payloads, including a client
        // U+FFFD mistakenly substituted for the native SQL unit0081.
        assert_eq!(actual, Err(concat_ws::Error::InvalidPayload));
    } else {
        assert_eq!(actual.unwrap().as_deref(), Some(expected));
    }
    assert_eq!(plan, saved);
}

#[test]
fn complete_ascii_raw_grids_and_prepared_bindings() {
    let fixture = fixture("translate-ascii-matching.json");
    let mut grids = 0;
    let mut prepared = 0;
    for container in fixture["containers"].as_array().unwrap() {
        for run in container["runs"].as_array().unwrap() {
            for observation in run.as_array().unwrap() {
                let input = &observation["input"];
                match input["kind"].as_str() {
                    Some("ASCII grid") => {
                        let result = &observation["result"];
                        assert_eq!(result["errors"], json!([]));
                        let row = &result["sets"][0]["rows"][0];
                        let raw = raw_rows(result);
                        assert_eq!(raw.len(), 1);
                        let d = domain(input);
                        let p = properties(
                            input["collation"].as_str().unwrap(),
                            &result["sets"][0]["columns"][4]["collation"],
                        );
                        let context = Context::new(d, p.clone()).unwrap();
                        let source = native(&row[0], d, p.collation.encoding).unwrap();
                        assert_eq!(source, (0..128).collect::<Vec<u16>>());
                        let mapping = native(&row[1], d, p.collation.encoding).unwrap();
                        assert_eq!(mapping, vec![input["mapping"].as_u64().unwrap() as u16]);
                        for (sentinel, text, projection) in [(2, 4, 5), (3, 6, 7)] {
                            let replacement =
                                native(&row[sentinel], d, p.collation.encoding).unwrap();
                            assert_eq!(replacement, vec![if sentinel == 2 { 233 } else { 8364 }]);
                            let expected =
                                decode(raw[0][text].as_ref().unwrap(), d, p.collation.encoding);
                            assert_eq!(units(&row[text]), Some(expected.clone()));
                            assert_eq!(
                                raw[0][text],
                                Some(hex(row[projection]["value"].as_str().unwrap()))
                            );
                            check_translation(
                                &context,
                                &p,
                                &source,
                                &mapping,
                                &replacement,
                                &expected,
                            );
                        }
                        grids += 1;
                    }
                    Some("prepared ASCII matching") => {
                        let p = &observation["prepared"];
                        assert_eq!(p["prepare"]["prepared"], true);
                        assert_eq!(p["prepare"]["errors"], json!([]));
                        let d = domain(input);
                        let props = properties(
                            input["collation"].as_str().unwrap(),
                            &p["prepare"]["sets"][0]["columns"][4]["collation"],
                        );
                        let context = Context::new(d, props.clone()).unwrap();
                        for (index, e) in p["executions"].as_array().unwrap().iter().enumerate() {
                            assert_wire(observation, e, d);
                            assert_eq!(e["values"], input["bindings"][index]);
                            assert_eq!(
                                e["result"]["sets"][0]["columns"],
                                p["prepare"]["sets"][0]["columns"]
                            );
                            let result = &e["result"];
                            let raw = raw_rows(result);
                            let plan = core_plan(&context, &props);
                            if index == 4 {
                                assert_eq!(result["errors"][0]["number"], 9828);
                                assert_eq!(
                                    result["errors"][0]["state"],
                                    if d == Domain::Nvarchar { 3 } else { 1 }
                                );
                                continue;
                            }
                            assert_eq!(result["errors"], json!([]));
                            let row = &result["sets"][0]["rows"][0];
                            if index == 3 {
                                assert!(raw[0][0].is_none());
                                assert!(raw[0][4].is_none());
                                assert!(raw[0][6].is_none());
                                if d == Domain::Nvarchar
                                    || props.collation.encoding == Encoding::Cp1252
                                {
                                    assert_eq!(
                                        concat_ws::evaluate_with_keys::<u16>(
                                            &plan,
                                            &[None, Some(vec![65]), Some(vec![233])],
                                            &|_| panic!("NULL cannot request keys")
                                        )
                                        .unwrap(),
                                        None
                                    );
                                }
                                continue;
                            }
                            let source = native(&row[0], d, props.collation.encoding).unwrap();
                            let map = native(&row[1], d, props.collation.encoding).unwrap();
                            for (rep, text) in [(2, 4), (3, 6)] {
                                let replacement =
                                    native(&row[rep], d, props.collation.encoding).unwrap();
                                let expected = decode(
                                    raw[0][text].as_ref().unwrap(),
                                    d,
                                    props.collation.encoding,
                                );
                                assert_eq!(units(&row[text]), Some(expected.clone()));
                                check_translation(
                                    &context,
                                    &props,
                                    &source,
                                    &map,
                                    &replacement,
                                    &expected,
                                );
                            }
                        }
                        assert_eq!(p["executions"][0]["result"], p["executions"][5]["result"]);
                        prepared += 1;
                    }
                    _ => {}
                }
            }
        }
    }
    assert_eq!(grids, 8192);
    assert_eq!(prepared, 64);
}

#[test]
fn forged_context_unknown_edges_and_bounded_max() {
    let context = default_context();
    let cert = context.certificate(&[0, 32, 65, 97], &[65, 97]).unwrap();
    assert_eq!(cert.key(&[65]), cert.key(&[97]));
    assert_ne!(cert.key(&[0]), cert.key(&[32]));
    assert_eq!(cert.key(&[98]), None);
    assert_eq!(cert.key(&[65, 97]), None);
    let mut props = context_properties();
    props.version = 2;
    assert_eq!(
        Context::new(Domain::Nvarchar, props),
        Err(Error::ContradictoryContext)
    );
    let mut props = context_properties();
    props.collation.name = "made up".into();
    assert_eq!(
        Context::new(Domain::Nvarchar, props),
        Err(Error::UnknownContext)
    );
    assert_eq!(
        context.certificate(&[0xd800], &[]),
        Err(Error::UnknownCharacterBoundary)
    );
    assert_eq!(
        context.certificate(&[0xd83d, 0xde00], &[]),
        Err(Error::UnknownCharacterBoundary)
    );
    assert_eq!(
        context.certificate(&[233], &[90]),
        Err(Error::UnknownRelationship)
    );
    let max = vec![65; matching::MAX_OPERAND_UNITS];
    let c = context.certificate(&max, &max).unwrap();
    assert_eq!(c.character_count(), 1);
    assert_eq!(c.key(&[65]), Some(0));
    let too_large = vec![65; matching::MAX_OPERAND_UNITS + 1];
    assert_eq!(
        context.certificate(&too_large, &[]),
        Err(Error::OperandLimit)
    );
    let distinct: (Vec<u16>, Vec<u16>) = ((0..257).collect(), vec![]);
    assert_eq!(
        context.certificate(&distinct.0, &distinct.1),
        Err(Error::CertificateLimit)
    );
}
fn context_properties() -> Properties {
    properties(
        "SQL_Latin1_General_CP1_CI_AS",
        &json!({"lcid":1033,"flags":13,"version":0,"sortId":52,"codepage":"CP1252"}),
    )
}

#[test]
fn finite_stable_controls_and_unknown_frontiers() {
    let fixture = fixture("translate-character-matching.json");
    let mut admitted = 0;
    let mut unknown = 0;
    let mut raw_checked = 0;
    let mut diagnostics = 0;
    for container in fixture["containers"].as_array().unwrap() {
        for run in container["runs"].as_array().unwrap() {
            for observation in run.as_array().unwrap() {
                let input = &observation["input"];
                if !matches!(
                    input["kind"].as_str(),
                    Some("pair controls" | "mapping boundary" | "byte identity")
                ) {
                    continue;
                }
                let result = &observation["result"];
                let raw = raw_rows(result);
                raw_checked += raw.len();
                let d = domain(input);
                let name = input["collation"].as_str().unwrap();
                let wire = result["sets"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .flat_map(|s| s["columns"].as_array().unwrap())
                    .find(|c| !c["collation"].is_null())
                    .map(|c| &c["collation"]);
                let Some(wire) = wire else {
                    diagnostics += 1;
                    continue;
                };
                let props = properties(name, wire);
                let context = Context::new(d, props.clone()).unwrap();
                // Native source/declarations are never reconstructed from lossy input
                // strings; ANSI recoding (including '?') remains the original domain.
                let operands = &result["sets"][0]["rows"][0];
                let source = native(&operands[0], d, props.collation.encoding);
                let rows = result["sets"].as_array().unwrap();
                let mut raw_at = 1;
                for (set_index, set) in rows.iter().enumerate().skip(1) {
                    if set["columns"][0]["type"] == "Int" {
                        break;
                    }
                    if set["rows"].as_array().unwrap().is_empty() {
                        continue;
                    }
                    let cells = &set["rows"][0];
                    let observed = units(&cells[0]);
                    let actual_raw = raw[raw_at][0]
                        .as_ref()
                        .map(|b| decode(b, d, props.collation.encoding));
                    let client_raw = raw[raw_at][0].as_ref().map(|b| {
                        if d == Domain::Varchar && props.collation.encoding == Encoding::Cp1252 {
                            decode(b, d, props.collation.encoding)
                                .into_iter()
                                .map(|u| {
                                    if [129, 141, 143, 144, 157].contains(&u) {
                                        0xfffd
                                    } else {
                                        u
                                    }
                                })
                                .collect::<Vec<u16>>()
                        } else {
                            decode(b, d, props.collation.encoding)
                        }
                    });
                    assert_eq!(client_raw, observed);
                    raw_at += 1;
                    let mapping_index = if set_index == 1 { 1 } else { 3 };
                    let replacement_index = mapping_index + 1;
                    let mapping = native(&operands[mapping_index], d, props.collation.encoding);
                    let replacement =
                        native(&operands[replacement_index], d, props.collation.encoding);
                    if let (Some(source), Some(mapping), Some(replacement), Some(expected)) =
                        (&source, &mapping, &replacement, &actual_raw)
                    {
                        match context.certificate(source, mapping) {
                            Ok(_) => {
                                check_translation(
                                    &context,
                                    &props,
                                    source,
                                    mapping,
                                    replacement,
                                    expected,
                                );
                                admitted += 1;
                            }
                            Err(
                                Error::UnknownCharacterBoundary
                                | Error::UnknownRelationship
                                | Error::InconsistentRelationships,
                            ) => unknown += 1,
                            other => panic!(
                                "unexpected certificate {other:?} in {}",
                                observation["name"]
                            ),
                        }
                    } else {
                        assert!(observed.is_none());
                    }
                }
                diagnostics += result["errors"].as_array().unwrap().len();
            }
        }
    }
    assert_eq!(admitted, 9284);
    assert_eq!(unknown, 1036);
    assert_eq!(raw_checked, 21648);
    assert_eq!(diagnostics, 624);
    eprintln!(
        "finite controls: {admitted} admitted, {unknown} explicit unknown, {raw_checked} complete raw rows, {diagnostics} retained diagnostics"
    );
}

#[test]
fn first_duplicate_no_chaining_context_before_null_and_supplementary() {
    let context = default_context();
    let props = context_properties();
    check_translation(
        &context,
        &props,
        &[97, 65, 97, 65],
        &[97, 65],
        &[88, 89],
        &[88, 88, 88, 88],
    );
    check_translation(
        &context,
        &props,
        &[97, 98, 97],
        &[97, 98],
        &[98, 99],
        &[98, 99, 98],
    );
    let mut props = context_properties();
    props.collation.supplementary = true;
    assert_eq!(
        Context::new(Domain::Nvarchar, props),
        Err(Error::ContradictoryContext)
    );
    let sc_props = properties(
        "Latin1_General_100_CI_AS_SC",
        &json!({"lcid":1033,"flags":13,"version":2,"sortId":0,"codepage":"CP1252"}),
    );
    let sc = Context::new(Domain::Nvarchar, sc_props.clone()).unwrap();
    check_translation(
        &sc,
        &sc_props,
        &[0xd83d, 0xde00, 0xd83d, 0xde01, 0xd83d, 0xde00],
        &[0xd83d, 0xde00, 0xd83d, 0xde01],
        &[88, 89],
        &[88, 89, 88],
    );
    assert_eq!(
        sc.certificate(&[0xd83d, 0xde00, 0xd83d], &[0xd83d, 0xde00]),
        Err(Error::UnknownCharacterBoundary)
    );
    // Empty and NULL cases do not authorize new matching facts.
    assert_eq!(sc.certificate(&[], &[]).unwrap().character_count(), 0);
    assert_eq!(
        sc.certificate(&[0x1f00], &[]),
        Err(Error::UnknownRelationship)
    );
    let mut plan = core_plan(&context, &context_properties());
    plan.encoding = Encoding::Utf8;
    assert_eq!(
        context.validate_plan(&plan),
        Err(Error::ContradictoryContext)
    );
}

fn assert_wire(observation: &Value, execution: &Value, d: Domain) {
    let names = if observation["input"]["kind"] == "prepared ASCII matching" {
        vec!["s", "m", "t", "u"]
    } else {
        vec!["s", "m", "t"]
    };
    let kind = if d == Domain::Nvarchar {
        "NVarChar"
    } else {
        "VarChar"
    };
    let info = if d == Domain::Nvarchar {
        "e700040904d00034"
    } else {
        "a700020904d00034"
    };
    for (index, name) in names.iter().enumerate() {
        let declaration = &observation["declarations"][index];
        assert_eq!(declaration["name"], *name);
        assert_eq!(declaration["type"], kind);
        assert_eq!(declaration["options"]["length"], 512);
        assert_eq!(declaration["typeInfo"], info);
        let wire = &execution["wire"][index];
        assert_eq!(wire["type"], kind);
        assert_eq!(wire["typeInfo"], info);
        let value = units(&execution["values"][*name]);
        match value {
            None => {
                assert_eq!(wire["length"], "ffff");
                assert_eq!(wire["payload"], "");
            }
            Some(units) => {
                let payload = if d == Domain::Nvarchar {
                    units.iter().flat_map(|u| u.to_le_bytes()).collect()
                } else {
                    msduck_core::encoding::encode_cp1252(&String::from_utf16(&units).unwrap())
                        .unwrap()
                };
                assert_eq!(hex(wire["payload"].as_str().unwrap()), payload);
                assert_eq!(
                    hex(wire["length"].as_str().unwrap()),
                    (payload.len() as u16).to_le_bytes()
                );
            }
        }
    }
}

#[test]
fn prepared_finite_replay_keeps_errors_null_and_malformed_unknown() {
    let fixture = fixture("translate-character-matching.json");
    let mut programs = 0;
    let mut executions = 0;
    let mut admitted = 0;
    let mut unknown = 0;
    let mut errors = 0;
    for container in fixture["containers"].as_array().unwrap() {
        for run in container["runs"].as_array().unwrap() {
            for o in run.as_array().unwrap() {
                if o["input"]["kind"] != "prepared typed matching" {
                    continue;
                }
                let d = domain(&o["input"]);
                let prepared = &o["prepared"];
                let columns = &prepared["prepare"]["sets"][0]["columns"];
                let props = properties(
                    o["input"]["collation"].as_str().unwrap(),
                    &columns[3]["collation"],
                );
                let context = Context::new(d, props.clone()).unwrap();
                let plan = core_plan(&context, &props);
                context.validate_plan(&plan).unwrap();
                assert_eq!(prepared["prepare"]["prepared"], true);
                assert_eq!(prepared["prepare"]["errors"], json!([]));
                assert_eq!(prepared["unprepare"]["errors"], json!([]));
                assert_eq!(prepared["unprepare"]["returnStatus"], 0);
                for (index, e) in prepared["executions"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .enumerate()
                {
                    executions += 1;
                    assert_wire(o, e, d);
                    assert_eq!(e["values"], o["input"]["bindings"][index]);
                    let result = &e["result"];
                    assert_eq!(&result["sets"][0]["columns"], columns);
                    let raw = raw_rows(result);
                    let s = units(&e["values"]["s"]);
                    let m = units(&e["values"]["m"]).unwrap();
                    let t = units(&e["values"]["t"]).unwrap();
                    if !result["errors"].as_array().unwrap().is_empty() {
                        assert_eq!(
                            result["errors"],
                            json!([{"number":9828,"state":if d==Domain::Nvarchar{3}else{1},"class":16,"lineNumber":1,"serverName":"msduck-translate-reference","procName":"","message":"The second and third arguments of the TRANSLATE built-in function must contain an equal number of characters."}])
                        );
                        errors += 1;
                        if d == Domain::Nvarchar || props.collation.encoding == Encoding::Cp1252 {
                            let error = concat_ws::evaluate_with_keys::<u16>(
                                &plan,
                                &[s, Some(m), Some(t)],
                                &|_| panic!("mismatch must precede key lookup"),
                            )
                            .unwrap_err();
                            let concat_ws::Error::Sql(error) = error else {
                                panic!("expected captured SQL diagnostic")
                            };
                            assert_eq!(error.number, 9828);
                            assert_eq!(error.state, if d == Domain::Nvarchar { 3 } else { 1 });
                            assert_eq!(error.severity, 16);
                            assert_eq!(
                                error.message,
                                result["errors"][0]["message"].as_str().unwrap()
                            );
                        }
                        continue;
                    }
                    let cells = &result["sets"][0]["rows"][0];
                    if d == Domain::Varchar
                        && props.collation.encoding == Encoding::Utf8
                        && [0, 1, 2].iter().any(|&i| {
                            !cells[i].is_null()
                                && std::str::from_utf8(&hex(cells[i]["value"].as_str().unwrap()))
                                    .is_err()
                        })
                    {
                        // Actual native bytes do not admit the descriptor's
                        // UTF8 decoding. Preserve the observation as unknown;
                        // do not infer a CP1252 fallback matching context.
                        assert_eq!(
                            concat_ws::evaluate_with_keys::<u16>(
                                &plan,
                                &[s, Some(m), Some(t)],
                                &|_| panic!("unknown UTF8 cannot request keys")
                            ),
                            Err(concat_ws::Error::UnknownEncoding)
                        );
                        unknown += 1;
                        continue;
                    }
                    let native_source = native(&cells[0], d, props.collation.encoding);
                    let native_map = native(&cells[1], d, props.collation.encoding).unwrap();
                    let native_replacement =
                        native(&cells[2], d, props.collation.encoding).unwrap();
                    let expected = raw[0][3]
                        .as_ref()
                        .map(|b| decode(b, d, props.collation.encoding));
                    assert_eq!(expected, units(&cells[3]));
                    match (native_source, expected) {
                        (None, None) => {
                            assert!(s.is_none());
                            if d == Domain::Nvarchar || props.collation.encoding == Encoding::Cp1252
                            {
                                assert_eq!(
                                    concat_ws::evaluate_with_keys::<u16>(
                                        &plan,
                                        &[None, Some(m), Some(t)],
                                        &|_| panic!("NULL keys")
                                    )
                                    .unwrap(),
                                    None
                                );
                            }
                        }
                        (Some(source), Some(expected)) => {
                            match context.certificate(&source, &native_map) {
                                Ok(_) => {
                                    check_translation(
                                        &context,
                                        &props,
                                        &source,
                                        &native_map,
                                        &native_replacement,
                                        &expected,
                                    );
                                    admitted += 1;
                                }
                                Err(
                                    Error::UnknownCharacterBoundary
                                    | Error::UnknownRelationship
                                    | Error::InconsistentRelationships,
                                ) => unknown += 1,
                                other => panic!("unexpected {other:?}"),
                            }
                        }
                        _ => panic!("native NULL result mismatch"),
                    }
                }
                let e = prepared["executions"].as_array().unwrap();
                assert_eq!(e[0]["result"], e.last().unwrap()["result"]);
                programs += 1;
            }
        }
    }
    assert_eq!(programs, 64);
    assert_eq!(executions, 416);
    assert!(admitted > 200);
    assert!(unknown > 0);
    assert!(errors > 0);
    eprintln!(
        "finite prepared: {admitted} admitted, {unknown} unknown, {errors} exact errors across {executions} bindings"
    );
}

#[test]
fn every_context_property_and_missing_transitive_edge_stays_explicit() {
    let context = default_context();
    for mutate in [
        |p: &mut Properties| p.lcid = 0,
        |p: &mut Properties| p.flags ^= 1,
        |p: &mut Properties| p.version = 1,
        |p: &mut Properties| p.sort_id = 0,
        |p: &mut Properties| p.collation.supplementary = true,
        |p: &mut Properties| p.collation.case_sensitive = true,
        |p: &mut Properties| p.collation.encoding = Encoding::Utf8,
    ] {
        let mut props = context_properties();
        mutate(&mut props);
        assert_eq!(
            Context::new(Domain::Nvarchar, props),
            Err(Error::ContradictoryContext)
        );
    }
    let cp = context.certificate(&[101, 233], &[101, 233]).unwrap();
    assert_ne!(cp.key(&[101]), cp.key(&[233]));
    // e/é and é/É are measured independently. Their existence cannot invent
    // the unobserved e/É edge, even if transitive folding seems plausible.
    assert_eq!(
        context.certificate(&[101, 233, 201], &[101, 233]),
        Err(Error::UnknownRelationship)
    );
    let ai_props = properties(
        "Latin1_General_100_CI_AI",
        &json!({"lcid":1033,"flags":15,"version":2,"sortId":0,"codepage":"CP1252"}),
    );
    let ai = Context::new(Domain::Nvarchar, ai_props).unwrap();
    let cert = ai.certificate(&[101, 233], &[101, 233]).unwrap();
    assert_eq!(cert.key(&[101]), cert.key(&[233]));
    let plan = core_plan(&context, &context_properties());
    let error = concat_ws::evaluate_with_keys::<u16>(
        &plan,
        &[Some(vec![]), Some(vec![97, 98]), Some(vec![88])],
        &|_| panic!("mismatch before empty input keys"),
    )
    .unwrap_err();
    assert!(matches!(error,concat_ws::Error::Sql(e) if e.number==9828&&e.state==3));
    assert_eq!(
        concat_ws::evaluate_with_keys::<u16>(
            &plan,
            &[Some(vec![]), Some(vec![0x1f00]), Some(vec![88])],
            &|_| panic!("empty input requires no unobserved keys")
        )
        .unwrap(),
        Some(vec![])
    );
}

#[test]
fn native_undefined_cp1252_byte_roundtrips_but_client_replacement_is_not_source() {
    let props = context_properties();
    let context = Context::new(Domain::Varchar, props.clone()).unwrap();
    assert_eq!(
        msduck_core::encoding::encode_cp1252(&decode_cp1252(&[0x81])).unwrap(),
        [0x81]
    );
    check_translation(&context, &props, &[129, 127], &[129], &[88], &[88, 127]);
    let plan = core_plan(&context, &props);
    assert_eq!(
        concat_ws::evaluate_with_keys::<u16>(
            &plan,
            &[Some(vec![0xfffd]), Some(vec![129]), Some(vec![88])],
            &|_| panic!("client replacement is not an encodable SQL operand")
        ),
        Err(concat_ws::Error::InvalidPayload)
    );
}
