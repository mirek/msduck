// Compile the pure module here until a separately claimed core export wires it.
#[path = "../src/charindex.rs"]
mod charindex;

use std::collections::BTreeSet;

use charindex::ArgType::{self, *};
use charindex::{
    Collation, Evaluation, Operand, Rejection, ResultType, ServerError, Start, StartArgument,
    resolve_charindex, resolve_patindex,
};
use serde_json::Value;

const REFERENCE: &str = include_str!("../../../reference/charindex-patindex.json");

/// Setup, environment and forms owned by the SQL binding successor.
const NOT_APPLICABLE: [&str; 9] = [
    "create source",
    "insert source",
    "create not null source",
    "insert not null source",
    "environment",
    "reuse",
    // Collation precedence and error 468 belong to charindex-patindex-sql-v1.
    "charindex collation conflict",
    "patindex collation conflict",
    // The ESCAPE syntax error 156 is a parser rule.
    "patindex escape clause rejected",
];

/// Captured results the core deliberately does not model: expansions,
/// ignorables and the unexplained surrogate results of the default collation.
const UNSUPPORTED: [&str; 13] = [
    "charindex expansion ss",
    "charindex ignorable character",
    "charindex after surrogate pair",
    "charindex find surrogate pair",
    "charindex find high surrogate",
    "charindex find high surrogate sc",
    "patindex after surrogate pair",
    "patindex underscore surrogate",
    "patindex two underscores surrogate",
    "patindex find surrogate pair",
    "rpc charindex surrogate",
    "rpc patindex surrogate underscore",
    "prepared patindex nvarchar#2",
];

#[derive(Clone, Debug)]
enum V {
    Nul,
    Str(Vec<u16>),
    Bytes(Vec<u8>),
    Integer(i64),
    Dec(i128, u8),
}

fn s(text: &str) -> V {
    V::Str(text.encode_utf16().collect())
}

fn units(units: &[u16]) -> V {
    V::Str(units.to_vec())
}

fn repeated(unit: &str, count: usize, tail: &str) -> V {
    s(&(unit.repeat(count) + tail))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Function {
    Charindex,
    Patindex,
}

#[derive(Clone, Debug)]
struct Call {
    function: Function,
    types: Vec<ArgType>,
    values: Vec<V>,
    collation: Collation,
}

impl Call {
    fn under(mut self, name: &str) -> Self {
        self.collation = Collation::from_name(name).unwrap();
        self
    }
}

fn ci(types: &[ArgType], values: Vec<V>) -> Call {
    Call {
        function: Function::Charindex,
        types: types.to_vec(),
        values,
        collation: Collation::DEFAULT,
    }
}

fn pi(types: &[ArgType], values: Vec<V>) -> Call {
    Call {
        function: Function::Patindex,
        types: types.to_vec(),
        values,
        collation: Collation::DEFAULT,
    }
}

/// Types only: the call is rejected before any value is evaluated.
fn ci_types(types: &[ArgType]) -> Call {
    ci(types, vec![V::Nul; types.len()])
}

fn pi_types(types: &[ArgType]) -> Call {
    pi(types, vec![V::Nul; types.len()])
}

#[derive(Debug, PartialEq)]
enum Ours {
    Rejected(Rejection),
    Evaluated(ResultType, Evaluation),
}

fn operand(value: &V) -> Option<Operand<'_>> {
    match value {
        V::Nul => None,
        V::Str(units) => Some(Operand::Text(units)),
        V::Bytes(bytes) => Some(Operand::Binary(bytes)),
        other => panic!("not a string operand: {other:?}"),
    }
}

fn text(value: &V) -> Option<&[u16]> {
    match value {
        V::Nul => None,
        V::Str(units) => Some(units),
        other => panic!("not a text operand: {other:?}"),
    }
}

fn start(value: Option<&V>) -> StartArgument {
    match value {
        None => StartArgument::Omitted,
        Some(V::Nul) => StartArgument::Null,
        Some(V::Integer(value)) => StartArgument::Value(Start::Integer(*value)),
        Some(V::Dec(unscaled, scale)) => StartArgument::Value(Start::Decimal {
            unscaled: *unscaled,
            scale: *scale,
        }),
        Some(other) => panic!("not a start: {other:?}"),
    }
}

fn run(call: &Call) -> Ours {
    match call.function {
        Function::Charindex => match resolve_charindex(&call.types) {
            Err(rejection) => Ours::Rejected(rejection),
            Ok(signature) => Ours::Evaluated(
                signature.result_type(),
                signature.evaluate(
                    operand(&call.values[0]),
                    operand(&call.values[1]),
                    start(call.values.get(2)),
                    call.collation,
                ),
            ),
        },
        Function::Patindex => match resolve_patindex(&call.types) {
            Err(rejection) => Ours::Rejected(rejection),
            Ok(signature) => Ours::Evaluated(
                signature.result_type(),
                signature.evaluate(text(&call.values[0]), text(&call.values[1]), call.collation),
            ),
        },
    }
}

