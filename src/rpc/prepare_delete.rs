//! Root backend lowering for a proven updatable single-table CTE DELETE.
//! The result is validated by the ordinary native binder before handle allocation.
use anyhow::Result;
use sqlparser::ast::*;

pub(super) fn lower(sql: &str) -> Result<Option<String>> {
    let statements = msduck_sql::batch::parse(sql)?;
    let [Statement::Query(query)] = statements.as_slice() else {
        return Ok(None);
    };
    let Some(with) = &query.with else {
        return Ok(None);
    };
    let [cte] = with.cte_tables.as_slice() else {
        return Ok(None);
    };
    if with.recursive
        || !cte.alias.columns.is_empty()
        || cte.from.is_some()
        || cte.materialized.is_some()
    {
        return Ok(None);
    }
    let SetExpr::Delete(Statement::Delete(delete)) = query.body.as_ref() else {
        return Ok(None);
    };
    let SetExpr::Select(select) = cte.query.body.as_ref() else {
        return Ok(None);
    };
    let [source] = select.from.as_slice() else {
        return Ok(None);
    };
    let (FromTable::WithFromKeyword(targets) | FromTable::WithoutKeyword(targets)) = &delete.from;
    let [target] = targets.as_slice() else {
        return Ok(None);
    };
    let TableFactor::Table {
        name: target_name, ..
    } = &target.relation
    else {
        return Ok(None);
    };
    if target_name.0.len() != 1
        || !target_name.0[0]
            .as_ident()
            .is_some_and(|name| name.value.eq_ignore_ascii_case(&cte.alias.name.value))
    {
        return Ok(None);
    }
    let TableFactor::Table { name, alias, .. } = &source.relation else {
        return Ok(None);
    };
    // A same-named unqualified source is a CTE self-reference, never a fallback
    // to a physical table. Native binding must retain that original barrier.
    if name.0.len() == 1
        && name.0[0]
            .as_ident()
            .is_some_and(|name| name.value.eq_ignore_ascii_case(&cte.alias.name.value))
    {
        return Ok(None);
    }
    if alias
        .as_ref()
        .is_some_and(|alias| !alias.columns.is_empty())
    {
        return Ok(None);
    }
    let wildcard = match select.projection.as_slice() {
        [SelectItem::Wildcard(options)] => *options == WildcardAdditionalOptions::default(),
        [
            SelectItem::QualifiedWildcard(
                SelectItemQualifiedWildcardKind::ObjectName(qualifier),
                options,
            ),
        ] if *options == WildcardAdditionalOptions::default() => {
            if let Some(alias) = alias {
                qualifier.0.len() == 1
                    && qualifier.0[0]
                        .as_ident()
                        .is_some_and(|name| name.value.eq_ignore_ascii_case(&alias.name.value))
            } else {
                qualifier == name || (qualifier.0.len() == 1 && qualifier.0.last() == name.0.last())
            }
        }
        _ => false,
    };
    if !wildcard {
        return Ok(None);
    }
    // Comparing structural defaults rejects every unproven modifier, including
    // TOP, DISTINCT, joins, grouping, ordering, OUTPUT and DELETE predicates.
    // Only source identity/alias and the CTE predicate are allowed to vary.
    let Statement::Query(plain) = msduck_sql::batch::parse("SELECT *")?.remove(0) else {
        unreachable!()
    };
    let mut inner = cte.query.clone();
    let SetExpr::Select(inner_select) = inner.body.as_mut() else {
        unreachable!()
    };
    inner_select.from.clear();
    inner_select.selection = None;
    inner_select.projection = vec![SelectItem::Wildcard(Default::default())];
    if inner != plain {
        return Ok(None);
    }
    let mut outer = query.clone();
    outer.with = None;
    outer.body = plain.body.clone();
    if outer != plain {
        return Ok(None);
    }
    let Statement::Delete(template) = msduck_sql::batch::parse("DELETE FROM __cte")?.remove(0)
    else {
        unreachable!()
    };
    let FromTable::WithFromKeyword(template_sources) = &template.from else {
        unreachable!()
    };
    let mut normalized_target = target.clone();
    if let TableFactor::Table { name, .. } = &mut normalized_target.relation {
        *name = ObjectName::from(vec![Ident::new("__cte")]);
    }
    if normalized_target != template_sources[0] {
        return Ok(None);
    }
    let mut normalized_delete = delete.clone();
    normalized_delete.from = template.from.clone();
    if normalized_delete != template {
        return Ok(None);
    }
    let mut normalized_source = source.clone();
    if let TableFactor::Table { name, alias, .. } = &mut normalized_source.relation {
        *name = ObjectName::from(vec![Ident::new("__cte")]);
        *alias = None;
    }
    if normalized_source != template_sources[0] {
        return Ok(None);
    }
    let mut lowered = delete.clone();
    lowered.from = FromTable::WithFromKeyword(vec![source.clone()]);
    lowered.selection = select.selection.clone();
    Ok(Some(Statement::Delete(lowered).to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_one_source_alias_and_predicate_without_copying_evaluation() {
        let sql = "WITH q AS (SELECT src.* FROM dbo.items AS src WHERE src.a>@p AND RAND(42)>0) DELETE FROM q";
        let lowered = lower(sql).unwrap().unwrap();
        assert_eq!(
            lowered,
            "DELETE FROM dbo.items AS src WHERE src.a > @p AND RAND(42) > 0"
        );
        assert_eq!(lowered.matches("RAND").count(), 1);
    }

    #[test]
    fn wider_or_ambiguous_shapes_keep_original_binding() {
        for sql in [
            "SELECT 1",
            "WITH q AS (SELECT * FROM q) DELETE FROM q",
            "WITH q AS (SELECT * FROM items), r AS (SELECT * FROM q) DELETE FROM q",
            "WITH q(x) AS (SELECT * FROM items) DELETE FROM q",
            "WITH q AS (SELECT a FROM items) DELETE FROM q",
            "WITH q AS (SELECT TOP (1) * FROM items) DELETE FROM q",
            "WITH q AS (SELECT DISTINCT * FROM items) DELETE FROM q",
            "WITH q AS (SELECT a.* FROM items a JOIN other b ON a.id=b.id) DELETE FROM q",
            "WITH q AS (SELECT * FROM items) DELETE FROM q WHERE a>@p",
            "WITH q AS (SELECT * FROM items) DELETE FROM q OUTPUT deleted.a",
        ] {
            assert!(lower(sql).unwrap().is_none(), "{sql}");
        }
    }
}
