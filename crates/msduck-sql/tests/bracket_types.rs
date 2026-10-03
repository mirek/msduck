//! Delimited system type names parse like their plain spellings
//! (docs/bracket-types.md).
use msduck_core::{
    character::{CharacterType, Family, Length},
    types::{DecimalType, Type},
};
use msduck_sql::{batch, dialect::ext::conversion::bracket_types::value_type, sql_type};
use sqlparser::ast::{DataType, Ident, ObjectName};

fn text(sql: &str) -> Vec<String> {
    batch::parse(sql)
        .unwrap_or_else(|error| panic!("{sql}: {error}"))
        .iter()
        .map(ToString::to_string)
        .collect()
}

fn same(delimited: &str, plain: &str) {
    assert_eq!(text(delimited), text(plain), "{delimited}");
}

#[test]
fn delimited_names_parse_like_plain_spellings() {
    for (delimited, plain) in [
        (
            "SELECT CONVERT([nvarchar](200), JSON_VALUE(N'{\"a\":\"foo\"}', N'$.a')) AS v",
            "SELECT CONVERT(nvarchar(200), JSON_VALUE(N'{\"a\":\"foo\"}', N'$.a')) AS v",
        ),
        (
            "SELECT CAST(1 AS [int]), TRY_CAST('1' AS [bigint]), TRY_CONVERT([decimal](10, 2), '1.5')",
            "SELECT CAST(1 AS int), TRY_CAST('1' AS bigint), TRY_CONVERT(decimal(10, 2), '1.5')",
        ),
        (
            "SELECT CONVERT([datetime2](3), x), CONVERT([varbinary](max), y), CONVERT([varchar](10), z, 112) FROM t",
            "SELECT CONVERT(datetime2(3), x), CONVERT(varbinary(max), y), CONVERT(varchar(10), z, 112) FROM t",
        ),
        (
            "SELECT CAST(1 AS [INT]), CAST(1 AS [int ]), CAST(1 AS \"int\"), CAST(1 AS [sys].[int]), CAST(1 AS sys.int)",
            "SELECT CAST(1 AS INT), CAST(1 AS int), CAST(1 AS int), CAST(1 AS int), CAST(1 AS int)",
        ),
        (
            "SELECT CAST(1.5 AS [money]), CAST(N'a' AS [nchar](2)), CAST('a' AS [uniqueidentifier]), CAST(1 AS [float](24))",
            "SELECT CAST(1.5 AS money), CAST(N'a' AS nchar(2)), CAST('a' AS uniqueidentifier), CAST(1 AS float(24))",
        ),
        (
            "CREATE TABLE [dbo].[items] ([id] [int] NOT NULL, [name] [nvarchar](100) NULL, [amount] [decimal](18, 2), [value] AS (CONVERT([nvarchar](200), JSON_VALUE([name], N'$.a'))) PERSISTED, [tenant] [nvarchar](450) DEFAULT CONVERT([nvarchar](450), SESSION_CONTEXT(N'foo')))",
            "CREATE TABLE [dbo].[items] ([id] int NOT NULL, [name] nvarchar(100) NULL, [amount] decimal(18, 2), [value] AS (CONVERT(nvarchar(200), JSON_VALUE([name], N'$.a'))) PERSISTED, [tenant] nvarchar(450) DEFAULT CONVERT(nvarchar(450), SESSION_CONTEXT(N'foo')))",
        ),
        (
            "ALTER TABLE [dbo].[items] ADD [extra] [varchar](10) NULL, [more] [nchar](3) NULL",
            "ALTER TABLE [dbo].[items] ADD [extra] varchar(10) NULL, [more] nchar(3) NULL",
        ),
        (
            "ALTER TABLE [dbo].[items] ALTER COLUMN [extra] [nvarchar](20) NULL",
            "ALTER TABLE [dbo].[items] ALTER COLUMN [extra] nvarchar(20) NULL",
        ),
        (
            "DECLARE @x [nvarchar](10) = N'abc', @y [int] = 5, @z AS [decimal](5, 2)",
            "DECLARE @x nvarchar(10) = N'abc', @y int = 5, @z AS decimal(5, 2)",
        ),
        (
            "IF 1 = 1 BEGIN SELECT CONVERT([nvarchar](5), 123) END ELSE SELECT (SELECT CAST(x AS [nvarchar](3)) FROM v)",
            "IF 1 = 1 BEGIN SELECT CONVERT(nvarchar(5), 123) END ELSE SELECT (SELECT CAST(x AS nvarchar(3)) FROM v)",
        ),
        (
            "SELECT * FROM OPENJSON(@j) WITH ([id] [int] '$.id', [name] [nvarchar](20), [who] sysname)",
            "SELECT * FROM OPENJSON(@j) WITH ([id] int '$.id', [name] nvarchar(20), [who] nvarchar(128))",
        ),
        (
            "CREATE VIEW dbo.v AS SELECT CAST(1 AS [bigint]) AS c",
            "CREATE VIEW dbo.v AS SELECT CAST(1 AS bigint) AS c",
        ),
        (
            "CREATE TABLE names (a [sysname] NULL, b sysname)",
            "CREATE TABLE names (a sysname NULL, b sysname)",
        ),
        (
            "DECLARE @x sysname, @y [sysname]; SELECT CAST(N'x' AS sysname), CONVERT([sysname], N'y')",
            "DECLARE @x nvarchar(128), @y nvarchar(128); SELECT CAST(N'x' AS nvarchar(128)), CONVERT(nvarchar(128), N'y')",
        ),
    ] {
        same(delimited, plain);
    }
}

