//! The columns of the relations a statement reads, for recognizing
//! references to Unicode carrier columns before translation.
//!
//! Names resolve against every relation the statement names, not per
//! scope: a reference counts as a carrier only when every relation that has
//! a column of that name (or every relation its qualifier names) agrees
//! that it is a carrier, and the statement defines no alias or derived
//! column of that name. Anything else, including columns of derived tables
//! and CTEs, is left alone.
use anyhow::Result;
use sqlparser::ast::*;
use std::collections::{HashMap, HashSet};
use std::ops::ControlFlow;

#[derive(Clone, Debug, PartialEq)]
pub(super) struct Column {
    pub carrier: bool,
    /// The SQL Server declaration of a carrier column, after [`Catalog::declare`].
    pub declared: Option<DataType>,
}

/// What a column reference resolves to.
pub(super) enum Resolution<'a> {
    Carrier(&'a Column),
    /// A column of a known relation that is not a carrier.
    Plain,
    /// A derived-table, CTE, alias or outer column, or an ambiguous name.
    Unknown,
}

/// A qualifier naming a derived table or table function, whose columns are
/// unknown here.
const DERIVED: &str = "";

#[derive(Default)]
pub(super) struct Catalog {
    /// Table name (lowercase, last part) to its columns (lowercase).
    tables: HashMap<String, HashMap<String, Column>>,
    /// Qualifier (alias or table name, lowercase) to table names.
    qualifiers: HashMap<String, HashSet<String>>,
    /// Names the statement defines itself (select-list aliases, CTE names
    /// and columns, derived-table columns), which a bare name may refer to.
    defined: HashSet<String>,
    /// The relations by their AST spelling, with their table name.
    names: HashMap<String, String>,
}

fn lower(ident: &Ident) -> String {
    ident.value.to_lowercase()
}

#[derive(Default)]
struct Relations {
    /// The relations by their AST spelling, with their lowercase last part.
    names: HashMap<String, String>,
    /// The same relations by their DuckDB spelling.
    backend: HashMap<String, String>,
    qualifiers: HashMap<String, HashSet<String>>,
    defined: HashSet<String>,
}

impl Relations {
    fn add(&mut self, name: &ObjectName, qualifier: Option<&Ident>) {
        let Some(ObjectNamePart::Identifier(table)) = name.0.last() else {
            return;
        };
        let table = lower(table);
        self.names.insert(name.to_string(), table.clone());
        // DuckDB quotes with `"` only; T-SQL names may still use brackets.
        let Some(parts) = name
            .0
            .iter()
            .map(|part| match part {
                ObjectNamePart::Identifier(ident) => {
                    Some(format!("\"{}\"", ident.value.replace('"', "\"\"")))
                }
                _ => None,
            })
            .collect::<Option<Vec<_>>>()
        else {
            return;
        };
        self.backend.insert(parts.join("."), table.clone());
        let qualifier = qualifier.map_or(table.clone(), lower);
        self.qualifiers.entry(qualifier).or_default().insert(table);
    }
}

impl Visitor for Relations {
    type Break = ();
    fn pre_visit_query(&mut self, query: &Query) -> ControlFlow<()> {
        if let Some(with) = &query.with {
            for cte in &with.cte_tables {
                self.defined.insert(lower(&cte.alias.name));
                for column in &cte.alias.columns {
                    self.defined.insert(lower(&column.name));
                }
            }
        }
        let mut bodies = vec![query.body.as_ref()];
        while let Some(body) = bodies.pop() {
            match body {
                SetExpr::Select(select) => {
                    for item in &select.projection {
                        if let SelectItem::ExprWithAlias { alias, .. } = item {
                            self.defined.insert(lower(alias));
                        }
                    }
                }
                SetExpr::SetOperation { left, right, .. } => {
                    bodies.push(left);
                    bodies.push(right);
                }
                _ => {}
            }
        }
        ControlFlow::Continue(())
    }
    fn pre_visit_table_factor(&mut self, factor: &TableFactor) -> ControlFlow<()> {
        if let Some(alias) = match factor {
            TableFactor::Table { alias, .. }
            | TableFactor::Derived { alias, .. }
            | TableFactor::TableFunction { alias, .. }
            | TableFactor::Function { alias, .. }
            | TableFactor::OpenJsonTable { alias, .. }
            | TableFactor::NestedJoin { alias, .. } => alias.as_ref(),
            _ => None,
        } {
            for column in &alias.columns {
                self.defined.insert(lower(&column.name));
            }
            if !matches!(factor, TableFactor::Table { args: None, .. }) {
                self.qualifiers
                    .entry(lower(&alias.name))
                    .or_default()
                    .insert(DERIVED.into());
            }
        }
        if let TableFactor::OpenJsonTable { columns, .. } = factor {
            for column in columns {
                self.defined.insert(lower(&column.name));
            }
        }
        if let TableFactor::Table {
            name,
            alias,
            args: None,
            ..
        } = factor
        {
            self.add(name, alias.as_ref().map(|a| &a.name));
        }
        ControlFlow::Continue(())
    }
    fn pre_visit_relation(&mut self, relation: &ObjectName) -> ControlFlow<()> {
        // DML targets outside FROM (INSERT, UPDATE, MERGE).
        self.add(relation, None);
        ControlFlow::Continue(())
    }
}