fn error(value: &Value) -> ServerError {
    ServerError {
        number: i32::try_from(value["number"].as_i64().unwrap()).unwrap(),
        state: u8::try_from(value["state"].as_u64().unwrap()).unwrap(),
        class: u8::try_from(value["class"].as_u64().unwrap()).unwrap(),
        message: value["message"].as_str().unwrap().to_owned(),
    }
}

fn descriptor(column: &Value) -> ResultType {
    assert_eq!(column["type"], "IntN");
    assert_eq!(column["flags"], 33);
    assert!(column["collation"].is_null());
    match column["length"].as_u64().unwrap() {
        4 => ResultType::Int,
        8 => ResultType::BigInt,
        other => panic!("unexpected length {other}"),
    }
}

fn cell(value: &Value, result: ResultType) -> Option<i64> {
    match (value, result) {
        (Value::Null, _) => None,
        (Value::Number(number), ResultType::Int) => Some(number.as_i64().unwrap()),
        // tedious returns BIGINT as a decimal string.
        (Value::String(text), ResultType::BigInt) => Some(text.parse().unwrap()),
        other => panic!("unexpected cell {other:?}"),
    }
}

/// The captured outcome of a single-column scalar result.
fn observed(result: &Value) -> Ours {
    let errors = result["errors"].as_array().unwrap();
    let sets = result["sets"].as_array().unwrap();
    if sets.is_empty() {
        assert_eq!(errors.len(), 1);
        return Ours::Rejected(Rejection::Error(error(&errors[0])));
    }
    assert_eq!(sets.len(), 1);
    let columns = sets[0]["columns"].as_array().unwrap();
    assert_eq!(columns.len(), 1);
    let result_type = descriptor(&columns[0]);
    let rows = sets[0]["rows"].as_array().unwrap();
    if rows.is_empty() {
        assert_eq!(errors.len(), 1);
        return Ours::Evaluated(result_type, Evaluation::Error(error(&errors[0])));
    }
    assert!(errors.is_empty());
    assert_eq!(rows.len(), 1);
    Ours::Evaluated(
        result_type,
        Evaluation::Value(cell(&rows[0][0], result_type)),
    )
}

#[derive(Default)]
struct Tally {
    compared: usize,
    unsupported: usize,
    names: BTreeSet<String>,
}

impl Tally {
    fn check(&mut self, key: &str, call: &Call, expected: Ours) {
        let ours = run(call);
        let unsupported = matches!(
            ours,
            Ours::Rejected(Rejection::Unsupported(_))
                | Ours::Evaluated(_, Evaluation::Unsupported(_))
        );
        if UNSUPPORTED.contains(&key) {
            assert!(unsupported, "{key}: expected an explicit gap, got {ours:?}");
            assert!(
                !matches!(expected, Ours::Rejected(Rejection::Unsupported(_))),
                "{key}"
            );
            self.unsupported += 1;
        } else {
            assert_eq!(ours, expected, "{key}");
            self.compared += 1;
        }
        self.names.insert(key.to_owned());
    }
}

