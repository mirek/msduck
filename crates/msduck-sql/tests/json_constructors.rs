#[path = "../src/json_constructor.rs"]
mod json_constructor;

use json_constructor::{
    Constructor, Error, NullClause, ResultType, Scalar, SqlType, array, constructor_result, modify,
    modify_signature, null_clause, object,
};
use msduck_core::{
    datetime2::DateTime2, datetimeoffset::DateTimeOffset, diagnostic::SqlError, json,
    value::Decimal,
};
use serde_json::Value;

type Outcome = Result<Option<String>, Error>;
use NullClause::{AbsentOnNull, NullOnNull};
use Scalar as S;
use SqlType as T;

/// The captured database default collation of every `nvarchar(max)` result.
const DATABASE_COLLATION: &str = "SQL_Latin1_General_CP1_CI_AS";
/// Every escaping probe: quote, backslash, slash, short escapes, other
/// controls, DEL, é, U+2028 and a surrogate pair.
const PROBE: &str = "q\"b\\s/t\tn\nr\rf\u{c}b\u{8}c\u{1}z\u{1f}d\u{7f}e\u{e9}u\u{2028}s\u{1F600}";

enum Replay {
    /// Expected outcomes per result column: column index, declared result
    /// type, then one outcome per row with an optional terminal error.
    Values(Vec<(usize, ResultType, Vec<Outcome>)>),
    /// Case-specific assertions already ran.
    Checked,
    NotApplicable(&'static str),
}

fn fixture() -> Value {
    serde_json::from_str(include_str!("../../../reference/json-constructors.json")).unwrap()
}

fn dt(text: &str) -> DateTime2 {
    DateTime2::parse_iso(text).unwrap()
}
fn dec(precision: u8, scale: u8, coefficient: i128) -> Scalar<'static> {
    S::Decimal(Decimal::new(precision, scale, coefficient).unwrap())
}
fn guid(text: &str) -> [u8; 16] {
    let hex: Vec<u8> = (0..32)
        .map(|i| u8::from_str_radix(&text.replace('-', "")[i * 2..i * 2 + 2], 16).unwrap())
        .take(16)
        .collect();
    let order = [3, 2, 1, 0, 5, 4, 7, 6, 8, 9, 10, 11, 12, 13, 14, 15];
    let mut bytes = [0; 16];
    for (stored, &canonical) in order.iter().enumerate() {
        bytes[stored] = hex[canonical];
    }
    bytes
}

fn o(pairs: &[(Scalar<'_>, Scalar<'_>)], written: &[NullClause]) -> Outcome {
    object(pairs, null_clause(Constructor::Object, written)?).map(Some)
}
fn a(elements: &[Scalar<'_>], written: &[NullClause]) -> Outcome {
    array(elements, null_clause(Constructor::Array, written)?).map(Some)
}
fn text(outcome: Outcome) -> String {
    outcome.unwrap().unwrap()
}
fn nv() -> ResultType {
    constructor_result(false, &[T::VarChar, T::Int])
}
fn one(outcome: Outcome) -> Replay {
    Replay::Values(vec![(0, nv(), vec![outcome])])
}
fn one_typed(result: ResultType, outcome: Outcome) -> Replay {
    Replay::Values(vec![(0, result, vec![outcome])])
}
/// JSON_MODIFY with its declared types checked first, as at compile time.
fn m(types: [SqlType; 3], input: Option<&str>, path: Option<&str>, value: Scalar<'_>) -> Replay {
    match modify_signature(&types) {
        Ok(result) => one_typed(result, modify(input, path, &value)),
        Err(error) => one(Err(error)),
    }
}
/// JSON_MODIFY over NVARCHAR input and a VARCHAR literal path.
fn mn(input: &str, path: &str, value: Scalar<'_>) -> Replay {
    let kind = match value {
        S::Null => T::UntypedNull,
        S::Int(_) => T::Int,
        S::BigInt(_) => T::BigInt,
        S::Bit(_) => T::Bit,
        S::Decimal(_) => T::Decimal,
        S::Float(_) => T::Float,
        _ => T::NVarChar,
    };
    m(
        [T::NVarChar, T::VarChar, kind],
        Some(input),
        Some(path),
        value,
    )
}
fn modified(input: &str, path: &str, value: Scalar<'_>) -> String {
    text(modify(Some(input), Some(path), &value))
}

/// Captured scalar-family operands named by the case suffix.
fn scalar(kind: &str) -> Scalar<'static> {
    match kind {
        "tinyint" => S::TinyInt(255),
        "smallint" => S::SmallInt(-32768),
        "int" => S::Int(-2147483648),
        "bigint" => S::BigInt(9223372036854775807),
        "bit true" => S::Bit(true),
        "bit false" => S::Bit(false),
        "decimal" => dec(10, 3, -12340),
        "decimal zero scale" => dec(38, 0, 12345678901234567890),
        "decimal small" => dec(10, 5, 1),
        "numeric" => dec(5, 2, 150),
        "money" => S::Money(123456),
        "smallmoney" => S::SmallMoney(-15000),
        "float" => S::Float(0.1),
        "float large" => S::Float(1e300),
        "float integral" => S::Float(3.0),
        "float negative zero" => S::Float(-0.0),
        "real" => S::Real(0.1),
        "date" => S::Date(dt("2024-01-02")),
        "time" => S::Time(dt("03:04:05.1234567"), 7),
        "datetime" => S::DateTime(dt("2024-01-02T03:04:05.123")),
        "smalldatetime" => S::SmallDateTime(dt("2024-01-02T03:04:00")),
        "datetime2" => S::DateTime2(dt("2024-01-02T03:04:05.1234567"), 7),
        "datetime2 zero scale" => S::DateTime2(dt("2024-01-02T03:04:05"), 0),
        "datetimeoffset" => S::DateTimeOffset(
            DateTimeOffset::parse_iso("2024-01-02T03:04:05.1+05:30").unwrap(),
            1,
        ),
        "uniqueidentifier" => S::UniqueIdentifier(guid("6F9619FF-8B86-D011-B42D-00C04FC964FF")),
        "char padded" => S::Char("ab", 5),
        "nchar padded" => S::NChar("ab", 5),
        "varchar" | "nvarchar" | "varchar max" | "nvarchar max" => S::Text("ab"),
        "binary" => S::Binary(&[1, 2]),
        "varbinary" | "varbinary max" => S::Binary(b"ABC"),
        "rowversion-like binary(8)" => S::Binary(&[0, 0, 0, 0, 0, 0, 7, 0xd1]),
        "xml" => S::Xml("<a>1</a>"),
        "sql_variant" => S::Variant(&S::Int(1)),
        "hierarchyid" | "geometry" => S::Clr,
        "typed null int" | "untyped null" => S::Null,
        _ => panic!("unmapped scalar case {kind}"),
    }
}

