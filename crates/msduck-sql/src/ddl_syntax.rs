//! Structural DDL validation; existence, constraints and writes belong to adapters.
use anyhow::{Result, bail, ensure};
use sqlparser::ast::*;
pub fn alter_table(table: &AlterTable) -> Result<()> {
    ensure!(
        !table.if_exists
            && !table.only
            && table.location.is_none()
            && table.on_cluster.is_none()
            && table.table_type.is_none(),
        "unsupported ALTER TABLE options"
    );
    ensure!(!table.operations.is_empty(), "empty ALTER TABLE");
    // T-SQL chooses one ADD, DROP or ALTER COLUMN alternative per statement.
    // A typed ALTER COLUMN expands internally to type + nullability operations.
    let same_action = match &table.operations[0] {
        AlterTableOperation::AddColumn { .. } => table.operations.iter().all(|op| matches!(op, AlterTableOperation::AddColumn { .. })),
        AlterTableOperation::DropColumn { .. } => table.operations.iter().all(|op| matches!(op, AlterTableOperation::DropColumn { .. })),
        AlterTableOperation::AlterColumn { column_name, .. } => table.operations.iter().all(|op| matches!(op, AlterTableOperation::AlterColumn { column_name: name, .. } if name == column_name)),
        _ => true,
    };
    ensure!(
        same_action,
        "ALTER TABLE cannot mix ADD, DROP or different ALTER COLUMN actions"
    );
    for operation in &table.operations {
        match operation {
            AlterTableOperation::AddColumn {
                if_not_exists,
                column_position,
                column_def,
                ..
            } => {
                ensure!(
                    !if_not_exists && column_position.is_none(),
                    "unsupported ADD COLUMN options"
                );
                for option in &column_def.options {
                    ensure!(
                        option.name.is_none()
                            && (matches!(
                                option.option,
                                ColumnOption::Identity(_)
                                    | ColumnOption::Null
                                    | ColumnOption::NotNull
                                    | ColumnOption::Default(_)
                                    | ColumnOption::Collation(_)
                            ) || crate::dialect::is_with_values(&option.option)),
                        "unsupported added column constraint"
                    );
                }
                ensure!(
                    column_def
                        .options
                        .iter()
                        .filter(|o| matches!(o.option, ColumnOption::Collation(_)))
                        .count()
                        <= 1,
                    "multiple column collation declarations"
                );
                let with_values = column_def
                    .options
                    .iter()
                    .filter(|o| crate::dialect::is_with_values(&o.option))
                    .count();
                ensure!(with_values <= 1, "duplicate WITH VALUES clause");
                ensure!(
                    with_values == 0
                        || column_def
                            .options
                            .iter()
                            .any(|o| matches!(o.option, ColumnOption::Default(_))),
                    "WITH VALUES requires a default"
                );
                ensure!(
                    column_def
                        .options
                        .iter()
                        .filter(|o| matches!(o.option, ColumnOption::Default(_)))
                        .count()
                        <= 1,
                    "duplicate column default"
                );
            }
            AlterTableOperation::AlterColumn { op, .. } => {
                ensure!(
                    matches!(
                        op,
                        AlterColumnOperation::SetDataType { using: None, .. }
                            | AlterColumnOperation::SetNotNull
                            | AlterColumnOperation::DropNotNull
                    ),
                    "unsupported ALTER COLUMN operation"
                );
            }
            AlterTableOperation::DropColumn { drop_behavior, .. } => {
                ensure!(drop_behavior.is_none(), "unsupported DROP COLUMN behavior");
            }
            _ => bail!("unsupported ALTER TABLE operation"),
        }
    }
    struct NoSessionValues;
    impl Visitor for NoSessionValues {
        type Break = String;
        fn pre_visit_expr(&mut self, expr: &Expr) -> std::ops::ControlFlow<String> {
            if matches!(expr, Expr::Identifier(id) if id.value.starts_with('@'))
                || matches!(expr, Expr::Function(f) if f.name.to_string().to_uppercase().starts_with("ERROR_"))
            {
                return std::ops::ControlFlow::Break(
                    "unsupported session value in ALTER TABLE default".into(),
                );
            }
            std::ops::ControlFlow::Continue(())
        }
    }
    if let std::ops::ControlFlow::Break(message) = table.visit(&mut NoSessionValues) {
        bail!(message);
    }
    Ok(())
}

pub fn truncate(statement: &Truncate) -> Result<()> {
    ensure!(
        statement.table
            && statement.table_names.len() == 1
            && !statement.if_exists
            && statement.partitions.is_none()
            && statement.identity.is_none()
            && statement.cascade.is_none()
            && statement.on_cluster.is_none(),
        "unsupported TRUNCATE options"
    );
    let target = &statement.table_names[0];
    ensure!(
        !target.only && !target.has_asterisk && (1..=2).contains(&target.name.0.len()),
        "unsupported TRUNCATE target"
    );
    Ok(())
}

pub fn protect(statement: &Statement) -> Result<()> {
    let names: Vec<&ObjectName> = match statement {
        Statement::CreateTable(t) => vec![&t.name],
        Statement::CreateView(v) => vec![&v.name],
        Statement::AlterTable(t) => vec![&t.name],
        Statement::AlterView { name, .. } => vec![name],
        Statement::Drop { names, .. } => names.iter().collect(),
        _ => vec![],
    };
    for name in names {
        ensure!(
            !(name.0.len() >= 2
                && name.0[name.0.len() - 2]
                    .as_ident()
                    .is_some_and(|i| i.value.eq_ignore_ascii_case("sys"))),
            "System catalog objects cannot be changed"
        );
    }
    Ok(())
}