fn scalar(name: &str) -> Option<Call> {
    let emoji = [0xD83D, 0xDE00];
    let emoji_b = [0xD83D, 0xDE00, u16::from(b'b')];
    let a_emoji = [u16::from(b'a'), 0xD83D, 0xDE00];
    let cafe = "caf\u{e9}";
    let char10 = "ab        ";
    Some(match name {
        "charindex basic" => ci(&[VarChar, VarChar], vec![s("b"), s("abcb")]),
        "charindex not found" => ci(&[VarChar, VarChar], vec![s("z"), s("abc")]),
        "charindex multi-character" => ci(&[VarChar, VarChar], vec![s("cb"), s("abcbcb")]),
        "charindex find longer than search" => ci(&[VarChar, VarChar], vec![s("abcd"), s("abc")]),
        "charindex null find" => ci(&[Null, VarChar], vec![V::Nul, s("abc")]),
        "charindex typed null find" => ci(&[VarChar, VarChar], vec![V::Nul, s("abc")]),
        "charindex null search" => ci(&[VarChar, VarChar], vec![s("a"), V::Nul]),
        "charindex null start" => ci(&[VarChar, VarChar, Null], vec![s("a"), s("abc"), V::Nul]),
        "charindex all null" => ci(&[Null, Null], vec![V::Nul, V::Nul]),
        "charindex all null with start" => ci(&[Null, Null, Null], vec![V::Nul, V::Nul, V::Nul]),
        "charindex empty find" => ci(&[VarChar, VarChar], vec![s(""), s("abc")]),
        "charindex empty find with start" => ci(
            &[VarChar, VarChar, Int],
            vec![s(""), s("abc"), V::Integer(2)],
        ),
        "charindex empty search" => ci(&[VarChar, VarChar], vec![s("a"), s("")]),
        "charindex both empty" => ci(&[VarChar, VarChar], vec![s(""), s("")]),
        "charindex start 0" => start_on("aba", Int, V::Integer(0)),
        "charindex start negative" => start_on("aba", Int, V::Integer(-5)),
        "charindex start 1" => start_on("aba", Int, V::Integer(1)),
        "charindex start 2" => start_on("aba", Int, V::Integer(2)),
        "charindex start at last" => start_on("aba", Int, V::Integer(3)),
        "charindex start past length" => start_on("aba", Int, V::Integer(4)),
        "charindex start far beyond" => start_on("aba", Int, V::Integer(1_000_000)),
        "charindex start int max" => start_on("aba", Int, V::Integer(2_147_483_647)),
        "charindex start bigint" => start_on("aba", BigInt, V::Integer(2)),
        "charindex start bigint beyond int" => start_on("aba", BigInt, V::Integer(3_000_000_000)),
        "charindex start bigint negative" => start_on("aba", BigInt, V::Integer(-3_000_000_000)),
        // An integer literal beyond INT is typed NUMERIC(10,0).
        "charindex start bigint literal" => start_on("aba", Decimal, V::Dec(3_000_000_000, 0)),
        "charindex start smallint" => start_on("aba", SmallInt, V::Integer(2)),
        "charindex start tinyint" => start_on("aba", TinyInt, V::Integer(2)),
        "charindex start decimal" => start_on("aba", Decimal, V::Dec(29, 1)),
        "charindex start float" => ci_types(&[VarChar, VarChar, Float]),
        "charindex start numeric string" | "charindex start invalid string" => {
            ci_types(&[VarChar, VarChar, VarChar])
        }
        "charindex start bit" => ci_types(&[VarChar, VarChar, Bit]),
        "charindex start date rejected" => ci_types(&[VarChar, VarChar, Date]),
        "charindex start decimal 2.1 on aab" => start_on("aab", Decimal, V::Dec(21, 1)),
        "charindex start decimal 2.5 on aab" => start_on("aab", Decimal, V::Dec(25, 1)),
        "charindex start decimal 2.9 on aab" => start_on("aab", Decimal, V::Dec(29, 1)),
        "charindex start decimal beyond int" => start_on("aab", Decimal, V::Dec(30_000_000_000, 1)),
        "charindex start money" => ci_types(&[VarChar, VarChar, Money]),
        "charindex max start bigint beyond int" => ci(
            &[VarChar, VarCharMax, BigInt],
            vec![s("a"), s("aab"), V::Integer(3_000_000_000)],
        ),
        "charindex max start bigint negative" => ci(
            &[VarChar, VarCharMax, BigInt],
            vec![s("b"), s("aab"), V::Integer(-3_000_000_000)],
        ),
        "charindex nvarchar" => ci(&[NVarChar, NVarChar], vec![s("b"), s("abc")]),
        "charindex mixed varchar find nvarchar search" => {
            ci(&[VarChar, NVarChar], vec![s("b"), s("abc")])
        }
        "charindex mixed nvarchar find varchar search" => {
            ci(&[NVarChar, VarChar], vec![s("b"), s("abc")])
        }
        "charindex char padded" => ci(&[VarChar, Char], vec![s("b"), s(char10)]),
        "charindex char padded space" => ci(&[VarChar, Char], vec![s(" "), s(char10)]),
        "charindex varchar max search" => ci(&[VarChar, VarCharMax], vec![s("b"), s("abc")]),
        "charindex nvarchar max search" => ci(&[NVarChar, NVarCharMax], vec![s("b"), s("abc")]),
        "charindex varchar max find" => ci(&[VarCharMax, VarChar], vec![s("b"), s("abc")]),
        "charindex nvarchar max find" => ci(&[NVarCharMax, NVarChar], vec![s("b"), s("abc")]),
        "charindex max null search" => ci(&[VarChar, VarCharMax], vec![s("b"), V::Nul]),
        "charindex max start bigint" => ci(
            &[VarChar, VarCharMax, BigInt],
            vec![s("b"), s("abcb"), V::Integer(3)],
        ),
        "charindex max beyond 8000" => ci(
            &[VarChar, VarCharMax],
            vec![s("b"), repeated("a", 9000, "b")],
        ),
        "charindex max start beyond 8000" => ci(
            &[VarChar, VarCharMax, Int],
            vec![s("a"), repeated("a", 9000, ""), V::Integer(8999)],
        ),
        "charindex nvarchar max beyond 4000" => ci(
            &[NVarChar, NVarCharMax],
            vec![s("b"), repeated("a", 5000, "b")],
        ),
        "charindex trailing space find" => ci(&[VarChar, VarChar], vec![s("a "), s("a")]),
        "charindex trailing space find matched" => {
            ci(&[VarChar, VarChar], vec![s("a "), s("ba b")])
        }
        "charindex space only find" => ci(&[VarChar, VarChar], vec![s(" "), s("ab ")]),
        "charindex space only find absent" => ci(&[VarChar, VarChar], vec![s(" "), s("ab")]),
        "charindex trailing space search" => ci(&[VarChar, VarChar], vec![s("b"), s("a  b  ")]),
        "charindex nvarchar trailing space find" => {
            ci(&[NVarChar, NVarChar], vec![s("a "), s("a")])
        }
        "charindex default case insensitive" => ci(&[VarChar, VarChar], vec![s("B"), s("abc")]),
        "charindex case sensitive" | "charindex case sensitive on find" => {
            ci(&[VarChar, VarChar], vec![s("B"), s("abc")]).under("Latin1_General_CS_AS")
        }
        "charindex accent sensitive default" => ci(&[NVarChar, NVarChar], vec![s("e"), s(cafe)]),
        "charindex accent insensitive" => {
            ci(&[NVarChar, NVarChar], vec![s("e"), s(cafe)]).under("Latin1_General_CI_AI")
        }
        "charindex bin2 case" => {
            ci(&[VarChar, VarChar], vec![s("B"), s("abcB")]).under("Latin1_General_BIN2")
        }
        "charindex bin2 accent" => {
            ci(&[NVarChar, NVarChar], vec![s("e"), s("caf\u{e9}e")]).under("Latin1_General_BIN2")
        }
        "charindex expansion ss" => ci(&[NVarChar, NVarChar], vec![s("ss"), s("stra\u{df}e")]),
        "charindex expansion ss bin2" => {
            ci(&[NVarChar, NVarChar], vec![s("ss"), s("stra\u{df}e")]).under("Latin1_General_BIN2")
        }
        "charindex ignorable character" => ci(&[NVarChar, NVarChar], vec![s("ab"), s("a\u{ad}b")]),
        "charindex ignorable character bin2" => {
            ci(&[NVarChar, NVarChar], vec![s("ab"), s("a\u{ad}b")]).under("Latin1_General_BIN2")
        }
        "charindex utf8 varchar" => ci(&[VarChar, VarChar], vec![s("b"), s("\u{e9}b")])
            .under("Latin1_General_100_CI_AS_SC_UTF8"),
        "charindex after surrogate pair" => {
            ci(&[NVarChar, NVarChar], vec![s("b"), units(&emoji_b)])
        }
        "charindex after surrogate pair sc" => {
            ci(&[NVarChar, NVarChar], vec![s("b"), units(&emoji_b)])
                .under("Latin1_General_100_CI_AS_SC")
        }
        "charindex find surrogate pair" => {
            ci(&[NVarChar, NVarChar], vec![units(&emoji), units(&a_emoji)])
        }
        "charindex find surrogate pair sc" => {
            ci(&[NVarChar, NVarChar], vec![units(&emoji), units(&a_emoji)])
                .under("Latin1_General_100_CI_AS_SC")
        }
        "charindex find high surrogate" => ci(
            &[NVarChar, NVarChar],
            vec![units(&emoji[..1]), units(&a_emoji)],
        ),
        "charindex find high surrogate sc" => ci(
            &[NVarChar, NVarChar],
            vec![units(&emoji[..1]), units(&a_emoji)],
        )
        .under("Latin1_General_100_CI_AS_SC"),
        "charindex start inside surrogate sc" => ci(
            &[NVarChar, NVarChar, Int],
            vec![s("b"), units(&emoji_b), V::Integer(2)],
        )
        .under("Latin1_General_100_CI_AS_SC"),
        "charindex find surrogate bin2" => ci(
            &[NVarChar, NVarChar],
            vec![units(&emoji[1..]), units(&a_emoji)],
        )
        .under("Latin1_General_BIN2"),
        "charindex int arguments" => ci_types(&[Int, Int]),
        "charindex int search" => ci(&[VarChar, Int], vec![s("3"), s("12345")]),
        "charindex int search nvarchar find" => ci(&[NVarChar, Int], vec![s("3"), s("12345")]),
        "charindex decimal search" => ci(&[VarChar, Decimal], vec![s("."), s("12.50")]),
        "charindex int find" => ci_types(&[Int, VarChar]),
        "charindex binary arguments" => ci(
            &[VarBinary, VarBinary],
            vec![V::Bytes(vec![0x62]), V::Bytes(vec![0x61, 0x62, 0x63])],
        ),
        "charindex varbinary max" => ci(
            &[VarBinary, VarBinaryMax],
            vec![V::Bytes(vec![0x62]), V::Bytes(vec![0x61, 0x62, 0x63])],
        ),
        "charindex text search" => ci(&[VarChar, ArgType::Text], vec![s("b"), s("abc")]),
        "charindex ntext search" => ci(&[NVarChar, NText], vec![s("b"), s("abc")]),
        "charindex xml rejected" => ci_types(&[VarChar, Xml]),
        "charindex uniqueidentifier" => ci(
            &[VarChar, UniqueIdentifier],
            vec![s("-"), s("00000000-0000-0000-0000-000000000000")],
        ),
        // DATETIME converts to VARCHAR in style 0.
        "charindex datetime" => ci(
            &[VarChar, DateTime],
            vec![s("2024"), s("Jan  2 2024 12:00AM")],
        ),
        "charindex too few arguments" => ci_types(&[VarChar]),
        "charindex too many arguments" => ci_types(&[VarChar, VarChar, Int, Int]),
        "patindex basic" => pv("%b%", "abcb"),
        "patindex not found" => pv("%z%", "abc"),
        "patindex no wildcard exact" => pv("abc", "abc"),
        "patindex no wildcard partial" => pv("b", "abc"),
        "patindex prefix only" => pv("a%", "abc"),
        "patindex suffix only" => pv("%c", "abc"),
        "patindex suffix only miss" => pv("%b", "abc"),
        "patindex percent only" => pv("%", "abc"),
        "patindex percent only empty" => pv("%", ""),
        "patindex empty pattern" => pv("", "abc"),
        "patindex empty pattern empty" => pv("", ""),
        "patindex empty search" => pv("%a%", ""),
        "patindex null pattern" => pi(&[Null, VarChar], vec![V::Nul, s("abc")]),
        "patindex null search" => pi_types(&[VarChar, Null]),
        "patindex typed null search" => pi(&[VarChar, NVarChar], vec![s("%a%"), V::Nul]),
        "patindex underscore" => pv("%b_d%", "abcd"),
        "patindex underscore only" => pv("_", "a"),
        "patindex underscore leading" => pv("%_c%", "abc"),
        "patindex range" => pv("%[c-e]%", "abxd"),
        "patindex set" => pv("%[xyz]%", "abcz"),
        "patindex negated set" => pv("%[^a]%", "aab"),
        "patindex negated range" => pv("%[^a-c]%", "abcd"),
        "patindex digit" => pv("%[0-9]%", "ab3c"),
        "patindex bracket percent" => pv("%[%]%", "ab%c"),
        "patindex bracket underscore" => pv("%[_]%", "ab_c"),
        "patindex bracket open bracket" => pv("%[[]%", "ab[c"),
        "patindex close bracket literal" => pv("%]%", "ab]c"),
        "patindex bracket close bracket" => pv("%[]]%", "ab]c"),
        "patindex bracket caret literal" => pv("%[a^]%", "x^"),
        "patindex bracket dash literal" => pv("%[-a]%", "x-"),
        "patindex bracket trailing dash" => pv("%[a-]%", "x-"),
        "patindex reversed range" => pv("%[z-a]%", "m"),
        "patindex empty brackets" => pv("%[]%", "ab[]c"),
        "patindex unclosed bracket" => pv("%[a%", "x[a"),
        "patindex backslash not escape" => pv("%\\%%", "a\\b"),
        "patindex multiple percent" => pv("%b%d%", "abcd"),
        "patindex nvarchar" => pn("%b%", "abc"),
        "patindex mixed pattern" => pi(&[VarChar, NVarChar], vec![s("%b%"), s("abc")]),
        "patindex varchar max" => pi(&[VarChar, VarCharMax], vec![s("%b%"), s("abc")]),
        "patindex nvarchar max" => pi(&[NVarChar, NVarCharMax], vec![s("%b%"), s("abc")]),
        "patindex max pattern" => pi(&[VarCharMax, VarChar], vec![s("%b%"), s("abc")]),
        "patindex max beyond 8000" => pi(
            &[VarChar, VarCharMax],
            vec![s("%b%"), repeated("a", 9000, "b")],
        ),
        "patindex char padded suffix" => pi(&[VarChar, Char], vec![s("%b"), s(char10)]),
        "patindex trailing space search suffix" => pv("%b", "ab  "),
        "patindex trailing space pattern" => pv("ab ", "ab"),
        "patindex trailing space both" => pv("%b ", "ab "),
        "patindex nvarchar trailing space search suffix" => pn("%b", "ab  "),
        "patindex space class" => pv("%[ ]%", "ab c"),
        "patindex default case insensitive" => pv("%B%", "abc"),
        "patindex case sensitive" => pv("%B%", "abc").under("Latin1_General_CS_AS"),
        "patindex range case insensitive" => pv("%[A-C]%", "xb"),
        "patindex range case sensitive" => pv("%[A-C]%", "xb").under("Latin1_General_CS_AS"),
        "patindex range bin2" => pv("%[A-C]%", "xbB").under("Latin1_General_BIN2"),
        "patindex range sql collation lowercase" => {
            pv("%[a-c]%", "xB").under("SQL_Latin1_General_CP1_CS_AS")
        }
        "patindex accent sensitive default" => pn("%e%", cafe),
        "patindex accent insensitive" => pn("%e%", cafe).under("Latin1_General_CI_AI"),
        "patindex accent range" => pn("%[a-f]%", "x\u{e9}"),
        "patindex accent range bin2" => pn("%[a-f]%", "x\u{e9}").under("Latin1_General_BIN2"),
        "patindex after surrogate pair" => {
            pi(&[NVarChar, NVarChar], vec![s("%b%"), units(&emoji_b)])
        }
        "patindex after surrogate pair sc" => {
            pi(&[NVarChar, NVarChar], vec![s("%b%"), units(&emoji_b)])
                .under("Latin1_General_100_CI_AS_SC")
        }
        "patindex underscore surrogate" => {
            pi(&[NVarChar, NVarChar], vec![s("_b"), units(&emoji_b)])
        }
        "patindex underscore surrogate sc" => {
            pi(&[NVarChar, NVarChar], vec![s("_b"), units(&emoji_b)])
                .under("Latin1_General_100_CI_AS_SC")
        }
        "patindex two underscores surrogate" => {
            pi(&[NVarChar, NVarChar], vec![s("__b"), units(&emoji_b)])
        }
        "patindex two underscores surrogate sc" => {
            pi(&[NVarChar, NVarChar], vec![s("__b"), units(&emoji_b)])
                .under("Latin1_General_100_CI_AS_SC")
        }
        "patindex find surrogate pair" => pi(
            &[NVarChar, NVarChar],
            vec![units(&[0x25, 0xD83D, 0xDE00, 0x25]), units(&a_emoji)],
        ),
        "patindex int search" => pi_types(&[VarChar, Int]),
        "patindex int pattern" => pi(&[Int, VarChar], vec![s("3"), s("3")]),
        "patindex binary search" => pi_types(&[VarChar, VarBinary]),
        "patindex text search" => pi(&[VarChar, ArgType::Text], vec![s("%b%"), s("abc")]),
        "patindex ntext search" => pi(&[NVarChar, NText], vec![s("%b%"), s("abc")]),
        "patindex xml rejected" => pi_types(&[VarChar, Xml]),
        "patindex start argument rejected" => pi_types(&[VarChar, VarChar, Int]),
        "patindex too few arguments" => pi_types(&[VarChar]),
        _ => return None,
    })
}