/// The retained source table, before the UPDATE case.
/// `(id, txt, ansi, doc)` of `dbo.json_ctor_src`.
type SourceRow = (
    i32,
    Option<&'static str>,
    Option<&'static str>,
    Option<&'static str>,
);
const SOURCE: [SourceRow; 3] = [
    (1, Some("a\"1"), Some("x"), Some("{\"list\":[]}")),
    (2, None, None, Some("{\"v\":0}")),
    (3, Some("k3"), Some("y"), None),
];
fn opt(value: Option<&str>) -> Scalar<'_> {
    value.map_or(S::Null, S::Text)
}

fn rows(case: &Value) -> &Vec<Value> {
    case["result"]["sets"].as_array().unwrap().last().unwrap()["rows"]
        .as_array()
        .unwrap()
}
fn describe(case: &Value, expected: &[(&str, ResultType)]) -> Replay {
    let rows = rows(case);
    assert_eq!(rows.len(), expected.len());
    for (row, (name, result)) in rows.iter().zip(expected) {
        assert_eq!(row[2], *name);
        assert_eq!(row[5], result.system_type_name());
        assert_eq!(row[6], -1);
        assert_eq!(row[3], true);
        assert_eq!(
            row[9],
            result.fixed_collation().unwrap_or(DATABASE_COLLATION)
        );
    }
    Replay::Checked
}
fn scalar_row(case: &Value) -> Vec<Value> {
    rows(case)[0].as_array().unwrap().clone()
}
fn utf16_len(text: &str) -> usize {
    text.encode_utf16().count()
}

