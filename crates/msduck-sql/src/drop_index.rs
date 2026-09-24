//! Ordered DROP INDEX binding over caller-owned catalog identities.
//! No backend access or implicit identifier collation lives here.
use sqlparser::{
    ast::{Ident, ObjectName, ObjectNamePart, ObjectType, Statement},
    dialect::MsSqlDialect,
    keywords::Keyword,
    parser::Parser,
    tokenizer::Token,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diagnostic {
    pub number: i32,
    pub state: u8,
    pub class: u8,
    pub message: String,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    Sql(Diagnostic),
    Unsupported(String),
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OptionClause {
    None,
    OnlineOff,
    MaxdopOne,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    pub table: ObjectName,
    pub index: Ident,
    pub option: OptionClause,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Request {
    pub if_exists: bool,
    pub targets: Vec<Target>,
}
#[derive(Clone, Debug)]
pub struct Table {
    pub id: u64,
    pub schema: String,
    pub name: String,
}
#[derive(Clone, Debug)]
pub struct Index {
    pub table_id: u64,
    pub id: u64,
    pub name: String,
    pub clustered: bool,
    pub constraint_backed: bool,
    /// Backend identity, supplied by catalog acquisition, never derived by
    /// dropping the requested table qualifier.
    pub backend_name: ObjectName,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BoundDrop {
    pub table_id: u64,
    pub index_id: u64,
    pub statement: Statement,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Plan {
    pub drops: Vec<BoundDrop>,
    /// Execute successful predecessors before emitting this diagnostic.
    pub terminal: Option<Diagnostic>,
}
fn unsupported(message: impl Into<String>) -> Error {
    Error::Unsupported(message.into())
}
fn diagnostic(number: i32, state: u8, class: u8, message: String) -> Diagnostic {
    Diagnostic {
        number,
        state,
        class,
        message,
    }
}
fn identifiers(name: &ObjectName) -> Result<Vec<&Ident>, Error> {
    name.0
        .iter()
        .map(|part| match part {
            ObjectNamePart::Identifier(id) => Ok(id),
            _ => Err(unsupported("Non-identifier object name")),
        })
        .collect()
}

/// Parse one complete DROP INDEX statement. Options outside captured behavior
/// are explicitly unsupported rather than discarded. This does not split batches.
pub fn parse(sql: &str) -> Result<Request, Error> {
    let dialect = MsSqlDialect {};
    let mut p = Parser::new(&dialect)
        .try_with_sql(sql)
        .map_err(|e| unsupported(e.to_string()))?;
    let request = parse_cursor(&mut p)?;
    let _ = p.consume_token(&Token::SemiColon);
    if p.next_token().token != Token::EOF {
        return Err(unsupported("Trailing DROP INDEX syntax"));
    }
    Ok(request)
}

/// Consume exactly one DROP INDEX statement from a caller-owned token cursor.
/// Leave its terminator or next statement untouched. Batch parsing remains the
/// caller's responsibility; quoted identifiers are retained as AST identifiers.
pub fn parse_cursor(p: &mut Parser<'_>) -> Result<Request, Error> {
    if !p.parse_keywords(&[Keyword::DROP, Keyword::INDEX]) {
        return Err(unsupported("Expected DROP INDEX"));
    }
    let if_exists = p.parse_keywords(&[Keyword::IF, Keyword::EXISTS]);
    let mut targets = vec![];
    loop {
        let name = p
            .parse_object_name(false)
            .map_err(|e| unsupported(e.to_string()))?;
        let mut parts = identifiers(&name)?.into_iter().cloned().collect::<Vec<_>>();
        let (table, index) = if p.parse_keyword(Keyword::ON) {
            if parts.len() != 1 {
                return Err(Error::Sql(diagnostic(
                    156,
                    1,
                    15,
                    "Incorrect syntax near the keyword 'ON'.".into(),
                )));
            }
            let table = p
                .parse_object_name(false)
                .map_err(|e| unsupported(e.to_string()))?;
            (table, parts.remove(0))
        } else {
            if parts.len() < 2 {
                return Err(Error::Sql(diagnostic(
                    159,
                    1,
                    15,
                    "Must specify the table name and index name for the DROP INDEX statement."
                        .into(),
                )));
            }
            let index = parts.pop().unwrap();
            (
                ObjectName(parts.into_iter().map(ObjectNamePart::Identifier).collect()),
                index,
            )
        };
        if !(1..=2).contains(&identifiers(&table)?.len()) {
            return Err(unsupported(
                "Cross-database/server DROP INDEX is not yet bound",
            ));
        }
        let option = if p.parse_keyword(Keyword::WITH) {
            p.expect_token(&Token::LParen)
                .map_err(|e| unsupported(e.to_string()))?;
            let name = p
                .parse_identifier()
                .map_err(|e| unsupported(e.to_string()))?;
            p.expect_token(&Token::Eq)
                .map_err(|e| unsupported(e.to_string()))?;
            let value = p.next_token().token;
            let option = match (name.value.to_ascii_uppercase().as_str(), value) {
                ("ONLINE", Token::Word(w)) if w.keyword == Keyword::OFF => OptionClause::OnlineOff,
                ("MAXDOP", Token::Number(n, false)) if n == "1" => OptionClause::MaxdopOne,
                _ => return Err(unsupported("Uncaptured DROP INDEX option")),
            };
            p.expect_token(&Token::RParen)
                .map_err(|e| unsupported(e.to_string()))?;
            option
        } else {
            OptionClause::None
        };
        targets.push(Target {
            table,
            index,
            option,
        });
        if !p.consume_token(&Token::Comma) {
            break;
        }
    }
    Ok(Request { if_exists, targets })
}

/// The caller supplies a complete catalog snapshot, identifier comparison and
/// schema search order. An incomplete catalog cannot be treated as absence.
pub fn bind(
    request: &Request,
    tables: &[Table],
    indexes: &[Index],
    schemas: &[&str],
    equal: impl Fn(&str, &str) -> bool,
) -> Result<Plan, Error> {
    if request.targets.is_empty() || schemas.is_empty() {
        return Err(unsupported("Empty request or schema search path"));
    }
    // IDs are the ownership boundary, so validate the snapshot before emitting
    // any plan. Duplicate IDs could otherwise attach another table's index.
    let mut table_ids = std::collections::HashSet::new();
    for table in tables {
        if !table_ids.insert(table.id) {
            return Err(unsupported("Duplicate table identity in catalog"));
        }
    }
    let mut index_ids = std::collections::HashSet::new();
    let mut backend_names = std::collections::HashSet::new();
    for index in indexes {
        if !table_ids.contains(&index.table_id)
            || !index_ids.insert((index.table_id, index.id))
            || !backend_names.insert(index.backend_name.clone())
            || identifiers(&index.backend_name)?.is_empty()
        {
            return Err(unsupported(
                "Inconsistent index ownership or backend identity",
            ));
        }
    }
    let mut plan = Plan {
        drops: vec![],
        terminal: None,
    };
    let mut removed = std::collections::HashSet::new();
    for target in &request.targets {
        let parts = identifiers(&target.table)?;
        let (schema, name) = match parts.as_slice() {
            [name] => (None, &name.value),
            [schema, name] => (Some(schema.value.as_str()), &name.value),
            _ => return Err(unsupported("Unsupported table qualification")),
        };
        let search = schema.map(|s| vec![s]).unwrap_or_else(|| schemas.to_vec());
        let mut table = None;
        for schema in search {
            let candidates = tables
                .iter()
                .filter(|t| equal(&t.schema, schema) && equal(&t.name, name))
                .collect::<Vec<_>>();
            if candidates.len() > 1 {
                return Err(unsupported("Ambiguous table catalog"));
            }
            if let Some(found) = candidates.first() {
                table = Some(*found);
                break;
            }
        }
        let path = parts
            .iter()
            .map(|id| id.value.as_str())
            .chain(std::iter::once(target.index.value.as_str()))
            .collect::<Vec<_>>()
            .join(".");
        let candidates = indexes
            .iter()
            .filter(|i| {
                table.is_some_and(|t| t.id == i.table_id)
                    && equal(&i.name, &target.index.value)
                    && !removed.contains(&(i.table_id, i.id))
            })
            .collect::<Vec<_>>();
        if candidates.len() > 1 {
            return Err(unsupported("Ambiguous index catalog"));
        }
        let Some(index) = candidates.first() else {
            if request.if_exists {
                continue;
            }
            plan.terminal = Some(diagnostic(
                3701,
                if table.is_some() { 7 } else { 6 },
                11,
                format!(
                    "Cannot drop the index '{path}', because it does not exist or you do not have permission."
                ),
            ));
            break;
        };
        if index.constraint_backed {
            return Err(unsupported(
                "Constraint-backed indexes require separate binding evidence",
            ));
        }
        if index.clustered && target.option != OptionClause::None {
            return Err(unsupported(
                "Clustered index options require separate evidence",
            ));
        }
        if target.option == OptionClause::MaxdopOne {
            plan.terminal = Some(diagnostic(
                3748,
                1,
                16,
                format!(
                    "Cannot drop non-clustered index '{path}' using drop clustered index clause."
                ),
            ));
            break;
        }
        if identifiers(&index.backend_name)?.is_empty() {
            return Err(unsupported("Missing backend index identity"));
        }
        plan.drops.push(BoundDrop {
            table_id: index.table_id,
            index_id: index.id,
            statement: Statement::Drop {
                object_type: ObjectType::Index,
                if_exists: false,
                names: vec![index.backend_name.clone()],
                cascade: false,
                restrict: false,
                purge: false,
                temporary: false,
                table: None,
            },
        });
        removed.insert((index.table_id, index.id));
    }
    Ok(plan)
}