fn start_on(search: &str, start_type: ArgType, start: V) -> Call {
    ci(
        &[VarChar, VarChar, start_type],
        vec![s("a"), s(search), start],
    )
}

fn pv(pattern: &str, search: &str) -> Call {
    pi(&[VarChar, VarChar], vec![s(pattern), s(search)])
}

fn pn(pattern: &str, search: &str) -> Call {
    pi(&[NVarChar, NVarChar], vec![s(pattern), s(search)])
}

/// Maps a captured tedious parameter to its bound type and value. INT
/// search values reach the core as their VARCHAR text.
fn parameter(kind: &str, length: &Value, value: &Value, is_start: bool) -> (ArgType, V) {
    let max = length == "max";
    let kind = match (kind, max) {
        ("VarChar", false) => VarChar,
        ("VarChar", true) => VarCharMax,
        ("NVarChar", false) => NVarChar,
        ("NVarChar", true) => NVarCharMax,
        ("Int", _) => Int,
        ("BigInt", _) => BigInt,
        other => panic!("unexpected parameter {other:?}"),
    };
    let value = match value {
        Value::Null => V::Nul,
        Value::String(text) if matches!(kind, BigInt) => V::Integer(text.parse().unwrap()),
        Value::String(text) => s(text),
        Value::Number(number) if is_start => V::Integer(number.as_i64().unwrap()),
        Value::Number(number) => s(&number.to_string()),
        other => panic!("unexpected value {other:?}"),
    };
    (kind, value)
}