fn replay(case: &Value) -> Replay {
    let name = case["name"].as_str().unwrap();
    if let Some(kind) = name.strip_prefix("object scalar ") {
        return one(o(&[(S::Text("v"), scalar(kind))], &[]));
    }
    if let Some(kind) = name.strip_prefix("array scalar ") {
        return one(a(&[scalar(kind)], &[]));
    }
    let x = S::Text("x");
    match name {
        "create source" | "insert source" => Replay::NotApplicable("fixture setup"),
        "object comma pair syntax"
        | "object missing value"
        | "object trailing comma"
        | "array trailing comma" => Replay::NotApplicable("parser syntax error"),
        "object json_query invalid" => Replay::NotApplicable("JSON_QUERY's own 13609 state 1"),

        "object empty" => one(o(&[], &[])),
        "object basic" => one(o(&[(S::Text("a"), S::Int(1)), (S::Text("b"), x)], &[])),
        "object unicode key literal" => one(o(&[(S::Text("a"), x)], &[])),
        "object null default" | "object null on null" | "object absent on null" => {
            let written: &[NullClause] = match name {
                "object null on null" => &[NullOnNull],
                "object absent on null" => &[AbsentOnNull],
                _ => &[],
            };
            one(o(
                &[(S::Text("a"), S::Null), (S::Text("b"), S::Int(1))],
                written,
            ))
        }
        "object absent all null" | "object absent typed null" => {
            one(o(&[(S::Text("a"), S::Null)], &[AbsentOnNull]))
        }
        "object null key" | "object typed null key" => one(o(&[(S::Null, S::Int(1))], &[])),
        "object integer key" => one(o(&[(S::Int(1), S::Int(2))], &[])),
        "object decimal key" => one(o(&[(dec(2, 1, 15), S::Int(2))], &[])),
        "object date key" => one(o(&[(S::Date(dt("2024-01-02")), S::Int(2))], &[])),
        "object binary key" => one(o(&[(S::Binary(b"A"), S::Int(2))], &[])),
        "object empty key" => one(o(&[(S::Text(""), S::Int(1))], &[])),
        "object padded char key" => one(o(&[(S::Char("k", 3), S::Int(1))], &[])),
        "object key escaping" => one(o(&[(S::Text(PROBE), S::Int(1))], &[])),
        "object value escaping" => one(o(&[(S::Text("v"), S::Text(PROBE))], &[])),
        "object ansi value escaping" => one(o(&[(S::Text("v"), S::Text("a\"b\\c\té"))], &[])),
        "object duplicate keys" => one(o(
            &[(S::Text("a"), S::Int(1)), (S::Text("a"), S::Int(2))],
            &[],
        )),
        "object duplicate keys absent" => one(o(
            &[(S::Text("a"), S::Null), (S::Text("a"), S::Int(2))],
            &[AbsentOnNull],
        )),
        "object nested" => {
            let inner = text(o(&[(S::Text("x"), S::Int(1))], &[]));
            let list = text(a(&[S::Int(1), S::Int(2)], &[]));
            one(o(
                &[
                    (S::Text("o"), S::Json(&inner)),
                    (S::Text("a"), S::Json(&list)),
                ],
                &[],
            ))
        }
        "object json_query input" => one(o(&[(S::Text("q"), S::Json("{\"x\":1}"))], &[])),
        "object json_query array input" => one(o(&[(S::Text("q"), S::Json("[1,2]"))], &[])),
        "object json_query null" => one(o(&[(S::Text("q"), S::Null)], &[])),
        "object json_query null absent" => one(o(&[(S::Text("q"), S::Null)], &[AbsentOnNull])),
        "object json text as string" => one(o(&[(S::Text("q"), S::Text("{\"x\":1}"))], &[])),
        "object json_value input" => one(o(&[(S::Text("q"), S::Text("1"))], &[])),
        "object json_modify input" => {
            let inner = modified("{\"x\":1}", "$.x", S::Int(2));
            one(o(&[(S::Text("q"), S::Json(&inner))], &[]))
        }
        "object many pairs" => {
            let keys = ["a", "b", "c", "d", "e", "f", "g", "h", "i", "j"];
            let pairs: Vec<_> = (1..)
                .zip(keys)
                .map(|(v, k)| (S::Text(k), S::Int(v)))
                .collect();
            one(o(&pairs, &[]))
        }
        "object long value" => {
            let long = "x".repeat(10000);
            let out = text(o(&[(S::Text("a"), S::Text(&long))], &[]));
            assert_eq!(
                scalar_row(case),
                [
                    (utf16_len(&out) * 2).to_string(),
                    utf16_len(&out).to_string()
                ]
            );
            Replay::Checked
        }
        "object long bounded value" => {
            let (x, y) = ("x".repeat(4000), "y".repeat(4000));
            let out = text(o(
                &[(S::Text("a"), S::Text(&x)), (S::Text("b"), S::Text(&y))],
                &[],
            ));
            assert_eq!(scalar_row(case), [(utf16_len(&out) * 2).to_string()]);
            Replay::Checked
        }
        "object isjson" | "array isjson" => {
            let out = if name == "object isjson" {
                text(o(&[(S::Text("a"), S::Int(1))], &[]))
            } else {
                text(a(&[S::Int(1), S::Null], &[]))
            };
            assert_eq!(
                scalar_row(case),
                [i32::from(json::valid(out.as_bytes(), 0))]
            );
            Replay::Checked
        }
        "object absent and null both" => {
            one(o(&[(S::Text("a"), S::Int(1))], &[NullOnNull, AbsentOnNull]))
        }
        "object returning json" => one_typed(
            constructor_result(true, &[T::VarChar, T::Int]),
            o(&[(S::Text("a"), S::Int(1))], &[]),
        ),
        "array returning json" => {
            one_typed(constructor_result(true, &[T::Int]), a(&[S::Int(1)], &[]))
        }
        "returning json describe" => {
            let result = constructor_result(true, &[T::Int]);
            describe(case, &[("o", result), ("a", result)])
        }
        "object json type value" => one_typed(
            constructor_result(false, &[T::VarChar, T::Json]),
            o(&[(S::Text("j"), S::Json("{\"x\":1}"))], &[]),
        ),
        "array json type value" => one_typed(
            constructor_result(false, &[T::Json]),
            a(&[S::Json("[1]")], &[]),
        ),
        "modify json type input" => m(
            [T::Json, T::VarChar, T::Int],
            Some("{\"a\":1}"),
            Some("$.a"),
            S::Int(2),
        ),
        "object describe" => describe(
            case,
            &[
                ("v", constructor_result(false, &[T::VarChar, T::Int])),
                ("e", constructor_result(false, &[])),
                ("s", constructor_result(false, &[T::VarChar, T::VarChar])),
            ],
        ),
        "array describe" => describe(
            case,
            &[
                ("v", constructor_result(false, &[T::Int])),
                ("e", constructor_result(false, &[])),
            ],
        ),
        "modify describe" => {
            let result = |input| modify_signature(&[input, T::VarChar, T::Int]).unwrap();
            describe(
                case,
                &[
                    ("n", result(T::NVarChar)),
                    ("v", result(T::VarChar)),
                    ("m", result(T::NVarChar)),
                ],
            )
        }
        "object select into" => {
            let results = [
                constructor_result(false, &[T::VarChar, T::Int]),
                constructor_result(false, &[T::Int]),
                modify_signature(&[T::VarChar, T::VarChar, T::Int]).unwrap(),
            ];
            let rows = rows(case);
            for ((row, result), name) in rows.iter().zip(results).zip(["o", "a", "m"]) {
                let declared = format!("{}(max)", row[1].as_str().unwrap());
                assert_eq!(row[0], name);
                assert_eq!(declared, result.system_type_name());
                assert_eq!(row[2], -1);
                assert_eq!(row[3], true);
                assert_eq!(row[4], DATABASE_COLLATION);
            }
            assert_eq!(rows.len(), 3);
            Replay::Checked
        }
        "object column source" => column(SOURCE.iter().map(|&(id, txt, ansi, _)| {
            o(
                &[
                    (S::Text("id"), S::Int(id)),
                    (S::Text("txt"), opt(txt)),
                    (S::Text("ansi"), opt(ansi)),
                ],
                &[],
            )
        })),
        "object column source absent" => column(
            SOURCE
                .iter()
                .map(|&(_, txt, _, _)| o(&[(S::Text("txt"), opt(txt))], &[AbsentOnNull])),
        ),
        "object column keys" | "object column null key" => {
            let filtered = name == "object column keys";
            column(
                SOURCE
                    .iter()
                    .filter(|row| !filtered || row.1.is_some())
                    .map(|&(id, txt, _, _)| o(&[(opt(txt), S::Int(id))], &[])),
            )
        }

        "array empty" => one(a(&[], &[])),
        "array basic" => one(a(&[S::Int(1), x, S::Text("y")], &[])),
        "array null default" | "array null on null" | "array absent on null" => {
            let written: &[NullClause] = match name {
                "array null on null" => &[NullOnNull],
                "array absent on null" => &[AbsentOnNull],
                _ => &[],
            };
            one(a(&[S::Int(1), S::Null, S::Int(2)], written))
        }
        "array all null default" => one(a(&[S::Null, S::Null], &[])),
        "array value escaping" => one(a(&[S::Text(PROBE)], &[])),
        "array nested" => {
            let pair = text(a(&[S::Int(1), S::Int(2)], &[]));
            let inner = text(o(&[(S::Text("a"), S::Int(1))], &[]));
            let empty = text(a(&[], &[]));
            one(a(&[S::Json(&pair), S::Json(&inner), S::Json(&empty)], &[]))
        }
        "array json_query input" => one(a(&[S::Json("[1,{\"a\":2}]")], &[])),
        "array json text as string" => one(a(&[S::Text("[1,2]")], &[])),
        "array column source" => column(
            SOURCE
                .iter()
                .map(|&(id, txt, ansi, _)| a(&[S::Int(id), opt(txt), opt(ansi)], &[])),
        ),
        "array column source null on null" => column(
            SOURCE
                .iter()
                .map(|&(id, txt, _, _)| a(&[S::Int(id), opt(txt)], &[NullOnNull])),
        ),
        "array long value" => {
            let long = "x".repeat(10000);
            let out = text(a(&[S::Text(&long)], &[]));
            assert_eq!(scalar_row(case), [(utf16_len(&out) * 2).to_string()]);
            Replay::Checked
        }

        "modify replace number" | "modify nvarchar max input" => mn("{\"a\":1}", "$.a", S::Int(2)),
        "modify replace string" => mn("{\"a\":1}", "$.a", x),
        "modify insert lax" | "modify variable path" | "modify expression path" => {
            mn("{\"a\":1}", "$.b", S::Int(2))
        }
        "modify insert explicit lax" => mn("{\"a\":1}", "lax $.b", S::Int(2)),
        "modify insert strict missing" => mn("{\"a\":1}", "strict $.b", S::Int(2)),
        "modify replace strict" => mn("{\"a\":1}", "strict $.a", S::Int(2)),
        "modify delete lax null" => mn("{\"a\":1,\"b\":2}", "$.a", S::Null),
        "modify delete missing lax" => mn("{\"a\":1}", "$.z", S::Null),
        "modify strict null sets null" => mn("{\"a\":1}", "strict $.a", S::Null),
        "modify strict null missing" => mn("{\"a\":1}", "strict $.z", S::Null),
        "modify append" => mn("{\"arr\":[1,2]}", "append $.arr", S::Int(3)),
        "modify append string" => mn("{\"arr\":[]}", "append $.arr", x),
        "modify append missing lax" => mn("{\"a\":1}", "append $.arr", S::Int(3)),
        "modify append missing strict" => mn("{\"a\":1}", "append strict $.arr", S::Int(3)),
        "modify append lax keyword order" => mn("{\"a\":1}", "append lax $.arr", S::Int(3)),
        "modify append non-array lax" => mn("{\"a\":1}", "append $.a", S::Int(3)),
        "modify append non-array strict" => mn("{\"a\":1}", "append strict $.a", S::Int(3)),
        "modify append null" => mn("{\"arr\":[1]}", "append $.arr", S::Null),
        "modify append json_query" => mn("{\"arr\":[1]}", "append $.arr", S::Json("{\"x\":1}")),
        "modify array index" => mn("{\"arr\":[1,2,3]}", "$.arr[1]", S::Int(9)),
        "modify array index out of range lax" => mn("{\"arr\":[1]}", "$.arr[5]", S::Int(9)),
        "modify array index out of range strict" => {
            mn("{\"arr\":[1]}", "strict $.arr[5]", S::Int(9))
        }
        "modify array index null" => mn("{\"arr\":[1,2,3]}", "$.arr[1]", S::Null),
        "modify root array element" => mn("[1,2]", "$[0]", x),
        "modify root" => mn("{\"a\":1}", "$", x),
        "modify nested existing" => mn("{\"o\":{\"x\":1}}", "$.o.x", S::Int(2)),
        "modify nested missing parent lax" => mn("{\"a\":1}", "$.o.x", S::Int(2)),
        "modify nested missing parent strict" => mn("{\"a\":1}", "strict $.o.x", S::Int(2)),
        "modify quoted key" => mn("{\"a b\":1}", "$.\"a b\"", S::Int(2)),
        "modify insert quoted key" => mn("{}", "$.\"a\\\"b\"", S::Int(2)),
        "modify duplicate keys" => mn("{\"a\":1,\"a\":2}", "$.a", S::Int(3)),
        "modify duplicate keys delete" => mn("{\"a\":1,\"a\":2}", "$.a", S::Null),
        "modify json_query value" => mn("{\"a\":1}", "$.a", S::Json("{\"x\":[1]}")),
        "modify json text as string" => mn("{\"a\":1}", "$.a", S::Text("{\"x\":1}")),
        "modify json_object value" => {
            let value = text(o(&[(S::Text("x"), S::Int(1))], &[]));
            mn("{\"a\":1}", "$.a", S::Json(&value))
        }
        "modify json_array value" => {
            let value = text(a(&[S::Int(1), S::Int(2)], &[]));
            mn("{\"a\":1}", "$.a", S::Json(&value))
        }
        "modify value escaping" => mn("{}", "$.v", S::Text(PROBE)),
        "modify preserves whitespace" => mn("{ \"a\" : 1 ,  \"b\" : [ 1 ] }", "$.a", S::Int(2)),
        "modify int" => mn("{}", "$.v", S::Int(7)),
        "modify bigint" => mn("{}", "$.v", S::BigInt(9223372036854775807)),
        "modify bit" => mn("{}", "$.v", S::Bit(true)),
        "modify decimal" => mn("{}", "$.v", dec(10, 3, -12340)),
        "modify float" => mn("{}", "$.v", S::Float(0.1)),
        "modify money"
        | "modify date"
        | "modify datetime2"
        | "modify uniqueidentifier"
        | "modify varbinary"
        | "modify xml" => {
            let value = match name {
                "modify money" => T::Money,
                "modify date" => T::Date,
                "modify datetime2" => T::DateTime2,
                "modify uniqueidentifier" => T::UniqueIdentifier,
                "modify varbinary" => T::VarBinary,
                _ => T::Xml,
            };
            one(modify_signature(&[T::NVarChar, T::VarChar, value]).map(|_| None))
        }
        "modify null input" => m(
            [T::NVarChar, T::VarChar, T::Int],
            None,
            Some("$.a"),
            S::Int(1),
        ),
        "modify untyped null input" => m(
            [T::UntypedNull, T::VarChar, T::Int],
            None,
            Some("$.a"),
            S::Int(1),
        ),
        "modify null path" => m(
            [T::NVarChar, T::UntypedNull, T::Int],
            Some("{\"a\":1}"),
            None,
            S::Int(1),
        ),
        "modify typed null path" => m(
            [T::NVarChar, T::NVarChar, T::Int],
            Some("{\"a\":1}"),
            None,
            S::Int(1),
        ),
        "modify invalid json" => mn("not json", "$.a", S::Int(1)),
        "modify empty json" => mn("", "$.a", S::Int(1)),
        "modify scalar json" => mn("1", "$.a", S::Int(1)),
        "modify truncated json" => mn("{\"a\":1", "$.a", S::Int(2)),
        "modify invalid path no dollar" => mn("{\"a\":1}", "a", S::Int(2)),
        "modify invalid path empty" => mn("{\"a\":1}", "", S::Int(2)),
        "modify invalid path trailing dot" => mn("{\"a\":1}", "$.", S::Int(2)),
        "modify invalid path bad mode" => mn("{\"a\":1}", "loose $.a", S::Int(2)),
        "modify invalid path wildcard" => mn("{\"a\":1}", "$.*", S::Int(2)),
        "modify invalid path bad index" => mn("{\"a\":[1]}", "$.a[x]", S::Int(2)),
        "modify invalid path negative index" => mn("{\"a\":[1]}", "$.a[-1]", S::Int(2)),
        "modify path through scalar lax" => mn("{\"a\":1}", "$.a.b", S::Int(2)),
        "modify path through scalar strict" => mn("{\"a\":1}", "strict $.a.b", S::Int(2)),
        "modify ansi input" => m(
            [T::VarChar, T::VarChar, T::NVarChar],
            Some("{\"a\":1}"),
            Some("$.a"),
            S::Text("xé"),
        ),
        "modify ansi growth" => {
            let long = "x".repeat(50);
            m(
                [T::VarChar, T::VarChar, T::VarChar],
                Some("{\"a\":1}"),
                Some("$.a"),
                S::Text(&long),
            )
        }
        "modify integer input" => {
            one(modify_signature(&[T::Int, T::VarChar, T::Int]).map(|_| None))
        }
        "modify integer path" => {
            one(modify_signature(&[T::NVarChar, T::Int, T::Int]).map(|_| None))
        }
        "modify rename key" => {
            let inner = modified("{\"a\":1}", "$.b", S::Text("1"));
            mn(&inner, "$.a", S::Null)
        }
        "modify too few arguments" => {
            one(modify_signature(&[T::NVarChar, T::VarChar]).map(|_| None))
        }
        "modify column source" => column(
            SOURCE
                .iter()
                .map(|&(_, txt, _, doc)| modify(doc, Some("$.v"), &opt(txt))),
        ),
        "modify update statement" => {
            // The SELECT after the UPDATE reads the stored NVARCHAR(200) column.
            let rows = rows(case);
            assert_eq!(rows.len(), SOURCE.len());
            for (row, &(id, _, _, doc)) in rows.iter().zip(&SOURCE) {
                let updated = modify(doc, Some("append $.list"), &S::Int(id)).unwrap();
                assert_eq!(row[0], id);
                assert_eq!(row[1].as_str(), updated.as_deref());
            }
            Replay::Checked
        }

        "bound object values" => one(o(
            &[
                (S::Text("s"), S::Text("a\"b")),
                (S::Text("n"), S::Int(7)),
                (S::Text("b"), S::Bit(true)),
                (S::Text("d"), dec(10, 3, 1500)),
            ],
            &[],
        )),
        "bound object nulls" => {
            let pairs = [(S::Text("s"), S::Null), (S::Text("n"), S::Null)];
            Replay::Values(vec![
                (0, nv(), vec![o(&pairs, &[])]),
                (1, nv(), vec![o(&pairs, &[AbsentOnNull])]),
            ])
        }
        "bound object key" | "bound object ansi key" => one(o(&[(S::Text("key"), S::Int(1))], &[])),
        "bound object null key" => one(o(&[(S::Null, S::Int(1))], &[])),
        "bound object max value" => one(o(&[(S::Text("v"), S::Text("max"))], &[])),
        "bound object json text" => Replay::Values(vec![
            (
                0,
                nv(),
                vec![o(&[(S::Text("v"), S::Text("{\"x\":1}"))], &[])],
            ),
            (
                1,
                nv(),
                vec![o(&[(S::Text("v"), S::Json("{\"x\":1}"))], &[])],
            ),
        ]),
        "bound array values" => one(a(
            &[
                x,
                S::BigInt(9007199254740993),
                S::Float(0.5),
                S::DateTime2(dt("2024-01-02T03:04:05.123"), 3),
            ],
            &[],
        )),
        "bound array nulls" => Replay::Values(vec![
            (0, nv(), vec![a(&[S::Null, S::Null], &[])]),
            (1, nv(), vec![a(&[S::Null, S::Null], &[NullOnNull])]),
        ]),
        _ if name.starts_with("bound modify") => bound_modify(case),
        _ => panic!("unmapped fixture case {name}"),
    }
}

