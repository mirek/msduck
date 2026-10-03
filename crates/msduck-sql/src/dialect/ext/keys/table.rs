//! PRIMARY KEY and UNIQUE constraints of CREATE TABLE.
//!
//! DuckDB enforces a constraint natively when its key columns have indexable
//! storage. That is SQL Server's rule when NULL cannot occur (primary keys,
//! and unique keys over NOT NULL columns). DuckDB lets any number of NULL
//! keys coexist, while SQL Server allows one, so a unique key over nullable
//! columns also gets a keys-managed unique index (see [`super::value`]); the
//! native constraint stays so that foreign keys can reference it. A key over
//! STRUCT storage (Unicode carriers, DATETIME2, DATETIMEOFFSET) is only
//! managed, and is removed from the statement DuckDB sees. DuckDB's native
//! constraints compare VARCHAR values exactly, so a key over CHAR or VARCHAR
//! columns also gets a managed index that ignores trailing spaces, and case
//! under a case-insensitive collation (the database default).
use sqlparser::ast::*;

/// Where a constraint is declared.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Origin {
    /// A column option: column index, option index.
    Column(usize, usize),
    /// A table constraint index.
    Table(usize),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Constraint {
    pub name: Option<String>,
    pub primary: bool,
    pub columns: Vec<String>,
    pub origin: Origin,
    /// Whether DuckDB enforces it as a constraint.
    pub native: bool,
    /// Whether a keys-managed index enforces it (alone, or with the native
    /// constraint for SQL Server's single NULL).
    pub managed: bool,
}

/// A SQL Server error: number, state, severity, message.
pub type Diagnostic = (i32, u8, u8, String);

fn struct_stored(kind: &DataType) -> bool {
    crate::character_storage::unicode_storage_type(kind).is_some()
        || matches!(crate::temporal_scale::datetime2(kind), Ok(Some(_)))
        || matches!(crate::temporal_scale::datetimeoffset(kind), Ok(Some(_)))
}

/// A CHAR or VARCHAR column. DuckDB's native constraint compares the stored
/// text exactly, while SQL Server ignores trailing spaces under every
/// collation and case under case-insensitive ones (the database default).
fn ansi(column: &ColumnDef) -> bool {
    matches!(
        crate::sql_type::declaration(&column.data_type),
        Ok(msduck_core::types::Type::Character(character)) if matches!(
            character.family(),
            msduck_core::character::Family::Char | msduck_core::character::Family::Varchar
        )
    )
}

fn variant(kind: &DataType) -> bool {
    matches!(kind, DataType::Custom(name, _) if name.to_string().eq_ignore_ascii_case("sql_variant"))
}

/// MAX, text, ntext, image and xml columns cannot be index keys.
fn keyable(kind: &DataType) -> bool {
    !matches!(
        kind,
        DataType::Varchar(Some(CharacterLength::Max))
            | DataType::Nvarchar(Some(CharacterLength::Max))
            | DataType::Varbinary(Some(BinaryLength::Max))
            | DataType::CharacterVarying(Some(CharacterLength::Max))
            | DataType::Text
    ) && !matches!(kind, DataType::Custom(name, _) if ["ntext", "image", "xml", "text"]
        .iter()
        .any(|n| name.to_string().eq_ignore_ascii_case(n)))
}

fn not_null(column: &ColumnDef) -> bool {
    column.options.iter().any(|option| {
        matches!(
            option.option,
            ColumnOption::NotNull | ColumnOption::PrimaryKey(_) | ColumnOption::Identity(_)
        )
    })
}

fn explicitly_null(column: &ColumnDef) -> bool {
    column
        .options
        .iter()
        .any(|option| matches!(option.option, ColumnOption::Null))
}

fn key_names(columns: &[IndexColumn]) -> Option<Vec<String>> {
    columns
        .iter()
        .map(|column| match &column.column.expr {
            Expr::Identifier(ident) => Some(ident.value.clone()),
            _ => None,
        })
        .collect()
}

/// SQL Server's follow-up to an invalid key definition.
fn failed(first: Diagnostic) -> Vec<Diagnostic> {
    vec![
        first,
        (
            1750,
            0,
            16,
            "Could not create constraint or index. See previous errors.".into(),
        ),
    ]
}

/// The key constraints of `table`, or SQL Server's errors for an invalid one.
pub fn constraints(table: &CreateTable) -> Result<Vec<Constraint>, Vec<Diagnostic>> {
    find(table).map_err(|error| match error.0 {
        8110 | 8168 | 40515 => vec![error],
        _ => failed(error),
    })
}

