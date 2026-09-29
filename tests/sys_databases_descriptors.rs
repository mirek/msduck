use msduck::engine::Session;
use msduck::server::Server;
use serde_json::Value;

const REFERENCE: &str = include_str!("../reference/sys-databases.json");

fn observation<'a>(fixture: &'a Value, name: &str) -> &'a Value {
    fixture["runs"][0]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["name"] == name)
        .unwrap_or_else(|| panic!("missing reference observation: {name}"))
}

// Encode only the pinned SQL Server descriptor families this view publishes.
// The expected flags, lengths, and collations come from the independent
// Tedious capture; the root metadata path under test produces the actual bytes.
fn reference_metadata(columns: &[Value], names: &[&str]) -> Vec<u8> {
    assert_eq!(columns.len(), names.len());
    let mut output = vec![0x81];
    output.extend_from_slice(&(columns.len() as u16).to_le_bytes());
    for (column, name) in columns.iter().zip(names) {
        output.extend_from_slice(&0u32.to_le_bytes());
        output.extend_from_slice(&(column["flags"].as_u64().unwrap() as u16).to_le_bytes());
        match column["type"].as_str().unwrap() {
            "NVarChar" => {
                output.push(0xe7);
                output
                    .extend_from_slice(&(column["length"].as_u64().unwrap() as u16).to_le_bytes());
                let c = &column["collation"];
                let info = c["lcid"].as_u64().unwrap() as u32
                    | (c["flags"].as_u64().unwrap() as u32) << 20
                    | (c["version"].as_u64().unwrap() as u32) << 28;
                output.extend_from_slice(&info.to_le_bytes());
                output.push(c["sortId"].as_u64().unwrap() as u8);
            }
            "Int" => output.push(0x38),
            "IntN" => output.extend_from_slice(&[0x26, column["length"].as_u64().unwrap() as u8]),
            "DateTime" => output.push(0x3d),
            "TinyInt" => output.push(0x30),
            "BitN" => output.extend_from_slice(&[0x68, column["length"].as_u64().unwrap() as u8]),
            other => panic!("uncaptured descriptor family: {other}"),
        }
        let units: Vec<_> = name.encode_utf16().collect();
        output.push(units.len() as u8);
        for unit in units {
            output.extend_from_slice(&unit.to_le_bytes());
        }
    }
    output
}

fn assert_metadata(session: &mut Session, sql: &str, expected: &[u8]) {
    let (response, ok) = session.batch_response(sql, &Default::default(), false, None);
    assert!(ok, "{sql}: {response:?}");
    assert!(
        response.starts_with(expected),
        "{sql}: metadata differs\nexpected: {expected:02x?}\nactual: {:02x?}",
        &response[..response.len().min(expected.len())]
    );
}

#[test]
fn published_columns_match_reference_for_star_projection_aliases_and_empty_results() {
    let fixture: Value = serde_json::from_str(REFERENCE).unwrap();
    assert_eq!(fixture["runs"][0], fixture["runs"][1]);
    let declarations = observation(&fixture, "declarations")["result"]["sets"][0]["rows"]
        .as_array()
        .unwrap();
    let columns = observation(&fixture, "published empty")["result"]["sets"][0]["columns"]
        .as_array()
        .unwrap();
    let names: Vec<_> = columns
        .iter()
        .map(|column| column["name"].as_str().unwrap())
        .collect();
    assert_eq!(names.len(), 13);
    assert_eq!(declarations.len(), names.len());
    for (index, name) in names.iter().enumerate() {
        assert_eq!(declarations[index][0], *name);
        assert_eq!(
            declarations[index][8],
            columns[index]["flags"].as_u64().unwrap() & 1 != 0
        );
    }
    let expected = reference_metadata(columns, &names);
    let server = Server::open(":memory:").unwrap();
    let db = server.connection().unwrap();
    db.databases().create(&db, "inventory").unwrap();
    let mut session = Session::new(db).unwrap();
    let selected = format!(
        "SELECT {} FROM sys.databases WHERE database_id=-1",
        names.join(",")
    );
    assert_metadata(&mut session, &selected, &expected);
    assert_metadata(
        &mut session,
        "SELECT * FROM sys.databases WHERE database_id=-1",
        &expected,
    );
    assert_metadata(
        &mut session,
        "SELECT * FROM sys.databases WHERE database_id=1",
        &expected,
    );
    // Database-qualified names use SQL Server database names and resolve
    // within the session's current database.
    assert_metadata(
        &mut session,
        "SELECT * FROM master.sys.databases WHERE database_id=-1",
        &expected,
    );
    session.use_database("inventory").unwrap();
    assert_metadata(
        &mut session,
        "SELECT * FROM inventory.sys.databases WHERE database_id=-1",
        &expected,
    );
    let aliases = ["database_name", "server_collation"];
    let selected = "SELECT name AS database_name, collation_name AS server_collation FROM sys.databases WHERE database_id=-1";
    let expected_aliases = reference_metadata(&[columns[0].clone(), columns[5].clone()], &aliases);
    assert_metadata(&mut session, selected, &expected_aliases);
    // The pinned image has 98 sys.databases columns; msduck currently publishes
    // only 13. Do not synthesize declarations for the unsupported remainder.
    assert_eq!(
        observation(&fixture, "complete empty")["result"]["sets"][0]["columns"]
            .as_array()
            .unwrap()
            .len(),
        98
    );
}
