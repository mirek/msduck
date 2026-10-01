//! FOR JSON AUTO: source classification, nesting plans and row grouping.
//!
//! SQL Server derives the JSON shape from the SELECT list, as captured in
//! reference/gaps-json_string.json:
//!
//! - Each table-like source (base table, view, CTE, derived table or VALUES)
//!   whose column first appears in the SELECT list opens a nesting level. The
//!   first such table is the top level; every later one nests as an array
//!   inside the level opened just before it, named by its alias or by the
//!   table name as written (`dbo.b`).
//! - A column of a table belongs to that table's level. Any other item
//!   (expressions, variables, columns of table-valued functions such as
//!   STRING_SPLIT or OPENJSON) belongs to the deepest level opened so far,
//!   or the top level when none is open yet.
//! - Within an object, its own properties come first in SELECT order,
//!   followed by the nested array.
//! - Consecutive rows whose table columns at a level (and all levels above)
//!   are equal share that level's object. Expressions do not take part in
//!   the comparison, and leaf objects are never merged.
//! - Set operations (UNION, ...) produce flat objects.
use super::error;
use sqlparser::ast::*;

pub const NO_TABLE: &str = "FOR JSON AUTO requires at least one table for generating JSON objects. Use FOR JSON PATH or add a FROM clause with a table name.";
pub const UNNAMED: &str = "Column expressions and data sources without names or aliases cannot be formatted as JSON text using FOR JSON clause. Add alias to the unnamed column or table.";
pub const ROOT_WITHOUT_ARRAY_WRAPPER: &str = "ROOT option and WITHOUT_ARRAY_WRAPPER option cannot be used together in FOR JSON. Remove one of these options.";

/// Alias of the relation inside a lowered STRING_SPLIT derived table, so a
/// rewritten table-valued function keeps its expression semantics here.
pub const SPLIT_RELATION: &str = "__msduck_string_split";

/// The first SELECT of a query body (the left-most branch of a set operation).
pub fn first_select(body: &SetExpr) -> Option<&Select> {
    match body {
        SetExpr::Select(select) => Some(select),
        SetExpr::Query(query) => first_select(&query.body),
        SetExpr::SetOperation { left, .. } => first_select(left),
        _ => None,
    }
}