fn find(table: &CreateTable) -> Result<Vec<Constraint>, Diagnostic> {
    let table_name = table
        .name
        .0
        .last()
        .map(|part| part.to_string())
        .unwrap_or_default();
    let mut found = vec![];
    for (c, column) in table.columns.iter().enumerate() {
        for (o, option) in column.options.iter().enumerate() {
            let (primary, inner_name) = match &option.option {
                ColumnOption::PrimaryKey(key) => (true, &key.name),
                ColumnOption::Unique(key) => (false, &key.name),
                _ => continue,
            };
            found.push(Constraint {
                name: option
                    .name
                    .as_ref()
                    .or(inner_name.as_ref())
                    .map(|n| n.value.clone()),
                primary,
                columns: vec![column.name.value.clone()],
                origin: Origin::Column(c, o),
                native: true,
                managed: false,
            });
        }
    }
    for (t, constraint) in table.constraints.iter().enumerate() {
        let (primary, name, columns) = match constraint {
            TableConstraint::PrimaryKey(key) => (true, &key.name, &key.columns),
            TableConstraint::Unique(key) => (false, &key.name, &key.columns),
            _ => continue,
        };
        let Some(columns) = key_names(columns) else {
            return Err((
                40515,
                1,
                16,
                "unsupported key expression in a constraint".into(),
            ));
        };
        found.push(Constraint {
            name: name.as_ref().map(|n| n.value.clone()),
            primary,
            columns,
            origin: Origin::Table(t),
            native: true,
            managed: false,
        });
    }
    if found.iter().filter(|c| c.primary).count() > 1 {
        return Err((
            8110,
            0,
            16,
            format!("Cannot add multiple PRIMARY KEY constraints to table '{table_name}'."),
        ));
    }
    let mut names: Vec<&str> = vec![];
    for name in found.iter().filter_map(|c| c.name.as_deref()) {
        if names.iter().any(|n| n.eq_ignore_ascii_case(name)) {
            return Err((
                8168,
                0,
                16,
                format!(
                    "Cannot create, drop, enable, or disable more than one constraint, column, index, or trigger named '{name}' in this context. Duplicate names are not allowed."
                ),
            ));
        }
        names.push(name);
    }
    let primary = found_primary(table);
    for constraint in &mut found {
        let mut stored = false;
        let mut nullable = false;
        let mut folded = false;
        let mut seen: Vec<&str> = vec![];
        for name in &constraint.columns {
            if seen.iter().any(|s| s.eq_ignore_ascii_case(name)) {
                return Err((
                    1909,
                    1,
                    16,
                    format!(
                        "Cannot use duplicate column names in index. Column name '{name}' listed more than once."
                    ),
                ));
            }
            seen.push(name);
            let Some(column) = table
                .columns
                .iter()
                .find(|c| c.name.value.eq_ignore_ascii_case(name))
            else {
                return Err((
                    1911,
                    1,
                    16,
                    format!(
                        "Column name '{name}' does not exist in the target table, index or view."
                    ),
                ));
            };
            if !keyable(&column.data_type) {
                return Err(super::value::invalid_key(&column.name.value, &table_name));
            }
            if constraint.primary && explicitly_null(column) {
                return Err((
                    8111,
                    1,
                    16,
                    format!(
                        "Cannot define PRIMARY KEY constraint on nullable column in table '{table_name}'."
                    ),
                ));
            }
            if variant(&column.data_type) {
                return Err((
                    40515,
                    1,
                    16,
                    format!("unsupported sql_variant key column '{name}'"),
                ));
            }
            stored |= struct_stored(&column.data_type);
            folded |= ansi(column);
            nullable |= !constraint.primary && !not_null(column) && !primary_column(&primary, name);
        }
        constraint.native = !stored;
        constraint.managed = stored || nullable || folded;
    }
    Ok(found)
}

/// Columns of the table's primary key (table- or column-level).
fn found_primary(table: &CreateTable) -> Vec<String> {
    for column in &table.columns {
        if column
            .options
            .iter()
            .any(|o| matches!(o.option, ColumnOption::PrimaryKey(_)))
        {
            return vec![column.name.value.clone()];
        }
    }
    for constraint in &table.constraints {
        if let TableConstraint::PrimaryKey(key) = constraint {
            return key_names(&key.columns).unwrap_or_default();
        }
    }
    vec![]
}

fn primary_column(primary: &[String], name: &str) -> bool {
    primary.iter().any(|p| p.eq_ignore_ascii_case(name))
}

/// Remove the constraints DuckDB cannot enforce from `table`, and make the
/// key columns of such a primary key NOT NULL, as SQL Server does.
pub fn strip(table: &mut CreateTable, constraints: &[Constraint]) {
    let mut options = vec![];
    let mut tables = vec![];
    for constraint in constraints.iter().filter(|c| !c.native) {
        match constraint.origin {
            Origin::Column(c, o) => options.push((c, o)),
            Origin::Table(t) => tables.push(t),
        }
        if constraint.primary {
            for name in &constraint.columns {
                if let Some(column) = table
                    .columns
                    .iter_mut()
                    .find(|c| c.name.value.eq_ignore_ascii_case(name))
                    && !not_null_option(column)
                {
                    column.options.push(ColumnOptionDef {
                        name: None,
                        option: ColumnOption::NotNull,
                    });
                }
            }
        }
    }
    options.sort_unstable();
    for (c, o) in options.into_iter().rev() {
        table.columns[c].options.remove(o);
    }
    tables.sort_unstable();
    for t in tables.into_iter().rev() {
        table.constraints.remove(t);
    }
}

fn not_null_option(column: &ColumnDef) -> bool {
    column
        .options
        .iter()
        .any(|option| matches!(option.option, ColumnOption::NotNull))
}

