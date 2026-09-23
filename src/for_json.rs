//! SQL result adapter for the deterministic FOR JSON PATH writer.
use crate::{
    engine::Parameter,
    tds::{self, Column, Type},
};
use anyhow::{Result, bail, ensure};
use duckdb::{
    Connection,
    types::{TimeUnit, Value},
};
use msduck_core::for_json::{self as json, Fragment, Number, Options, PathPlan, Writer};
use msduck_sql::for_json as syntax;
use sqlparser::ast::{SelectItem, Statement};
use std::collections::{HashMap, HashSet};

mod native;
pub use native::register;

pub struct Output {
    options: Options,
    fragments: HashSet<String>,
}
impl Output {
    pub fn take(db: &Connection, statement: &mut Statement) -> Result<Option<Self>> {
        Self::take_scoped(db, statement, &crate::query_catalog::Scope::default())
    }
    fn take_scoped(
        db: &Connection,
        statement: &mut Statement,
        scope: &crate::query_catalog::Scope,
    ) -> Result<Option<Self>> {
        let Some(options) = syntax::take(statement)? else {
            return Ok(None);
        };
        let Statement::Query(query) = statement else {
            unreachable!()
        };
        let mut fragments = HashSet::new();
        for item in &syntax::projection(&query.body)?.projection {
            let expr = match item {
                SelectItem::ExprWithAlias { expr, .. } | SelectItem::UnnamedExpr(expr) => expr,
                _ => continue,
            };
            let Some(name) = syntax::name(item)? else {
                continue;
            };
            if syntax::fragment(expr) {
                fragments.insert(name.to_owned());
            }
        }
        if let Some(fields) = crate::query_catalog::projection_in_scope(db, query, scope)? {
            for field in fields {
                if field.json_fragment {
                    fragments.insert(field.name.clone());
                }
            }
        }
        Ok(Some(Self { options, fragments }))
    }
    pub fn names(db: &Connection, source: &str, values: &[Value]) -> Result<Vec<String>> {
        let mut describe = db.prepare(&format!("DESCRIBE {source}"))?;
        Ok(describe
            .query_map(duckdb::params_from_iter(values.iter()), |row| row.get(0))?
            .collect::<duckdb::Result<Vec<_>>>()?)
    }
    pub fn plan(&self, names: &[String]) -> Result<PathPlan> {
        Ok(PathPlan::new(names).map_err(syntax::diagnostic)?)
    }
    pub fn writer<'a>(&self, plan: &'a PathPlan) -> Result<Writer<'a>> {
        Ok(Writer::new(plan, self.options.clone()).map_err(syntax::diagnostic)?)
    }
    pub fn row(
        &self,
        writer: &mut Writer<'_>,
        names: &[String],
        values: &[Value],
        types: &[Option<Type>],
    ) -> Result<()> {
        let strings = values
            .iter()
            .enumerate()
            .map(|(i, value)| spelling(value, types.get(i).and_then(Option::as_ref)))
            .collect::<Result<Vec<_>>>()?;
        let row = values
            .iter()
            .zip(&strings)
            .zip(names)
            .map(|((value, text), name)| {
                Ok(match value {
                    Value::Null => json::Value::Null,
                    Value::Boolean(v) => json::Value::Boolean(*v),
                    Value::Text(_) if self.fragments.contains(name) => {
                        json::Value::Json(Fragment::new(text)?)
                    }
                    Value::TinyInt(_)
                    | Value::SmallInt(_)
                    | Value::Int(_)
                    | Value::BigInt(_)
                    | Value::HugeInt(_)
                    | Value::UTinyInt(_)
                    | Value::USmallInt(_)
                    | Value::UInt(_)
                    | Value::UBigInt(_)
                    | Value::Float(_)
                    | Value::Double(_)
                    | Value::Decimal(_) => json::Value::Number(Number::new(text)?),
                    _ => json::Value::Text(text),
                })
            })
            .collect::<Result<Vec<_>>>()?;
        writer.push(&row)?;
        ensure!(
            writer.buffered_bytes() <= tds::MAX_MESSAGE,
            "result exceeds current 16 MiB response limit"
        );
        Ok(())
    }
    pub fn encode(text: String) -> Result<Vec<u8>> {
        ensure!(
            text.encode_utf16().count() <= (tds::MAX_MESSAGE - 256) / 2,
            "result exceeds current 16 MiB response limit"
        );
        let mut out = Vec::new();
        tds::metadata(
            &mut out,
            &[Column {
                collation: None,
                properties: Default::default(),
                name: "JSON_F52E2B61-18A1-11d1-B105-00805F49916B".into(),
                kind: Type::Text,
            }],
        )?;
        out.push(0xd1);
        crate::engine::encode_value(&mut out, &Type::Text, &Value::Text(text))?;
        Ok(out)
    }
}
fn nanos(unit: TimeUnit, value: i64) -> i128 {
    i128::from(value)
        * match unit {
            TimeUnit::Second => 1_000_000_000,
            TimeUnit::Millisecond => 1_000_000,
            TimeUnit::Microsecond => 1_000,
            TimeUnit::Nanosecond => 1,
        }
}
fn spelling(value: &Value, kind: Option<&Type>) -> Result<String> {
    Ok(match value {
        Value::Null | Value::Boolean(_) => String::new(),
        Value::Text(text) if matches!(kind, Some(Type::DateTimeOffset(_))) => {
            let (civil, offset) = text
                .rsplit_once(' ')
                .ok_or_else(|| anyhow::anyhow!("invalid native DATETIMEOFFSET representation"))?;
            format!("{civil}{offset}")
        }
        Value::Text(text) => text.clone(),
        Value::TinyInt(v) => v.to_string(),
        Value::SmallInt(v) => v.to_string(),
        Value::Int(v) => v.to_string(),
        Value::BigInt(v) => v.to_string(),
        Value::HugeInt(v) => v.to_string(),
        Value::UTinyInt(v) => v.to_string(),
        Value::USmallInt(v) => v.to_string(),
        Value::UInt(v) => v.to_string(),
        Value::UBigInt(v) => v.to_string(),
        Value::Float(v) => v.to_string(),
        Value::Double(v) => v.to_string(),
        Value::Decimal(v) => v.to_string(),
        Value::Blob(bytes) => json::base64(bytes),
        Value::Date32(days) => {
            crate::datetime2::DateTime2::from_unix_nanos(i128::from(*days) * 86_400_000_000_000)?
                .format_iso(0)?[..10]
                .into()
        }
        Value::Timestamp(unit, value) => {
            crate::datetime2::DateTime2::from_unix_nanos(nanos(*unit, *value))?.format_iso(3)?
        }
        Value::Time64(unit, value) => {
            let scale = match kind {
                Some(Type::Time(scale)) => *scale,
                _ => 7,
            };
            crate::datetime2::DateTime2::from_unix_nanos(nanos(*unit, *value))?.format_iso(scale)?
                [11..]
                .into()
        }
        _ => bail!("unsupported FOR JSON value type"),
    })
}

