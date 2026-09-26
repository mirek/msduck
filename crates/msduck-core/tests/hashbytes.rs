// Compile the pure module here until a separately claimed core export is free.
#[path = "../src/hashbytes.rs"]
mod hashbytes;

use hashbytes::{
    Algorithm, ArgumentType, BindError, RESULT_TYPE, Unsupported, bind, hashbytes, md4, md5,
    resolve_algorithm, sha1, sha256, sha512,
};
use serde_json::{Value, json};

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(text: &str) -> Vec<u8> {
    text.as_bytes()
        .chunks_exact(2)
        .map(|b| u8::from_str_radix(std::str::from_utf8(b).unwrap(), 16).unwrap())
        .collect()
}

#[test]
fn standard_test_vectors() {
    let alphabet = "abcdefghijklmnopqrstuvwxyz";
    let alnum = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
    let digits = "1234567890".repeat(8);
    // RFC 1320 appendix A.5.
    for (input, digest) in [
        ("", "31d6cfe0d16ae931b73c59d7e0c089c0"),
        ("a", "bde52cb31de33e46245e05fbdbd6fb24"),
        ("abc", "a448017aaf21d8525fc10ae87aa6729d"),
        ("message digest", "d9130a8164549fe818874806e1c7014b"),
        (alphabet, "d79e1c308aa5bbcdeea8ed63df412da9"),
        (alnum, "043f8582f241db351ce627e153e7f0e4"),
        (&digits, "e33b4ddc9c38f2199c3e7b164fcc0536"),
    ] {
        assert_eq!(hex(&md4(input.as_bytes())), digest, "MD4 {input:?}");
    }
    // RFC 1321 appendix A.5.
    for (input, digest) in [
        ("", "d41d8cd98f00b204e9800998ecf8427e"),
        ("a", "0cc175b9c0f1b6a831c399e269772661"),
        ("abc", "900150983cd24fb0d6963f7d28e17f72"),
        ("message digest", "f96b697d7cb7938d525a2f31aaf161d0"),
        (alphabet, "c3fcd3d76192e4007dfb496cca67e13b"),
        (alnum, "d174ab98d277d9f5a5611c2c9f419d9f"),
        (&digits, "57edf4a22be3c955ac49da2e2107b67a"),
    ] {
        assert_eq!(hex(&md5(input.as_bytes())), digest, "MD5 {input:?}");
    }
    // FIPS 180 examples: one block, two blocks and one million 'a'.
    let two_32 = b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq";
    let two_64 = b"abcdefghbcdefghicdefghijdefghijkefghijklfghijklmghijklmnhijklmnoijklmnopjklmnopqklmnopqrlmnopqrsmnopqrstnopqrstu";
    let million = vec![b'a'; 1_000_000];
    assert_eq!(hex(&sha1(b"")), "da39a3ee5e6b4b0d3255bfef95601890afd80709");
    assert_eq!(
        hex(&sha1(b"abc")),
        "a9993e364706816aba3e25717850c26c9cd0d89d"
    );
    assert_eq!(
        hex(&sha1(two_32)),
        "84983e441c3bd26ebaae4aa1f95129e5e54670f1"
    );
    assert_eq!(
        hex(&sha1(&million)),
        "34aa973cd4c4daa4f61eeb2bdbad27316534016f"
    );
    assert_eq!(
        hex(&sha256(b"")),
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
    assert_eq!(
        hex(&sha256(b"abc")),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    assert_eq!(
        hex(&sha256(two_32)),
        "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
    );
    assert_eq!(
        hex(&sha256(&million)),
        "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0"
    );
    assert_eq!(
        hex(&sha512(b"")),
        "cf83e1357eefb8bdf1542850d66d8007d620e4050b5715dc83f4a921d36ce9ce47d0d13c5d85f2b0ff8318d2877eec2f63b931bd47417a81a538327af927da3e"
    );
    assert_eq!(
        hex(&sha512(b"abc")),
        "ddaf35a193617abacc417349ae20413112e6fa4e89a97ea20a9eeee64b55d39a2192992a274fc1a836ba3c23a3feebbd454d4423643ce80e2a9ac94fa54ca49f"
    );
    assert_eq!(
        hex(&sha512(two_64)),
        "8e959b75dae313da8cf4f72814fc143f8f7779c6eb9f7fa17299aeadb6889018501d289e4900f7e4331b99dec4b5433ac7d329eeb6dd26545e96e55b874be909"
    );
    assert_eq!(
        hex(&sha512(&million)),
        "e718483d0ce769644e2e42c7bc15b4638e1f98b13b2044285632a803afa973ebde0ff244877ea60a4cb0432ce577c31beb009c5c2c49aa2e4eadb217ad8cc09b"
    );
}

#[test]
fn algorithm_names_and_unsupported_spellings() {
    assert_eq!(resolve_algorithm("sha"), Ok(Some(Algorithm::Sha1)));
    assert_eq!(
        resolve_algorithm("Sha2_512   "),
        Ok(Some(Algorithm::Sha2_512))
    );
    assert_eq!(resolve_algorithm("MD2"), Ok(None));
    assert_eq!(
        resolve_algorithm("MD5\t"),
        Err(Unsupported(
            "HASHBYTES algorithm names with control or non-ASCII characters were not captured"
        ))
    );
    assert!(resolve_algorithm("ſHA1").is_err());
    // NULL input is NULL whatever the algorithm spelling.
    assert_eq!(hashbytes(Some("MD5\t"), None), Ok(None));
    assert!(hashbytes(Some("MD5\t"), Some(b"abc")).is_err());
    for algorithm in [
        Algorithm::Md4,
        Algorithm::Md5,
        Algorithm::Sha1,
        Algorithm::Sha2_256,
        Algorithm::Sha2_512,
    ] {
        assert_eq!(algorithm.digest(b"").len(), algorithm.digest_len());
    }
    // Uncaptured diagnostics stay unsupported rather than guessed.
    use ArgumentType::*;
    for arguments in [
        &[UntypedNull, UntypedNull][..],
        &[Int, Int],
        &[Varchar, Uncaptured],
        &[Char, Varchar],
        &[Varchar, Varchar, Int],
        &[],
    ] {
        assert!(
            matches!(bind(arguments), Err(BindError::Unsupported(_))),
            "{arguments:?}"
        );
    }
}

/// A bound argument: its static type and the exact bytes SQL Server hashes.
struct Arg {
    kind: ArgumentType,
    bytes: Option<Vec<u8>>,
}

/// VARCHAR/CHAR under the captured default collation SQL_Latin1_General_CP1_CI_AS
/// (code page 1252). Only characters the fixture uses are mapped.
fn cp1252(text: &str) -> Vec<u8> {
    text.chars()
        .map(|c| match c {
            '\0'..='\x7f' | '\u{a0}'..='\u{ff}' => c as u8,
            _ => panic!("fixture text outside the mapped code page 1252 subset: {c:?}"),
        })
        .collect()
}

fn utf16le(text: &str) -> Vec<u8> {
    text.encode_utf16().flat_map(u16::to_le_bytes).collect()
}

fn varchar(text: &str) -> Arg {
    Arg {
        kind: ArgumentType::Varchar,
        bytes: Some(cp1252(text)),
    }
}
/// VARCHAR under Latin1_General_100_CI_AS_SC_UTF8.
fn varchar_utf8(text: &str) -> Arg {
    Arg {
        kind: ArgumentType::Varchar,
        bytes: Some(text.as_bytes().to_vec()),
    }
}
fn nvarchar(text: &str) -> Arg {
    Arg {
        kind: ArgumentType::Nvarchar,
        bytes: Some(utf16le(text)),
    }
}
fn char_padded(text: &str, length: usize) -> Arg {
    Arg {
        kind: ArgumentType::Char,
        bytes: Some(cp1252(&format!("{text:length$}"))),
    }
}
fn nchar_padded(text: &str, length: usize) -> Arg {
    Arg {
        kind: ArgumentType::Nchar,
        bytes: Some(utf16le(&format!("{text:length$}"))),
    }
}
fn varbinary(bytes: &[u8]) -> Arg {
    Arg {
        kind: ArgumentType::Varbinary,
        bytes: Some(bytes.to_vec()),
    }
}
fn null(kind: ArgumentType) -> Arg {
    Arg { kind, bytes: None }
}

/// Bind and evaluate HASHBYTES(algorithm, input), as a fixture cell.
fn cell(algorithm: Arg, input: Arg) -> Value {
    assert_eq!(bind(&[algorithm.kind, input.kind]), Ok(RESULT_TYPE));
    // Decode the algorithm name from the bytes of its own type.
    let name = algorithm.bytes.map(|bytes| match algorithm.kind {
        ArgumentType::Nvarchar => String::from_utf16(
            &bytes
                .chunks_exact(2)
                .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
                .collect::<Vec<_>>(),
        )
        .unwrap(),
        _ => bytes.iter().map(|&b| char::from(b)).collect(),
    });
    match hashbytes(name.as_deref(), input.bytes.as_deref()).unwrap() {
        Some(digest) => json!({"kind": "binary", "value": hex(&digest)}),
        None => Value::Null,
    }
}

fn h(algorithm: &str, input: Arg) -> Value {
    cell(varchar(algorithm), input)
}

enum Expected {
    Rows(Vec<Vec<Value>>),
    Error(Vec<ArgumentType>),
}

fn rows(rows: Vec<Vec<Value>>) -> Option<Expected> {
    Some(Expected::Rows(rows))
}
fn row(cells: Vec<Value>) -> Option<Expected> {
    rows(vec![cells])
}
fn error(arguments: &[ArgumentType]) -> Option<Expected> {
    Some(Expected::Error(arguments.to_vec()))
}

fn batch_case(name: &str) -> Option<Expected> {
    use ArgumentType::*;
    let abc = || varchar("abc");
    let a = |n| "a".repeat(n);
    if let Some(rest) = name.strip_prefix("hashbytes ") {
        let parts: Vec<_> = rest.split(' ').collect();
        if let [algorithm, kind] = parts[..]
            && ["MD2", "MD4", "MD5", "SHA", "SHA1", "SHA2_256", "SHA2_512"].contains(&algorithm)
        {
            let input = match kind {
                "varchar" => abc(),
                "nvarchar" => nvarchar("abc"),
                "varbinary" => varbinary(b"abc"),
                _ => return None,
            };
            return row(vec![h(algorithm, input)]);
        }
    }
    match name {
        "hashbytes lowercase algorithm" => row(vec![h("sha2_256", abc()), h("Md5", abc())]),
        "hashbytes unicode algorithm" => row(vec![cell(nvarchar("SHA2_256"), abc())]),
        "hashbytes algorithm trailing space" => row(vec![h("MD5 ", abc())]),
        "hashbytes algorithm leading space" => row(vec![h(" MD5", abc())]),
        "hashbytes invalid algorithm" => row(vec![h("SHA3_256", abc())]),
        "hashbytes sha2_384 algorithm" => row(vec![h("SHA2_384", abc())]),
        "hashbytes empty algorithm" => row(vec![h("", abc())]),
        "hashbytes null algorithm" => row(vec![cell(null(Varchar), abc())]),
        "hashbytes untyped null algorithm" => error(&[UntypedNull, Varchar]),
        "hashbytes invalid algorithm null input" => row(vec![h("SHA3_256", null(Varchar))]),
        "hashbytes invalid algorithm per row" => {
            rows(vec![vec![h("MD5", abc())], vec![h("BOGUS", abc())]])
        }
        "hashbytes invalid algorithm empty source" => {
            assert_eq!(bind(&[Varchar, Varchar]), Ok(RESULT_TYPE));
            rows(vec![])
        }
        "hashbytes algorithm variable" => row(vec![h("SHA1", abc())]),
        // dbo.hash_src: alg VARCHAR(20), txt NVARCHAR(20).
        "hashbytes algorithm column" => rows(vec![
            vec![json!("MD5"), h("MD5", nvarchar("abc"))],
            vec![json!("sha1"), h("sha1", nvarchar("abc"))],
            vec![json!("SHA2_256"), h("SHA2_256", null(Nvarchar))],
        ]),
        "hashbytes integer algorithm" => error(&[Int, Varchar]),
        "hashbytes null varchar input" => row(vec![h("SHA2_256", null(Varchar))]),
        "hashbytes null nvarchar input" => row(vec![h("SHA2_256", null(Nvarchar))]),
        "hashbytes untyped null input" => error(&[Varchar, UntypedNull]),
        "hashbytes empty varchar" => row(vec![
            h("SHA2_256", varchar("")),
            h("SHA2_256", nvarchar("")),
            h("SHA2_256", varbinary(b"")),
        ]),
        "hashbytes case sensitivity" => row(vec![h("MD5", varchar("ABC")), h("MD5", abc())]),
        "hashbytes trailing spaces" => {
            row(vec![h("MD5", varchar("abc ")), h("MD5", nvarchar("abc "))])
        }
        "hashbytes char padding" => row(vec![
            h("MD5", char_padded("abc", 5)),
            h("MD5", nchar_padded("abc", 5)),
        ]),
        "hashbytes cp1252 character" => row(vec![h("MD5", varchar("é")), h("MD5", nvarchar("é"))]),
        // Both need the UTF-8 collation's bytes, not code page 1252.
        "hashbytes utf8 collation" | "hashbytes utf8 column" => {
            row(vec![h("MD5", varchar_utf8("é"))])
        }
        "hashbytes supplementary character" => row(vec![h("MD5", nvarchar("😀"))]),
        "hashbytes collation clause" => row(vec![h("MD5", abc())]),
        "hashbytes varchar max" => row(vec![h("SHA2_256", abc())]),
        "hashbytes nvarchar max" => row(vec![h("SHA2_256", nvarchar("abc"))]),
        "hashbytes varbinary max" => row(vec![h("SHA2_256", varbinary(b"abc"))]),
        "hashbytes varchar 8000 bytes" => row(vec![h("SHA2_256", varchar(&a(8000)))]),
        "hashbytes varchar 10000 bytes" => row(vec![h("SHA2_256", varchar(&a(10000)))]),
        "hashbytes nvarchar 10000 bytes" => row(vec![h("SHA2_256", nvarchar(&a(5000)))]),
        "hashbytes varbinary 10000 bytes" => {
            row(vec![h("SHA2_512", varbinary(a(10000).as_bytes()))])
        }
        "hashbytes md5 10000 bytes" => row(vec![h("MD5", varchar(&a(10000)))]),
        "hashbytes integer input" => error(&[Varchar, Int]),
        "hashbytes decimal input" => error(&[Varchar, Numeric]),
        "hashbytes datetime input" => error(&[Varchar, Datetime]),
        "hashbytes uniqueidentifier input" => error(&[Varchar, UniqueIdentifier]),
        "hashbytes xml input" => error(&[Varchar, Xml]),
        "hashbytes text input" => error(&[Varchar, Text]),
        "hashbytes one argument" => error(&[Varchar]),
        "hashbytes three arguments" => error(&[Varchar, Varchar, Varchar]),
        "hashbytes unnamed column" => row(vec![h("MD5", abc())]),
        "hashbytes datalength" => row(["MD5", "SHA1", "SHA2_512"]
            .into_iter()
            .map(|name| {
                let algorithm = resolve_algorithm(name).unwrap().unwrap();
                let length = algorithm.digest(b"abc").len();
                assert_eq!(length, algorithm.digest_len());
                json!(length)
            })
            .collect()),
        "hashbytes select into descriptor" => row(vec![
            json!("h"),
            json!("varbinary"),
            json!(RESULT_TYPE.max_length),
            json!(RESULT_TYPE.nullable),
        ]),
        _ => None,
    }
}

fn parameter(kind: &str, value: &Value) -> Arg {
    let (kind, bytes) = match kind {
        "VarChar" => (ArgumentType::Varchar, value.as_str().map(cp1252)),
        "NVarChar" => (ArgumentType::Nvarchar, value.as_str().map(utf16le)),
        "VarBinary" => (
            ArgumentType::Varbinary,
            (!value.is_null()).then(|| unhex(value["value"].as_str().unwrap())),
        ),
        "Int" => (ArgumentType::Int, None),
        other => panic!("unexpected RPC parameter type {other}"),
    };
    Arg { kind, bytes }
}

fn compare(name: &str, expected: Expected, result: &Value) {
    match expected {
        Expected::Rows(rows) => {
            assert!(result["errors"].as_array().unwrap().is_empty(), "{name}");
            let set = &result["sets"][0];
            assert_eq!(set["rows"], json!(rows), "{name}");
            let mut hash_columns = 0;
            for column in set["columns"].as_array().unwrap() {
                if column["type"] == "VarBinary" {
                    assert_eq!(column["length"], json!(RESULT_TYPE.max_length), "{name}");
                    hash_columns += 1;
                }
            }
            if name != "hashbytes datalength" && name != "hashbytes select into descriptor" {
                assert!(hash_columns > 0, "{name}");
            }
        }
        Expected::Error(arguments) => {
            let Err(BindError::Sql {
                number,
                state,
                class,
                message,
            }) = bind(&arguments)
            else {
                panic!("{name}: expected a SQL error");
            };
            assert!(result["sets"].as_array().unwrap().is_empty(), "{name}");
            let errors = result["errors"].as_array().unwrap();
            assert_eq!(errors.len(), 1, "{name}");
            let captured = &errors[0];
            assert_eq!(captured["number"], json!(number), "{name}");
            assert_eq!(captured["state"], json!(state), "{name}");
            assert_eq!(captured["class"], json!(class), "{name}");
            assert_eq!(captured["message"], json!(message), "{name}");
        }
    }
}

#[test]
fn every_captured_hashbytes_case_in_every_run() {
    let fixture: Value =
        serde_json::from_str(include_str!("../../../reference/hashbytes-checksum.json")).unwrap();
    let mut runs = 0;
    for container in fixture["containers"].as_array().unwrap() {
        for run in container["runs"].as_array().unwrap() {
            runs += 1;
            let (mut batches, mut rpcs, mut executions) = (0, 0, 0);
            for case in run.as_array().unwrap() {
                let name = case["name"].as_str().unwrap();
                if !case["sql"].as_str().unwrap().contains("HASHBYTES") {
                    continue;
                }
                if let Some(prepared) = case.get("prepared") {
                    // The capture script declares @algorithm VARCHAR(20) and
                    // @input NVARCHAR(20). Columns c and b are CHECKSUM and
                    // BINARY_CHECKSUM, which this module does not implement.
                    let column = &prepared["prepare"]["sets"][0]["columns"][0];
                    assert_eq!(column["type"], "VarBinary");
                    assert_eq!(column["length"], json!(RESULT_TYPE.max_length));
                    for execution in prepared["executions"].as_array().unwrap() {
                        let values = &execution["values"];
                        let expected = cell(
                            parameter("VarChar", &values["algorithm"]),
                            parameter("NVarChar", &values["input"]),
                        );
                        assert_eq!(execution["result"]["sets"][0]["rows"][0][0], expected);
                        executions += 1;
                    }
                } else if let Some(parameters) = case.get("parameters") {
                    let [algorithm, input] = &parameters.as_array().unwrap()[..] else {
                        panic!("{name}: expected two parameters");
                    };
                    let algorithm =
                        parameter(algorithm["type"].as_str().unwrap(), &algorithm["value"]);
                    let input = parameter(input["type"].as_str().unwrap(), &input["value"]);
                    let expected = if bind(&[algorithm.kind, input.kind]).is_err() {
                        Expected::Error(vec![algorithm.kind, input.kind])
                    } else {
                        Expected::Rows(vec![vec![cell(algorithm, input)]])
                    };
                    compare(name, expected, &case["result"]);
                    rpcs += 1;
                } else {
                    let expected =
                        batch_case(name).unwrap_or_else(|| panic!("unmapped fixture case {name}"));
                    compare(name, expected, &case["result"]);
                    batches += 1;
                }
            }
            assert_eq!((batches, rpcs, executions), (67, 11, 5));
        }
    }
    assert_eq!(runs, 4);
}
