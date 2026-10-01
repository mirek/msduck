//! BACKUP DATABASE, RESTORE HEADERONLY/FILELISTONLY/VERIFYONLY/DATABASE and
//! msdb history with in-process sessions. Expected numbers, states and
//! messages come from reference/gaps-backup.json; the tedious client view is
//! covered by tests/compat/backup.test.mjs.
use msduck::engine::Session;
use msduck::server::Server;
use std::path::{Path, PathBuf};

fn run(session: &mut Session, sql: &str) -> (Vec<u8>, bool) {
    session.batch_response(sql, &Default::default(), false, None)
}

fn ok(session: &mut Session, sql: &str) {
    let (tokens, success) = run(session, sql);
    assert!(success, "{sql}: {:?}", diagnostics(&tokens));
}

/// ERROR and INFO tokens (number, state, message) of a response made of
/// diagnostics, environment changes and DONE tokens; result sets end the
/// scan.
fn diagnostics(tokens: &[u8]) -> Vec<(char, i32, u8, String)> {
    let mut found = Vec::new();
    let mut at = 0;
    while at < tokens.len() {
        match tokens[at] {
            kind @ (0xaa | 0xab) => {
                let length = u16::from_le_bytes([tokens[at + 1], tokens[at + 2]]) as usize;
                let body = &tokens[at + 3..at + 3 + length];
                let number = i32::from_le_bytes(body[..4].try_into().unwrap());
                let state = body[4];
                let units = u16::from_le_bytes([body[6], body[7]]) as usize;
                let message: Vec<u16> = body[8..8 + units * 2]
                    .chunks(2)
                    .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
                    .collect();
                found.push((
                    if kind == 0xaa { 'E' } else { 'I' },
                    number,
                    state,
                    String::from_utf16_lossy(&message),
                ));
                at += 3 + length;
            }
            0xe3 => at += 3 + u16::from_le_bytes([tokens[at + 1], tokens[at + 2]]) as usize,
            0xfd..=0xff => at += 13,
            _ => break,
        }
    }
    found
}

fn errors(session: &mut Session, sql: &str) -> Vec<(i32, u8)> {
    let (tokens, success) = run(session, sql);
    assert!(!success, "{sql} succeeded");
    diagnostics(&tokens)
        .into_iter()
        .filter(|(kind, ..)| *kind == 'E')
        .map(|(_, number, state, _)| (number, state))
        .collect()
}

fn infos(session: &mut Session, sql: &str) -> Vec<i32> {
    let (tokens, success) = run(session, sql);
    assert!(success, "{sql}: {:?}", diagnostics(&tokens));
    diagnostics(&tokens)
        .into_iter()
        .filter(|(kind, ..)| *kind == 'I')
        .map(|(_, number, ..)| number)
        .collect()
}

fn scalar<T: duckdb::types::FromSql>(session: &Session, sql: &str) -> T {
    session.db.query_row(sql, [], |row| row.get(0)).unwrap()
}