fn column(outcomes: impl Iterator<Item = Outcome>) -> Replay {
    let mut result = Vec::new();
    for outcome in outcomes {
        let stop = outcome.is_err();
        result.push(outcome);
        if stop {
            break;
        }
    }
    Replay::Values(vec![(1, nv(), result)])
}

/// RPC JSON_MODIFY(@doc,@path,@value) replayed from the retained parameters.
fn bound_modify(case: &Value) -> Replay {
    let parameters = case["parameters"].as_array().unwrap();
    let parameter = |name: &str| {
        let p = parameters.iter().find(|p| p["name"] == name).unwrap();
        let kind = match p["type"].as_str().unwrap() {
            "NVarChar" => T::NVarChar,
            "VarChar" => T::VarChar,
            "Int" => T::Int,
            other => panic!("unmapped parameter type {other}"),
        };
        (kind, &p["value"])
    };
    let (doc_type, doc) = parameter("doc");
    let (path_type, path) = parameter("path");
    let (value_type, value) = parameter("value");
    let value = match value {
        Value::Null => S::Null,
        Value::String(text) => S::Text(text),
        Value::Number(n) => S::Int(n.as_i64().unwrap().try_into().unwrap()),
        _ => panic!("unmapped parameter value"),
    };
    m(
        [doc_type, path_type, value_type],
        doc.as_str(),
        path.as_str(),
        value,
    )
}

