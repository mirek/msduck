//! decimal(p,0) and numeric(p,0) IDENTITY columns.
//!
//! The column gets the same private allocator as integer identity columns
//! (src/identity.rs): a `main.__msduck_identity_<hex>` sequence as its
//! default and a row in `main.__msduck_identity_definitions`, so
//! IDENT_SEED/IDENT_INCR/IDENT_CURRENT, sys.identity_columns, TRUNCATE and
//! DROP TABLE treat it like any other identity. Sequences are BIGINT, so
//! values are limited to the bigint range and the column's precision.
use super::{constraint_error, error};
use anyhow::{Result, bail};
use sqlparser::ast::*;

/// A planned allocator, installed in the CREATE TABLE transaction.
pub(super) struct Allocator {
    sequence: String,
    seed: i64,
    increment: i64,
    min: i64,
    max: i64,
}

impl Allocator {
    pub(super) fn install(&self, db: &duckdb::Connection) -> Result<()> {
        crate::identity::catalog(db)?;
        db.execute_batch(&format!(
            "CREATE SEQUENCE {} INCREMENT BY {} MINVALUE {} MAXVALUE {} START WITH {} NO CYCLE",
            self.sequence, self.increment, self.min, self.max, self.seed
        ))?;
        db.execute(
            "INSERT INTO main.__msduck_identity_definitions VALUES(?,?,?)",
            duckdb::params![self.sequence, self.seed, self.increment],
        )?;
        Ok(())
    }
}

fn invalid(column: &str) -> anyhow::Error {
    error(
        2749,
        2,
        format!(
            "Identity column '{column}' must be of data type int, bigint, smallint, tinyint, or decimal or numeric with a scale of 0, unencrypted, and constrained to be nonnullable."
        ),
    )
}

/// The precision of an exact numeric type with scale 0; Err for other
/// types SQL Server rejects with 2749; None for integer types (handled by
/// src/identity.rs) and types this module cannot classify (alias types).
fn precision(data_type: &DataType) -> Result<Option<u64>, ()> {
    match data_type {
        DataType::Decimal(info) | DataType::Numeric(info) | DataType::Dec(info) => match info {
            ExactNumberInfo::None => Ok(Some(18)),
            ExactNumberInfo::Precision(p) | ExactNumberInfo::PrecisionAndScale(p, 0) => {
                Ok(Some(*p))
            }
            ExactNumberInfo::PrecisionAndScale(..) => Err(()),
        },
        DataType::Float(_)
        | DataType::Real
        | DataType::Double(_)
        | DataType::DoublePrecision
        | DataType::Bit(_)
        | DataType::Boolean
        | DataType::Char(_)
        | DataType::Varchar(_)
        | DataType::Nvarchar(_)
        | DataType::Text
        | DataType::Binary(_)
        | DataType::Varbinary(_)
        | DataType::Date
        | DataType::Datetime(_)
        | DataType::Time(..)
        | DataType::Uuid => Err(()),
        _ => Ok(None),
    }
}

fn integer(expr: &Expr) -> Result<i64> {
    let text = expr.to_string().replace(' ', "");
    match text.parse::<i128>() {
        Ok(value) => i64::try_from(value).map_err(|_| {
            anyhow::anyhow!(
                "unsupported decimal identity seed or increment beyond the bigint range"
            )
        }),
        Err(_) => bail!("unsupported identity parameters"),
    }
}

/// Plan the decimal identity columns of CREATE TABLE: each is replaced by
/// its allocator default. Other identity types SQL Server rejects fail with
/// 2749; integer identity columns are left to the built-in path.
pub(super) fn plan_create(
    db: &duckdb::Connection,
    table: &mut CreateTable,
) -> Result<Vec<Allocator>> {
    let table_name = table
        .name
        .0
        .last()
        .and_then(|part| part.as_ident())
        .map(|id| id.value.clone())
        .unwrap_or_default();
    let identities = table
        .columns
        .iter()
        .filter(|column| {
            column
                .options
                .iter()
                .any(|option| matches!(option.option, ColumnOption::Identity(_)))
        })
        .count();
    let mut planned = vec![];
    for column in &mut table.columns {
        let Some(property) = column
            .options
            .iter()
            .find_map(|option| match &option.option {
                ColumnOption::Identity(property) => Some(property.clone()),
                _ => None,
            })
        else {
            continue;
        };
        let precision = match precision(&column.data_type) {
            Err(()) => return Err(invalid(&column.name.value)),
            Ok(None) => continue,
            Ok(Some(precision)) => precision,
        };
        if identities > 1 {
            return Err(error(
                2744,
                2,
                format!(
                    "Multiple identity columns specified for table '{table_name}'. Only one identity column per table is allowed."
                ),
            ));
        }
        if column
            .options
            .iter()
            .any(|option| matches!(option.option, ColumnOption::Null))
        {
            return Err(error(
                8147,
                1,
                format!(
                    "Could not create IDENTITY attribute on nullable column '{}', table '{table_name}'.",
                    column.name.value
                ),
            ));
        }
        if column
            .options
            .iter()
            .any(|option| matches!(option.option, ColumnOption::Default(_)))
        {
            return Err(constraint_error(
                1754,
                format!(
                    "Defaults cannot be created on columns with an IDENTITY attribute. Table '{table_name}', column '{}'.",
                    column.name.value
                ),
            ));
        }
        let IdentityPropertyKind::Identity(property) = property else {
            bail!("unsupported AUTOINCREMENT property");
        };
        if property.order.is_some() {
            bail!("unsupported identity ordering option");
        }
        let (seed, increment) = match &property.parameters {
            None => (1, 1),
            Some(IdentityPropertyFormatKind::FunctionCall(p)) => {
                (integer(&p.seed)?, integer(&p.increment)?)
            }
            Some(_) => bail!("unsupported identity parameters"),
        };
        if increment == 0 {
            bail!("zero identity increments are not yet supported");
        }
        let max = if precision <= 18 {
            10i64.pow(precision as u32) - 1
        } else {
            i64::MAX
        };
        let min = if precision <= 18 { -max } else { i64::MIN };
        if !(min..=max).contains(&seed) {
            bail!("Arithmetic overflow error converting IDENTITY seed.");
        }
        let uuid: String = db.query_row("SELECT CAST(uuid() AS VARCHAR)", [], |r| r.get(0))?;
        let sequence = format!("main.__msduck_identity_{}", uuid.replace('-', ""));
        column
            .options
            .retain(|option| !matches!(option.option, ColumnOption::Identity(_)));
        column.options.push(ColumnOptionDef {
            name: None,
            option: ColumnOption::Default(Expr::Function(Function {
                name: ObjectName::from(vec![Ident::new("nextval")]),
                uses_odbc_syntax: false,
                parameters: FunctionArguments::None,
                args: FunctionArguments::List(FunctionArgumentList {
                    duplicate_treatment: None,
                    args: vec![FunctionArg::Unnamed(FunctionArgExpr::Expr(Expr::Value(
                        Value::SingleQuotedString(sequence.clone()).into(),
                    )))],
                    clauses: vec![],
                }),
                filter: None,
                null_treatment: None,
                over: None,
                within_group: vec![],
            })),
        });
        if !column
            .options
            .iter()
            .any(|option| matches!(option.option, ColumnOption::NotNull))
        {
            column.options.push(ColumnOptionDef {
                name: None,
                option: ColumnOption::NotNull,
            });
        }
        planned.push(Allocator {
            sequence,
            seed,
            increment,
            min,
            max,
        });
    }
    Ok(planned)
}
