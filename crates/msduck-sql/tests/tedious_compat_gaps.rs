//! Syntax from issue #697 over the public parser and hint rules; engine and
//! client behavior are covered by tests/tedious_compat_gaps.{rs,test.mjs}.
use msduck_sql::dialect::{computed_column, table_hints};
use sqlparser::ast::Statement;

#[test]
fn reported_batch_parses_and_normalizes() {
    let mut statements = msduck_sql::batch::parse(
        "create table ComputedProbe (id int not null identity(1, 1) primary key, name varchar(100) null, nameUpper as upper(name) persisted);
         create index IX_ComputedProbe_nameUpper on ComputedProbe (nameUpper);
         create table NonclusteredProbe (content nvarchar(max) not null, [version] varchar(64) not null constraint NonclusteredProbe_Pk primary key nonclustered);
         create table HintProbe (id int not null primary key);
         select max(id) from HintProbe with (readcommittedlock);
         select count(*) from HintProbe (nolock)",
    )
    .unwrap();
    table_hints::normalize(&mut statements).unwrap();
    assert_eq!(statements.len(), 6);
    let Statement::CreateTable(computed) = &statements[0] else {
        panic!("CREATE TABLE expected")
    };
    let (expr, persisted) = computed_column::computed(&computed.columns[2]).unwrap();
    assert_eq!(
        (expr.to_string().as_str(), persisted),
        ("upper(name)", true)
    );
    assert!(
        statements[2]
            .to_string()
            .ends_with("CONSTRAINT NonclusteredProbe_Pk PRIMARY KEY)")
    );
    assert_eq!(
        statements[5].to_string(),
        "SELECT count(*) FROM HintProbe WITH (nolock)"
    );
}
