//! Statements that must respect constraints they do not name: DROP TABLE and
//! TRUNCATE TABLE of referenced tables, and column changes that constraints
//! depend on.
use super::catalog::{self, Constraint, Table, Type};
use super::{Transaction, errors, translate};
use crate::engine::{Execution, Parameter, Session, ext};
use anyhow::Result;
use sqlparser::ast::*;
use std::collections::HashMap;

fn written(name: &ObjectName) -> String {
    name.0
        .iter()
        .map(|part| match part.as_ident() {
            Some(ident) => ident.value.clone(),
            None => part.to_string(),
        })
        .collect::<Vec<_>>()
        .join(".")
}

fn resolve(session: &Session, name: &ObjectName) -> Result<Option<Table>> {
    let Some((schema, table)) = catalog::split_name(name, &session.database.name) else {
        return Ok(None);
    };
    if table.starts_with('#') {
        return Ok(None);
    }
    catalog::table(&session.db, &schema, &table)
}

fn referenced_by_other(constraints: &[Constraint], table: i32, gone: &[i32]) -> bool {
    constraints.iter().any(|c| {
        c.kind == Type::Foreign
            && c.referenced.as_ref().is_some_and(|r| r.id == table)
            && c.table.id != table
            && !gone.contains(&c.table.id)
    })
}

/// A table or view cannot take the name of a constraint or named default in
/// its schema (2714); object names are unique per schema.
pub(crate) fn new_object(session: &Session, name: &ObjectName, state: u8) -> Result<()> {
    let Some((schema, object)) = catalog::split_name(name, &session.database.name) else {
        return Ok(());
    };
    if object.starts_with('#') {
        return Ok(());
    }
    let taken: bool = session.db.query_row(
        "SELECT EXISTS(SELECT 1 FROM sys.objects o JOIN main.__msduck_schemas s USING(schema_id)
         WHERE lower(s.name)=lower(?) AND lower(o.name)=lower(?) AND rtrim(o.type) IN ('C','F','PK','UQ','D'))",
        [&schema, &object],
        |row| row.get(0),
    )?;
    if taken {
        return Err(errors::error(
            2714,
            state,
            format!("There is already an object named '{object}' in the database."),
        )
        .into());
    }
    Ok(())
}

/// DROP TABLE: refuse referenced tables (3726), forget dropped constraints.
pub(crate) fn drop_tables(
    session: &mut Session,
    statement: &Statement,
    parameters: &mut HashMap<String, Parameter>,
) -> Result<Option<Execution>> {
    let Statement::Drop {
        object_type: ObjectType::Table,
        names,
        ..
    } = statement
    else {
        return Ok(None);
    };
    let constraints = catalog::load(&session.db)?;
    if constraints.is_empty() {
        return Ok(None);
    }
    let mut gone = Vec::new();
    let mut affected = false;
    for name in names {
        let Some(table) = resolve(session, name)? else {
            continue;
        };
        gone.push(table.id);
        if referenced_by_other(&constraints, table.id, &gone) {
            return Err(errors::referenced_table(&written(name)));
        }
        affected |= constraints.iter().any(|c| c.table.id == table.id);
    }
    if !affected {
        return Ok(None);
    }
    let transaction = Transaction::begin(session)?;
    let result = ext::reenter(session, "constraints", |session| {
        session.execute(statement.clone(), parameters)
    })
    .and_then(|execution| {
        catalog::prune(&session.db)?;
        Ok(execution)
    });
    transaction.finish(session, result).map(Some)
}

/// TRUNCATE TABLE of a table referenced by a foreign key fails (4712), even
/// when the key is disabled or no row refers to it.
pub(crate) fn truncate(session: &mut Session, statement: &Statement) -> Result<()> {
    let Statement::Truncate(truncate) = statement else {
        return Ok(());
    };
    let constraints = catalog::load(&session.db)?;
    if !constraints.iter().any(|c| c.kind == Type::Foreign) {
        return Ok(());
    }
    for target in &truncate.table_names {
        if let Some(table) = resolve(session, &target.name)?
            && referenced_by_other(&constraints, table.id, &[])
        {
            return Err(errors::truncate_referenced(&written(&target.name)));
        }
    }
    Ok(())
}

/// What an object that depends on a column is.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Dependency {
    /// A DEFAULT, and whether SQL Server generated its name.
    Default {
        generated: bool,
    },
    Check,
    Foreign,
}

