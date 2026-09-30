//! Nonexecuting preparation descriptions over explicit catalog declarations.
use crate::{engine::Session, parameter::Parameter, tds};
use anyhow::{Result, bail, ensure};
use msduck_core::{catalog::TypeMetadata, types::Type as SqlType, value::Value};
use msduck_sql::{binding_scope::Scope, projection};
use sqlparser::ast::{Query, Statement};
use std::collections::HashMap;

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
    let fields = projection::query_fields(&catalog, query, &scope)
        .ok_or_else(|| anyhow::anyhow!("unsupported prepared result declarations"))?;
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
        [Statement::Insert(_)] => {
            let mut out = Vec::new();
            tds::done(&mut out, 0xff, 17, 0xc3, 0);
            out
        }
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
