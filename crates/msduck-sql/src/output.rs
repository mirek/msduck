//! OUTPUT checks over syntax alone; image acquisition and sink writes are adapters.
use msduck_core::diagnostic::SqlError;
use sqlparser::ast::*;
use std::ops::ControlFlow;

pub fn validate<T: Visit>(node: &T) -> Result<(), SqlError> {
    struct Statements;
    impl Visitor for Statements {
        type Break = SqlError;
        fn pre_visit_statement(&mut self, statement: &Statement) -> ControlFlow<SqlError> {
            let (output, unavailable) = match statement {
                Statement::Insert(insert) => (&insert.output, Some("deleted")),
                Statement::Update(update) => (&update.output, None),
                Statement::Delete(delete) => (&delete.output, Some("inserted")),
                _ => return ControlFlow::Continue(()),
            };
            let Some(OutputClause::Output { select_items, .. }) = output else {
                return ControlFlow::Continue(());
            };
            if select_items
                .iter()
                .any(|item| matches!(item, SelectItem::Wildcard(_)))
            {
                return ControlFlow::Break(SqlError::syntax(102, 1, "Incorrect syntax near '*'."));
            }
            select_items.visit(&mut Expressions {
                unavailable,
                parents: Vec::new(),
            })
        }
    }
    struct Expressions {
        unavailable: Option<&'static str>,
        parents: Vec<(bool, usize)>,
    }
    impl Visitor for Expressions {
        type Break = SqlError;
        fn pre_visit_expr(&mut self, expr: &Expr) -> ControlFlow<SqlError> {
            if let Expr::Function(function) = expr {
                if function.over.is_some() {
                    return ControlFlow::Break(SqlError::syntax(
                        4108,
                        1,
                        crate::window_placement::ERROR,
                    ));
                }
                if crate::aggregate::is_aggregate(function) {
                    return ControlFlow::Break(SqlError::syntax(
                        158,
                        1,
                        "An aggregate may not appear in the OUTPUT clause.",
                    ));
                }
            }
            let datepart_keyword = self.parents.last_mut().is_some_and(|(calendar, children)| {
                let first = *calendar && *children == 0;
                *children += 1;
                first
            });
            let calendar = matches!(expr, Expr::Function(function) if matches!(function.name.to_string().to_ascii_uppercase().as_str(), "DATEPART" | "DATENAME" | "DATEADD" | "DATEDIFF" | "DATEDIFF_BIG"));
            self.parents.push((calendar, 0));
            if let Expr::Identifier(id) = expr
                && !datepart_keyword
                && !id.value.starts_with('@')
            {
                return ControlFlow::Break(SqlError::new(
                    207,
                    1,
                    format!("Invalid column name '{}'.", id.value),
                ));
            }
            if matches!(
                expr,
                Expr::Subquery(_) | Expr::Exists { .. } | Expr::InSubquery { .. }
            ) {
                return ControlFlow::Break(SqlError::syntax(
                    10705,
                    1,
                    "Subqueries are not allowed in the OUTPUT clause.",
                ));
            }
            if let Expr::CompoundIdentifier(parts) = expr
                && let Some(unavailable) = self.unavailable
                && parts
                    .first()
                    .is_some_and(|part| part.value.eq_ignore_ascii_case(unavailable))
            {
                let name = parts
                    .iter()
                    .map(|part| part.value.as_str())
                    .collect::<Vec<_>>()
                    .join(".");
                return ControlFlow::Break(SqlError::new(
                    4104,
                    1,
                    format!("The multi-part identifier \"{name}\" could not be bound."),
                ));
            }
            ControlFlow::Continue(())
        }
        fn post_visit_expr(&mut self, _expr: &Expr) -> ControlFlow<SqlError> {
            self.parents.pop();
            ControlFlow::Continue(())
        }
    }
    match node.visit(&mut Statements) {
        ControlFlow::Continue(()) => Ok(()),
        ControlFlow::Break(error) => Err(error),
    }
}

/// Logical result description for the native single-image execution path.
/// UPDATE plans needing deleted images and sink plans require materialization.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Operation {
    Insert,
    Update,
    Delete,
}

