//! Effectful acquisition and Arrow decoding for checked projection plans.
use crate::parameter::Parameter;
use anyhow::{Result, bail, ensure};
use duckdb::{
    Connection,
    arrow::{
        array::{Array, Int32Array, LargeStringArray, StringArray, UInt8Array},
        record_batch::RecordBatch,
    },
};
use msduck_core::{catalog::TypeMetadata, diagnostic::SqlError};
use msduck_sql::{binding_scope::Scope, checked_projection::Plan};
use sqlparser::ast::{DataType, Query};
use std::collections::HashMap;

pub fn plan(
    db: &Connection,
    query: &Query,
    parameters: &HashMap<String, Parameter>,
) -> Result<Option<Plan>> {
    let mut outer = Scope::default();
    for (name, parameter) in parameters {
        let id = match parameter.ast_type() {
            DataType::Int(_) | DataType::Integer(_) => 56,
            DataType::BigInt(_) => 127,
            _ => continue,
        };
        outer.parameters.insert(
            name.to_lowercase(),
            TypeMetadata {
                system_type_id: Some(id),
                ..Default::default()
            },
        );
    }
    for name in ["@@trancount", "@@rowcount", "@@error"] {
        outer.parameters.insert(
            name.into(),
            TypeMetadata {
                system_type_id: Some(56),
                ..Default::default()
            },
        );
    }
    let scopes = crate::query_catalog::scopes(db, query, &outer)?;
    Ok(msduck_sql::checked_projection::plan(query, &scopes.body))
}

pub fn diagnostic(batch: &RecordBatch, row: usize, width: usize) -> Result<Option<SqlError>> {
    ensure!(
        width > 0 && width <= 32 && batch.num_columns() == width * 5 && row < batch.num_rows(),
        "invalid checked projection shape"
    );
    for column in 0..width {
        let first = width + column * 4;
        let fields = (0..4).map(|i| batch.column(first + i)).collect::<Vec<_>>();
        if fields[0].is_null(row) {
            ensure!(
                fields.iter().all(|field| field.is_null(row)),
                "incomplete checked projection diagnostic"
            );
            continue;
        }
        ensure!(
            batch.column(column).is_null(row) && fields.iter().all(|field| !field.is_null(row)),
            "invalid checked projection diagnostic validity"
        );
        let Some(numbers) = fields[0].as_any().downcast_ref::<Int32Array>() else {
            bail!("invalid checked projection error number")
        };
        let Some(states) = fields[1].as_any().downcast_ref::<UInt8Array>() else {
            bail!("invalid checked projection error state")
        };
        let Some(severities) = fields[2].as_any().downcast_ref::<UInt8Array>() else {
            bail!("invalid checked projection error severity")
        };
        let message = if let Some(text) = fields[3].as_any().downcast_ref::<StringArray>() {
            text.value(row)
        } else if let Some(text) = fields[3].as_any().downcast_ref::<LargeStringArray>() {
            text.value(row)
        } else {
            bail!("invalid checked projection error message")
        };
        let mut error = SqlError::new(numbers.value(row), states.value(row), message);
        error.severity = severities.value(row);
        return Ok(Some(error));
    }
    Ok(None)
}

/// The complete checked plan supplies declared integer result types even when
/// ordinary catalog inference leaves a literal's type unresolved. Provenance
/// and names still come from the original, aligned public projection.
pub fn metadata(plan: &Plan, fields: &[crate::query_catalog::Field]) -> Result<Option<Vec<u8>>> {
    use crate::tds::{self, Column, Type};
    use msduck_sql::checked_expression::Kind;
    if fields.len() != plan.width || plan.kinds.len() != plan.width {
        return Ok(None);
    }
    let columns = fields
        .iter()
        .zip(&plan.kinds)
        .map(|(field, kind)| Column {
            name: field.name.clone(),
            properties: field.properties,
            collation: None,
            kind: match kind {
                Kind::Int => Type::Int(4),
                Kind::BigInt => Type::Int(8),
                Kind::Boolean => Type::Bit,
            },
        })
        .collect::<Vec<_>>();
    let mut result = Vec::new();
    tds::metadata(&mut result, &columns)?;
    Ok(Some(result))
}

#[cfg(test)]
mod tests {
    #[test]
    fn typed_null_projection_keeps_transaction_committable() {
        let server = crate::server::Server::open(":memory:").unwrap();
        let mut session = crate::engine::Session::new(server.connection().unwrap()).unwrap();
        let (tokens, ok) = session.batch_response(
            "BEGIN TRAN; SELECT NULL/0 AS n,CAST(NULL AS BIGINT)+1 AS b; COMMIT",
            &Default::default(),
            false,
            None,
        );
        assert!(ok, "{tokens:?}");
    }
    #[test]
    fn null_projection_binding_survives_existing_operand_lowering() {
        use sqlparser::{ast::Statement, parser::Parser};
        let server = crate::server::Server::open(":memory:").unwrap();
        let db = server.connection().unwrap();
        let parameters = Default::default();
        let mut statement = Parser::parse_sql(
            &msduck_sql::dialect::ServerDialect,
            "SELECT NULL/0 AS n,CAST(NULL AS BIGINT)+1 AS b",
        )
        .unwrap()
        .remove(0);
        msduck_sql::variant_cast::mark(&mut statement);
        let Statement::Query(query) = &mut statement else {
            panic!()
        };
        let fields = crate::query_catalog::bind_query_with_parameters(&db, query, &parameters)
            .unwrap()
            .unwrap();
        let plan = super::plan(&db, query, &parameters).unwrap().unwrap();
        assert!(super::metadata(&plan, &fields).unwrap().is_some());
        crate::query_catalog::bind_binary_operations(&db, &mut statement, &parameters).unwrap();
        crate::concat_lower::statement(&mut statement, &parameters).unwrap();
        crate::aggregate_columns::annotate(&db, &mut statement, &parameters).unwrap();
        crate::concat_lower::annotated_unicode_casts(&mut statement);
        crate::query_catalog::bind_unicode_operations(&db, &mut statement, &parameters).unwrap();
        crate::for_json::lower_nested(&db, &mut statement, &parameters).unwrap();
        let Statement::Query(query) = &statement else {
            panic!()
        };
        assert!(
            super::plan(&db, query, &parameters).unwrap().is_some(),
            "{query}"
        );
    }
}