fn parameterized(sql: &str, parameters: &[Value], values: &dyn Fn(&str) -> Value) -> Call {
    let function = if sql.contains("CHARINDEX(") {
        Function::Charindex
    } else {
        assert!(sql.contains("PATINDEX("));
        Function::Patindex
    };
    let mut types = Vec::new();
    let mut bound = Vec::new();
    for parameter_value in parameters {
        let name = parameter_value["name"].as_str().unwrap();
        let (kind, value) = parameter(
            parameter_value["type"].as_str().unwrap(),
            &parameter_value["options"]["length"],
            &values(name),
            name == "start",
        );
        types.push(kind);
        bound.push(value);
    }
    let call = Call {
        function,
        types,
        values: bound,
        collation: Collation::DEFAULT,
    };
    if sql.contains("COLLATE Latin1_General_CS_AS") {
        call.under("Latin1_General_CS_AS")
    } else {
        call
    }
}

struct Row {
    id: i64,
    f: V,
    s: V,
    nf: V,
    ns: V,
    ms: V,
    st: V,
    p: V,
    np: V,
}

/// dbo.ci_src as inserted by the captured setup batch.
fn source_rows() -> Vec<Row> {
    let row = |id, f: V, text: V, st: V, p: V| Row {
        id,
        nf: f.clone(),
        ns: text.clone(),
        ms: text.clone(),
        np: p.clone(),
        f,
        s: text,
        st,
        p,
    };
    vec![
        row(1, s("b"), s("abcb"), V::Integer(3), s("%b%")),
        row(2, s("z"), s("abc"), V::Integer(1), s("%z%")),
        row(3, V::Nul, s("abc"), V::Nul, V::Nul),
        row(4, s("a"), V::Nul, V::Integer(-1), s("%a%")),
        row(5, s(""), s("abc"), V::Integer(0), s("")),
    ]
}

