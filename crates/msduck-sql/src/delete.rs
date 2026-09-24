//! Lower SQL Server DELETE target aliases to DuckDB DELETE USING.
use anyhow::Result;
use sqlparser::ast::*;
use std::ops::ControlFlow;

pub fn canonicalize(statement: &mut Statement) -> Result<()> {
    struct Resolve;
    impl VisitorMut for Resolve {
        type Break = String;
        fn pre_visit_statement(&mut self, statement: &mut Statement) -> ControlFlow<String> {
            if let Statement::Delete(delete) = statement
                && let Err(error) = resolve(delete)
            {
                return ControlFlow::Break(error.to_string());
            }
            ControlFlow::Continue(())
        }
    }
    if let ControlFlow::Break(error) = statement.visit(&mut Resolve) {
        anyhow::bail!(error);
    }
    Ok(())
}

fn resolve(delete: &mut Delete) -> Result<()> {
    let (FromTable::WithFromKeyword(targets) | FromTable::WithoutKeyword(targets)) =
        &mut delete.from;
    anyhow::ensure!(targets.len() == 1, "DELETE requires one target");
    let target = &mut targets[0];
    anyhow::ensure!(target.joins.is_empty(), "unsupported joined DELETE target");
    if let Some(sources) = &mut delete.using {
        crate::update::resolve_from_target(target, sources, &mut delete.selection, "DELETE")?;
        if sources.is_empty() {
            delete.using = None;
        }
    }
    Ok(())
}
