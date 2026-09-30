//! Nonexecuting preparation descriptions over explicit catalog declarations.
use crate::{engine::Session, parameter::Parameter, tds};
use anyhow::{Result, bail, ensure};
use msduck_core::{catalog::TypeMetadata, types::Type as SqlType, value::Value};
use msduck_sql::{binding_scope::Scope, projection};
use sqlparser::ast::{
    DataType, ExactNumberInfo, Expr, FunctionArg, FunctionArgExpr, FunctionArguments, Query,
    Statement, VisitMut, VisitorMut,
};
use std::collections::HashMap;
use std::ops::ControlFlow;

// This clone is used only for declaration inference, never execution. Native
// validation has already checked the original seed expression and function.
fn expression_declarations(query: &Query, parameters: &HashMap<String, Parameter>) -> Query {
    struct Declare<'a>(&'a HashMap<String, Parameter>);
    impl VisitorMut for Declare<'_> {
        type Break = ();
        fn pre_visit_expr(&mut self, expression: &mut Expr) -> ControlFlow<()> {
            // COUNT's name alone cannot prove operand acceptance. Preserve its
            // declaration barriers even when enclosed in CASE/arithmetic. The
            // original projection already supplies proven COUNT declarations.
            if sqlparser::ast::visit_expressions(expression, |node| {
                if matches!(node, Expr::Function(f) if matches!(f.name.to_string().to_ascii_lowercase().as_str(), "count" | "count_big")) {
                    ControlFlow::Break(())
                } else { ControlFlow::Continue(()) }
            }).is_break() {
                return ControlFlow::Continue(());
            }
            if msduck_sql::expression_metadata::conditional::literal_null(expression) {
                return ControlFlow::Continue(());
            }
            let mut numeric = expression.clone();
            msduck_sql::case_types::lower(&mut numeric, self.0);
            let kind = msduck_sql::case_types::integer_rank(expression, self.0)
                .map(|rank| match rank {
                    0 => DataType::TinyInt(None),
                    1 => DataType::SmallInt(None),
                    2 => DataType::Int(None),
                    _ => DataType::BigInt(None),
                })
                .or_else(|| {
                    msduck_sql::case_types::is_bit(expression, self.0)
                        .then_some(DataType::Bit(None))
                })
                .or_else(|| {
                    msduck_sql::expression_metadata::storage::kind(&numeric, self.0, &|_| None)
                })
                .or_else(
                    || match msduck_sql::result_types::expression_type(expression)? {
                        msduck_sql::result_types::ResultType::Character { family, length } => {
                            let kind =
                                msduck_core::character::CharacterType::new(family, length).ok()?;
                            Some(msduck_sql::sql_type::ast(SqlType::Character(kind)))
                        }
                        msduck_sql::result_types::ResultType::Time(scale) => {
                            Some(msduck_sql::sql_type::ast(SqlType::Time(
                                msduck_core::types::Scale::new(scale).ok()?,
                            )))
                        }
                        msduck_sql::result_types::ResultType::Money(kind) => {
                            Some(msduck_sql::sql_type::ast(match kind {
                                msduck_core::money::MoneyType::Money => SqlType::Money,
                                msduck_core::money::MoneyType::SmallMoney => SqlType::SmallMoney,
                            }))
                        }
                    },
                );
            if let Some(kind) = kind {
                *expression = Expr::Convert {
                    is_try: false,
                    expr: Box::new(Expr::Value(sqlparser::ast::Value::Null.into())),
                    data_type: Some(kind),
                    charset: None,
                    target_before_value: true,
                    styles: vec![],
                };
                return ControlFlow::Continue(());
            }
            if let Expr::Function(f) = expression {
                let name = f.name.to_string().to_ascii_lowercase();
                let fixed = if name == "newid"
                    && matches!(&f.args, FunctionArguments::List(a) if a.args.is_empty())
                {
                    Some(DataType::Uuid)
                } else if matches!(name.as_str(), "stdev" | "stdevp" | "var" | "varp")
                    && msduck_sql::aggregate::validate(f).is_ok()
                {
                    Some(DataType::Double(ExactNumberInfo::None))
                } else {
                    None
                };
                if let Some(kind) = fixed {
                    *expression = Expr::Convert {
                        is_try: false,
                        expr: Box::new(Expr::Value(sqlparser::ast::Value::Null.into())),
                        data_type: Some(kind),
                        charset: None,
                        target_before_value: true,
                        styles: vec![],
                    };
                    return ControlFlow::Continue(());
                }
            }
            if let Expr::Function(f) = expression
                && f.name.to_string().eq_ignore_ascii_case("rand")
                && matches!(&f.args, FunctionArguments::List(a) if matches!(a.args.as_slice(), [] | [FunctionArg::Unnamed(FunctionArgExpr::Expr(_))]) && a.clauses.is_empty() && a.duplicate_treatment.is_none())
                && f.over.is_none()
                && f.filter.is_none()
                && f.within_group.is_empty()
                && f.null_treatment.is_none()
                && matches!(f.parameters, FunctionArguments::None)
                && !f.uses_odbc_syntax
            {
                *expression = Expr::Convert {
                    is_try: false,
                    expr: Box::new(Expr::Value(sqlparser::ast::Value::Null.into())),
                    data_type: Some(DataType::Double(ExactNumberInfo::None)),
                    charset: None,
                    target_before_value: true,
                    styles: vec![],
                };
            }
            ControlFlow::Continue(())
        }
    }
    let mut declared = query.clone();
    let _ = declared.visit(&mut Declare(parameters));
    declared
}