fn check_descriptor(case: &Value, index: usize, result: ResultType) {
    let column = &case["result"]["sets"].as_array().unwrap().last().unwrap()["columns"][index];
    assert_eq!(column["length"], 65535, "{}", case["name"]);
    assert_eq!(column["flags"], 33);
    let collation = &column["collation"];
    match result {
        ResultType::NVarCharMax => {
            assert_eq!(column["type"], "NVarChar", "{}", case["name"]);
            assert_eq!(collation["sortId"], 52);
        }
        ResultType::Json => {
            assert_eq!(column["type"], "VarChar", "{}", case["name"]);
            assert_eq!(collation["codepage"], "utf-8");
        }
    }
}

fn check_error(case: &Value, error: &Error) {
    let name = &case["name"];
    let (compile, error): (bool, &SqlError) = match error {
        Error::Compile(error) => (true, error),
        Error::Runtime(error) => (false, error),
        Error::Unsupported(what) => panic!("{name}: unsupported {what}"),
    };
    let errors = case["result"]["errors"].as_array().unwrap();
    assert_eq!(errors.len(), 1, "{name}");
    let expected = &errors[0];
    assert_eq!(expected["number"], error.number, "{name}");
    assert_eq!(expected["state"], error.state, "{name}");
    assert_eq!(expected["class"], error.severity, "{name}");
    assert_eq!(expected["message"], error.message.as_str(), "{name}");
    let sets = case["result"]["sets"].as_array().unwrap().len();
    assert_eq!(sets, usize::from(!compile), "{name}: descriptor timing");
    assert_eq!(case["result"]["done"][0]["rowCount"], Value::Null, "{name}");
}

