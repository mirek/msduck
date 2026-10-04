//! The columns of the relations a statement reads, for recognizing
//! references to Unicode carrier columns before translation.
//!
//! References resolve within explicit query/select scopes, with nearest-scope
//! alias shadowing and correlated fallback. Physical descriptions of same-named
//! tables remain conservative; derived and CTE columns remain unknown here.
use anyhow::Result;
use sqlparser::ast::*;
use std::collections::{HashMap, HashSet};
use std::ops::ControlFlow;

#[derive(Clone, Debug, PartialEq)]
pub(super) struct Column {
    pub carrier: bool,
    /// Direct OPENJSON source; ISNULL already has carrier-aware dispatch.
    pub openjson: bool,
    /// Whether the backend type is text (VARCHAR or a carrier).
    pub text: bool,
    /// The SQL Server declaration, after [`Catalog::declare`] for physical columns.
    pub declared: Option<DataType>,
    /// A declared collation other than the database default.
    pub collation: Option<String>,
}

/// What a column reference resolves to.
pub(super) enum Resolution<'a> {
    Carrier(&'a Column),
    /// A column of a known relation that is not a carrier, and whether it
    /// is VARCHAR text.
    Plain {
        text: bool,
    },
    /// A derived-table, CTE, alias or outer column, or an ambiguous name.
    Unknown,
}

/// A qualifier naming a derived table or table function, whose columns are
/// unknown here.
const DERIVED: &str = "";

#[derive(Clone, Default)]
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
    scopes: Vec<Scope>,
}

#[derive(Clone, Default)]
struct Scope {
    tables: HashSet<String>,
    qualifiers: HashMap<String, HashSet<String>>,
    defined: HashSet<String>,
    /// Known names can rule out a collision without inventing column types.
    unknown_columns: HashMap<String, Option<HashSet<String>>>,
    ctes: HashMap<String, Option<HashSet<String>>>,
}

fn projected_names(query: &Query) -> Option<HashSet<String>> {
    fn body_names(body: &SetExpr) -> Option<HashSet<String>> {
        match body {
            SetExpr::Select(select) => select
                .projection
                .iter()
                .map(|item| match item {
                    SelectItem::ExprWithAlias { alias, .. } => Some(lower(alias)),
                    SelectItem::UnnamedExpr(Expr::Identifier(id)) => Some(lower(id)),
                    SelectItem::UnnamedExpr(Expr::CompoundIdentifier(parts)) => {
                        parts.last().map(lower)
                    }
                    _ => None,
                })
                .collect(),
            SetExpr::SetOperation { left, .. } => body_names(left),
            SetExpr::Query(query) => projected_names(query),
            _ => None,
        }
    }
    body_names(&query.body)
}

// Predicate collection precedes execution's target canonicalization. Resolve
// alias targets on a private copy, retaining WITH for CTE shadowing rules.
// A rejected write keeps its original conservative bindings; execution reports
// the canonicalizer's diagnostic later.
fn canonical_write(statement: &Statement) -> Option<Statement> {
    if !matches!(statement, Statement::Update(_) | Statement::Delete(_))
        && !matches!(statement, Statement::Query(query)
            if matches!(query.body.as_ref(), SetExpr::Update(_) | SetExpr::Delete(_)))
    {
        return None;
    }
    let mut canonical = statement.clone();
    msduck_sql::update::canonicalize(&mut canonical).ok()?;
    msduck_sql::delete::canonicalize(&mut canonical).ok()?;
    Some(canonical)
}

fn canonical_node<T: 'static>(node: &T) -> Option<Statement> {
    (node as &dyn std::any::Any)
        .downcast_ref::<Statement>()
        .and_then(canonical_write)
}