pub(super) struct Description {
    pub prefix: Vec<u8>,
    pub status: i32,
}

fn wire(info: &TypeMetadata) -> Option<tds::Type> {
    use tds::Type;
    let width = || u16::try_from(info.max_length?).ok();
    let unicode = || {
        let bytes = width()?;
        (bytes.is_multiple_of(2) && bytes > 0 && bytes <= 8000).then_some(bytes / 2)
    };
    Some(match info.system_type_id? {
        48 => Type::Int(1),
        52 => Type::Int(2),
        56 => Type::Int(4),
        127 => Type::Int(8),
        104 => Type::Bit,
        59 => Type::Float(4),
        62 => Type::Float(8),
        106 | 108 => {
            let declaration =
                msduck_core::types::DecimalType::new(info.precision?, info.scale?).ok()?;
            Type::Decimal(declaration.precision(), declaration.scale())
        }
        60 => Type::Money(8),
        122 => Type::Money(4),
        36 => Type::Guid,
        40 => Type::Date,
        41 => Type::Time(msduck_core::types::Scale::new(info.scale?).ok()?.get()),
        58 => Type::LegacyDateTime(4),
        61 => Type::DateTime,
        42 => Type::DateTime2(msduck_core::types::Scale::new(info.scale?).ok()?.get()),
        43 => Type::DateTimeOffset(msduck_core::types::Scale::new(info.scale?).ok()?.get()),
        98 => Type::Variant,
        167 if info.max_length == Some(-1) => Type::Varchar(u16::MAX),
        167 => Type::Varchar(width().filter(|n| (1..=8000).contains(n))?),
        175 => Type::Char(width().filter(|n| (1..=8000).contains(n))?),
        231 if info.max_length == Some(-1) => Type::Text,
        231 => Type::Nvarchar(unicode()?),
        239 => Type::Nchar(unicode()?),
        165 if info.max_length == Some(-1) => Type::Varbinary(u16::MAX),
        165 => Type::Varbinary(width().filter(|n| (1..=8000).contains(n))?),
        173 => Type::FixedBinary(width().filter(|n| (1..=8000).contains(n))?),
        _ => return None,
    })
}

fn query_description(
    session: &Session,
    query: &Query,
    parameters: &HashMap<String, Parameter>,
) -> Result<Vec<u8>> {
    let catalog = crate::query_catalog::snapshot(&session.db, query)?;
    let mut scope = Scope::default();
    for (name, parameter) in parameters {
        if let Some(info) = catalog.cast_info(&parameter.ast_type()) {
            scope.parameters.insert(name.to_lowercase(), info);
        }
    }
    let mut fields = projection::query_fields(&catalog, query, &scope)
        .ok_or_else(|| anyhow::anyhow!("unsupported prepared result declarations"))?;
    let mut declaration_query = query.clone();
    crate::aggregate_columns::annotate(&session.db, &mut declaration_query, parameters)
        .map_err(anyhow::Error::msg)?;
    if let Some(declared) = projection::query_fields(
        &catalog,
        &expression_declarations(&declaration_query, parameters),
        &scope,
    ) {
        ensure!(
            declared.len() == fields.len(),
            "prepared declaration alignment changed"
        );
        for (field, declaration) in fields.iter_mut().zip(declared) {
            if field.info.is_none() && declaration.info.is_some() {
                field.info = declaration.info;
                if field.properties.origin == msduck_core::result::Origin::Unknown {
                    field.properties = declaration.properties;
                }
                field.collation = declaration.collation;
            }
        }
    }
    ensure!(
        !fields.is_empty(),
        "unsupported empty prepared result declarations"
    );
    let aligned = crate::result_metadata::Aligned::new(fields.len(), &fields, &[]);
    let columns = fields
        .iter()
        .enumerate()
        .map(|(index, field)| {
            let kind = field.info.as_ref().and_then(wire).ok_or_else(|| {
                anyhow::anyhow!("unsupported prepared result type for {}", field.name)
            })?;
            Ok(aligned.column(index, field.name.clone(), kind))
        })
        .collect::<Result<Vec<_>>>()?;
    let mut out = Vec::new();
    tds::metadata(&mut out, &columns)?;
    match projection::order::infer(&catalog, query, &scope) {
        projection::order::Plan::Token(ordinals) => tds::order::encode(&mut out, &ordinals)?,
        projection::order::Plan::NoToken => {}
        projection::order::Plan::Unknown(_) => bail!("unsupported prepared ORDER declaration"),
    }
    tds::done(&mut out, 0xff, 17, 0xc1, 0);
    ensure!(
        out.len() <= tds::MAX_MESSAGE - 1024,
        "prepared metadata exceeds message bound"
    );
    Ok(out)
}