fn check(case: &Value, values: Vec<(usize, ResultType, Vec<Outcome>)>) {
    let name = &case["name"];
    let sets = case["result"]["sets"].as_array().unwrap();
    for (column, result, outcomes) in values {
        let (rows, error) = match outcomes.split_last() {
            Some((Err(error), rows)) => (rows, Some(error)),
            _ => (&outcomes[..], None),
        };
        match error {
            Some(error) => check_error(case, error),
            None => assert!(case["result"]["errors"].as_array().unwrap().is_empty()),
        }
        if sets.is_empty() {
            assert!(rows.is_empty(), "{name}");
            continue;
        }
        check_descriptor(case, column, result);
        let captured = self::rows(case);
        assert_eq!(captured.len(), rows.len(), "{name}: row count");
        for (row, outcome) in captured.iter().zip(rows) {
            let got = outcome.as_ref().unwrap_or_else(|e| panic!("{name}: {e:?}"));
            let expected = row[column].as_str();
            assert_eq!(got.as_deref(), expected, "{name}");
            assert!(expected.is_some() || row[column].is_null(), "{name}");
        }
    }
}

#[test]
fn replays_every_applicable_case_in_every_retained_run() {
    let fixture = fixture();
    let runs: Vec<&Value> = fixture["containers"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|container| container["runs"].as_array().unwrap())
        .collect();
    assert_eq!(runs.len(), 4);
    for run in runs {
        let (mut values, mut checked, mut skipped) = (0, 0, Vec::new());
        for case in run.as_array().unwrap() {
            match replay(case) {
                Replay::Values(values_for_case) => {
                    check(case, values_for_case);
                    values += 1;
                }
                Replay::Checked => checked += 1,
                Replay::NotApplicable(reason) => {
                    skipped.push((case["name"].as_str().unwrap(), reason));
                }
            }
        }
        assert_eq!((values, checked), (229, 11));
        assert_eq!(
            skipped,
            [
                ("create source", "fixture setup"),
                ("insert source", "fixture setup"),
                ("object comma pair syntax", "parser syntax error"),
                (
                    "object json_query invalid",
                    "JSON_QUERY's own 13609 state 1"
                ),
                ("object missing value", "parser syntax error"),
                ("object trailing comma", "parser syntax error"),
                ("array trailing comma", "parser syntax error"),
            ]
        );
    }
}

