//! Batch-scope declaration and syntax checks over explicit inputs.
//! Initializers are not evaluated and caller inputs are never mutated.
use crate::{batch::variable_type, parameter::Parameter};
use anyhow::{Result, bail, ensure};
use msduck_core::value::Value as ParameterValue;
use sqlparser::ast::*;
use std::{collections::HashMap, ops::ControlFlow};

pub fn try_catch_parts(statement: &Statement) -> Option<(&[Statement], &[Statement])> {
    if let Statement::StartTransaction {
        modifier: Some(TransactionModifier::Try),
        exception: Some(handlers),
        statements,
        has_end_keyword: true,
        ..
    } = statement
        && handlers.len() == 1
        && handlers[0].idents.is_empty()
    {
        Some((statements, &handlers[0].statements))
    } else {
        None
    }
}

// Resolve declarations and references before executing any branch or DML.
// SQL variables have batch scope even when their DECLARE branch is skipped.
pub fn validate_transaction_syntax(statement: &Statement) -> Result<()> {
    if try_catch_parts(statement).is_some() {
        return Ok(());
    }
    match statement {
        Statement::StartTransaction {
            modes,
            modifier,
            exception,
            ..
        } => {
            ensure!(
                modifier.is_none() && exception.is_none(),
                "unsupported transaction or exception modifier"
            );
            ensure!(modes.is_empty(), "unsupported SQL transaction mode");
        }
        Statement::Commit {
            chain,
            end,
            modifier,
        } => {
            ensure!(
                !end && modifier.is_none(),
                "unsupported standalone END or exception terminator"
            );
            ensure!(!chain, "unsupported SQL COMMIT AND CHAIN");
        }
        Statement::Rollback { chain, .. } => {
            ensure!(!chain, "unsupported SQL ROLLBACK AND CHAIN");
        }
        _ => {}
    }
    Ok(())
}

fn validate_schema_statement(statement: &Statement, standalone: bool) -> Result<()> {
    let check_name = |name: &ObjectName| -> Result<()> {
        ensure!(name.0.len() == 1, "unsupported qualified schema name");
        let id = name.0[0]
            .as_ident()
            .ok_or_else(|| anyhow::anyhow!("unsupported schema name"))?;
        ensure!(
            id.value.encode_utf16().count() <= 128,
            "Schema names are limited to 128 UTF-16 units"
        );
        ensure!(
            !matches!(
                id.value.to_ascii_lowercase().as_str(),
                "dbo" | "sys" | "information_schema" | "guest" | "main" | "temp"
            ),
            "unsupported change to built-in schema"
        );
        Ok(())
    };
    match statement {
        Statement::CreateSchema {
            schema_name,
            or_replace,
            if_not_exists,
            with,
            options,
            default_collate_spec,
            clone,
        } => {
            ensure!(
                standalone,
                "CREATE SCHEMA must be a separate batch; embedded schema elements are unsupported"
            );
            ensure!(
                !or_replace
                    && !if_not_exists
                    && with.is_none()
                    && options.is_none()
                    && default_collate_spec.is_none()
                    && clone.is_none(),
                "unsupported CREATE SCHEMA options"
            );
            let SchemaName::Simple(name) = schema_name else {
                bail!("unsupported schema AUTHORIZATION");
            };
            check_name(name)?;
        }
        Statement::Drop {
            object_type: ObjectType::Schema,
            names,
            cascade,
            restrict,
            purge,
            temporary,
            table,
            ..
        } => {
            ensure!(
                names.len() == 1
                    && !cascade
                    && !restrict
                    && !purge
                    && !temporary
                    && table.is_none(),
                "unsupported DROP SCHEMA options"
            );
            check_name(&names[0])?;
        }
        _ => {}
    }
    Ok(())
}

