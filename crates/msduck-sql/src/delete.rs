//! Lower SQL Server DELETE target aliases to DuckDB DELETE USING.
use anyhow::Result;
use sqlparser::ast::*;
use std::ops::ControlFlow;

pub fn canonicalize(statement: &mut Statement) -> Result<()> {
    struct Resolve;
    impl VisitorMut for Resolve {
        // Keep the error value, so SQL Server numbers such as 8154 survive.
        type Break = anyhow::Error;
        fn pre_visit_statement(&mut self, statement: &mut Statement) -> ControlFlow<anyhow::Error> {
            if let Statement::Delete(delete) = statement
                && let Err(error) = resolve(delete)
            {
                return ControlFlow::Break(error);
            }
            ControlFlow::Continue(())
        }
    }
    if let ControlFlow::Break(error) = statement.visit(&mut Resolve) {
        return Err(error);
    }
    Ok(())
}

/// Whether a DELETE names its target inside a FROM tree that needs the
/// row-identity form: outer joins, APPLY or a nested join around the target.
pub fn has_outer_target(delete: &Delete) -> bool {
    let (FromTable::WithFromKeyword(targets) | FromTable::WithoutKeyword(targets)) = &delete.from;
    match (targets.as_slice(), &delete.using) {
        ([target], Some(sources)) => {
            matches!(
                crate::update::outer_target(target, sources, "DELETE"),
                Ok(Some(_))
            )
        }
        _ => false,
    }
}

fn resolve(delete: &mut Delete) -> Result<()> {
    let (FromTable::WithFromKeyword(targets) | FromTable::WithoutKeyword(targets)) =
        &mut delete.from;
    anyhow::ensure!(targets.len() == 1, "DELETE requires one target");
    let target = &mut targets[0];
    anyhow::ensure!(target.joins.is_empty(), "unsupported joined DELETE target");
    if let Some(sources) = &mut delete.using
        && let Some(found) = crate::update::outer_target(target, sources, "DELETE")?
    {
        // DELETE <copy alias> FROM <tree> WHERE <where> becomes
        // DELETE FROM <table> AS __msduck_outer_target WHERE
        // __msduck_outer_target.rowid IN (SELECT <copy>.rowid FROM <tree>
        // WHERE <where>). The subquery keeps the SQL Server scope of the
        // tree, and a row matched by several joined rows is deleted once.
        let copy = crate::update::qualifier(&found)?;
        // Only a fixed template is parsed; user inputs remain AST nodes.
        let Statement::Query(mut query) = sqlparser::parser::Parser::parse_sql(
            &sqlparser::dialect::GenericDialect {},
            "SELECT 1",
        )?
        .remove(0) else {
            unreachable!()
        };
        let SetExpr::Select(select) = query.body.as_mut() else {
            unreachable!()
        };
        select.projection = vec![SelectItem::UnnamedExpr(Expr::CompoundIdentifier(vec![
            copy,
            Ident::new("rowid"),
        ]))];
        select.from = delete.using.take().unwrap_or_default();
        select.selection = delete.selection.take();
        target.relation = crate::update::renamed(found, Ident::new(crate::update::OUTER_TARGET));
        delete.selection = Some(Expr::InSubquery {
            expr: Box::new(Expr::CompoundIdentifier(vec![
                Ident::new(crate::update::OUTER_TARGET),
                Ident::new("rowid"),
            ])),
            subquery: query,
            negated: false,
        });
        return Ok(());
    }
    if let Some(sources) = &mut delete.using {
        crate::update::resolve_from_target(target, sources, &mut delete.selection, "DELETE")?;
        if sources.is_empty() {
            delete.using = None;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn canonical(sql: &str) -> String {
        let mut statement = crate::batch::parse(sql).unwrap().remove(0);
        canonicalize(&mut statement).unwrap();
        statement.to_string()
    }

    #[test]
    fn outer_trees_delete_the_target_rows_selected_by_the_whole_tree() {
        assert_eq!(
            canonical(
                "DELETE target FROM items target LEFT JOIN foo source ON source.id = target.id WHERE source.id IS NULL"
            ),
            "DELETE FROM items AS __msduck_outer_target WHERE __msduck_outer_target.rowid IN (SELECT target.rowid FROM items target LEFT JOIN foo source ON source.id = target.id WHERE source.id IS NULL)"
        );
        assert_eq!(
            canonical("DELETE FROM items FROM foo s FULL JOIN items ON items.id = s.id"),
            "DELETE FROM items AS __msduck_outer_target WHERE __msduck_outer_target.rowid IN (SELECT items.rowid FROM foo s FULL JOIN items ON items.id = s.id)"
        );
        // Flat inner trees keep DELETE USING.
        assert_eq!(
            canonical("DELETE t FROM items t JOIN foo s ON s.id = t.id"),
            "DELETE FROM items t USING foo s WHERE s.id = t.id"
        );
    }
}