pub struct NativePlan {
    pub projection: Box<Query>,
    pub operation: Operation,
    pub sink: Option<Sink>,
    pub paired: bool,
}

/// Separate image acquisition from expression evaluation. The adapter owns the
/// relation's lifetime and must retain logical fields independently of storage.
pub struct MaterializedPlan {
    /// Empty source query describing the image relation without evaluating DML.
    pub image: Box<Query>,
    /// Original OUTPUT expressions over the adapter-provided image relation.
    pub projection: Box<Query>,
}

/// An OUTPUT destination is a relation and an optional ordered column list,
/// never a scalar function despite the parser's representation of `t(a,b)`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Sink {
    pub table: ObjectName,
    pub columns: Option<Vec<Ident>>,
}

impl Sink {
    pub fn parse(into: &SelectInto) -> anyhow::Result<Self> {
        anyhow::ensure!(
            !into.temporary && !into.unlogged && !into.table && into.targets.len() == 1,
            "unsupported OUTPUT destination"
        );
        let (table, columns) = match &into.targets[0] {
            Expr::Identifier(id) => (ObjectName::from(vec![id.clone()]), None),
            Expr::CompoundIdentifier(ids) => (ObjectName::from(ids.clone()), None),
            Expr::Function(function) => {
                anyhow::ensure!(
                    !function.uses_odbc_syntax
                        && matches!(function.parameters, FunctionArguments::None)
                        && function.within_group.is_empty()
                        && function.filter.is_none()
                        && function.null_treatment.is_none()
                        && function.over.is_none(),
                    "unsupported OUTPUT destination column list"
                );
                let FunctionArguments::List(args) = &function.args else {
                    anyhow::bail!("unsupported OUTPUT destination column list");
                };
                anyhow::ensure!(
                    args.duplicate_treatment.is_none()
                        && args.clauses.is_empty()
                        && !args.args.is_empty(),
                    "unsupported OUTPUT destination column list"
                );
                let columns = args
                    .args
                    .iter()
                    .map(|arg| match arg {
                        FunctionArg::Unnamed(FunctionArgExpr::Expr(Expr::Identifier(id))) => {
                            Ok(id.clone())
                        }
                        _ => anyhow::bail!("OUTPUT destination columns must be identifiers"),
                    })
                    .collect::<anyhow::Result<Vec<_>>>()?;
                (function.name.clone(), Some(columns))
            }
            _ => anyhow::bail!("unsupported OUTPUT destination"),
        };
        anyhow::ensure!(
            !table.0.is_empty() && table.0.iter().all(|part| part.as_ident().is_some()),
            "unsupported OUTPUT destination name"
        );
        Ok(Self { table, columns })
    }
}

/// Joined UPDATE keeps its original source tree until catalog binding. The
/// adapter must intercept this form before the target-only RETURNING path.
pub fn joined_update(statement: &Statement) -> Option<(&Update, Option<&With>)> {
    let (update, with) = match statement {
        Statement::Update(update) => (update, None),
        Statement::Query(query) => {
            let SetExpr::Update(Statement::Update(update)) = query.body.as_ref() else {
                return None;
            };
            (update, query.with.as_ref())
        }
        _ => return None,
    };
    (update.output.is_some() && update.from.is_some()).then_some((update, with))
}

pub fn has<T: Visit>(node: &T) -> bool {
    struct Find;
    impl Visitor for Find {
        type Break = ();
        fn pre_visit_statement(&mut self, statement: &Statement) -> ControlFlow<()> {
            let found = match statement {
                Statement::Insert(insert) => insert.output.is_some(),
                Statement::Update(update) => update.output.is_some(),
                Statement::Delete(delete) => delete.output.is_some(),
                _ => false,
            };
            if found {
                ControlFlow::Break(())
            } else {
                ControlFlow::Continue(())
            }
        }
    }
    matches!(node.visit(&mut Find), ControlFlow::Break(()))
}