/// Whether the body combines several SELECTs; their rows stay flat.
pub fn flat(body: &SetExpr) -> bool {
    match body {
        SetExpr::Query(query) => flat(&query.body),
        SetExpr::Select(_) => false,
        _ => true,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SourceKind {
    /// Opens a nesting level.
    Table,
    /// Table-valued function: its columns behave like expressions.
    Function,
}

/// A row source of the FROM clause, in written order.
#[derive(Clone, Debug)]
pub struct Source {
    pub kind: SourceKind,
    /// The nested array's property name: the alias, or the name as written.
    pub key: String,
    alias: Option<String>,
    parts: Vec<String>,
}

impl Source {
    /// Whether a column qualifier (`b` in `b.x`, `dbo.b` in `dbo.b.x`)
    /// names this source. An alias hides the table name.
    pub fn matches(&self, qualifier: &[String]) -> bool {
        if qualifier.is_empty() {
            return false;
        }
        let qualifier = qualifier
            .iter()
            .map(|part| part.to_lowercase())
            .collect::<Vec<_>>();
        if let Some(alias) = &self.alias {
            return qualifier.len() == 1 && qualifier[0] == alias.to_lowercase();
        }
        let parts = self
            .parts
            .iter()
            .map(|part| part.to_lowercase())
            .collect::<Vec<_>>();
        parts.len() >= qualifier.len() && parts[parts.len() - qualifier.len()..] == qualifier[..]
    }
}

fn ident_parts(name: &ObjectName) -> Vec<String> {
    name.0
        .iter()
        .filter_map(|part| part.as_ident().map(|ident| ident.value.clone()))
        .collect()
}

/// Whether a derived table is a lowered STRING_SPLIT.
pub fn is_split(subquery: &Query) -> bool {
    matches!(subquery.body.as_ref(), SetExpr::Select(select)
        if matches!(select.from.as_slice(), [table] if matches!(&table.relation,
            TableFactor::UNNEST { alias: Some(alias), .. }
            | TableFactor::Derived { alias: Some(alias), .. }
            | TableFactor::Table { alias: Some(alias), .. }
            | TableFactor::Function { alias: Some(alias), .. }
                if alias.name.value == SPLIT_RELATION)))
}

fn collect(factor: &TableFactor, out: &mut Vec<Source>) {
    let alias_name = |alias: &Option<TableAlias>| alias.as_ref().map(|a| a.name.value.clone());
    match factor {
        TableFactor::NestedJoin {
            table_with_joins, ..
        } => {
            collect(&table_with_joins.relation, out);
            for join in &table_with_joins.joins {
                collect(&join.relation, out);
            }
        }
        TableFactor::Table {
            name, alias, args, ..
        } => {
            let parts = ident_parts(name);
            let alias = alias_name(alias);
            let key = alias.clone().unwrap_or_else(|| parts.join("."));
            out.push(Source {
                kind: if args.is_some() {
                    SourceKind::Function
                } else {
                    SourceKind::Table
                },
                key,
                alias,
                parts,
            });
        }
        TableFactor::Derived {
            subquery, alias, ..
        } => {
            let alias = alias_name(alias);
            out.push(Source {
                kind: if is_split(subquery) {
                    SourceKind::Function
                } else {
                    SourceKind::Table
                },
                key: alias.clone().unwrap_or_default(),
                alias,
                parts: Vec::new(),
            });
        }
        TableFactor::Pivot { alias, .. } | TableFactor::Unpivot { alias, .. } => {
            let alias = alias_name(alias);
            out.push(Source {
                kind: SourceKind::Table,
                key: alias.clone().unwrap_or_default(),
                alias,
                parts: Vec::new(),
            });
        }
        TableFactor::OpenJsonTable { alias, .. } => out.push(Source {
            kind: SourceKind::Function,
            key: alias_name(alias).unwrap_or_else(|| "openjson".into()),
            alias: alias_name(alias).or_else(|| Some("openjson".into())),
            parts: Vec::new(),
        }),
        TableFactor::Function { name, alias, .. } => {
            let parts = ident_parts(name);
            out.push(Source {
                kind: SourceKind::Function,
                key: alias_name(alias).unwrap_or_else(|| parts.join(".")),
                alias: alias_name(alias),
                parts,
            })
        }
        other => {
            let alias = match other {
                TableFactor::UNNEST { alias, .. }
                | TableFactor::TableFunction { alias, .. }
                | TableFactor::JsonTable { alias, .. } => alias_name(alias),
                _ => None,
            };
            out.push(Source {
                kind: SourceKind::Function,
                key: alias.clone().unwrap_or_default(),
                alias,
                parts: Vec::new(),
            })
        }
    }
}

/// Row sources of a SELECT in FROM-clause order, joins flattened.
pub fn sources(select: &Select) -> Vec<Source> {
    let mut out = Vec::new();
    for table in &select.from {
        collect(&table.relation, &mut out);
        for join in &table.joins {
            collect(&join.relation, &mut out);
        }
    }
    out
}

/// What a SELECT item refers to.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Reference {
    /// `*` (None) or `q.*` (the source index, when `q` names one).
    Star(Option<usize>),
    /// A column; `source` is None when its source is not known.
    Column {
        source: Option<usize>,
        name: String,
    },
    Expression {
        name: String,
    },
}

fn column_ids(expr: &Expr) -> Option<Vec<&Ident>> {
    match expr {
        Expr::Identifier(id) if !id.value.starts_with('@') => Some(vec![id]),
        Expr::CompoundIdentifier(ids) => Some(ids.iter().collect()),
        Expr::Nested(inner) => column_ids(inner),
        _ => None,
    }
}

/// Classify a SELECT item. `unqualified` resolves an unqualified column
/// name to the index of the one source that has it.
pub fn reference(
    item: &SelectItem,
    sources: &[Source],
    unqualified: &dyn Fn(&str) -> Option<usize>,
) -> Result<Reference, String> {
    let resolve = |ids: &[&Ident]| -> Option<usize> {
        if ids.len() == 1 {
            return unqualified(&ids[0].value);
        }
        let qualifier = ids[..ids.len() - 1]
            .iter()
            .map(|id| id.value.clone())
            .collect::<Vec<_>>();
        let mut matching = sources
            .iter()
            .enumerate()
            .filter(|(_, source)| source.matches(&qualifier));
        let first = matching.next()?.0;
        matching.next().is_none().then_some(first)
    };
    Ok(match item {
        SelectItem::Wildcard(_) => Reference::Star(None),
        SelectItem::QualifiedWildcard(kind, _) => {
            let SelectItemQualifiedWildcardKind::ObjectName(name) = kind else {
                return Err(error(13605, 1, 16, UNNAMED));
            };
            let qualifier = ident_parts(name);
            let mut matching = sources
                .iter()
                .enumerate()
                .filter(|(_, source)| source.matches(&qualifier));
            let found = matching.next().map(|(index, _)| index);
            Reference::Star(if matching.next().is_none() {
                found
            } else {
                None
            })
        }
        SelectItem::ExprWithAlias { expr, alias } => match column_ids(expr) {
            Some(ids) => Reference::Column {
                source: resolve(&ids),
                name: alias.value.clone(),
            },
            None => Reference::Expression {
                name: alias.value.clone(),
            },
        },
        SelectItem::UnnamedExpr(expr) => match column_ids(expr) {
            Some(ids) => Reference::Column {
                source: resolve(&ids),
                name: ids.last().expect("column identifiers").value.clone(),
            },
            None => return Err(error(13605, 1, 16, UNNAMED)),
        },
        _ => return Err(error(13605, 1, 16, UNNAMED)),
    })
}

/// Compile-time checks: a table source (13600) and named items (13605).
pub fn validate(body: &SetExpr) -> Result<(), String> {
    let select = first_select(body).ok_or_else(|| error(13600, 1, 16, NO_TABLE))?;
    if !sources(select)
        .iter()
        .any(|source| source.kind == SourceKind::Table)
    {
        return Err(error(13600, 1, 16, NO_TABLE));
    }
    for item in &select.projection {
        reference(item, &[], &|_| None)?;
    }
    Ok(())
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Column {
    pub name: String,
    pub level: usize,
    /// A table column: part of its level's grouping key.
    pub key: bool,
}

/// Column placement for every projected value, in result order, and the
/// property name of each nested level (index 0 is the top level).
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Plan {
    pub columns: Vec<Column>,
    pub levels: Vec<String>,
}

/// Place every result column. `fields` lists a source's column names, for
/// star expansion. Unknown stars are reported as unsupported.
pub fn plan(
    body: &SetExpr,
    unqualified: &dyn Fn(&str) -> Option<usize>,
    fields: &dyn Fn(usize) -> Option<Vec<String>>,
) -> Result<Plan, String> {
    let select = first_select(body).ok_or_else(|| error(13600, 1, 16, NO_TABLE))?;
    let sources = sources(select);
    let flat = flat(body);
    let mut plan = Plan::default();
    // Source index of each opened level.
    let mut opened: Vec<usize> = Vec::new();
    let mut place = |plan: &mut Plan, source: Option<usize>, name: String| {
        let table = source.filter(|&index| !flat && sources[index].kind == SourceKind::Table);
        let level = match table {
            Some(index) => match opened.iter().position(|&open| open == index) {
                Some(level) => level,
                None => {
                    opened.push(index);
                    plan.levels.push(sources[index].key.clone());
                    opened.len() - 1
                }
            },
            None => opened.len().saturating_sub(1),
        };
        plan.columns.push(Column {
            name,
            level,
            key: table.is_some(),
        });
    };
    for item in &select.projection {
        match reference(item, &sources, unqualified)? {
            Reference::Star(only) => {
                let indices = match only {
                    Some(index) => vec![index],
                    None if matches!(item, SelectItem::Wildcard(_)) => (0..sources.len()).collect(),
                    None => return Err("unsupported FOR JSON AUTO qualified star".into()),
                };
                for index in indices {
                    let names = fields(index).ok_or_else(|| {
                        "FOR JSON AUTO star requires known source columns".to_string()
                    })?;
                    for name in names {
                        place(&mut plan, Some(index), name);
                    }
                }
            }
            Reference::Column { source, name } => place(&mut plan, source, name),
            Reference::Expression { name } => place(&mut plan, None, name),
        }
    }
    if plan.levels.is_empty() {
        // Only expressions (or a flat set operation): one unnamed level.
        let key = sources
            .iter()
            .find(|source| source.kind == SourceKind::Table)
            .map(|source| source.key.clone())
            .unwrap_or_default();
        plan.levels.push(key);
    }
    Ok(plan)
}

/// One row's contribution to one level: its rendered properties (JSON
/// members without braces) and its grouping key.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct LevelRow {
    pub members: Vec<u16>,
    pub key: Vec<Option<Vec<u16>>>,
}

struct Open {
    key: Vec<Option<Vec<u16>>>,
    text: Vec<u16>,
    children: usize,
}

/// Streaming AUTO nesting over ordered rows. Produces the top-level objects;
/// wrapping (array, ROOT, WITHOUT_ARRAY_WRAPPER) belongs to the caller.
pub struct Grouper {
    names: Vec<Vec<u16>>,
    stack: Vec<Open>,
    objects: Vec<Vec<u16>>,
    units: usize,
    limit: usize,
}

pub const OUTPUT_LIMIT: &str = "FOR JSON result exceeds the configured output limit";

impl Grouper {
    /// `levels` are the nested property names (index 0 is unused).
    pub fn new(levels: &[String], limit: usize) -> Self {
        Self {
            names: levels.iter().map(|name| quoted(name)).collect(),
            stack: Vec::new(),
            objects: Vec::new(),
            units: 0,
            limit,
        }
    }

    fn grow(&mut self, units: usize) -> Result<(), &'static str> {
        self.units = self
            .units
            .checked_add(units)
            .filter(|&n| n <= self.limit)
            .ok_or(OUTPUT_LIMIT)?;
        Ok(())
    }

    pub fn push(&mut self, row: Vec<LevelRow>) -> Result<(), &'static str> {
        let last = self.names.len() - 1;
        if row.len() != last + 1 {
            return Err("FOR JSON AUTO row width does not match its plan");
        }
        let depth = if self.stack.is_empty() {
            0
        } else {
            (0..last)
                .find(|&level| self.stack[level].key != row[level].key)
                .unwrap_or(last)
        };
        while self.stack.len() > depth {
            self.close();
        }
        for (level, part) in row.into_iter().enumerate().skip(depth) {
            let mut text = Vec::with_capacity(part.members.len() + 2);
            text.push(u16::from(b'{'));
            text.extend_from_slice(&part.members);
            if level < last {
                if !part.members.is_empty() {
                    text.push(u16::from(b','));
                }
                text.extend_from_slice(&self.names[level + 1]);
                text.push(u16::from(b':'));
                text.push(u16::from(b'['));
            }
            // Closing punctuation and separators are reserved here too.
            self.grow(text.len() + 3)?;
            self.stack.push(Open {
                key: part.key,
                text,
                children: 0,
            });
        }
        Ok(())
    }

    fn close(&mut self) {
        let last = self.names.len() - 1;
        let open = self.stack.pop().expect("an open AUTO level");
        let mut text = open.text;
        if self.stack.len() < last {
            text.push(u16::from(b']'));
        }
        text.push(u16::from(b'}'));
        match self.stack.last_mut() {
            Some(parent) => {
                if parent.children > 0 {
                    parent.text.push(u16::from(b','));
                }
                parent.text.extend(text);
                parent.children += 1;
            }
            None => self.objects.push(text),
        }
    }

    /// The completed top-level objects, in order.
    pub fn finish(mut self) -> Vec<Vec<u16>> {
        while !self.stack.is_empty() {
            self.close();
        }
        self.objects
    }
}