fn value(call: Call) -> (ResultType, Option<i64>) {
    match run(&call) {
        Ours::Evaluated(result_type, Evaluation::Value(value)) => (result_type, value),
        other => panic!("{other:?}"),
    }
}

/// Column-sourced, WHERE and describe-first-result-set cases.
fn multi_column(name: &str, result: &Value) -> bool {
    const NAMES: [&str; 7] = [
        "charindex columns",
        "patindex columns",
        "charindex not null column descriptor",
        "patindex not null column descriptor",
        "charindex where filter",
        "patindex where filter",
        "charindex result metadata",
    ];
    if !NAMES.contains(&name) {
        return false;
    }
    assert_eq!(result["errors"].as_array().unwrap().len(), 0, "{name}");
    let set = &result["sets"][0];
    let columns = set["columns"].as_array().unwrap();
    let rows = set["rows"].as_array().unwrap();
    match name {
        "charindex columns" | "patindex columns" => {
            let expected_rows = source_rows();
            assert_eq!(rows.len(), expected_rows.len());
            for (row, source) in rows.iter().zip(expected_rows) {
                assert_eq!(row[0], source.id);
                let calls = if name == "charindex columns" {
                    vec![
                        ci(
                            &[VarChar, VarChar],
                            vec![source.f.clone(), source.s.clone()],
                        ),
                        ci(&[NVarChar, NVarChar], vec![source.nf, source.ns]),
                        ci(&[VarChar, VarCharMax], vec![source.f.clone(), source.ms]),
                        ci(
                            &[VarChar, VarChar, Int],
                            vec![source.f, source.s, source.st],
                        ),
                    ]
                } else {
                    vec![
                        pi(
                            &[VarChar, VarChar],
                            vec![source.p.clone(), source.s.clone()],
                        ),
                        pi(&[NVarChar, NVarChar], vec![source.np, source.ns]),
                        pi(&[VarChar, VarCharMax], vec![source.p, source.ms]),
                    ]
                };
                assert_eq!(columns.len(), calls.len() + 1);
                for (index, call) in calls.into_iter().enumerate() {
                    let expected_type = descriptor(&columns[index + 1]);
                    let (result_type, value) = value(call);
                    assert_eq!(result_type, expected_type, "{name}");
                    assert_eq!(value, cell(&row[index + 1], expected_type), "{name}");
                }
            }
        }
        "charindex not null column descriptor" | "patindex not null column descriptor" => {
            let call = if name.starts_with("charindex") {
                ci(&[VarChar, VarChar], vec![s("b"), s("abc")])
            } else {
                pv("%b%", "abc")
            };
            assert_eq!(run(&call), observed(result), "{name}");
        }
        "charindex where filter" | "patindex where filter" => {
            let ids: Vec<i64> = source_rows()
                .into_iter()
                .filter(|source| {
                    let call = if name.starts_with("charindex") {
                        ci(
                            &[VarChar, VarChar],
                            vec![source.f.clone(), source.s.clone()],
                        )
                    } else {
                        pi(
                            &[VarChar, VarChar],
                            vec![source.p.clone(), source.s.clone()],
                        )
                    };
                    value(call).1.is_some_and(|position| position > 0)
                })
                .map(|source| source.id)
                .collect();
            let expected: Vec<i64> = rows.iter().map(|row| row[0].as_i64().unwrap()).collect();
            assert_eq!(ids, expected);
        }
        "charindex result metadata" => {
            let signatures = [
                resolve_charindex(&[VarChar, VarChar])
                    .unwrap()
                    .result_type(),
                resolve_charindex(&[NVarChar, NVarCharMax])
                    .unwrap()
                    .result_type(),
                resolve_patindex(&[VarChar, VarChar]).unwrap().result_type(),
                resolve_patindex(&[VarChar, VarCharMax])
                    .unwrap()
                    .result_type(),
                resolve_charindex(&[VarChar, VarChar])
                    .unwrap()
                    .result_type(),
                resolve_charindex(&[VarChar, VarCharMax])
                    .unwrap()
                    .result_type(),
            ];
            assert_eq!(rows.len(), signatures.len());
            for (row, result_type) in rows.iter().zip(signatures) {
                let expected = match result_type {
                    ResultType::Int => "int",
                    ResultType::BigInt => "bigint",
                };
                assert_eq!(row[1], expected);
                assert_eq!(row[2], true);
            }
        }
        _ => unreachable!(),
    }
    true
}