fn returning_items(select_items: &[SelectItem], image: &str) -> anyhow::Result<Vec<SelectItem>> {
    let mut returning = select_items.to_vec();
    struct Image<'a>(&'a str);
    impl VisitorMut for Image<'_> {
        type Break = String;
        fn pre_visit_expr(&mut self, expr: &mut Expr) -> ControlFlow<String> {
            if let Expr::CompoundIdentifier(parts) = expr {
                if parts.len() != 2 || !parts[0].value.eq_ignore_ascii_case(self.0) {
                    return ControlFlow::Break(
                        "OUTPUT with old or joined-source rows is not supported yet.".into(),
                    );
                }
                *expr = Expr::Identifier(parts[1].clone());
            }
            ControlFlow::Continue(())
        }
    }
    for item in &mut returning {
        if let SelectItem::QualifiedWildcard(
            SelectItemQualifiedWildcardKind::ObjectName(name),
            options,
        ) = item
        {
            anyhow::ensure!(
                name.0.len() == 1
                    && name.0[0]
                        .as_ident()
                        .is_some_and(|id| id.value.eq_ignore_ascii_case(image)),
                "OUTPUT wildcard requires an available row image"
            );
            *item = SelectItem::Wildcard(options.clone());
        }
        if let ControlFlow::Break(error) = item.visit(&mut Image(image)) {
            anyhow::bail!(error);
        }
    }
    Ok(returning)
}

impl NativePlan {
    pub fn requires_materialization(&self) -> bool {
        struct Parameters;
        impl Visitor for Parameters {
            type Break = ();
            fn pre_visit_expr(&mut self, expr: &Expr) -> ControlFlow<()> {
                if matches!(expr, Expr::Identifier(id) if id.quote_style.is_none()
                    && id.value.starts_with('@') && !id.value.starts_with("@@"))
                {
                    ControlFlow::Break(())
                } else {
                    ControlFlow::Continue(())
                }
            }
        }
        let SetExpr::Select(select) = self.projection.body.as_ref() else {
            return false;
        };
        matches!(
            select.projection.visit(&mut Parameters),
            ControlFlow::Break(())
        )
    }

    /// Keep DML inputs in their original statement; capture only its row image.
    /// Private relation names are explicit adapter inputs, never generated here.
    pub fn materialize(
        &self,
        statement: &mut Statement,
        relation: ObjectName,
    ) -> anyhow::Result<MaterializedPlan> {
        let mut image = self.projection.clone();
        let mut projection = self.projection.clone();
        // The acquired image is a base target table. Source CTEs belong to the
        // DML and must not be rerun while describing or evaluating its output.
        image.with = None;
        projection.with = None;
        let SetExpr::Select(source) = image.body.as_mut() else {
            anyhow::bail!("unsupported OUTPUT image query");
        };
        source.projection = vec![SelectItem::Wildcard(Default::default())];
        source.selection = Some(Expr::Value(Value::Boolean(false).into()));
        let SetExpr::Select(result) = projection.body.as_mut() else {
            anyhow::bail!("unsupported OUTPUT projection query");
        };
        anyhow::ensure!(result.from.len() == 1, "OUTPUT requires one image source");
        let TableFactor::Table { name, .. } = &mut result.from[0].relation else {
            anyhow::bail!("unsupported OUTPUT image relation");
        };
        *name = relation;
        self.bind_projection(statement, &[SelectItem::Wildcard(Default::default())])?;
        Ok(MaterializedPlan { image, projection })
    }