/// A fresh directory for backup files.
fn directory(name: &str) -> PathBuf {
    let directory = std::env::temp_dir().join(format!(
        "msduck-gaps-backup-{name}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&directory).unwrap();
    directory
}

fn path(directory: &Path, file: &str) -> String {
    directory.join(file).to_str().unwrap().to_owned()
}

/// Rows of a backend query as text: NVARCHAR carriers are decoded from
/// UTF-16, NULL is "NULL", and other values should be cast to VARCHAR.
fn rows(session: &Session, sql: &str) -> Vec<Vec<String>> {
    use duckdb::types::Value;
    fn text(value: Value) -> String {
        match value {
            Value::Null => "NULL".into(),
            Value::Text(text) => text,
            Value::Struct(fields) => match fields.values().next() {
                Some(Value::Blob(bytes)) => String::from_utf16_lossy(
                    &bytes
                        .chunks(2)
                        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
                        .collect::<Vec<_>>(),
                ),
                other => format!("{other:?}"),
            },
            other => format!("{other:?}"),
        }
    }
    let mut statement = session.db.prepare(sql).unwrap();
    let mut result = statement.query([]).unwrap();
    let mut out = Vec::new();
    while let Some(row) = result.next().unwrap() {
        let columns = row.as_ref().column_count();
        out.push((0..columns).map(|i| text(row.get(i).unwrap())).collect());
    }
    out
}

fn row(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
}

fn fixture(session: &mut Session, database: &str) {
    ok(session, &format!("CREATE DATABASE {database}"));
    ok(session, &format!("USE {database}"));
    ok(
        session,
        "CREATE TABLE dbo.t(id INT IDENTITY(1,1) PRIMARY KEY, v NVARCHAR(20) NOT NULL, d DATETIME NULL)",
    );
    ok(
        session,
        "INSERT dbo.t(v, d) VALUES (N'a', '2024-01-02T03:04:05'), (N'ü', NULL)",
    );
    ok(session, "CREATE VIEW dbo.v AS SELECT id, v FROM dbo.t");
    ok(session, "USE master");
}

#[test]
fn backup_restores_into_a_clone_with_contents_files_and_history() {
    let backups = directory("clone");
    let device = path(&backups, "foo.bak");
    let server = Server::open(":memory:").unwrap();
    let mut session = Session::new(server.connection().unwrap()).unwrap();
    fixture(&mut session, "foo");
    assert_eq!(
        infos(
            &mut session,
            &format!(
                "BACKUP DATABASE foo TO DISK=N'{device}' WITH FORMAT, INIT, NAME=N'foo', DESCRIPTION=N'full', COMPRESSION, CHECKSUM, STATS"
            )
        ),
        [
            3211, 3211, 3211, 3211, 3211, 3211, 3211, 3211, 3211, 3211, 4035, 4035, 3014
        ]
    );
    // msdb was created with SQL Server's ID and records the backup.
    assert_eq!(
        rows(
            &session,
            "SELECT CAST(database_id AS VARCHAR) FROM sys.databases WHERE name='msdb'"
        ),
        [row(&["4"])]
    );
    let (tokens, success) = run(&mut session, "SELECT TOP(1) * FROM msdb..backupmediafamily");
    assert!(success, "{:?}", diagnostics(&tokens));
    let device_units: Vec<u8> = device.encode_utf16().flat_map(u16::to_le_bytes).collect();
    assert!(
        tokens
            .windows(device_units.len())
            .any(|window| window == device_units),
        "backupmediafamily does not name the device"
    );
    assert_eq!(
        rows(
            &session,
            "SELECT physical_device_name, CAST(device_type AS VARCHAR), CAST(family_sequence_number AS VARCHAR) FROM msdb.dbo.backupmediafamily"
        ),
        [row(&[&device, "2", "1"])]
    );
    assert_eq!(
        rows(
            &session,
            "SELECT database_name, name, description, type, CAST(position AS VARCHAR), CAST(has_backup_checksums AS VARCHAR), CAST(is_copy_only AS VARCHAR), compression_algorithm, recovery_model, CAST(flags AS VARCHAR) FROM msdb.dbo.backupset"
        ),
        [row(&[
            "foo",
            "foo",
            "full",
            "D",
            "1",
            "true",
            "false",
            "MS_XPRESS",
            "SIMPLE",
            "528"
        ])]
    );
    assert_eq!(
        rows(
            &session,
            "SELECT logical_name, file_type FROM msdb.dbo.backupfile ORDER BY file_number"
        ),
        [row(&["foo", "D"]), row(&["foo_log", "L"])]
    );
    assert_eq!(
        rows(
            &session,
            "SELECT CAST(is_compressed AS VARCHAR), software_name FROM msdb.dbo.backupmediaset"
        ),
        [row(&["true", "Microsoft SQL Server"])]
    );
    // A clone, with REPLACE, MOVE, RECOVERY and STATS.
    assert_eq!(
        infos(
            &mut session,
            &format!(
                "RESTORE DATABASE foo_copy FROM DISK=N'{device}' WITH REPLACE, MOVE N'foo' TO N'/data/foo_copy.mdf', MOVE N'foo_log' TO N'/data/foo_copy.ldf', RECOVERY, STATS=50"
            )
        ),
        [3211, 3211, 4035, 4035, 3014]
    );
    // The source can go; the clone is independent.
    ok(&mut session, "DROP DATABASE foo");
    ok(&mut session, "USE foo_copy");
    assert_eq!(
        rows(
            &session,
            "SELECT CAST(id AS VARCHAR), v, CAST(d AS VARCHAR) FROM foo_copy.dbo.t ORDER BY id"
        ),
        [
            row(&["1", "a", "2024-01-02 03:04:05"]),
            row(&["2", "ü", "NULL"])
        ]
    );
    assert_eq!(
        rows(
            &session,
            "SELECT CAST(count(*) AS VARCHAR) FROM foo_copy.dbo.v"
        ),
        [row(&["2"])]
    );
    // IDENTITY continues and declared types still apply.
    ok(&mut session, "INSERT dbo.t(v) VALUES (N'c')");
    assert_eq!(
        rows(
            &session,
            "SELECT CAST(max(id) AS VARCHAR) FROM foo_copy.dbo.t"
        ),
        [row(&["3"])]
    );
    assert_eq!(
        errors(
            &mut session,
            "INSERT dbo.t(v) VALUES (N'this value is far too long')"
        ),
        [(2628, 1)]
    );
    assert_eq!(
        rows(
            &session,
            "SELECT CAST(file_id AS VARCHAR), CAST(type AS VARCHAR), type_desc, name, physical_name, state_desc FROM foo_copy.sys.database_files"
        ),
        [
            row(&["1", "0", "ROWS", "foo", "/data/foo_copy.mdf", "ONLINE"]),
            row(&["2", "1", "LOG", "foo_log", "/data/foo_copy.ldf", "ONLINE"])
        ]
    );
    assert_eq!(
        rows(
            &session,
            "SELECT CAST(database_id AS VARCHAR), CAST(file_id AS VARCHAR), name FROM sys.master_files WHERE database_id IN (1, 4) ORDER BY database_id, file_id"
        ),
        [
            row(&["1", "1", "master"]),
            row(&["1", "2", "mastlog"]),
            row(&["4", "1", "MSDBData"]),
            row(&["4", "2", "MSDBLog"])
        ]
    );
    ok(&mut session, "USE master");
    assert_eq!(
        rows(
            &session,
            "SELECT destination_database_name, restore_type, CAST(\"replace\" AS VARCHAR), CAST(recovery AS VARCHAR), CAST(backup_set_id AS VARCHAR), user_name FROM msdb.dbo.restorehistory"
        ),
        [row(&["foo_copy", "D", "true", "true", "1", "sa"])]
    );
    assert_eq!(
        rows(
            &session,
            "SELECT destination_phys_name FROM msdb.dbo.restorefile ORDER BY file_number"
        ),
        [row(&["/data/foo_copy.mdf"]), row(&["/data/foo_copy.ldf"])]
    );
    std::fs::remove_dir_all(&backups).unwrap();
}

#[test]
fn restore_follows_sql_server_errors_and_replace_semantics() {
    let backups = directory("errors");
    let device = path(&backups, "foo.bak");
    let server = Server::open(":memory:").unwrap();
    let mut session = Session::new(server.connection().unwrap()).unwrap();
    fixture(&mut session, "foo");
    ok(
        &mut session,
        &format!("BACKUP DATABASE foo TO DISK=N'{device}'"),
    );
    // Another database without REPLACE: 3154. REPLACE overwrites it in
    // place, keeping its database ID.
    ok(&mut session, "CREATE DATABASE bar");
    let bar: i32 = scalar(
        &session,
        "SELECT database_id FROM sys.databases WHERE name='bar'",
    );
    let restore_bar = format!(
        "RESTORE DATABASE bar FROM DISK=N'{device}' WITH MOVE N'foo' TO N'/data/bar.mdf', MOVE N'foo_log' TO N'/data/bar.ldf'"
    );
    assert_eq!(errors(&mut session, &restore_bar), [(3154, 4), (3013, 1)]);
    ok(&mut session, &format!("{restore_bar}, REPLACE"));
    assert_eq!(
        scalar::<i32>(
            &session,
            "SELECT database_id FROM sys.databases WHERE name='bar'"
        ),
        bar
    );
    assert_eq!(scalar::<i64>(&session, "SELECT count(*) FROM bar.dbo.t"), 2);
    // In use by this session: 3102; by another session: 3101.
    ok(&mut session, "USE bar");
    assert_eq!(
        errors(&mut session, &format!("{restore_bar}, REPLACE")),
        [(3102, 1), (3013, 1)]
    );
    ok(&mut session, "USE master");
    let mut other = Session::new(server.connection().unwrap()).unwrap();
    ok(&mut other, "USE bar");
    assert_eq!(
        errors(&mut session, &format!("{restore_bar}, REPLACE")),
        [(3101, 1), (3013, 1)]
    );
    drop(other);
    // The same family restores without REPLACE.
    ok(&mut session, &restore_bar);
    // Without MOVE, the backed up files belong to foo.
    assert_eq!(
        errors(
            &mut session,
            &format!("RESTORE DATABASE foo_y FROM DISK=N'{device}'")
        ),
        [
            (1834, 1),
            (3156, 4),
            (1834, 1),
            (3156, 4),
            (3119, 1),
            (3013, 1)
        ]
    );
    assert_eq!(
        errors(
            &mut session,
            &format!(
                "RESTORE DATABASE foo_z FROM DISK=N'{device}' WITH MOVE N'nope' TO N'/data/z.mdf'"
            )
        ),
        [(3234, 2), (3013, 1)]
    );
    let missing = path(&backups, "missing.bak");
    let (tokens, _) = run(
        &mut session,
        &format!("RESTORE DATABASE foo_x FROM DISK=N'{missing}'"),
    );
    assert_eq!(
        diagnostics(&tokens)
            .into_iter()
            .map(|(_, number, state, message)| (number, state, message))
            .collect::<Vec<_>>(),
        [
            (
                3201,
                2,
                format!(
                    "Cannot open backup device '{missing}'. Operating system error 2(The system cannot find the file specified.)."
                )
            ),
            (
                3013,
                1,
                "RESTORE DATABASE is terminating abnormally.".into()
            )
        ]
    );
    assert_eq!(
        errors(
            &mut session,
            &format!("RESTORE HEADERONLY FROM DISK=N'{missing}'")
        ),
        [(3201, 2), (3013, 1)]
    );
    assert_eq!(
        errors(
            &mut session,
            &format!("RESTORE DATABASE foo_x FROM DISK=N'{device}' WITH FILE=3")
        ),
        [(3287, 1), (3013, 1)]
    );
    assert_eq!(
        errors(
            &mut session,
            &format!(
                "BEGIN TRAN; RESTORE DATABASE foo_x FROM DISK=N'{device}' WITH MOVE N'foo' TO N'/data/x.mdf', MOVE N'foo_log' TO N'/data/x.ldf'"
            )
        ),
        [(3021, 0), (3013, 1)]
    );
    ok(&mut session, "ROLLBACK");
    assert_eq!(
        errors(&mut session, "BACKUP DATABASE nosuch TO DISK=N'/tmp/x.bak'"),
        [(911, 11), (3013, 1)]
    );
    assert_eq!(
        errors(
            &mut session,
            &format!("BACKUP LOG foo TO DISK=N'{}'", path(&backups, "log.bak"))
        ),
        [(4208, 1), (3013, 1)]
    );
    assert_eq!(
        errors(
            &mut session,
            &format!(
                "BACKUP DATABASE foo TO DISK=N'{}'",
                path(&backups, "nodir/x.bak")
            )
        ),
        [(3201, 1), (3013, 1)]
    );
    assert_eq!(
        errors(
            &mut session,
            &format!("BACKUP DATABASE foo TO DISK=N'{device}' WITH BOGUS")
        ),
        [(155, 1)]
    );
    assert_eq!(errors(&mut session, "DROP DATABASE msdb"), [(3708, 4)]);
    // A backup never overwrites a database file.
    let file: String = scalar(
        &session,
        "SELECT path FROM duckdb_databases() WHERE database_name='foo'",
    );
    assert_eq!(
        errors(
            &mut session,
            &format!("BACKUP DATABASE foo TO DISK=N'{file}'")
        ),
        [(3201, 1), (3013, 1)]
    );
    // A damaged payload is detected.
    let mut bytes = std::fs::read(&device).unwrap();
    let last = bytes.len() - 100;
    bytes[last] ^= 0xff;
    let damaged = path(&backups, "damaged.bak");
    std::fs::write(&damaged, bytes).unwrap();
    assert_eq!(
        errors(
            &mut session,
            &format!("RESTORE VERIFYONLY FROM DISK=N'{damaged}'")
        )
        .len(),
        2
    );
    std::fs::write(&damaged, b"not a backup, but long enough to be one").unwrap();
    assert_eq!(
        errors(
            &mut session,
            &format!("RESTORE HEADERONLY FROM DISK=N'{damaged}'")
        ),
        [(3241, 0), (3013, 1)]
    );
    std::fs::write(&damaged, b"short").unwrap();
    assert_eq!(
        errors(
            &mut session,
            &format!("RESTORE HEADERONLY FROM DISK=N'{damaged}'")
        ),
        [(3254, 1), (3013, 1)]
    );
    std::fs::remove_dir_all(&backups).unwrap();
}

#[test]
fn noinit_appends_backup_sets_that_restore_by_position() {
    let backups = directory("append");
    let device = path(&backups, "foo.bak");
    let server = Server::open(":memory:").unwrap();
    let mut session = Session::new(server.connection().unwrap()).unwrap();
    fixture(&mut session, "foo");
    ok(
        &mut session,
        &format!("BACKUP DATABASE foo TO DISK=N'{device}'"),
    );
    ok(
        &mut session,
        "USE foo; INSERT dbo.t(v) VALUES (N'later'); USE master",
    );
    // Variables name the database and the device.
    ok(
        &mut session,
        &format!(
            "DECLARE @db nvarchar(128) = N'foo', @device nvarchar(400) = N'{device}'; BACKUP DATABASE @db TO DISK = @device WITH NOINIT, COPY_ONLY"
        ),
    );
    assert_eq!(
        rows(
            &session,
            "SELECT CAST(position AS VARCHAR), CAST(media_set_id AS VARCHAR), CAST(is_copy_only AS VARCHAR) FROM msdb.dbo.backupset ORDER BY backup_set_id"
        ),
        [row(&["1", "1", "false"]), row(&["2", "1", "true"])]
    );
    for (file, count) in [(1, 2), (2, 3)] {
        ok(
            &mut session,
            &format!(
                "RESTORE DATABASE f{file} FROM DISK=N'{device}' WITH FILE={file}, MOVE N'foo' TO N'/data/f{file}.mdf', MOVE N'foo_log' TO N'/data/f{file}.ldf'"
            ),
        );
        assert_eq!(
            scalar::<i64>(&session, &format!("SELECT count(*) FROM f{file}.dbo.t")),
            count
        );
    }
    // INIT keeps the media but replaces its sets.
    ok(
        &mut session,
        &format!("BACKUP DATABASE f1 TO DISK=N'{device}' WITH INIT"),
    );
    assert_eq!(
        errors(
            &mut session,
            &format!("RESTORE FILELISTONLY FROM DISK=N'{device}' WITH FILE=2")
        ),
        [(3287, 1), (3013, 1)]
    );
    assert_eq!(
        infos(
            &mut session,
            &format!("RESTORE VERIFYONLY FROM DISK=N'{device}'")
        ),
        [3262]
    );
    assert_eq!(
        rows(
            &session,
            "SELECT CAST(count(DISTINCT media_set_id) AS VARCHAR), CAST(max(position) AS VARCHAR) FROM msdb.dbo.backupset"
        ),
        [row(&["1", "2"])]
    );
    std::fs::remove_dir_all(&backups).unwrap();
}

#[test]
fn backups_are_transactionally_consistent() {
    let backups = directory("consistent");
    let device = path(&backups, "foo.bak");
    let server = Server::open(":memory:").unwrap();
    let mut session = Session::new(server.connection().unwrap()).unwrap();
    fixture(&mut session, "foo");
    let mut writer = Session::new(server.connection().unwrap()).unwrap();
    ok(&mut writer, "USE foo");
    ok(
        &mut writer,
        "BEGIN TRAN; INSERT dbo.t(v) VALUES (N'uncommitted'); UPDATE dbo.t SET v = N'changed' WHERE id = 1",
    );
    ok(
        &mut session,
        &format!("BACKUP DATABASE foo TO DISK=N'{device}'"),
    );
    ok(&mut writer, "COMMIT");
    ok(
        &mut session,
        &format!(
            "RESTORE DATABASE snap FROM DISK=N'{device}' WITH MOVE N'foo' TO N'/data/snap.mdf', MOVE N'foo_log' TO N'/data/snap.ldf'"
        ),
    );
    assert_eq!(
        rows(
            &session,
            "SELECT CAST(id AS VARCHAR), v FROM snap.dbo.t ORDER BY id"
        ),
        [row(&["1", "a"]), row(&["2", "ü"])]
    );
    assert_eq!(scalar::<i64>(&session, "SELECT count(*) FROM foo.dbo.t"), 3);
    std::fs::remove_dir_all(&backups).unwrap();
}

#[test]
fn file_backed_servers_keep_backups_msdb_and_restored_databases() {
    let data = directory("file-backed");
    let device = path(&data, "foo.bak");
    let primary = path(&data, "primary.duckdb");
    {
        let server = Server::open(&primary).unwrap();
        let mut session = Session::new(server.connection().unwrap()).unwrap();
        fixture(&mut session, "foo");
        ok(
            &mut session,
            &format!("BACKUP DATABASE foo TO DISK=N'{device}' WITH FORMAT"),
        );
        ok(
            &mut session,
            &format!(
                "RESTORE DATABASE foo_copy FROM DISK=N'{device}' WITH MOVE N'foo' TO N'/data/foo_copy.mdf', MOVE N'foo_log' TO N'/data/foo_copy.ldf'"
            ),
        );
        // master can be backed up too, but not restored.
        let master = path(&data, "master.bak");
        ok(
            &mut session,
            &format!("BACKUP DATABASE master TO DISK=N'{master}'"),
        );
        assert_eq!(
            errors(
                &mut session,
                &format!(
                    "RESTORE DATABASE m2 FROM DISK=N'{master}' WITH MOVE N'master' TO N'/data/m2.mdf', MOVE N'mastlog' TO N'/data/m2.ldf'"
                )
            )
            .last(),
            Some(&(3013, 1))
        );
    }
    let server = Server::open(&primary).unwrap();
    let session = Session::new(server.connection().unwrap()).unwrap();
    assert_eq!(
        rows(
            &session,
            "SELECT CAST(database_id AS VARCHAR) FROM sys.databases WHERE name = 'msdb'"
        ),
        [row(&["4"])]
    );
    assert_eq!(
        scalar::<i64>(&session, "SELECT count(*) FROM msdb.dbo.backupset"),
        2
    );
    assert_eq!(
        scalar::<i64>(&session, "SELECT count(*) FROM msdb.dbo.restorehistory"),
        1
    );
    assert_eq!(
        scalar::<i64>(&session, "SELECT count(*) FROM foo_copy.dbo.t"),
        2
    );
    assert_eq!(
        rows(
            &session,
            "SELECT name, physical_name FROM foo_copy.sys.database_files ORDER BY file_id"
        ),
        [
            row(&["foo", "/data/foo_copy.mdf"]),
            row(&["foo_log", "/data/foo_copy.ldf"])
        ]
    );
    drop(session);
    drop(server);
    std::fs::remove_dir_all(&data).unwrap();
}