#[test]
fn replays_every_applicable_case_in_all_retained_runs() {
    let reference: Value = serde_json::from_str(REFERENCE).unwrap();
    let containers = reference["containers"].as_array().unwrap();
    assert_eq!(containers.len(), 2);
    let mut runs = 0;
    for container in containers {
        for run_cases in container["runs"].as_array().unwrap() {
            runs += 1;
            let cases = run_cases.as_array().unwrap();
            assert_eq!(cases.len(), 211);
            let mut tally = Tally::default();
            let mut skipped = 0;
            let mut multi = 0;
            for case in cases {
                let name = case["name"].as_str().unwrap();
                let sql = case["sql"].as_str().unwrap();
                if NOT_APPLICABLE.contains(&name) {
                    skipped += 1;
                    continue;
                }
                match case["protocol"].as_str() {
                    None => {
                        if multi_column(name, &case["result"]) {
                            multi += 1;
                            continue;
                        }
                        let call = scalar(name).unwrap_or_else(|| panic!("unmapped case {name}"));
                        tally.check(name, &call, observed(&case["result"]));
                    }
                    Some("sp_executesql") => {
                        let parameters = case["parameters"].as_array().unwrap();
                        let lookup = |wanted: &str| {
                            parameters
                                .iter()
                                .find(|parameter| parameter["name"] == wanted)
                                .unwrap()["value"]
                                .clone()
                        };
                        let call = parameterized(sql, parameters, &lookup);
                        tally.check(name, &call, observed(&case["result"]));
                    }
                    Some("sp_prepare/sp_execute/sp_unprepare") => {
                        let parameters = case["parameters"].as_array().unwrap();
                        let preparation = &case["preparation"]["sets"][0]["columns"][0];
                        for (index, execution) in
                            case["executions"].as_array().unwrap().iter().enumerate()
                        {
                            let lookup = |wanted: &str| execution["values"][wanted].clone();
                            let call = parameterized(sql, parameters, &lookup);
                            let key = format!("{name}#{index}");
                            if let Ours::Evaluated(result_type, _) = run(&call) {
                                assert_eq!(result_type, descriptor(preparation), "{key}");
                            }
                            tally.check(&key, &call, observed(&execution["result"]));
                        }
                    }
                    other => panic!("unexpected protocol {other:?}"),
                }
            }
            assert_eq!(skipped, NOT_APPLICABLE.len());
            assert_eq!(multi, 7);
            assert_eq!(tally.unsupported, UNSUPPORTED.len());
            // 211 programs less 9 not applicable, 7 multi-column checks and
            // 4 prepared programs, plus their 19 executions.
            assert_eq!(tally.compared + tally.unsupported, 211 - 9 - 7 - 4 + 19);
            for key in UNSUPPORTED {
                assert!(tally.names.contains(key), "{key}");
            }
        }
    }
    assert_eq!(runs, 4);
}