    /// Attach projection expressions bound against the logical image scope.
    pub fn bind_projection(
        &self,
        statement: &mut Statement,
        items: &[SelectItem],
    ) -> anyhow::Result<()> {
        if let Statement::Query(query) = statement
            && let SetExpr::Insert(inner) | SetExpr::Update(inner) | SetExpr::Delete(inner) =
                query.body.as_mut()
        {
            return self.bind_projection(inner, items);
        }
        let image = match self.operation {
            Operation::Insert | Operation::Update => "inserted",
            Operation::Delete => "deleted",
        };
        let mut items = items.to_vec();
        if self.paired {
            // Both images have the same declarations. This target-only form
            // binds OUTPUT legality; paired execution never evaluates it.
            struct OldImage;
            impl VisitorMut for OldImage {
                type Break = ();
                fn pre_visit_expr(&mut self, expr: &mut Expr) -> ControlFlow<()> {
                    if let Expr::CompoundIdentifier(parts) = expr
                        && parts
                            .first()
                            .is_some_and(|id| id.value.eq_ignore_ascii_case("deleted"))
                    {
                        parts[0] = Ident::new("inserted");
                    }
                    ControlFlow::Continue(())
                }
            }
            for item in &mut items {
                if let SelectItem::QualifiedWildcard(
                    SelectItemQualifiedWildcardKind::ObjectName(name),
                    _,
                ) = item
                    && (name.0.len() == 1
                        && name.0[0]
                            .as_ident()
                            .is_some_and(|id| id.value.eq_ignore_ascii_case("deleted")))
                {
                    *name = ObjectName::from(vec![Ident::new("inserted")]);
                }
                let _ = VisitMut::visit(item, &mut OldImage);
            }
        }
        let returning = returning_items(&items, image)?;
        match statement {
            Statement::Insert(insert) => insert.returning = Some(returning),
            Statement::Update(update) => update.returning = Some(returning),
            Statement::Delete(delete) => delete.returning = Some(returning),
            _ => anyhow::bail!("OUTPUT result attached to a non-DML statement"),
        }
        Ok(())
    }
}