impl Catalog {
    /// The columns of the relations `node` reads, from DuckDB's catalog.
    pub fn load<T: Visit>(db: &duckdb::Connection, node: &T) -> Result<Self> {
        let mut relations = Relations::default();
        let _ = node.visit(&mut relations);
        let mut catalog = Catalog {
            tables: HashMap::new(),
            qualifiers: relations.qualifiers,
            defined: relations.defined,
            names: HashMap::new(),
        };
        for (spelling, table) in &relations.backend {
            // A CTE of the same name hides the table.
            if catalog.defined.contains(table) {
                continue;
            }
            let Ok(mut query) = db.prepare("SELECT lower(name), type FROM pragma_table_info(?)")
            else {
                continue;
            };
            // Names DuckDB cannot resolve (table functions, missing tables)
            // contribute nothing.
            let Ok(rows) = query.query_map([spelling], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            }) else {
                continue;
            };
            let columns = catalog.tables.entry(table.clone()).or_default();
            for row in rows {
                let Ok((name, kind)) = row else { continue };
                let column = Column {
                    carrier: crate::unicode_carrier::is_storage_name(&kind),
                    declared: None,
                };
                match columns.get(&name) {
                    // Same-named tables in several schemas or databases
                    // count as carriers only when they agree.
                    Some(existing) if !existing.carrier || !column.carrier => {
                        columns.insert(
                            name,
                            Column {
                                carrier: false,
                                declared: None,
                            },
                        );
                    }
                    Some(_) => {}
                    None => {
                        columns.insert(name, column);
                    }
                }
            }
        }
        catalog.names = relations.names;
        Ok(catalog)
    }

    /// Add the SQL Server declarations of the carrier columns, which costs
    /// a full catalog snapshot.
    pub fn declare<T: Visit>(&mut self, db: &duckdb::Connection, node: &T) -> Result<()> {
        let snapshot = crate::query_catalog::snapshot(db, node)?;
        let mut types: HashMap<(String, String), Option<DataType>> = HashMap::new();
        for (spelling, fields) in &snapshot.tables {
            let Some(table) = self.names.get(spelling) else {
                continue;
            };
            for field in fields {
                let kind = field
                    .info
                    .as_ref()
                    .and_then(|info| info.logical_type())
                    .map(msduck_sql::sql_type::ast);
                let key = (table.clone(), field.name.to_lowercase());
                // Disagreeing declarations of same-named tables: unknown.
                match types.get(&key) {
                    Some(existing) if *existing != kind => {
                        types.insert(key, None);
                    }
                    Some(_) => {}
                    None => {
                        types.insert(key, kind);
                    }
                }
            }
        }
        for (table, columns) in &mut self.tables {
            for (name, column) in columns.iter_mut() {
                if column.carrier {
                    column.declared = types.get(&(table.clone(), name.clone())).cloned().flatten();
                }
            }
        }
        Ok(())
    }

    pub fn has_carriers(&self) -> bool {
        self.tables
            .values()
            .any(|columns| columns.values().any(|c| c.carrier))
    }

    /// Whether some relation has a carrier column of this (lowercase) name.
    pub fn carrier_named(&self, name: &str) -> bool {
        self.tables
            .values()
            .any(|columns| columns.get(name).is_some_and(|c| c.carrier))
    }

    /// The carrier column `expr` refers to, if it unambiguously refers to one.
    pub fn carrier(&self, expr: &Expr) -> Option<&Column> {
        match self.resolve(expr) {
            Resolution::Carrier(column) => Some(column),
            _ => None,
        }
    }

    /// Resolve a column reference against the relations of the statement.
    pub fn resolve(&self, expr: &Expr) -> Resolution<'_> {
        let (tables, name): (Vec<&String>, String) = match expr {
            Expr::Nested(inner) => return self.resolve(inner),
            Expr::Identifier(ident) if !self.defined.contains(&lower(ident)) => {
                (self.tables.keys().collect(), lower(ident))
            }
            Expr::CompoundIdentifier(parts) if parts.len() >= 2 => {
                let qualifier = lower(&parts[parts.len() - 2]);
                let Some(tables) = self.qualifiers.get(&qualifier) else {
                    return Resolution::Unknown;
                };
                // A derived table or a relation DuckDB could not describe.
                if tables.iter().any(|t| !self.tables.contains_key(t)) {
                    return Resolution::Unknown;
                }
                let Some(last) = parts.last() else {
                    return Resolution::Unknown;
                };
                (tables.iter().collect(), lower(last))
            }
            _ => return Resolution::Unknown,
        };
        let mut found: Option<&Column> = None;
        let mut plain = false;
        for table in tables {
            let Some(column) = self.tables.get(table).and_then(|c| c.get(&name)) else {
                continue;
            };
            if !column.carrier {
                plain = true;
            } else if found.is_some_and(|f| f != column) {
                return Resolution::Unknown;
            } else {
                found = Some(column);
            }
        }
        match (found, plain) {
            (Some(column), false) => Resolution::Carrier(column),
            (None, true) => Resolution::Plain,
            _ => Resolution::Unknown,
        }
    }
}