/// A JSON string literal for a property name.
pub fn quoted(name: &str) -> Vec<u16> {
    let units = name.encode_utf16().collect::<Vec<_>>();
    let escaped = msduck_core::json_escape::escape_utf16(&units, &[106, 115, 111, 110])
        .expect("JSON format is supported");
    let mut out = Vec::with_capacity(escaped.len() + 2);
    out.push(u16::from(b'"'));
    out.extend_from_slice(&escaped);
    out.push(u16::from(b'"'));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dialect::ServerDialect;
    use sqlparser::parser::Parser;

    fn body(sql: &str) -> Box<SetExpr> {
        let Statement::Query(query) = Parser::parse_sql(&ServerDialect, sql).unwrap().remove(0)
        else {
            unreachable!()
        };
        query.body
    }

    fn text(units: &[u16]) -> String {
        String::from_utf16(units).unwrap()
    }

    fn member(name: &str, value: &str) -> Vec<u16> {
        let mut out = quoted(name);
        out.push(u16::from(b':'));
        out.extend(value.encode_utf16());
        out
    }

    #[test]
    fn levels_follow_first_appearance_and_expressions_join_the_deepest_level() {
        let unqualified = |name: &str| match name {
            "name" => Some(0),
            "x" => Some(1),
            _ => None,
        };
        let fields = |_| None;
        let plan = plan(
            &body("SELECT 5 AS k, a.id, b.x, a.name, 1 AS e, c.y FROM dbo.a a JOIN dbo.b b ON 1=1 JOIN c ON 1=1"),
            &unqualified,
            &fields,
        )
        .unwrap();
        let placed = plan
            .columns
            .iter()
            .map(|c| (c.name.as_str(), c.level, c.key))
            .collect::<Vec<_>>();
        assert_eq!(
            placed,
            [
                ("k", 0, false),
                ("id", 0, true),
                ("x", 1, true),
                ("name", 0, true),
                ("e", 1, false),
                ("y", 2, true)
            ]
        );
        assert_eq!(plan.levels, ["a", "b", "c"]);
        // Unaliased tables are named as written; TVF columns are expressions.
        let plan = plan_of(
            "SELECT a.id, b.x, s.value FROM dbo.a JOIN dbo.b ON 1=1 CROSS APPLY STRING_SPLIT('a', ',') s",
        );
        assert_eq!(plan.levels, ["dbo.a", "dbo.b"]);
        assert_eq!(plan.columns[2].level, 1);
        assert!(!plan.columns[2].key);
        // An expression of a later table's column stays an expression.
        let plan = plan_of("SELECT a.id+0 AS e, b.x FROM dbo.a a JOIN dbo.b b ON 1=1");
        assert_eq!(plan.levels, ["b"]);
        assert!(plan.columns.iter().all(|c| c.level == 0));
        // Set operations are flat.
        let plan = plan_of("SELECT a.id, b.x FROM a JOIN b ON 1=1 UNION ALL SELECT 1, 'z'");
        assert_eq!(plan.levels, ["a"]);
        assert!(plan.columns.iter().all(|c| c.level == 0 && !c.key));
    }

    fn plan_of(sql: &str) -> Plan {
        plan(&body(sql), &|_| None, &|_| None).unwrap()
    }

    #[test]
    fn compile_errors() {
        let code = |sql: &str| {
            let Statement::Query(query) = Parser::parse_sql(&ServerDialect, sql).unwrap().remove(0)
            else {
                unreachable!()
            };
            super::super::validate_for_json_auto(&query)
                .err()
                .map(|e| e.split(':').nth(1).unwrap().parse::<i32>().unwrap())
        };
        assert_eq!(code("SELECT 1 AS value FOR JSON AUTO"), Some(13600));
        assert_eq!(
            code("SELECT value FROM STRING_SPLIT('a,b', ',') FOR JSON AUTO"),
            Some(13600)
        );
        assert_eq!(code("SELECT a.id+1 FROM a FOR JSON AUTO"), Some(13605));
        assert_eq!(
            code("SELECT 1 FOR JSON AUTO, ROOT('r'), WITHOUT_ARRAY_WRAPPER"),
            Some(13620)
        );
        assert_eq!(code("SELECT (id) FROM a FOR JSON AUTO"), None);
        assert_eq!(code("SELECT x FROM (VALUES (1)) v(x) FOR JSON AUTO"), None);
    }

    #[test]
    fn grouping_merges_consecutive_equal_parents_only() {
        let key = |v: &str| vec![Some(v.encode_utf16().collect::<Vec<_>>())];
        let row = |a: &str, b: &str, c: Option<&str>| {
            vec![
                LevelRow {
                    members: member("id", a),
                    key: key(a),
                },
                LevelRow {
                    members: member("x", b),
                    key: key(b),
                },
                LevelRow {
                    members: c.map(|c| member("y", c)).unwrap_or_default(),
                    key: vec![c.map(|c| c.encode_utf16().collect())],
                },
            ]
        };
        let mut grouper = Grouper::new(&["a".into(), "b".into(), "c".into()], 1 << 20);
        grouper.push(row("1", "\"p\"", Some("7"))).unwrap();
        grouper.push(row("1", "\"p\"", Some("8"))).unwrap();
        grouper.push(row("1", "\"q\"", None)).unwrap();
        grouper.push(row("2", "\"r\"", Some("9"))).unwrap();
        grouper.push(row("1", "\"p\"", Some("7"))).unwrap();
        let objects = grouper.finish();
        assert_eq!(
            objects.iter().map(|o| text(o)).collect::<Vec<_>>(),
            [
                r#"{"id":1,"b":[{"x":"p","c":[{"y":7},{"y":8}]},{"x":"q","c":[{}]}]}"#,
                r#"{"id":2,"b":[{"x":"r","c":[{"y":9}]}]}"#,
                r#"{"id":1,"b":[{"x":"p","c":[{"y":7}]}]}"#,
            ]
        );
        let mut leaf = Grouper::new(&["d".into()], 1 << 20);
        for _ in 0..2 {
            leaf.push(vec![LevelRow {
                members: member("k", "1"),
                key: key("1"),
            }])
            .unwrap();
        }
        assert_eq!(leaf.finish().len(), 2);
        let mut small = Grouper::new(&["d".into()], 8);
        assert!(
            small
                .push(vec![LevelRow {
                    members: member("long", "123456"),
                    key: key("1"),
                }])
                .is_err()
        );
    }
}