#[test]
fn uncaptured_behavior_is_explicitly_unsupported() {
    let unsupported = |outcome: Outcome| assert!(matches!(outcome, Err(Error::Unsupported(_))));
    unsupported(o(&[(S::Bit(true), S::Int(1))], &[]));
    unsupported(o(&[(S::Clr, S::Int(1))], &[]));
    unsupported(o(&[(S::Text("v"), S::Variant(&S::Bit(true)))], &[]));
    unsupported(o(&[(S::Text("v"), S::Char("é", 3))], &[]));
    unsupported(o(&[(S::Text("v"), S::NChar("abcd", 3))], &[]));
    unsupported(o(&[(S::Text("v"), S::Json("{"))], &[]));
    unsupported(a(&[S::Int(1)], &[NullOnNull, AbsentOnNull]));
    unsupported(o(&[], &[NullOnNull, NullOnNull]));
    unsupported(modify_signature(&[T::NVarChar, T::VarChar, T::Int, T::Int]).map(|_| None));
    unsupported(modify_signature(&[T::Char, T::VarChar, T::Int]).map(|_| None));
    unsupported(modify_signature(&[T::NVarChar, T::Xml, T::Int]).map(|_| None));
    unsupported(modify_signature(&[T::NVarChar, T::VarChar, T::Real]).map(|_| None));
    for (doc, path, value) in [
        ("{\"a\":1}", "STRICT $.a", S::Int(1)),
        ("{\"a\":1}", "strict a", S::Int(1)),
        ("{\"a\":1}", "strict $", S::Int(1)),
        ("{\"a\":1}", "strict $.", S::Int(1)),
        ("{\"a\":1}", " $.a", S::Int(1)),
        ("{\"a\":1}", "$.a b", S::Int(1)),
        ("{\"a\":1}", "$[*]", S::Int(1)),
        ("{\"a\":1}", "$.a[01]", S::Int(1)),
        ("{\"a\":1}", "$.a[1", S::Int(1)),
        ("{\"a\":1}", "$.\"a", S::Int(1)),
        ("{\"a\":1}", "$[0]", S::Int(1)),
        ("[1]", "$.a", S::Int(1)),
        ("{\"a\":1}", "$.a", S::Money(1)),
        ("{\"a\":x}", "$.a", S::Int(1)),
        ("  x", "$.a", S::Int(1)),
        (" ", "$.a", S::Int(1)),
        ("x", "a", S::Int(1)),
        ("{\"a\":[1]}", "strict $.a[0]", S::Null),
        ("{\"a\":[1]}", "$.a[3]", S::Null),
        ("{\"a\":1}", "append $.b.c", S::Int(1)),
        ("{\"a\":1}", "append $.a.c", S::Int(1)),
        ("{\"a\":1}", "append $.b", S::Null),
    ] {
        unsupported(modify(Some(doc), Some(path), &value));
    }
}