/// SQL Server's name for an unnamed constraint: `PK__table__HEX` or
/// `UQ__table__HEX`, with the table name cut to eight characters.
pub fn generated_name(primary: bool, table: &str, seed: u64) -> String {
    let short: String = table.chars().take(8).collect();
    format!(
        "{}__{short}__{:016X}",
        if primary { "PK" } else { "UQ" },
        seed.wrapping_mul(0x9E37_79B9_7F4A_7C15)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(sql: &str) -> CreateTable {
        let Statement::CreateTable(table) = crate::batch::parse(sql).unwrap().remove(0) else {
            panic!()
        };
        table
    }

    #[test]
    fn keys_needing_sql_server_semantics_are_managed() {
        let t = table(
            "CREATE TABLE items (id nvarchar(450) NOT NULL CONSTRAINT pk_items PRIMARY KEY NONCLUSTERED, code int UNIQUE, n int NOT NULL CONSTRAINT uq_n UNIQUE)",
        );
        let found = constraints(&t).unwrap();
        assert_eq!(
            found
                .iter()
                .map(|c| (c.name.as_deref(), c.primary, c.managed))
                .collect::<Vec<_>>(),
            [
                (Some("pk_items"), true, true),
                (None, false, true),
                (Some("uq_n"), false, false)
            ]
        );
        let mut stripped = t.clone();
        strip(&mut stripped, &found);
        assert_eq!(
            Statement::CreateTable(stripped).to_string(),
            "CREATE TABLE items (id NVARCHAR(450) NOT NULL, code INT UNIQUE, n INT NOT NULL CONSTRAINT uq_n UNIQUE)"
        );
        let t = table(
            "CREATE TABLE items (id int NOT NULL, occurred datetimeoffset NOT NULL, CONSTRAINT pk_items PRIMARY KEY CLUSTERED(id, occurred))",
        );
        let found = constraints(&t).unwrap();
        assert!(found[0].managed && !found[0].native && found[0].primary);
        assert_eq!(found[0].columns, ["id", "occurred"]);
        let mut stripped = t.clone();
        strip(&mut stripped, &found);
        assert!(stripped.constraints.is_empty());
        let t = table("CREATE TABLE plain (id int PRIMARY KEY, v int)");
        assert!(!constraints(&t).unwrap()[0].managed && constraints(&t).unwrap()[0].native);
        // A unique key over primary key columns cannot hold NULL.
        let t = table("CREATE TABLE k (a int, b int, PRIMARY KEY (a, b), UNIQUE (b, a))");
        assert!(constraints(&t).unwrap().iter().all(|c| !c.managed));
        // CHAR/VARCHAR keys also get a managed index (trailing spaces, and
        // case under a case-insensitive collation).
        let t = table(
            "CREATE TABLE c (a varchar(10) NOT NULL PRIMARY KEY, b char(4) COLLATE Latin1_General_CS_AS NOT NULL UNIQUE, c varchar(4) COLLATE Latin1_General_CI_AI NOT NULL UNIQUE)",
        );
        assert_eq!(
            constraints(&t)
                .unwrap()
                .iter()
                .map(|c| (c.native, c.managed))
                .collect::<Vec<_>>(),
            [(true, true), (true, true), (true, true)]
        );
        let t = table("CREATE TABLE m (a int NOT NULL PRIMARY KEY (a))");
        let mut stripped = t.clone();
        strip(&mut stripped, &constraints(&t).unwrap());
        assert_eq!(Statement::CreateTable(stripped).to_string(), t.to_string());
    }

    #[test]
    fn invalid_keys_report_sql_server_errors() {
        assert_eq!(
            constraints(&table(
                "CREATE TABLE t (a int PRIMARY KEY, b int PRIMARY KEY)"
            ))
            .unwrap_err()[0]
                .0,
            8110
        );
        assert_eq!(
            constraints(&table("CREATE TABLE t (a int NULL PRIMARY KEY)")).unwrap_err(),
            [
                (
                    8111,
                    1,
                    16,
                    "Cannot define PRIMARY KEY constraint on nullable column in table 't'.".into()
                ),
                (
                    1750,
                    0,
                    16,
                    "Could not create constraint or index. See previous errors.".into()
                )
            ]
        );
        assert_eq!(
            constraints(&table("CREATE TABLE t (a int, UNIQUE (b))")).unwrap_err()[0].0,
            1911
        );
        assert_eq!(
            constraints(&table("CREATE TABLE t (a int, UNIQUE (a, A))")).unwrap_err()[0].0,
            1909
        );
        assert_eq!(
            constraints(&table("CREATE TABLE t (a nvarchar(max) PRIMARY KEY)")).unwrap_err()[0].0,
            1919
        );
        assert_eq!(
            constraints(&table(
                "CREATE TABLE t (a int CONSTRAINT k UNIQUE, b int CONSTRAINT K UNIQUE)"
            ))
            .unwrap_err()
            .len(),
            1
        );
        assert_eq!(
            generated_name(true, "itemsandmore", 1).len(),
            "PK__itemsand__".len() + 16
        );
    }
}