// Collect only sources in this SELECT or statement; nested queries have their
// own frames. Visiting a SELECT separately also separates UNION branch sources.
#[derive(Default)]
struct LocalRelations {
    relations: Relations,
    unknown_columns: HashMap<String, Option<HashSet<String>>>,
    depth: usize,
}
impl Visitor for LocalRelations {
    type Break = ();
    fn pre_visit_query(&mut self, _: &Query) -> ControlFlow<()> {
        self.depth += 1;
        ControlFlow::Continue(())
    }
    fn post_visit_query(&mut self, _: &Query) -> ControlFlow<()> {
        self.depth -= 1;
        ControlFlow::Continue(())
    }
    // SELECT aliases are outputs, not bindings visible to their own expressions.
    // ORDER BY consumers resolve aliases against the projection explicitly.
    fn pre_visit_table_factor(&mut self, factor: &TableFactor) -> ControlFlow<()> {
        if self.depth == 0 {
            let result = self.relations.pre_visit_table_factor(factor);
            if let TableFactor::Derived {
                alias: Some(alias),
                subquery,
                ..
            } = factor
            {
                let names = if alias.columns.is_empty() {
                    projected_names(subquery)
                } else {
                    Some(alias.columns.iter().map(|c| lower(&c.name)).collect())
                };
                let key = format!("\0derived{}", self.unknown_columns.len());
                if let Some(tables) = self.relations.qualifiers.get_mut(&lower(&alias.name)) {
                    tables.remove(DERIVED);
                    tables.insert(key.clone());
                }
                self.unknown_columns.insert(key, names);
            }
            result
        } else {
            ControlFlow::Continue(())
        }
    }
    fn pre_visit_relation(&mut self, name: &ObjectName) -> ControlFlow<()> {
        if self.depth == 0 {
            self.relations.pre_visit_relation(name)
        } else {
            ControlFlow::Continue(())
        }
    }
}

fn scalar_variable(ident: &Ident) -> bool {
    ident.quote_style.is_none() && ident.value.starts_with('@')
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
    /// OPENJSON row sources, by a name no relation can have.
    openjson: HashMap<String, HashMap<String, Column>>,
    /// FROM/APPLY order: each lateral source inherits only after its predecessors.
    /// One pass bounds work and never attempts to solve invalid cyclic references.
    openjson_inputs: Vec<(String, Expr, bool)>,
}

/// The columns of an OPENJSON row source: the default schema's key
/// (NVARCHAR(4000)), value (NVARCHAR(MAX)) and type, or the declared WITH
/// columns, whose NVARCHAR and NCHAR values are carriers.
fn openjson_columns(columns: &[OpenJsonTableColumn], input: &Expr) -> HashMap<String, Column> {
    let column = |declared: DataType| {
        let family = match msduck_sql::sql_type::declaration(&declared) {
            Ok(msduck_core::types::Type::Character(kind)) => Some(kind.family()),
            _ => None,
        };
        let carrier = matches!(
            family,
            Some(msduck_core::character::Family::Nvarchar | msduck_core::character::Family::Nchar)
        );
        Column {
            carrier,
            openjson: true,
            text: family.is_some(),
            declared: Some(declared),
            collation: None,
        }
    };
    msduck_sql::expression_metadata::openjson::columns(columns)
        .into_iter()
        .filter_map(|(name, declared)| {
            declared.map(|kind| {
                let name = name.to_lowercase();
                let mut column = column(kind);
                if column.text {
                    column.collation = if columns.is_empty() && name == "key" {
                        Some("Latin1_General_BIN2".into())
                    } else {
                        source_collation(None, input)
                    };
                }
                (name, column)
            })
        })
        .collect()
}