#[test]
fn modify_spans_nested_whitespace_and_container_edges() {
    let cases = [
        ("  {\"a\":1}", "$.a", S::Int(2), "  {\"a\":2}"),
        ("{\"a\":1,\"b\":2}", "$.b", S::Null, "{\"a\":1}"),
        (
            "{\"a\":1,\"b\":2,\"c\":3}",
            "$.b",
            S::Null,
            "{\"a\":1,\"c\":3}",
        ),
        (
            "{\"o\":{\"x\":[1,{\"y\":2}]}}",
            "$.o.x[1].y",
            S::Text("é/"),
            "{\"o\":{\"x\":[1,{\"y\":\"é\\/\"}]}}",
        ),
        ("{\"a\":[]}", "strict $.a[0]", S::Int(1), ""),
        ("{\"\\u0061\":1}", "$.a", S::Int(2), "{\"\\u0061\":2}"),
        (
            "{\"a\":\"x]}\"}",
            "append $.b",
            S::Int(1),
            "{\"a\":\"x]}\",\"b\":[1]}",
        ),
        ("[[1],[2]]", "append $[1]", S::Int(3), "[[1],[2,3]]"),
        ("{\"a\":1}", "$.é", S::Int(2), "{\"a\":1,\"é\":2}"),
    ];
    for (doc, path, value, expected) in cases {
        let outcome = modify(Some(doc), Some(path), &value);
        if expected.is_empty() {
            assert!(matches!(outcome, Err(Error::Runtime(ref e)) if e.number == 13608));
        } else {
            assert_eq!(outcome.unwrap().as_deref(), Some(expected), "{doc} {path}");
        }
    }
    for truncated in ["{\"a\":tr", "[1,", "{\"a\":\"x\\u00", "{\"a\":-", "[1e"] {
        let Err(Error::Runtime(error)) = modify(Some(truncated), Some("$.a"), &S::Int(1)) else {
            panic!("{truncated}");
        };
        assert_eq!(
            error.message,
            format!(
                "JSON text is not properly formatted. Unexpected character '.' is found at position {}.",
                truncated.len()
            )
        );
    }
}

#[test]
fn modify_signature_partitions_every_declared_type() {
    let all = [
        T::UntypedNull,
        T::TinyInt,
        T::SmallInt,
        T::Int,
        T::BigInt,
        T::Bit,
        T::Decimal,
        T::Numeric,
        T::Money,
        T::SmallMoney,
        T::Float,
        T::Real,
        T::Date,
        T::Time,
        T::DateTime,
        T::SmallDateTime,
        T::DateTime2,
        T::DateTimeOffset,
        T::UniqueIdentifier,
        T::Char,
        T::VarChar,
        T::NChar,
        T::NVarChar,
        T::Binary,
        T::VarBinary,
        T::Xml,
        T::Json,
        T::SqlVariant,
        T::Clr,
    ];
    let classify = |types: [SqlType; 3]| match modify_signature(&types) {
        Ok(_) => 'a',
        Err(Error::Compile(error)) if error.number == 8116 => 'r',
        Err(Error::Unsupported(_)) => 'u',
        Err(error) => panic!("{error:?}"),
    };
    let inputs: String = all
        .iter()
        .map(|&t| classify([t, T::VarChar, T::Int]))
        .collect();
    let paths: String = all
        .iter()
        .map(|&t| classify([T::NVarChar, t, T::Int]))
        .collect();
    let values: String = all
        .iter()
        .map(|&t| classify([T::NVarChar, T::VarChar, t]))
        .collect();
    assert_eq!(inputs, "auuruuuuuuuuuuuuuuuuauauuuauu");
    assert_eq!(paths, "ruuruuuuuuuuuuuuuuuuauauuuuuu");
    assert_eq!(values, "auuaaaauruauruuururuauaurrauu");
    assert_eq!(constructor_result(false, &all), ResultType::Json);
    assert_eq!(
        constructor_result(false, &all[..26]),
        ResultType::NVarCharMax
    );
    assert_eq!(constructor_result(true, &[]), ResultType::Json);
}