pub fn variables(
    statements: &[Statement],
    parameters: &HashMap<String, Parameter>,
) -> Result<HashMap<String, Parameter>> {
    struct Variables {
        values: HashMap<String, Parameter>,
        loop_depth: usize,
        allow_view: bool,
        allow_schema: bool,
    }
    impl Visitor for Variables {
        type Break = String;
        fn pre_visit_query(&mut self, query: &Query) -> ControlFlow<String> {
            if query.with.is_some()
                && let Err(error) = crate::cte_columns::validate(query, &Default::default())
            {
                return ControlFlow::Break(error.message);
            }

            if matches!(query.for_clause, Some(ForClause::Json { .. })) {
                let mut statement = Statement::Query(Box::new(query.clone()));
                if let Err(error) = crate::for_json::take(&mut statement) {
                    return ControlFlow::Break(error.to_string());
                }
            }
            ControlFlow::Continue(())
        }
        fn pre_visit_table_factor(&mut self, factor: &TableFactor) -> ControlFlow<String> {
            if let Some(name) = crate::openjson_path::path_variable(factor) {
                return self.pre_visit_expr(&Expr::Identifier(Ident::new(name)));
            }
            ControlFlow::Continue(())
        }
        fn pre_visit_statement(&mut self, statement: &Statement) -> ControlFlow<String> {
            if let Err(error) = crate::ddl_syntax::protect(statement) {
                return ControlFlow::Break(error.to_string());
            }
            if let Statement::Truncate(truncate) = statement
                && let Err(error) = crate::ddl_syntax::truncate(truncate)
            {
                return ControlFlow::Break(error.to_string());
            }

            if let Err(error) = validate_schema_statement(statement, self.allow_schema) {
                return ControlFlow::Break(error.to_string());
            }

            if let Statement::AlterTable(table) = statement
                && let Err(error) = crate::ddl_syntax::alter_table(table)
            {
                return ControlFlow::Break(error.to_string());
            }
            if matches!(statement, Statement::AlterView { .. }) {
                if !self.allow_view {
                    return ControlFlow::Break(
                        "ALTER VIEW must be the only statement in a batch".into(),
                    );
                }
                if let Err(error) = crate::view_definition::alter_definition(statement) {
                    return ControlFlow::Break(error.to_string());
                }
            }
            if let Statement::CreateView(view) = statement {
                if !self.allow_view {
                    return ControlFlow::Break(
                        "CREATE VIEW must be the only statement in a batch".into(),
                    );
                }
                if let Err(error) = crate::view_definition::validate(view) {
                    return ControlFlow::Break(error.to_string());
                }
            }
            if let Err(error) = validate_transaction_syntax(statement) {
                return ControlFlow::Break(error.to_string());
            }
            if matches!(statement, Statement::While(_)) {
                self.loop_depth += 1;
            }
            if crate::dialect::loop_control(statement).is_some() && self.loop_depth == 0 {
                return ControlFlow::Break("loop control outside WHILE".into());
            }
            if let Statement::Declare { stmts } = statement {
                for declaration in stmts {
                    if declaration.declare_type.is_some() {
                        return ControlFlow::Break("unsupported non-scalar declaration".into());
                    }
                    let Some(kind) = declaration.data_type.clone() else {
                        return ControlFlow::Break("missing variable type".into());
                    };
                    let kind = match variable_type(kind) {
                        Ok(kind) => kind,
                        Err(error) => return ControlFlow::Break(error.to_string()),
                    };
                    for name in &declaration.names {
                        let name = name.value.to_lowercase();
                        if !name.starts_with('@') || name.starts_with("@@") {
                            return ControlFlow::Break("invalid local variable name".into());
                        }
                        if self.values.contains_key(&name) {
                            return ControlFlow::Break(format!(
                                "The variable name {name} has already been declared"
                            ));
                        }
                        self.values.insert(
                            name,
                            Parameter {
                                value: ParameterValue::Null,
                                data_type: kind,
                            },
                        );
                    }
                }
            }
            if let Statement::Set(Set::SingleAssignment { variable, .. }) = statement {
                let name = variable.to_string().to_lowercase();
                if name.starts_with('@') && !self.values.contains_key(&name) {
                    return ControlFlow::Break(format!("Must declare the scalar variable {name}"));
                }
            }
            ControlFlow::Continue(())
        }
        fn post_visit_statement(&mut self, statement: &Statement) -> ControlFlow<String> {
            if matches!(statement, Statement::While(_)) {
                self.loop_depth -= 1;
            }
            ControlFlow::Continue(())
        }
        fn pre_visit_expr(&mut self, expression: &Expr) -> ControlFlow<String> {
            if let Expr::Identifier(id) = expression {
                let name = id.value.to_lowercase();
                if name.starts_with('@')
                    && !name.starts_with("@@")
                    && !self.values.contains_key(&name)
                {
                    return ControlFlow::Break(format!("Must declare the scalar variable {name}"));
                }
            }
            ControlFlow::Continue(())
        }
        fn pre_visit_select(&mut self, select: &Select) -> ControlFlow<String> {
            for item in &select.projection {
                if let SelectItem::ExprWithAlias { alias, .. } = item {
                    let name = alias.value.to_lowercase();
                    if alias.quote_style.is_none()
                        && name.starts_with('@')
                        && !self.values.contains_key(&name)
                    {
                        return ControlFlow::Break(format!(
                            "Must declare the scalar variable {name}"
                        ));
                    }
                }
            }
            ControlFlow::Continue(())
        }
    }
    let mut visitor = Variables {
        values: parameters.clone(),
        loop_depth: 0,
        allow_schema: matches!(statements, [Statement::CreateSchema { .. }]),
        allow_view: matches!(
            statements,
            [Statement::CreateView(_) | Statement::AlterView { .. }]
        ),
    };
    for statement in statements {
        crate::window_placement::validate(statement).map_err(anyhow::Error::msg)?;
        crate::grouping_syntax::validate(statement).map_err(anyhow::Error::msg)?;
        crate::predicate::validate(statement)?;
        if let ControlFlow::Break(error) = Visit::visit(statement, &mut visitor) {
            bail!(error);
        }
        crate::output::validate(statement)?;
        crate::aggregate::validate_update(statement)?;
        crate::session_function::validate(statement)?;
        crate::replicate::validate(statement)?;
        crate::left_right::validate(statement)?;
        crate::unary_operator::validate(statement, &visitor.values)?;
        crate::raiserror::validate(statement, &visitor.values)?;
    }
    Ok(visitor.values)
}