/// Bind nested JSON metadata before backend lowering, retaining correlation in the AST.
pub fn lower_nested<T: sqlparser::ast::VisitMut>(
    db: &Connection,
    statement: &mut T,
    parameters: &HashMap<String, Parameter>,
) -> Result<()> {
    use sqlparser::ast::{Query, VisitorMut};
    use std::ops::ControlFlow;
    struct Find;
    impl VisitorMut for Find {
        type Break = ();
        fn pre_visit_query(&mut self, query: &mut Query) -> ControlFlow<()> {
            if matches!(
                query.for_clause,
                Some(sqlparser::ast::ForClause::Json { .. })
            ) {
                ControlFlow::Break(())
            } else {
                ControlFlow::Continue(())
            }
        }
    }
    if statement.visit(&mut Find).is_continue() {
        return Ok(());
    }
    struct Lower<'a> {
        db: &'a Connection,
        parameters: &'a HashMap<String, Parameter>,
        scopes: Vec<crate::query_catalog::QueryScopes>,
    }
    impl VisitorMut for Lower<'_> {
        type Break = anyhow::Error;
        fn pre_visit_query(&mut self, query: &mut Query) -> ControlFlow<Self::Break> {
            let inherited = self
                .scopes
                .last_mut()
                .map(|parent| {
                    parent
                        .definitions
                        .pop_front()
                        .unwrap_or_else(|| parent.body.clone())
                })
                .unwrap_or_default();
            match crate::query_catalog::scopes(self.db, query, &inherited) {
                Ok(scope) => {
                    self.scopes.push(scope);
                    ControlFlow::Continue(())
                }
                Err(error) => ControlFlow::Break(error.into()),
            }
        }
        fn post_visit_query(&mut self, query: &mut Query) -> ControlFlow<Self::Break> {
            let scope = self.scopes.pop().expect("balanced query scopes");
            if !matches!(
                query.for_clause,
                Some(sqlparser::ast::ForClause::Json { .. })
            ) {
                return ControlFlow::Continue(());
            }
            let result = (|| -> Result<()> {
                let mut statement = Statement::Query(Box::new(query.clone()));
                let output =
                    Output::take_scoped(self.db, &mut statement, &scope.inherited)?.unwrap();
                let Statement::Query(mut source) = statement else {
                    unreachable!()
                };
                crate::query_catalog::expand_qualified_stars(
                    self.db,
                    &mut source,
                    &scope.inherited,
                )?;
                let fields =
                    crate::query_catalog::projection_in_scope(self.db, &source, &scope.inherited)?;
                let items = &syntax::projection(&source.body)?.projection;
                let explicit = items.iter().map(syntax::name).collect::<Result<Vec<_>>>()?;
                let names = if explicit.iter().all(Option::is_some) {
                    explicit
                        .into_iter()
                        .flatten()
                        .map(str::to_owned)
                        .collect::<Vec<_>>()
                } else {
                    let fields = fields.as_ref().ok_or_else(|| {
                        anyhow::anyhow!("nested FOR JSON star requires known source columns")
                    })?;
                    ensure!(
                        !fields.is_empty(),
                        "nested FOR JSON star requires known source columns"
                    );
                    fields.iter().map(|field| field.name.clone()).collect()
                };
                let flags = names
                    .iter()
                    .map(|name| output.fragments.contains(name))
                    .collect::<Vec<_>>();
                let types = crate::result_types::bound_projection(
                    self.db,
                    &Statement::Query(source.clone()),
                    self.parameters,
                )?;
                let scales = (0..names.len())
                    .map(|i| match types.get(i) {
                        Some(Some(Type::Time(scale))) => Some(*scale),
                        _ => fields
                            .as_ref()
                            .and_then(|fields| fields.get(i))
                            .and_then(|field| {
                                crate::query_catalog::time_scale(field.info.as_ref())
                            }),
                    })
                    .collect::<Vec<_>>();
                let spec = serde_json::to_string(&(
                    &names,
                    flags,
                    output.options.include_null_values,
                    scales,
                ))?;
                *query = *source;
                syntax::lower(query, names, spec, &output.options)
            })();
            match result {
                Ok(()) => ControlFlow::Continue(()),
                Err(e) => ControlFlow::Break(e),
            }
        }
    }
    match statement.visit(&mut Lower {
        db,
        parameters,
        scopes: Vec::new(),
    }) {
        ControlFlow::Continue(()) => Ok(()),
        ControlFlow::Break(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn preparation_does_not_evaluate_and_execution_visits_each_source_row_once() {
        let server = crate::server::Server::open(":memory:").unwrap();
        let mut session = crate::engine::Session::new(server.connection().unwrap()).unwrap();
        session
            .db
            .execute_batch("CREATE SEQUENCE json_counter START 1")
            .unwrap();
        let sql = "SELECT nextval('json_counter') AS n FROM range(6000) FOR JSON PATH";
        session.validate_prepared_sql(sql, &[]).unwrap();
        let first: i64 = session
            .db
            .query_row("SELECT nextval('json_counter')", [], |row| row.get(0))
            .unwrap();
        assert_eq!(first, 1);
        let response = session.batch(sql, &HashMap::new(), false);
        assert_eq!(session.last_error, 0, "{response:?}");
        let next: i64 = session
            .db
            .query_row("SELECT nextval('json_counter')", [], |row| row.get(0))
            .unwrap();
        assert_eq!(next, 6002);
        let invalid = "SELECT *, nextval('json_counter') AS n FROM (SELECT 1 AS n) s FOR JSON PATH";
        assert!(session.validate_prepared_sql(invalid, &[]).is_err());
        session.batch(invalid, &HashMap::new(), false);
        assert_eq!(session.last_error, 13601);
        let next: i64 = session
            .db
            .query_row("SELECT nextval('json_counter')", [], |row| row.get(0))
            .unwrap();
        assert_eq!(next, 6003);
    }
}

#[cfg(test)]
mod correlated_star_tests {
    #[test]
    fn outer_star_expansion_never_copies_or_executes_volatile_source_expressions() {
        let server = crate::server::Server::open(":memory:").unwrap();
        let mut session = crate::engine::Session::new(server.connection().unwrap()).unwrap();
        session
            .db
            .execute_batch("CREATE SEQUENCE json_star_calls START 1")
            .unwrap();
        assert!(session.batch_response(
            "CREATE TABLE dbo.json_star_inputs(n INT); INSERT INTO dbo.json_star_inputs SELECT CAST(i AS INT) FROM range(6000) r(i)",
            &Default::default(), false, None,
        ).1);
        let sql = "SELECT (SELECT p.* FOR JSON PATH) AS payload FROM (SELECT nextval('json_star_calls') AS n FROM dbo.json_star_inputs) p";
        session.validate_prepared_sql(sql, &[]).unwrap();
        assert_eq!(
            session
                .db
                .query_row("SELECT nextval('json_star_calls')", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            1
        );
        assert!(
            session
                .batch_response(sql, &Default::default(), false, None)
                .1
        );
        assert_eq!(
            session
                .db
                .query_row("SELECT currval('json_star_calls')", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            6001
        );
    }
}