#[test]
fn names_that_are_not_system_types_are_unchanged() {
    for sql in [
        "SELECT [int], [date], t.[time] FROM (SELECT 1 AS [int], 2 [date], 3 AS [time]) t",
        "INSERT t ([int], [date]) VALUES (1, 2)",
        "CREATE TABLE t ([int] int, [date] date)",
        "SELECT CAST(1 AS [notatype]), CONVERT([notatype](5), 1), CAST(1 AS [dbo].[int])",
    ] {
        let parsed = text(sql);
        assert_eq!(text(&parsed.join("; ")), parsed, "{sql}");
    }
    assert_eq!(
        text("SELECT CAST(1 AS [notatype]), CAST(1 AS [dbo].[int])"),
        ["SELECT CAST(1 AS [notatype]), CAST(1 AS [dbo].[int])"]
    );
}

#[test]
fn statements_around_a_resolved_statement_keep_their_boundaries() {
    same(
        "SELECT 1 AS a SELECT CAST(2 AS [int]) AS b SELECT 3 AS c",
        "SELECT 1 AS a SELECT CAST(2 AS int) AS b SELECT 3 AS c",
    );
    same(
        "SELECT CAST(1 AS [int]) AS a; SELECT [int] FROM (SELECT 4 AS [int]) t; DECLARE @v [bit]",
        "SELECT CAST(1 AS int) AS a; SELECT [int] FROM (SELECT 4 AS [int]) t; DECLARE @v bit",
    );
    assert_eq!(text("SELECT 1 /* [int] */ SELECT 2").len(), 2);
    assert!(batch::parse("SELECT CAST(1 AS [int] AS a").is_err());
    assert!(batch::parse("SELECT CONVERT([nvarchar](5) 1)").is_err());
}

#[test]
fn declarations_resolve_delimited_names_and_sysname() {
    let custom =
        |name: Ident, args: Vec<String>| DataType::Custom(ObjectName::from(vec![name]), args);
    let declared = |kind: DataType| sql_type::declaration(&kind).unwrap();
    assert_eq!(
        declared(custom(Ident::with_quote('[', "int"), vec![])),
        Type::Int
    );
    assert_eq!(
        declared(custom(
            Ident::with_quote('[', "decimal"),
            vec!["5".into(), "2".into()]
        )),
        Type::Decimal(DecimalType::new(5, 2).unwrap())
    );
    let sysname =
        Type::Character(CharacterType::new(Family::Nvarchar, Length::Bounded(128)).unwrap());
    assert_eq!(declared(custom(Ident::new("sysname"), vec![])), sysname);
    assert_eq!(
        declared(custom(Ident::with_quote('[', "sysname"), vec![])),
        sysname
    );
    let error = sql_type::declaration(&custom(Ident::with_quote('[', "notatype"), vec![]));
    assert_eq!(
        error.unwrap_err().to_string(),
        "unsupported scalar declaration [notatype]"
    );

    let parameters =
        batch::parameter_declarations("@a [int], @b [nvarchar](5), @c sysname, @d [sys].[bit]")
            .unwrap();
    let kinds: Vec<_> = parameters.into_iter().map(|(_, kind)| kind).collect();
    assert_eq!(
        kinds,
        [
            Type::Int,
            Type::Character(CharacterType::new(Family::Nvarchar, Length::Bounded(5)).unwrap()),
            sysname,
            Type::Bit,
        ]
    );
}

#[test]
fn value_types_name_only_system_types() {
    let custom = |parts: Vec<Ident>| DataType::Custom(ObjectName::from(parts), vec![]);
    assert_eq!(
        value_type(&custom(vec![Ident::with_quote('[', "int")])),
        Some(DataType::Int(None))
    );
    assert_eq!(
        value_type(&custom(vec![Ident::new("sys"), Ident::new("int")])),
        Some(DataType::Int(None))
    );
    assert_eq!(
        value_type(&custom(vec![Ident::with_quote('[', "notatype")])),
        None
    );
    assert_eq!(
        value_type(&custom(vec![
            Ident::with_quote('[', "dbo"),
            Ident::with_quote('[', "int")
        ])),
        None
    );
    assert_eq!(value_type(&custom(vec![Ident::new("money")])), None);
    assert_eq!(value_type(&DataType::Int(None)), None);
}

#[test]
fn catalog_text_renders_delimited_names_like_sql_server() {
    use msduck_sql::dialect::ext::catalog::definition::source_definition;
    // Captured from SQL Server 2025 (docs/bracket-types.md).
    assert_eq!(
        source_definition("CONVERT([nvarchar](200),JSON_VALUE(body,N'$.a'))").as_deref(),
        Some("(CONVERT([nvarchar](200),json_value([body],N'$.a')))")
    );
    assert_eq!(
        source_definition("CONVERT([nvarchar](450),SESSION_CONTEXT(N'foo'))").as_deref(),
        Some("(CONVERT([nvarchar](450),session_context(N'foo')))")
    );
    assert_eq!(
        source_definition("CAST(SESSION_CONTEXT(N'n') AS [int])").as_deref(),
        Some("(CONVERT([int],session_context(N'n')))")
    );
    assert_eq!(source_definition("CONVERT([notatype],1)"), None);
}