/// Preserve a logical projection while lowering available row images to
/// RETURNING. Destination routing is described separately for the adapter.
/// Unsupported images or malformed destinations leave the input AST unchanged.
pub fn lower_native(statement: &mut Statement) -> anyhow::Result<Option<NativePlan>> {
    if let Statement::Query(query) = statement {
        if let SetExpr::Insert(inner) | SetExpr::Update(inner) | SetExpr::Delete(inner) =
            query.body.as_mut()
        {
            let mut plan = lower_native(inner)?;
            if let Some(plan) = &mut plan {
                plan.projection.with = query.with.clone();
            }
            return Ok(plan);
        }
        return Ok(None);
    }
    let (output, target, image, operation) = match &*statement {
        Statement::Insert(insert) if insert.output.is_some() => {
            let TableObject::TableName(name) = &insert.table else {
                anyhow::bail!("unsupported OUTPUT target");
            };
            (insert.output.as_ref(), name, "inserted", Operation::Insert)
        }
        Statement::Update(update) if update.output.is_some() => {
            let TableFactor::Table { name, .. } = &update.table.relation else {
                anyhow::bail!("unsupported OUTPUT target");
            };
            (update.output.as_ref(), name, "inserted", Operation::Update)
        }
        Statement::Delete(delete) if delete.output.is_some() => {
            let (FromTable::WithFromKeyword(tables) | FromTable::WithoutKeyword(tables)) =
                &delete.from;
            anyhow::ensure!(tables.len() == 1, "OUTPUT requires one delete target");
            let TableFactor::Table { name, .. } = &tables[0].relation else {
                anyhow::bail!("unsupported OUTPUT target");
            };
            (delete.output.as_ref(), name, "deleted", Operation::Delete)
        }
        _ => return Ok(None),
    };
    let Some(OutputClause::Output {
        select_items,
        into_table,
        ..
    }) = output
    else {
        anyhow::bail!("unsupported OUTPUT syntax");
    };
    let sink = into_table.as_ref().map(Sink::parse).transpose()?;
    struct Deleted;
    impl Visitor for Deleted {
        type Break = ();
        fn pre_visit_expr(&mut self, expr: &Expr) -> ControlFlow<()> {
            if matches!(expr, Expr::CompoundIdentifier(parts) if parts.first().is_some_and(|id| id.value.eq_ignore_ascii_case("deleted")))
            {
                ControlFlow::Break(())
            } else {
                ControlFlow::Continue(())
            }
        }
    }
    let paired = operation == Operation::Update && (
        matches!(select_items.visit(&mut Deleted), ControlFlow::Break(())) ||
        select_items.iter().any(|item| matches!(item, SelectItem::QualifiedWildcard(SelectItemQualifiedWildcardKind::ObjectName(name), _) if (name.0.len() == 1 && name.0[0].as_ident().is_some_and(|id| id.value.eq_ignore_ascii_case("deleted")))))
    );
    if paired && let Statement::Update(update) = &*statement {
        anyhow::ensure!(
            update.from.is_none() && update.table.joins.is_empty(),
            "paired OUTPUT with joined-source rows is not supported yet"
        );
    }
    let returning = if paired {
        vec![SelectItem::Wildcard(Default::default())]
    } else {
        returning_items(select_items, image)?
    };
    // A fixed template supplies structural defaults; all user data remains AST.
    let mut template = crate::batch::parse("SELECT 1 FROM __msduck_output AS inserted")?;
    let Statement::Query(mut projection) = template.remove(0) else {
        unreachable!()
    };
    let SetExpr::Select(select) = projection.body.as_mut() else {
        unreachable!()
    };
    select.projection = select_items.clone();
    let TableFactor::Table {
        name,
        alias: Some(alias),
        ..
    } = &mut select.from[0].relation
    else {
        unreachable!()
    };
    *name = target.clone();
    alias.name = Ident::new(image);
    if paired {
        projection =
            crate::output_update::logical_projection(target.clone(), select_items.clone())?;
    }
    match statement {
        Statement::Insert(insert) => {
            insert.output = None;
            insert.returning = Some(returning);
        }
        Statement::Update(update) => {
            update.output = None;
            update.returning = Some(returning);
        }
        Statement::Delete(delete) => {
            delete.output = None;
            delete.returning = Some(returning);
        }
        _ => unreachable!(),
    }
    Ok(Some(NativePlan {
        projection,
        operation,
        sink,
        paired,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn materialized_plans_keep_output_parameters_separate_from_dml_inputs() {
        for sql in [
            "INSERT INTO t(id) OUTPUT inserted.id,@p AS extra VALUES(@id)",
            "UPDATE t SET id=@id OUTPUT inserted.id,@p AS extra",
            "DELETE FROM t OUTPUT deleted.id,@p AS extra WHERE id=@id",
            "WITH c AS (SELECT @id AS id) INSERT INTO t(id) OUTPUT inserted.id,@p AS extra INTO sink(id,n) SELECT id FROM c",
        ] {
            let mut statement = crate::batch::parse(sql).unwrap().remove(0);
            let plan = lower_native(&mut statement).unwrap().unwrap();
            assert!(plan.requires_materialization());
            let original = plan.projection.clone();
            let materialized = plan
                .materialize(
                    &mut statement,
                    ObjectName::from(vec![Ident::with_quote('"', "private image")]),
                )
                .unwrap();
            assert_eq!(plan.projection, original);
            assert!(statement.to_string().ends_with("RETURNING *"));
            assert!(statement.to_string().contains("@id"));
            assert!(!statement.to_string().contains("@p"));
            assert!(!materialized.image.to_string().contains('@'));
            assert!(materialized.image.to_string().contains("FROM t AS"));
            assert!(materialized.image.to_string().ends_with("WHERE false"));
            assert!(materialized.projection.to_string().contains("@p AS extra"));
            assert!(
                materialized
                    .projection
                    .to_string()
                    .contains("FROM \"private image\" AS")
            );
            assert!(materialized.projection.with.is_none());
            assert!(materialized.image.with.is_none());
        }
        for sql in [
            "INSERT INTO t(id) OUTPUT inserted.id VALUES(@id)",
            "WITH c AS (SELECT @id AS id) INSERT INTO t(id) OUTPUT inserted.id SELECT id FROM c",
            "UPDATE t SET id=@id OUTPUT inserted.id,@@ROWCOUNT AS previous_count",
        ] {
            let mut statement = crate::batch::parse(sql).unwrap().remove(0);
            assert!(
                !lower_native(&mut statement)
                    .unwrap()
                    .unwrap()
                    .requires_materialization()
            );
        }
    }

    #[test]
    fn destinations_retain_qualified_names_column_order_and_omitted_lists() {
        for sql in [
            "INSERT INTO t(id) OUTPUT inserted.id,inserted.n INTO [dbo].[sink table]([b],[a]) VALUES(1)",
            "UPDATE t SET n=1 OUTPUT inserted.id,inserted.n INTO [dbo].[sink table]([b],[a])",
            "DELETE FROM t OUTPUT deleted.id,deleted.n INTO [dbo].[sink table]([b],[a])",
            "WITH c AS (SELECT 1 AS id) INSERT INTO t(id) OUTPUT inserted.id,inserted.n INTO [dbo].[sink table]([b],[a]) SELECT id FROM c",
        ] {
            let mut statement = crate::batch::parse(sql).unwrap().remove(0);
            let plan = lower_native(&mut statement).unwrap().unwrap();
            let sink = plan.sink.unwrap();
            assert_eq!(sink.table.to_string(), "[dbo].[sink table]");
            let columns = sink.columns.unwrap();
            assert_eq!(
                columns
                    .iter()
                    .map(|id| id.value.as_str())
                    .collect::<Vec<_>>(),
                ["b", "a"]
            );
            assert!(columns.iter().all(|id| id.quote_style == Some('[')));
            assert!(!statement.to_string().contains("sink table"));
            assert!(!plan.projection.to_string().contains("sink table"));
        }
        for name in ["sink", "dbo.sink", "[dbo].[sink table]"] {
            let sql = format!("DELETE FROM t OUTPUT deleted.* INTO {name}");
            let mut statement = crate::batch::parse(&sql).unwrap().remove(0);
            let sink = lower_native(&mut statement).unwrap().unwrap().sink.unwrap();
            assert_eq!(sink.table.to_string(), name);
            assert_eq!(sink.columns, None);
        }
    }

    #[test]
    fn native_plan_keeps_logical_projection_and_rejects_unavailable_images_atomically() {
        for sql in [
            "INSERT INTO t(id) OUTPUT inserted.id,inserted.n+1 AS next_n VALUES(1)",
            "DELETE FROM t OUTPUT deleted.* WHERE id=1",
            "UPDATE t SET n=2 OUTPUT inserted.n AS new_n WHERE id=1",
        ] {
            let mut statement = crate::batch::parse(sql).unwrap().remove(0);
            let plan = lower_native(&mut statement).unwrap().unwrap();
            assert!(!has(&statement));
            assert!(statement.to_string().contains("RETURNING"));
            assert!(plan.projection.to_string().contains("FROM t AS"));
        }
        for sql in [
            "UPDATE t SET n=2 OUTPUT source.n FROM source",
            "INSERT INTO t(id) OUTPUT inserted.id INTO sink(id+1) VALUES(1)",
        ] {
            let mut statement = crate::batch::parse(sql).unwrap().remove(0);
            let before = statement.clone();
            assert!(lower_native(&mut statement).is_err());
            assert_eq!(statement, before);
        }
    }

    #[test]
    fn rejects_reference_output_errors_before_batch_execution() {
        for (sql, number, severity) in [
            ("INSERT INTO t(id) OUTPUT * VALUES(1)", 102, 15),
            ("INSERT INTO t(id) OUTPUT id VALUES(1)", 207, 16),
            ("DELETE FROM t OUTPUT id WHERE id=1", 207, 16),
            ("UPDATE t SET n=1 OUTPUT n", 207, 16),
            ("UPDATE t SET n=1 OUTPUT DATEPART(year,year)", 207, 16),
            ("INSERT INTO t(id) OUTPUT deleted.id VALUES(1)", 4104, 16),
            ("DELETE FROM t OUTPUT inserted.id WHERE id=1", 4104, 16),
            (
                "UPDATE t SET n=n+1 OUTPUT (SELECT 1) AS subquery_value WHERE id=1",
                10705,
                15,
            ),
        ] {
            let batch =
                crate::batch::parse(&format!("INSERT INTO guard VALUES(1); {sql}")).unwrap();
            let before = batch.clone();
            let error = crate::preflight::variables(&batch, &Default::default()).unwrap_err();
            let error = error.downcast_ref::<SqlError>().unwrap();
            assert_eq!(
                (error.number, error.state, error.severity),
                (number, 1, severity)
            );
            assert_eq!(batch, before);
        }
        let batch = crate::batch::parse("UPDATE t SET n=n+1 OUTPUT deleted.n,inserted.n WHERE id=1; DELETE FROM t OUTPUT deleted.id").unwrap();
        validate(&batch).unwrap();
        let batch = crate::batch::parse(
            "UPDATE t SET n=1 OUTPUT DATEPART(year,inserted.d),DATEADD(day,1,inserted.d)",
        )
        .unwrap();
        validate(&batch).unwrap();
    }
}