#[test]
fn collation_names_outside_the_modelled_families_are_rejected() {
    for name in [
        "Latin1_General_CI_AS_KS",
        "Latin1_General_CI_AS_WS",
        "Latin1_General_BIN",
        "Latin1_General_CI_AS_SC",
        "Japanese_CI_AS",
        "SQL_Latin1_General_CP1_BIN2",
    ] {
        assert_eq!(Collation::from_name(name), None, "{name}");
    }
    assert_eq!(
        Collation::from_name("SQL_Latin1_General_CP1_CI_AS"),
        Some(Collation::DEFAULT)
    );
    for (spelled, canonical) in [
        (
            "sql_latin1_general_cp1_ci_as",
            "SQL_Latin1_General_CP1_CI_AS",
        ),
        ("latin1_general_ci_as", "Latin1_General_CI_AS"),
        ("LATIN1_GENERAL_BIN2", "Latin1_General_BIN2"),
        (
            "latin1_general_100_ci_as_sc_utf8",
            "Latin1_General_100_CI_AS_SC_UTF8",
        ),
    ] {
        assert_eq!(
            Collation::from_name(spelled),
            Collation::from_name(canonical),
            "{spelled}"
        );
        assert!(Collation::from_name(spelled).is_some(), "{spelled}");
    }
}

#[test]
fn uncaptured_forms_stay_explicitly_unsupported() {
    let unsupported = |call: Call| {
        assert!(
            matches!(
                run(&call),
                Ours::Rejected(Rejection::Unsupported(_))
                    | Ours::Evaluated(_, Evaluation::Unsupported(_))
            ),
            "{call:?}"
        );
    };
    // Range ties below the primary level and symbol order.
    unsupported(pv("%[a-c]%", "A").under("Latin1_General_CS_AS"));
    unsupported(pv("%[!-z]%", "a"));
    // Trailing search spaces for a mixed pattern/search family.
    unsupported(pi(&[VarChar, NVarChar], vec![s("%b"), s("ab ")]));
    // Characters outside the modelled linguistic set.
    unsupported(ci(&[NVarChar, NVarChar], vec![s("b"), s("\u{ff42}")]));
    // Overflowing start with a NULL operand, and DECIMAL start on MAX.
    unsupported(ci(
        &[VarChar, VarChar, BigInt],
        vec![V::Nul, s("a"), V::Integer(3_000_000_000)],
    ));
    unsupported(ci(
        &[VarChar, VarCharMax, Decimal],
        vec![s("a"), s("a"), V::Dec(2, 0)],
    ));
    // Uncaptured argument types and mixed binary/character operands.
    unsupported(ci_types(&[VarChar, VarChar, ArgType::Other]));
    unsupported(ci_types(&[VarChar, VarBinary]));
    unsupported(ci_types(&[Int, Xml]));
    unsupported(ci_types(&[NChar, VarChar]));
    unsupported(ci_types(&[VarChar, Binary]));
}