/// Character casts preserve the input collation; an explicit COLLATE wins.
/// Column inheritance uses declaration facts in the completed source scope,
/// never the JSON text or result values.
fn source_collation(catalog: Option<&Catalog>, mut input: &Expr) -> Option<String> {
    // Every iteration consumes an AST wrapper; casts through non-character
    // domains reset collation instead of retaining an earlier text label.
    loop {
        match input {
            Expr::Collate { collation, .. } => {
                return Some(match collation.0.as_slice() {
                    [ObjectNamePart::Identifier(name)] => name.value.clone(),
                    _ => collation.to_string(),
                });
            }
            Expr::Nested(inner) => input = inner,
            Expr::Cast {
                expr, data_type, ..
            }
            | Expr::Convert {
                expr,
                data_type: Some(data_type),
                ..
            } if matches!(
                msduck_sql::sql_type::declaration(data_type),
                Ok(msduck_core::types::Type::Character(_))
            ) =>
            {
                input = expr
            }
            Expr::Identifier(_) | Expr::CompoundIdentifier(_) => {
                return catalog
                    .and_then(|catalog| catalog.collation(input))
                    .map(str::to_owned);
            }
            _ => return None,
        }
    }
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
            if !matches!(
                factor,
                TableFactor::Table { args: None, .. } | TableFactor::OpenJsonTable { .. }
            ) {
                self.qualifiers
                    .entry(lower(&alias.name))
                    .or_default()
                    .insert(DERIVED.into());
            }
        }
        if let TableFactor::OpenJsonTable {
            columns,
            alias,
            json_expr,
            ..
        } = factor
        {
            // WITH names are source columns, not projection/derived aliases.
            // A column list in the alias renames the columns: unknown here.
            let renamed = alias.as_ref().is_some_and(|a| !a.columns.is_empty());
            let name = format!("\0openjson{}", self.openjson.len());
            self.qualifiers
                .entry(alias.as_ref().map_or("openjson".into(), |a| lower(&a.name)))
                .or_default()
                .insert(if renamed {
                    DERIVED.into()
                } else {
                    name.clone()
                });
            if !renamed {
                self.openjson_inputs
                    .push((name.clone(), json_expr.clone(), columns.is_empty()));
                self.openjson
                    .insert(name, openjson_columns(columns, json_expr));
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

/// Declared collation names of a relation's character columns other than
/// the database default, by lowercase column name. A name that compares
/// like the default is kept: columns of different collations conflict.
fn collations(db: &duckdb::Connection, spelling: &str) -> HashMap<String, String> {
    use msduck_sql::dialect::ext::keys::collation::{DEFAULT, sensitivity};
    let Ok(mut query) = db.prepare(
        "SELECT lower(c.name), d.collation_name FROM main.__msduck_column_info c
         JOIN main.__msduck_declared_columns d USING(object_id, column_id)
         WHERE c.object_id = __msduck_object_id(?, NULL) AND d.collation_name IS NOT NULL",
    ) else {
        return HashMap::new();
    };
    let Ok(rows) = query.query_map([spelling], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    }) else {
        return HashMap::new();
    };
    rows.filter_map(Result::ok)
        .filter(|(_, collation)| {
            !collation.eq_ignore_ascii_case(DEFAULT) && sensitivity(collation).is_some()
        })
        .collect()
}

impl Catalog {
    fn local_scope<T: Visit>(&self, node: &T) -> Self {
        let mut local = LocalRelations::default();
        let _ = node.visit(&mut local);
        let mut catalog = self.clone();
        let mut relations = local.relations;
        let mut renamed = HashMap::new();
        for (name, columns) in relations.openjson {
            let scoped = format!("\0scope{}-{name}", self.scopes.len());
            catalog.tables.insert(scoped.clone(), columns);
            renamed.insert(name, scoped);
        }
        for tables in relations.qualifiers.values_mut() {
            *tables = tables
                .iter()
                .map(|t| renamed.get(t).unwrap_or(t).clone())
                .collect();
        }
        let mut unknown_columns = local.unknown_columns;
        for table in relations.qualifiers.values().flatten() {
            if self.tables.contains_key(table) || unknown_columns.contains_key(table) {
                continue;
            }
            if let Some(names) = self
                .scopes
                .iter()
                .rev()
                .find_map(|scope| scope.ctes.get(table))
            {
                unknown_columns.insert(table.clone(), names.clone());
            }
        }
        catalog.scopes.push(Scope {
            tables: relations.qualifiers.values().flatten().cloned().collect(),
            qualifiers: relations.qualifiers,
            defined: relations.defined,
            unknown_columns,
            ctes: HashMap::new(),
        });
        for (name, input, default_schema) in relations.openjson_inputs {
            let collation = source_collation(Some(&catalog), &input);
            let Some(scoped) = renamed.get(&name) else {
                continue;
            };
            if let Some(columns) = catalog.tables.get_mut(scoped) {
                for (name, column) in columns {
                    if column.text && !(default_schema && name == "key") {
                        column.collation = collation.clone();
                    }
                }
            }
        }
        catalog
    }

    pub fn select_scope(&self, select: &Select) -> Self {
        self.local_scope(select)
    }

    pub fn query_scope(&self, query: &Query) -> Self {
        let mut catalog = self.clone();
        let mut scope = Scope::default();
        if let Some(with) = &query.with {
            for cte in &with.cte_tables {
                scope.defined.insert(lower(&cte.alias.name));
                scope
                    .defined
                    .extend(cte.alias.columns.iter().map(|c| lower(&c.name)));
                let names = if cte.alias.columns.is_empty() {
                    projected_names(&cte.query)
                } else {
                    Some(cte.alias.columns.iter().map(|c| lower(&c.name)).collect())
                };
                scope.ctes.insert(lower(&cte.alias.name), names);
            }
        }
        catalog.scopes.push(scope);
        // WITH can wrap writes in a Query whose body never visits a SELECT.
        // Keep its targets/FROM sources visible, excluding nested queries.
        let canonical = matches!(query.body.as_ref(), SetExpr::Update(_) | SetExpr::Delete(_))
            .then(|| canonical_write(&Statement::Query(Box::new(query.clone()))))
            .flatten();
        let query = match &canonical {
            Some(Statement::Query(query)) => query.as_ref(),
            _ => query,
        };
        match query.body.as_ref() {
            SetExpr::Insert(statement)
            | SetExpr::Update(statement)
            | SetExpr::Delete(statement)
            | SetExpr::Merge(statement) => catalog.local_scope(statement),
            _ => catalog,
        }
    }

    /// The columns of the relations `node` reads, from DuckDB's catalog.
    pub fn load<T: Visit + 'static>(db: &duckdb::Connection, node: &T) -> Result<Self> {
        let mut relations = Relations::default();
        let canonical = canonical_node(node);
        if let Some(canonical) = &canonical {
            let _ = canonical.visit(&mut relations);
        } else {
            let _ = node.visit(&mut relations);
        }
        let mut catalog = Catalog {
            tables: relations.openjson,
            qualifiers: relations.qualifiers,
            defined: relations.defined,
            names: HashMap::new(),
            scopes: Vec::new(),
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
            let collations = collations(db, spelling);
            let columns = catalog.tables.entry(table.clone()).or_default();
            for row in rows {
                let Ok((name, kind)) = row else { continue };
                let carrier = crate::unicode_carrier::is_storage_name(&kind);
                let column = Column {
                    carrier,
                    openjson: false,
                    text: carrier || kind == "VARCHAR",
                    declared: None,
                    collation: collations.get(&name).cloned(),
                };
                match columns.get(&name) {
                    // Same-named tables in several schemas or databases
                    // count as carriers only when they agree.
                    Some(existing) if !existing.carrier || !column.carrier => {
                        let text = existing.text || column.text;
                        let collation = (existing.collation == column.collation)
                            .then(|| column.collation.clone())
                            .flatten();
                        columns.insert(
                            name,
                            Column {
                                carrier: false,
                                openjson: false,
                                text,
                                declared: None,
                                collation,
                            },
                        );
                    }
                    // Carriers of same-named tables keep a collation only
                    // when they agree.
                    Some(existing) if existing.collation != column.collation => {
                        let mut merged = column;
                        merged.collation = None;
                        merged.text = true;
                        columns.insert(name, merged);
                    }
                    Some(_) => {}
                    None => {
                        columns.insert(name, column);
                    }
                }
            }
        }
        catalog.names = relations.names;
        Ok(match &canonical {
            Some(canonical) => catalog.local_scope(canonical),
            None => catalog.local_scope(node),
        })
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
                // OPENJSON columns keep their declarations.
                if let Some(declared) = types.get(&(table.clone(), name.clone())) {
                    column.declared = declared.clone();
                }
            }
        }
        Ok(())
    }

    /// An agreed explicit declaration; unknown source/alias barriers still apply.
    pub fn declaration(&self, expr: &Expr) -> Option<DataType> {
        let columns = self.candidates(expr)?;
        let first = columns.first()?.declared.as_ref()?;
        columns
            .iter()
            .all(|column| column.declared.as_ref() == Some(first))
            .then(|| first.clone())
    }

    /// Whether some relation has a carrier column of this (lowercase) name.
    pub fn carrier_named(&self, name: &str) -> bool {
        self.tables
            .values()
            .any(|columns| columns.get(name).is_some_and(|c| c.carrier))
    }

    pub fn has_carriers(&self) -> bool {
        self.tables
            .values()
            .any(|columns| columns.values().any(|c| c.carrier))
    }

    /// Whether some relation has a column with a declared collation.
    pub fn has_collations(&self) -> bool {
        self.tables
            .values()
            .any(|columns| columns.values().any(|c| c.collation.is_some()))
    }

    /// Whether some relation has a character column.
    pub fn has_text(&self) -> bool {
        self.tables
            .values()
            .any(|columns| columns.values().any(|c| c.text))
    }

    /// The collation of the column `expr` refers to, when it unambiguously
    /// refers to a column with a collation of its own.
    pub fn collation(&self, expr: &Expr) -> Option<&str> {
        let columns = self.candidates(expr)?;
        let mut found: Option<Option<&str>> = None;
        for column in columns {
            match found {
                Some(existing) if existing != column.collation.as_deref() => return None,
                _ => found = Some(column.collation.as_deref()),
            }
        }
        found.flatten()
    }

    /// Every column of the statement's relations that `expr` may name; `None`
    /// for references this catalog cannot resolve (derived tables, CTEs,
    /// aliases, outer or missing columns).
    fn candidates(&self, expr: &Expr) -> Option<Vec<&Column>> {
        if let Expr::Nested(inner) = expr {
            return self.candidates(inner);
        }
        if !self.scopes.is_empty() {
            for scope in self.scopes.iter().rev() {
                let (tables, name) = match expr {
                    Expr::Identifier(id) if !scalar_variable(id) => {
                        if scope.defined.contains(&lower(id)) {
                            return None;
                        }
                        (&scope.tables, lower(id))
                    }
                    Expr::CompoundIdentifier(parts) if parts.len() >= 2 => {
                        let qualifier = lower(&parts[parts.len() - 2]);
                        let Some(tables) = scope.qualifiers.get(&qualifier) else {
                            if scope.defined.contains(&qualifier) {
                                return None;
                            }
                            continue;
                        };
                        (tables, lower(parts.last()?))
                    }
                    _ => return None,
                };
                // Unknown local sources can shadow a bare reference unless
                // their explicit output names prove the column is absent.
                if tables.iter().any(|t| {
                    !self.tables.contains_key(t)
                        && (matches!(expr, Expr::CompoundIdentifier(_))
                            || scope
                                .unknown_columns
                                .get(t)
                                .and_then(Option::as_ref)
                                .is_none_or(|names| names.contains(&name)))
                }) {
                    return None;
                }
                let columns: Vec<&Column> = tables
                    .iter()
                    .filter_map(|t| self.tables.get(t).and_then(|c| c.get(&name)))
                    .collect();
                if !columns.is_empty() {
                    return Some(columns);
                }
                if matches!(expr, Expr::CompoundIdentifier(_)) {
                    return None; // A local qualifier shadows outer qualifiers.
                }
            }
            return None;
        }
        let (tables, name): (Vec<&String>, String) = match expr {
            Expr::Nested(inner) => return self.candidates(inner),
            Expr::Identifier(ident)
                if !scalar_variable(ident) && !self.defined.contains(&lower(ident)) =>
            {
                (self.tables.keys().collect(), lower(ident))
            }
            Expr::CompoundIdentifier(parts) if parts.len() >= 2 => {
                let tables = self.qualifiers.get(&lower(&parts[parts.len() - 2]))?;
                if tables.iter().any(|t| !self.tables.contains_key(t)) {
                    return None;
                }
                (tables.iter().collect(), lower(parts.last()?))
            }
            _ => return None,
        };
        let found: Vec<&Column> = tables
            .into_iter()
            .filter_map(|table| self.tables.get(table).and_then(|c| c.get(&name)))
            .collect();
        (!found.is_empty()).then_some(found)
    }

    /// The collation of the character columns `expr` may name (`Some(None)`
    /// for the database default), when they agree.
    pub fn agreed_collation(&self, expr: &Expr) -> Option<Option<&str>> {
        let columns = self.candidates(expr)?;
        let first = columns.first()?.collation.as_deref();
        columns
            .iter()
            .all(|c| c.text && c.collation.as_deref() == first)
            .then_some(first)
    }

    /// Whether every column `expr` may name is character data.
    pub fn textual(&self, expr: &Expr) -> bool {
        self.candidates(expr)
            .is_some_and(|columns| columns.iter().all(|c| c.text))
    }

    /// Whether every column `expr` may name is CHAR or VARCHAR data.
    pub fn ansi(&self, expr: &Expr) -> bool {
        self.candidates(expr)
            .is_some_and(|columns| columns.iter().all(|c| c.text && !c.carrier))
    }

    /// Whether every column `expr` may name is character data under the
    /// database default collation.
    pub fn default_text(&self, expr: &Expr) -> bool {
        use msduck_sql::dialect::ext::keys::collation::{Sensitivity, sensitivity};
        self.candidates(expr).is_some_and(|columns| {
            columns.iter().all(|c| {
                c.text
                    && c.collation
                        .as_deref()
                        .is_none_or(|name| sensitivity(name) == Some(Sensitivity::Default))
            })
        })
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
        let Some(columns) = self.candidates(expr) else {
            return Resolution::Unknown;
        };
        let mut found: Option<&Column> = None;
        let mut plain = false;
        let mut text = false;
        for column in columns {
            if !column.carrier {
                plain = true;
                text |= column.text;
            } else if found.is_some_and(|f| f != column) {
                return Resolution::Unknown;
            } else {
                found = Some(column);
            }
        }
        match (found, plain) {
            (Some(column), false) => Resolution::Carrier(column),
            (None, true) => Resolution::Plain { text },
            _ => Resolution::Unknown,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlparser::{dialect::MsSqlDialect, parser::Parser};

    fn catalog(sql: &str) -> Catalog {
        let statement = Parser::parse_sql(&MsSqlDialect {}, sql).unwrap().remove(0);
        let mut relations = Relations::default();
        let _ = statement.visit(&mut relations);
        Catalog {
            tables: relations.openjson,
            qualifiers: relations.qualifiers,
            defined: relations.defined,
            names: relations.names,
            scopes: Vec::new(),
        }
    }

    #[test]
    fn openjson_collations_follow_declared_input_without_reading_values() {
        let catalog =
            catalog("SELECT j.value FROM OPENJSON(@doc COLLATE [Latin1_General_100_CS_AS]) j");
        assert_eq!(
            catalog.collation(&qualified("j", "key")),
            Some("Latin1_General_BIN2")
        );
        assert_eq!(
            catalog.collation(&qualified("j", "value")),
            Some("Latin1_General_100_CS_AS")
        );
        let catalog = self::catalog(
            "SELECT j.v FROM OPENJSON(@doc COLLATE Latin1_General_100_CS_AS) WITH(v NVARCHAR(2)) j",
        );
        assert_eq!(
            catalog.collation(&qualified("j", "v")),
            Some("Latin1_General_100_CS_AS")
        );
    }

    #[test]
    fn non_character_casts_reset_input_collation() {
        for input in [
            "CAST(CAST(@doc COLLATE Latin1_General_100_CS_AS AS VARBINARY(MAX)) AS NVARCHAR(MAX))",
            "CONVERT(NVARCHAR(MAX), CONVERT(VARBINARY(MAX), @doc COLLATE Latin1_General_100_CS_AS))",
        ] {
            let catalog = catalog(&format!("SELECT j.value FROM OPENJSON({input}) j"));
            assert_eq!(catalog.collation(&qualified("j", "value")), None);
        }
        let catalog = catalog(
            "SELECT j.value FROM OPENJSON(CAST(@doc COLLATE Latin1_General_100_CS_AS AS NVARCHAR(MAX))) j",
        );
        assert_eq!(
            catalog.collation(&qualified("j", "value")),
            Some("Latin1_General_100_CS_AS")
        );
    }

    #[test]
    fn chained_openjson_sources_inherit_in_from_order() {
        // Reverse alias spelling rules out accidentally sorting the aliases.
        for aliases in [("z", "a", "m"), ("a", "z", "m")] {
            let (first, second, third) = aliases;
            let sql = format!(
                "SELECT {third}.value FROM OPENJSON(@doc COLLATE Latin1_General_100_CS_AS) {first} \
                 CROSS APPLY OPENJSON({first}.value) {second} \
                 CROSS APPLY OPENJSON({second}.value) {third}"
            );
            let statement = Parser::parse_sql(&MsSqlDialect {}, &sql).unwrap().remove(0);
            let Statement::Query(query) = statement else {
                panic!("expected query")
            };
            let SetExpr::Select(select) = query.body.as_ref() else {
                panic!("expected select")
            };
            let scoped = catalog(&sql).select_scope(select);
            for alias in [first, second, third] {
                assert_eq!(
                    scoped.collation(&qualified(alias, "value")),
                    Some("Latin1_General_100_CS_AS")
                );
                assert_eq!(
                    scoped.collation(&qualified(alias, "key")),
                    Some("Latin1_General_BIN2")
                );
            }
        }
    }

    fn qualified(alias: &str, name: &str) -> Expr {
        Expr::CompoundIdentifier(vec![Ident::new(alias), Ident::new(name)])
    }

    #[test]
    fn openjson_default_declarations_ignore_document_values() {
        for document in ["NULL", "N'{}'", "@document"] {
            let catalog = catalog(&format!("SELECT j.[key] FROM OPENJSON({document}) j"));
            let key = catalog.carrier(&qualified("j", "KEY")).unwrap();
            assert_eq!(
                key.declared,
                Some(DataType::Nvarchar(Some(CharacterLength::IntegerLength {
                    length: 4000,
                    unit: None
                })))
            );
            assert_eq!(
                catalog.carrier(&qualified("j", "value")).unwrap().declared,
                Some(DataType::Nvarchar(Some(CharacterLength::Max)))
            );
            assert!(matches!(
                catalog.resolve(&qualified("j", "type")),
                Resolution::Plain { text: false }
            ));
            assert!(catalog.carrier(&qualified("j", "missing")).is_none());
        }
    }

    #[test]
    fn openjson_with_columns_retain_character_families_and_widths() {
        let catalog = catalog(
            "SELECT * FROM OPENJSON(NULL) WITH (u nvarchar(12), n nchar(3), a varchar(7), c char(4), number int) j",
        );
        assert_eq!(
            catalog.carrier(&qualified("j", "u")).unwrap().declared,
            Some(DataType::Nvarchar(Some(CharacterLength::IntegerLength {
                length: 12,
                unit: None
            })))
        );
        assert!(catalog.carrier(&qualified("j", "n")).is_some());
        assert!(
            catalog
                .carrier(&Expr::Identifier(Ident::new("u")))
                .is_some()
        );
        assert!(catalog.ansi(&qualified("j", "a")));
        assert!(catalog.ansi(&qualified("j", "c")));
        assert!(!catalog.textual(&qualified("j", "number")));
        assert!(catalog.carrier(&qualified("j", "key")).is_none());
    }

    #[test]
    fn openjson_default_qualifier_and_ambiguous_aliases_are_conservative() {
        let source = catalog("SELECT [value] FROM OPENJSON(NULL)");
        assert!(source.carrier(&qualified("openjson", "value")).is_some());
        assert!(
            source
                .carrier(&Expr::Identifier(Ident::new("value")))
                .is_some()
        );
        let source = catalog(
            "SELECT j.value FROM OPENJSON(NULL) j CROSS JOIN OPENJSON(NULL) WITH (value varchar(4)) j",
        );
        assert!(matches!(
            source.resolve(&qualified("j", "value")),
            Resolution::Unknown
        ));
        let source =
            catalog("SELECT j.value FROM OPENJSON(NULL) j CROSS JOIN (SELECT 1 AS value) j");
        assert!(matches!(
            source.resolve(&qualified("j", "value")),
            Resolution::Unknown
        ));
    }

    #[test]
    fn openjson_with_names_do_not_capture_scalar_variables() {
        let catalog = catalog(
            "SELECT j.[@p] FROM OPENJSON(NULL) WITH ([@p] nvarchar(12), [@@ROWCOUNT] nvarchar(8)) j",
        );
        for name in ["@p", "@@ROWCOUNT"] {
            assert!(matches!(
                catalog.resolve(&Expr::Identifier(Ident::new(name))),
                Resolution::Unknown
            ));
            assert!(!catalog.textual(&Expr::Identifier(Ident::new(name))));
            assert!(
                catalog
                    .carrier(&Expr::Identifier(Ident::with_quote('[', name)))
                    .is_some()
            );
            assert!(catalog.carrier(&qualified("j", name)).is_some());
        }
    }

    #[test]
    fn projection_aliases_still_hide_bare_names() {
        let catalog = catalog("SELECT 1 AS value, j.value FROM OPENJSON(NULL) j");
        assert!(matches!(
            catalog.resolve(&Expr::Identifier(Ident::new("value"))),
            Resolution::Unknown
        ));
        assert!(catalog.carrier(&qualified("j", "value")).is_some());
    }
}
