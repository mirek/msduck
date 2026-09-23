//! Default OPENJSON rows retain source text, duplicate keys and lexical numbers.
use duckdb::{
    core::{DataChunkHandle, Inserter, LogicalTypeHandle as Type, LogicalTypeId as Id},
    vscalar::{ScalarFunctionSignature, VScalar},
    vtab::arrow::WritableVector,
};
use msduck_core::{diagnostic::SqlError, openjson::rows};
use sqlparser::{ast::*, dialect::GenericDialect, parser::Parser};
mod binary;
mod schema;
pub fn diagnostic(message: &str) -> Option<SqlError> {
    msduck_core::openjson::diagnostic(message).or_else(|| schema::diagnostic(message))
}
struct OpenJson;
impl VScalar for OpenJson {
    type State = ();
    fn invoke(
        _: &(),
        input: &mut DataChunkHandle,
        output: &mut dyn WritableVector,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let len = input.len();
        let inputs = [input.flat_vector(0), input.flat_vector(1)];
        let mut all = Vec::new();
        let mut result = output.list_vector();
        for row in 0..len {
            let offset = all.len();
            if !inputs.iter().any(|v| v.row_is_null(row as u64)) {
                let mut texts = Vec::with_capacity(2);
                for input in &inputs {
                    // Exact VARCHAR arguments: copy inline storage before borrowing its bytes.
                    let mut value = unsafe {
                        input.as_slice_with_len::<duckdb::ffi::duckdb_string_t>(len)[row]
                    };
                    let bytes = unsafe {
                        std::slice::from_raw_parts(
                            duckdb::ffi::duckdb_string_t_data(&mut value).cast::<u8>(),
                            duckdb::ffi::duckdb_string_t_length(value) as usize,
                        )
                    };
                    texts.push(std::str::from_utf8(bytes)?.to_owned());
                }
                all.extend(rows(&texts[0], &texts[1])?);
            }
            result.set_entry(row, offset, all.len() - offset);
        }
        let child = result.struct_child(all.len());
        result.try_set_len(all.len())?;
        let keys = child.child(0, all.len());
        let mut values = child.child(1, all.len());
        let mut kinds = child.child(2, all.len());
        for (i, row) in all.iter().enumerate() {
            keys.insert(i, row.key.as_str());
            if let Some(value) = &row.value {
                values.insert(i, value.as_str());
            } else {
                values.set_null(i);
            }
            // The child is INTEGER and reserved for all output entries.
            unsafe {
                kinds.as_mut_slice_with_len::<i32>(all.len())[i] = row.kind;
            }
        }
        Ok(())
    }
    fn signatures() -> Vec<ScalarFunctionSignature> {
        vec![ScalarFunctionSignature::exact(
            vec![Id::Varchar.into(), Id::Varchar.into()],
            Type::list(&Type::struct_type(&[
                ("key", Id::Varchar.into()),
                ("value", Id::Varchar.into()),
                ("type", Id::Integer.into()),
            ])),
        )]
    }
}
pub fn register(db: &duckdb::Connection) -> duckdb::Result<()> {
    db.register_scalar_function::<OpenJson>("__msduck_openjson")?;
    schema::register(db)?;
    binary::register(db)
}
fn path_expression(path: &Option<ValueWithSpan>) -> Expr {
    match path {
        Some(path) if matches!(&path.value,Value::Placeholder(name) if name.starts_with('@')) => {
            let Value::Placeholder(name) = &path.value else {
                unreachable!()
            };
            Expr::Identifier(Ident::with_span(path.span, name.clone()))
        }
        Some(path) => Expr::Value(path.clone()),
        None => Expr::Value(Value::SingleQuotedString("$".into()).into()),
    }
}
pub fn lower(factor: &mut TableFactor) -> Result<(), String> {
    let TableFactor::OpenJsonTable {
        json_expr,
        json_path,
        columns,
        alias,
    } = factor
    else {
        return Ok(());
    };

    let mut statement = Parser::parse_sql(&GenericDialect{}, "SELECT x.r.key AS key, x.r.value AS value, x.r.type AS type FROM UNNEST(__msduck_openjson(NULL,NULL)) AS x(r)").map_err(|e|e.to_string())?.remove(0);
    if !columns.is_empty() {
        let Statement::Query(query) = &mut statement else {
            unreachable!()
        };
        let SetExpr::Select(select) = query.body.as_mut() else {
            unreachable!()
        };
        select.projection = schema::projection(columns)?;
    }
    struct Bind {
        explicit: bool,
        source: Expr,
        path: Expr,
    }
    impl VisitorMut for Bind {
        type Break = ();
        fn pre_visit_expr(&mut self, e: &mut Expr) -> std::ops::ControlFlow<()> {
            if let Expr::Function(f) = e
                && f.name.to_string() == "__msduck_openjson"
            {
                let text = |expr| Expr::Cast {
                    kind: CastKind::Cast,
                    expr: Box::new(expr),
                    data_type: DataType::Text,
                    format: None,
                };
                *e = crate::engine::binary_function(
                    if self.explicit {
                        "__msduck_openjson_sources"
                    } else {
                        "__msduck_openjson"
                    },
                    text(self.source.clone()),
                    text(self.path.clone()),
                );
                return std::ops::ControlFlow::Break(());
            }
            std::ops::ControlFlow::Continue(())
        }
    }
    let _ = VisitMut::visit(
        &mut statement,
        &mut Bind {
            explicit: !columns.is_empty(),
            source: json_expr.clone(),
            path: path_expression(json_path),
        },
    );
    let Statement::Query(subquery) = statement else {
        unreachable!()
    };
    *factor = TableFactor::Derived {
        lateral: true,
        sample: None,
        subquery,
        alias: Some(alias.clone().unwrap_or_else(|| TableAlias {
            name: Ident::new("openjson"),
            columns: vec![],
            explicit: false,
            at: None,
        })),
    };
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn template_binding_does_not_revisit_user_expressions() {
        let mut statement = Parser::parse_sql(
            &crate::dialect::ServerDialect,
            "SELECT * FROM OPENJSON(__msduck_openjson(NULL,NULL))",
        )
        .unwrap()
        .remove(0);
        struct Lower;
        impl VisitorMut for Lower {
            type Break = String;
            fn pre_visit_table_factor(
                &mut self,
                factor: &mut TableFactor,
            ) -> std::ops::ControlFlow<String> {
                if let Err(error) = lower(factor) {
                    return std::ops::ControlFlow::Break(error);
                }
                std::ops::ControlFlow::Continue(())
            }
        }
        assert!(VisitMut::visit(&mut statement, &mut Lower).is_continue());
        assert_eq!(
            statement.to_string().matches("__msduck_openjson").count(),
            2
        );
    }
    #[test]
    fn native_nested_vectors_and_single_evaluation() {
        let db = duckdb::Connection::open_in_memory().unwrap();
        register(&db).unwrap();
        let document = format!(
            "[{}]",
            (0..6000)
                .map(|i| if i % 17 == 0 {
                    "null".into()
                } else {
                    i.to_string()
                })
                .collect::<Vec<_>>()
                .join(",")
        );
        let wrong:i64=db.query_row("SELECT count(*) FROM UNNEST(__msduck_openjson(?, '$')) t(r) WHERE r.value IS DISTINCT FROM CASE WHEN CAST(r.key AS INT)%17=0 THEN NULL ELSE r.key END OR r.type <> CASE WHEN CAST(r.key AS INT)%17=0 THEN 0 ELSE 2 END",[document],|r|r.get(0)).unwrap();
        assert_eq!(wrong, 0);
        db.execute_batch("CREATE SEQUENCE doc_calls; CREATE SEQUENCE path_calls")
            .unwrap();
        let count:i64=db.query_row("SELECT count(*) FROM (SELECT unnest(__msduck_openjson(printf('[%d,null]',nextval('doc_calls')),CASE WHEN nextval('path_calls')%17=0 THEN NULL ELSE '$' END)) r FROM range(6000))",[],|r|r.get(0)).unwrap();
        assert_eq!(count, (6000 - 6000 / 17) * 2);
        for sequence in ["doc_calls", "path_calls"] {
            let n: i64 = db
                .query_row(&format!("SELECT currval('{sequence}')"), [], |r| r.get(0))
                .unwrap();
            assert_eq!(n, 6000);
        }
    }
}