/// Objects that depend on `column`: defaults first, then constraints, each
/// in creation order.
fn dependents(
    session: &Session,
    constraints: &[Constraint],
    table: &Table,
    column: &str,
) -> Result<Vec<(i32, String, Dependency)>> {
    let mut objects: Vec<(i32, String, Dependency)> = catalog::defaults_of(&session.db, table)?
        .into_iter()
        .filter(|(_, _, c, _)| c.eq_ignore_ascii_case(column))
        .map(|(id, name, _, generated)| (id, name, Dependency::Default { generated }))
        .collect();
    // Key constraints are checked by the keys feature, which allows widening
    // a key column.
    for constraint in constraints
        .iter()
        .filter(|c| !matches!(c.kind, Type::Primary | Type::Unique))
    {
        let own = constraint.table.id == table.id
            && constraint
                .columns
                .iter()
                .any(|c| c.eq_ignore_ascii_case(column));
        let referenced = constraint.kind == Type::Foreign
            && constraint
                .referenced
                .as_ref()
                .is_some_and(|r| r.id == table.id)
            && constraint
                .referenced_columns
                .iter()
                .any(|c| c.eq_ignore_ascii_case(column));
        if own || referenced {
            let dependency = if constraint.kind == Type::Check {
                Dependency::Check
            } else {
                Dependency::Foreign
            };
            objects.push((constraint.id, constraint.name.clone(), dependency));
        }
    }
    // The reference lists the most recently created object first.
    objects.sort_by_key(|(id, _, _)| std::cmp::Reverse(*id));
    Ok(objects)
}

/// Whether ALTER COLUMN from `old` to `new` keeps a dependent object, as
/// SQL Server allows: a DEFAULT allows another length, precision or scale
/// of the same type, a CHECK another length of the same variable-length
/// type.
fn allowed(dependency: Dependency, old: &DataType, new: &DataType) -> bool {
    let (Some(old), Some(new)) = (
        msduck_sql::catalog_shape::declaration(old),
        msduck_sql::catalog_shape::declaration(new),
    ) else {
        return false;
    };
    if old.name != new.name {
        return false;
    }
    match dependency {
        Dependency::Default { .. } => true,
        Dependency::Check => {
            matches!(old.name, "varchar" | "nvarchar" | "varbinary")
                && old.precision == new.precision
                && old.scale == new.scale
        }
        Dependency::Foreign => false,
    }
}

/// ALTER TABLE DROP COLUMN and ALTER COLUMN on columns that CHECK or FOREIGN
/// KEY constraints or defaults use fail with 5074 and 4922. Changing only
/// nullability is allowed, and the changes [`allowed`] lists. Key columns
/// are the keys feature's.
pub(crate) fn alter_table(session: &mut Session, statement: &mut Statement) -> Result<()> {
    let Statement::AlterTable(alter) = statement else {
        return Ok(());
    };
    let Some(table) = resolve(session, &alter.name)? else {
        return Ok(());
    };
    let constraints = catalog::load(&session.db)?;
    for operation in &alter.operations {
        match operation {
            AlterTableOperation::DropColumn { column_names, .. } => {
                for column in column_names {
                    // A DEFAULT without a declared name goes with its column:
                    // msduck has always dropped such columns, although SQL
                    // Server refuses them (5074) too.
                    let objects: Vec<_> = dependents(session, &constraints, &table, &column.value)?
                        .into_iter()
                        .filter(|(_, _, dependency)| {
                            !matches!(dependency, Dependency::Default { generated: true })
                        })
                        .collect();
                    if !objects.is_empty() {
                        let names: Vec<String> = objects.into_iter().map(|(_, n, _)| n).collect();
                        return Err(errors::dependent_column(
                            &column.value,
                            &names,
                            "DROP COLUMN",
                        ));
                    }
                }
            }
            AlterTableOperation::AlterColumn {
                column_name,
                op: AlterColumnOperation::SetDataType { data_type, .. },
            } => {
                let objects = dependents(session, &constraints, &table, &column_name.value)?;
                if objects.is_empty() {
                    continue;
                }
                let columns = catalog::columns(&session.db, &table)?;
                let declared = columns
                    .iter()
                    .find(|c| c.name.eq_ignore_ascii_case(&column_name.value))
                    .map(|column| translate::declared_type(session, &table, column))
                    .transpose()?;
                let unchanged = declared.as_ref().is_some_and(|declared| {
                    declared
                        .to_string()
                        .eq_ignore_ascii_case(&data_type.to_string())
                });
                if unchanged {
                    continue;
                }
                // As for DROP COLUMN, a DEFAULT without a declared name
                // follows its column, as msduck always allowed.
                let names: Vec<String> = objects
                    .into_iter()
                    .filter(|(_, _, dependency)| {
                        !matches!(dependency, Dependency::Default { generated: true })
                            && !declared
                                .as_ref()
                                .is_some_and(|declared| allowed(*dependency, declared, data_type))
                    })
                    .map(|(_, n, _)| n)
                    .collect();
                if !names.is_empty() {
                    return Err(errors::dependent_column(
                        &column_name.value,
                        &names,
                        "ALTER COLUMN",
                    ));
                }
            }
            _ => {}
        }
    }
    Ok(())
}