pub(super) fn describe(
    session: &Session,
    sql: &str,
    declarations: &[(String, SqlType)],
) -> Result<Description> {
    let statements = msduck_sql::batch::parse(sql)?;
    let parameters = declarations
        .iter()
        .map(|(name, kind)| {
            (
                name.clone(),
                Parameter {
                    data_type: *kind,
                    value: Value::Null,
                },
            )
        })
        .collect();
    // Declaration collection never evaluates initializers or parameter values.
    let parameters = msduck_sql::preflight::variables(&statements, &parameters)?;
    if statements.len() > 1 && statements.iter().all(|s| matches!(s, Statement::Query(_))) {
        return Ok(Description {
            prefix: vec![],
            status: 8182,
        });
    }
    let prefix = match statements.as_slice() {
        [Statement::Query(query)] => query_description(session, query, &parameters)?,
        [Statement::Insert(insert)] if insert.output.is_none() && insert.returning.is_none() => {
            let mut out = Vec::new();
            tds::done(&mut out, 0xff, 17, 0xc3, 0);
            out
        }
        [Statement::Insert(_)] => bail!("unsupported prepared INSERT result declarations"),
        // Existing non-result/control-flow preparation stays available. These
        // shapes have no proven description here and are documented gaps.
        _ => vec![],
    };
    Ok(Description { prefix, status: 0 })
}

pub(super) fn handle(out: &mut Vec<u8>, name: &str, value: Option<i32>) -> Result<()> {
    use tds::return_value::{Declaration, Parameter as Output, Status, Value as OutputValue};
    let units: Vec<_> = name.trim_start_matches('@').encode_utf16().collect();
    tds::return_value::encode(
        out,
        &Output {
            ordinal: 0,
            name: &units,
            status: Status::Output,
            user_type: 0,
            flags: 0,
            declaration: Declaration::Int(4),
            value: value.map_or(OutputValue::Null, |n| OutputValue::Int(i64::from(n))),
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn declaration_enrichment_preserves_count_null_barriers_in_outer_expressions() {
        for sql in [
            "SELECT COUNT(NULL)",
            "SELECT COUNT_BIG(NULL)",
            "SELECT COUNT(NULL)+1",
            "SELECT CASE WHEN 1=1 THEN COUNT(NULL) ELSE 0 END",
        ] {
            let Statement::Query(query) = msduck_sql::batch::parse(sql).unwrap().remove(0) else {
                panic!("query")
            };
            let declared = expression_declarations(&query, &HashMap::new());
            assert!(declared.to_string().contains("COUNT"), "{sql}");
            assert!(declared.to_string().contains("NULL"), "{sql}");
        }
    }

    #[test]
    fn character_metadata_requires_valid_declared_byte_capacity() {
        for id in [167, 175, 231, 239, 165, 173] {
            for width in [None, Some(-2), Some(0), Some(8001)] {
                assert!(
                    wire(&TypeMetadata {
                        system_type_id: Some(id),
                        max_length: width,
                        ..Default::default()
                    })
                    .is_none()
                );
            }
        }
        for id in [231, 239] {
            assert!(
                wire(&TypeMetadata {
                    system_type_id: Some(id),
                    max_length: Some(3),
                    ..Default::default()
                })
                .is_none()
            );
        }
        assert!(matches!(
            wire(&TypeMetadata {
                system_type_id: Some(231),
                max_length: Some(16),
                ..Default::default()
            }),
            Some(tds::Type::Nvarchar(8))
        ));
        assert!(matches!(
            wire(&TypeMetadata {
                system_type_id: Some(231),
                max_length: Some(-1),
                ..Default::default()
            }),
            Some(tds::Type::Text)
        ));
        assert!(wire(&TypeMetadata::default()).is_none());
    }

    #[test]
    fn handle_output_is_nullable_and_never_truncates_names() {
        let mut out = Vec::new();
        handle(&mut out, "@handle", None).unwrap();
        assert_eq!(out[0], 0xac);
        assert_eq!(out[3], 6);
        assert_eq!(&out[out.len() - 4..], &[0, 0x26, 4, 0]);
        assert!(handle(&mut Vec::new(), &"h".repeat(256), Some(1)).is_err());
    }
}
