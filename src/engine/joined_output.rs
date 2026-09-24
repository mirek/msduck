//! Session parameter binding for the separately materialized joined stages.
use super::*;

pub struct BoundOutputQuery {
    pub query: Box<Query>,
    pub values: Vec<Value>,
}
pub struct BoundOutputUpdate {
    pub update: Update,
    pub values: Vec<Value>,
}
pub struct PreparedJoinedOutput {
    pub candidates: BoundOutputQuery,
    pub images: BoundOutputQuery,
    pub write: BoundOutputUpdate,
    pub output: BoundOutputQuery,
    pub fields: Vec<crate::query_catalog::Field>,
}

pub(super) struct JoinedExecution {
    plan: PreparedJoinedOutput,
    candidates: ObjectName,
    images: ObjectName,
    sink: Option<crate::output_sink::Bound>,
}

/// Substitute a generated single-source stage for description only. Executing
/// this composed query would erase the materialization/evaluation boundaries.
fn inline_source(query: &mut Query, input: Box<Query>) -> Result<()> {
    let SetExpr::Select(select) = query.body.as_mut() else {
        bail!("joined stage description requires SELECT")
    };
    inline_tree(&mut select.from, input)
}
fn inline_tree(sources: &mut [TableWithJoins], input: Box<Query>) -> Result<()> {
    let [source] = sources else {
        bail!("joined stage requires one materialized source")
    };
    ensure!(
        source.joins.is_empty(),
        "joined stage source must already be materialized"
    );
    let TableFactor::Table {
        alias: Some(alias),
        args: None,
        ..
    } = &source.relation
    else {
        bail!("joined stage requires an aliased image relation")
    };
    source.relation = TableFactor::Derived {
        lateral: false,
        subquery: input,
        alias: Some(alias.clone()),
        sample: None,
    };
    Ok(())
}

fn offset_parameters<T: VisitMut>(node: &mut T, offset: usize) -> Result<()> {
    struct Offset(usize);
    impl VisitorMut for Offset {
        type Break = String;
        fn pre_visit_expr(&mut self, expr: &mut Expr) -> ControlFlow<String> {
            if let Expr::Value(value) = expr
                && let sqlparser::ast::Value::Placeholder(slot) = &mut value.value
            {
                let Some(index) = slot.strip_prefix('$').and_then(|s| s.parse::<usize>().ok())
                else {
                    return ControlFlow::Break("expected a bound stage parameter".into());
                };
                *slot = format!("${}", index + self.0);
            }
            ControlFlow::Continue(())
        }
    }
    if let ControlFlow::Break(error) = node.visit(&mut Offset(offset)) {
        bail!(error);
    }
    Ok(())
}

impl Session {
    pub(super) fn plan_joined_execution(
        &self,
        update: &Update,
        with: Option<With>,
        parameters: &HashMap<String, Parameter>,
    ) -> Result<JoinedExecution> {
        let candidates = crate::output_image::name();
        let images = crate::output_image::name();
        let alias = images.0.last().unwrap().as_ident().unwrap().clone();
        let plan = self.prepare_joined_output(
            update,
            with,
            parameters,
            candidates.clone(),
            images.clone(),
            alias,
        )?;
        let Some(OutputClause::Output { into_table, .. }) = &update.output else {
            unreachable!()
        };
        let sink = into_table
            .as_ref()
            .map(|into| {
                let sink = msduck_sql::output::Sink::parse(into)?;
                let sink = crate::output_sink::bind(&self.db, &sink, &plan.fields)?;
                self.validate_prepared_statements(
                    vec![sink.statement.clone()],
                    &sink.declarations,
                )?;
                Ok::<_, anyhow::Error>(sink)
            })
            .transpose()?;
        Ok(JoinedExecution {
            plan,
            candidates,
            images,
            sink,
        })
    }

    pub(super) fn execute_joined_output(
        &mut self,
        execution: JoinedExecution,
    ) -> Result<Execution> {
        let JoinedExecution {
            plan,
            candidates: candidate_name,
            images: image_name,
            sink,
        } = execution;
        let types = crate::result_types::from_fields(&plan.fields);
        let candidates = crate::output_image::Image::create_bound(
            &self.db,
            candidate_name,
            &plan.candidates.query,
            &plan.candidates.values,
        )?;
        let images = crate::output_image::Image::create_bound(
            &self.db,
            image_name,
            &plan.images.query,
            &plan.images.values,
        )?;
        let mut output = self.db.prepare(&plan.output.query.to_string())?;
        let metadata = sink
            .is_none()
            .then(|| crate::query_error::describe(&output, &plan.fields, &types))
            .flatten();
        let describe = |error: anyhow::Error| match &metadata {
            Some(metadata) => crate::query_error::attach_context(error, metadata.clone(), 0xc5),
            None if sink.is_some() => {
                crate::output_sink::failed(error, msduck_sql::output::Operation::Update)
            }
            None => error,
        };
        {
            let mut capture = self.db.prepare(&plan.candidates.query.to_string())?;
            candidates
                .append(
                    capture
                        .query_arrow(duckdb::params_from_iter(plan.candidates.values.iter()))
                        .map_err(anyhow::Error::new)
                        .map_err(&describe)?,
                )
                .map_err(&describe)?;
        }
        {
            let mut capture = self.db.prepare(&plan.images.query.to_string())?;
            let assignment_error = |error: anyhow::Error| {
                let diagnostic = runtime_diagnostic(&error.to_string());
                let error = match diagnostic {
                    Some(mut diagnostic)
                        if diagnostic.number == 8115
                            && diagnostic.message
                                == "Arithmetic overflow error converting expression to data type int." =>
                    {
                        diagnostic.state = 2;
                        error.context(diagnostic)
                    }
                    _ => error,
                };
                describe(error)
            };
            images
                .append(
                    capture
                        .query_arrow(duckdb::params_from_iter(plan.images.values.iter()))
                        .map_err(anyhow::Error::new)
                        .map_err(&assignment_error)?,
                )
                .map_err(&assignment_error)?;
        }
        self.db
            .execute(
                &plan.write.update.to_string(),
                duckdb::params_from_iter(plan.write.values.iter()),
            )
            .map_err(anyhow::Error::new)
            .map_err(&describe)?;
        let batches = output
            .query_arrow(duckdb::params_from_iter(plan.output.values.iter()))
            .map_err(anyhow::Error::new)
            .map_err(&describe)?;
        if let Some(sink) = sink {
            let rows = Self::collect_output_rows(batches, sink.declarations.len())?;
            drop(output);
            images.close()?;
            candidates.close()?;
            return self.store_output_rows(sink, rows, msduck_sql::output::Operation::Update);
        }
        let schema = batches.get_schema();
        let (tokens, count) =
            Self::encode_batches(batches, &schema, &plan.fields, &types).map_err(&describe)?;
        drop(output);
        images.close()?;
        candidates.close()?;
        Ok(Execution::result_set(tokens, Some(count), 0xc5))
    }

    /// Prepare typed stages from a logical UPDATE. The caller still owns DML
    /// syntax/preflight, transaction routing, materialization and result delivery.
    pub fn prepare_joined_output(
        &self,
        update: &Update,
        with: Option<With>,
        parameters: &HashMap<String, Parameter>,
        candidate_relation: ObjectName,
        image_relation: ObjectName,
        alias: Ident,
    ) -> Result<PreparedJoinedOutput> {
        batch_variables(&[Statement::Update(update.clone())], parameters)?;
        let binding = crate::output_join::bind(&self.db, update, with, parameters)?;
        let Some(OutputClause::Output { select_items, .. }) = &update.output else {
            bail!("joined preparation requires OUTPUT")
        };
        let mut physical = Statement::Update(update.clone());
        let Statement::Update(target) = &mut physical else {
            unreachable!()
        };
        target.table.relation = binding.target.clone();
        crate::update::expand_compound(&self.db, &mut physical)?;
        let Statement::Update(mut physical) = physical else {
            unreachable!()
        };
        let money = binding.assignment_money(&physical)?;
        let defaults = physical.assignments.iter_mut().map(|a| {
            let default = matches!(&a.value, Expr::Identifier(id) if id.quote_style.is_none() && id.value.eq_ignore_ascii_case("DEFAULT"));
            if default { a.value = Expr::Value(sqlparser::ast::Value::Null.into()); }
            default
        }).collect::<Vec<_>>();
        let mut images = binding.images(
            &physical,
            candidate_relation.clone(),
            image_relation.clone(),
            alias,
        )?;
        images.capture.with = binding.candidates.capture.with.clone();
        let mut output_query = images.projection(select_items)?;
        let fields = binding.output_fields(select_items)?;
        let key = |name: &ObjectName| {
            name.0
                .iter()
                .map(|p| p.as_ident().unwrap().value.to_lowercase())
                .collect::<Vec<_>>()
        };
        let relations = [
            (key(&candidate_relation), binding.candidate_declarations()?),
            (key(&image_relation), binding.image_declarations(&images)?),
        ]
        .into_iter()
        .collect();
        crate::aggregate_columns::annotate_relations(
            &self.db,
            &mut images.capture,
            parameters,
            &relations,
        )
        .map_err(anyhow::Error::msg)?;
        crate::aggregate_columns::annotate_relations(
            &self.db,
            &mut output_query,
            parameters,
            &relations,
        )
        .map_err(anyhow::Error::msg)?;

        let mut candidate = binding.candidates.capture.clone();
        let SetExpr::Select(select) = candidate.body.as_mut() else {
            unreachable!()
        };
        let qualify = select.qualify.take();
        crate::aggregate_columns::annotate(&self.db, &mut candidate, parameters)
            .map_err(anyhow::Error::msg)?;
        let SetExpr::Select(select) = candidate.body.as_mut() else {
            unreachable!()
        };
        select.qualify = qualify;
        let candidates = self.bind_candidate_query(candidate, parameters)?;
        let mut captured = self.bind_output_query(images.capture.clone(), parameters)?;
        let output = self.bind_output_query(output_query, parameters)?;

        let SetExpr::Select(select) = captured.query.body.as_mut() else {
            unreachable!()
        };
        let mut slots = Vec::new();
        for (assignment, default) in physical.assignments.iter_mut().zip(defaults) {
            let AssignmentTarget::ColumnName(name) = &assignment.target else {
                bail!("unsupported joined assignment")
            };
            let column = name
                .0
                .last()
                .and_then(|p| p.as_ident())
                .ok_or_else(|| anyhow::anyhow!("missing assigned column"))?;
            let image = images
                .columns
                .iter()
                .find(|image| image.column.value.eq_ignore_ascii_case(&column.value))
                .ok_or_else(|| anyhow::anyhow!("missing assigned image"))?;
            let slot = select.projection.iter().position(|item| matches!(item, SelectItem::ExprWithAlias { alias, .. } if alias == &image.inserted))
                .ok_or_else(|| anyhow::anyhow!("missing inserted slot"))?;
            let SelectItem::ExprWithAlias { expr, .. } = &select.projection[slot] else {
                unreachable!()
            };
            assignment.value = if default {
                Expr::Identifier(Ident::new("DEFAULT"))
            } else {
                expr.clone()
            };
            slots.push(slot);
        }
        let converted = crate::update::lower_captured(&self.db, physical, &money)?;
        for (assignment, slot) in converted.assignments.into_iter().zip(slots) {
            let SelectItem::ExprWithAlias { expr, .. } = &mut select.projection[slot] else {
                unreachable!()
            };
            *expr = assignment.value;
        }
        let mut write = images.write;
        let values = self.bind_output_stage(&mut write, parameters)?;
        let plan = PreparedJoinedOutput {
            candidates,
            images: captured,
            output,
            fields,
            write: BoundOutputUpdate {
                update: write,
                values,
            },
        };
        self.validate_joined_stages(&plan)?;
        Ok(plan)
    }

    fn bind_output_stage<T: VisitMut>(
        &self,
        node: &mut T,
        parameters: &HashMap<String, Parameter>,
    ) -> Result<Vec<Value>> {
        let mut translator = Translator {
            parameters,
            values: vec![],
            parameter_slots: HashMap::new(),
            transactions: self.transactions,
            transaction_doomed: self.transaction_doomed,
            original_login: &self.original_login,
            rowcount: self.rowcount,
            last_error: self.last_error,
            caught_error: self.caught_error.as_ref(),
        };
        if let ControlFlow::Break(error) = node.visit(&mut translator) {
            bail!(error);
        }
        Ok(translator.values)
    }
    fn bind_output_query(
        &self,
        mut query: Box<Query>,
        parameters: &HashMap<String, Parameter>,
    ) -> Result<BoundOutputQuery> {
        let values = self.bind_output_stage(&mut query, parameters)?;
        Ok(BoundOutputQuery { query, values })
    }

    fn bind_candidate_query(
        &self,
        mut query: Box<Query>,
        parameters: &HashMap<String, Parameter>,
    ) -> Result<BoundOutputQuery> {
        let SetExpr::Select(select) = query.body.as_mut() else {
            bail!("candidate capture requires SELECT")
        };
        // Only this planner-owned predicate is native SQL. User expressions
        // still pass through the complete T-SQL translator and validation.
        let mut qualify = select.qualify.take();
        struct NativeIdentifiers;
        impl VisitorMut for NativeIdentifiers {
            type Break = String;
            fn pre_visit_ident(&mut self, ident: &mut Ident) -> ControlFlow<String> {
                if ident.quote_style == Some('[') {
                    ident.quote_style = Some('"');
                }
                ControlFlow::Continue(())
            }
        }
        let _ = VisitMut::visit(&mut qualify, &mut NativeIdentifiers);
        let mut bound = self.bind_output_query(query, parameters)?;
        let SetExpr::Select(select) = bound.query.body.as_mut() else {
            bail!("candidate capture requires SELECT")
        };
        select.qualify = qualify;
        Ok(bound)
    }

    fn validate_joined_stages(&self, plan: &PreparedJoinedOutput) -> Result<()> {
        let mut described_images = plan.images.query.clone();
        offset_parameters(&mut described_images, plan.candidates.values.len())?;
        inline_source(&mut described_images, plan.candidates.query.clone())?;
        let preceding = plan.candidates.values.len() + plan.images.values.len();
        let mut described_output = plan.output.query.clone();
        offset_parameters(&mut described_output, preceding)?;
        inline_source(&mut described_output, described_images.clone())?;
        self.db.prepare(&described_output.to_string())?;

        let mut described_write = plan.write.update.clone();
        offset_parameters(&mut described_write, preceding)?;
        let Some(UpdateTableFromKind::AfterSet(sources)) = &mut described_write.from else {
            bail!("joined write requires its captured image source")
        };
        inline_tree(sources, described_images)?;
        self.db.prepare(&described_write.to_string())?;
        Ok(())
    }

    /// Bind an already validated/typed joined plan to this session's values.
    /// This does not replace language preflight, operand typing, or assignment
    /// storage conversions. Each stage owns its parameter slots. Preparation
    /// uses composed derived queries solely for native binding: it never creates
    /// images, executes the DML, or evaluates a user expression.
    pub fn bind_joined_output(
        &self,
        binding: &crate::output_join::Binding,
        images: &msduck_sql::output_update::Plan,
        items: &[SelectItem],
        parameters: &HashMap<String, Parameter>,
    ) -> Result<PreparedJoinedOutput> {
        for (name, metadata) in &binding.scope.parameters {
            if let Some(kind) = metadata.logical_type() {
                ensure!(
                    parameters
                        .get(name)
                        .is_some_and(|parameter| parameter.data_type == kind),
                    "joined parameter declarations changed; acquire a fresh binding"
                );
            }
        }
        let candidate = binding.candidates.capture.clone();
        let mut capture = images.capture.clone();
        // RHS subqueries may refer to the original CTE declarations even after
        // their outer source rows have been captured. Direct source references
        // already address slots and cannot re-run those CTEs.
        capture.with = candidate.with.clone();
        let output = images.projection(items)?;
        let fields = binding.output_fields(items)?;

        let candidates = self.bind_candidate_query(candidate, parameters)?;
        let images_query = self.bind_output_query(capture, parameters)?;
        let output = self.bind_output_query(output, parameters)?;
        let mut write = images.write.clone();
        let values = self.bind_output_stage(&mut write, parameters)?;

        let plan = PreparedJoinedOutput {
            candidates,
            images: images_query,
            write: BoundOutputUpdate {
                update: write,
                values,
            },
            output,
            fields,
        };
        self.validate_joined_stages(&plan)?;
        Ok(plan)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use msduck_core::character::{CharacterType, Family, Length};

    #[test]
    fn joined_dispatch_prepares_then_executes_once_and_rolls_back_failures() {
        let server = crate::server::Server::open(":memory:").unwrap();
        let mut session = Session::new(server.connection().unwrap()).unwrap();
        let mut parameters = HashMap::new();
        for statement in parse_batch("CREATE TABLE dispatch_target(id INT PRIMARY KEY,n INT); INSERT INTO dispatch_target VALUES(10,1),(20,2); CREATE TABLE dispatch_sink(id INT,n INT NOT NULL)").unwrap() {
            session.execute(statement, &mut parameters).unwrap();
        }
        let sql = "WITH s AS (SELECT * FROM (VALUES(10,7),(10,7),(99,5)) v(id,extra)) UPDATE t SET id=t.id+100,n=COALESCE(s.extra,t.n) OUTPUT deleted.id,inserted.id,deleted.n,inserted.n,s.extra FROM dispatch_target t FULL JOIN s ON t.id=s.id";
        let rows = |session: &Session| {
            session
                .db
                .prepare("SELECT id,n FROM dispatch_target ORDER BY id")
                .unwrap()
                .query_map([], |r| Ok((r.get::<_, i32>(0)?, r.get::<_, i32>(1)?)))
                .unwrap()
                .collect::<duckdb::Result<Vec<_>>>()
                .unwrap()
        };
        session.validate_prepared_sql(sql, &[]).unwrap();
        assert_eq!(rows(&session), vec![(10, 1), (20, 2)]);
        assert_eq!(session.db.query_row("SELECT count(*) FROM duckdb_tables() WHERE database_name='temp' AND table_name LIKE '__msduck_output_image_%'",[],|r|r.get::<_,i64>(0)).unwrap(),0);
        session.begin_transaction(0, "").unwrap();
        let execution = session
            .execute(parse_batch(sql).unwrap().remove(0), &mut parameters)
            .unwrap();
        assert_eq!(execution.command, 0xc5);
        assert_eq!(execution.count, Some(2));
        assert_eq!(rows(&session), vec![(110, 7), (120, 2)]);
        session.rollback_transaction("").unwrap();
        assert_eq!(rows(&session), vec![(10, 1), (20, 2)]);
        session
            .db
            .execute_batch("CREATE SEQUENCE dispatch_evaluation")
            .unwrap();
        let volatile = "UPDATE t SET n=CAST(nextval('dispatch_evaluation') AS INT) OUTPUT deleted.n,inserted.n FROM dispatch_target t JOIN (VALUES(10),(10)) s(id) ON t.id=s.id";
        session.validate_prepared_sql(volatile, &[]).unwrap();
        assert_eq!(session.db.query_row("SELECT last_value FROM duckdb_sequences() WHERE sequence_name='dispatch_evaluation'",[],|r|r.get::<_,Option<i64>>(0)).unwrap(),None);
        let execution = session
            .execute(parse_batch(volatile).unwrap().remove(0), &mut parameters)
            .unwrap();
        assert_eq!(execution.count, Some(1));
        assert_eq!(
            session
                .db
                .query_row("SELECT currval('dispatch_evaluation')", [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            1
        );
        for sql in [
            "UPDATE t SET id=1 OUTPUT deleted.id,inserted.id FROM dispatch_target t JOIN (VALUES(10),(20)) s(id) ON t.id=s.id",
            "UPDATE t SET n=NULL OUTPUT deleted.id,inserted.n INTO dispatch_sink FROM dispatch_target t JOIN (VALUES(10),(20)) s(id) ON t.id=s.id",
            "UPDATE t SET n=2147483648 OUTPUT deleted.id,inserted.n FROM dispatch_target t JOIN (VALUES(10),(20)) s(id) ON t.id=s.id",
        ] {
            assert!(
                session
                    .execute(parse_batch(sql).unwrap().remove(0), &mut parameters)
                    .is_err(),
                "{sql}"
            );
            assert_eq!(rows(&session), vec![(10, 1), (20, 2)], "{sql}");
            assert_eq!(
                session
                    .db
                    .query_row("SELECT count(*) FROM dispatch_sink", [], |r| r
                        .get::<_, i64>(0))
                    .unwrap(),
                0
            );
            assert_eq!(session.db.query_row("SELECT count(*) FROM duckdb_tables() WHERE database_name='temp' AND table_name LIKE '__msduck_output_image_%'",[],|r|r.get::<_,i64>(0)).unwrap(),0);
        }
        let sql = "UPDATE t SET n=t.n+s.extra OUTPUT deleted.id,inserted.n INTO dispatch_sink FROM dispatch_target t JOIN (VALUES(10,7),(10,7)) s(id,extra) ON t.id=s.id";
        session.validate_prepared_sql(sql, &[]).unwrap();
        let execution = session
            .execute(parse_batch(sql).unwrap().remove(0), &mut parameters)
            .unwrap();
        assert_eq!(execution.count, Some(1));
        assert_eq!(rows(&session), vec![(10, 8), (20, 2)]);
        assert_eq!(
            session
                .db
                .query_row("SELECT id,n FROM dispatch_sink", [], |r| Ok((
                    r.get::<_, i32>(0)?,
                    r.get::<_, i32>(1)?
                )))
                .unwrap(),
            (10, 8)
        );
    }

    #[test]
    fn typed_preparation_matches_reference_aggregate_and_window_errors() {
        let reference: serde_json::Value =
            serde_json::from_str(include_str!("../../reference/output-typed-stages.json")).unwrap();
        let server = crate::server::Server::open(":memory:").unwrap();
        let mut session = Session::new(server.connection().unwrap()).unwrap();
        assert!(
            session
                .batch_response(
                    reference["results"][0]["setup"].as_str().unwrap(),
                    &HashMap::new(),
                    false,
                    None
                )
                .1
        );
        for result in reference["results"]
            .as_array()
            .unwrap()
            .iter()
            .skip(1)
            .take(3)
        {
            let Statement::Update(update) = Parser::parse_sql(
                &crate::dialect::ServerDialect,
                result["query"].as_str().unwrap(),
            )
            .unwrap()
            .remove(0) else {
                unreachable!()
            };
            let error = session
                .prepare_joined_output(
                    &update,
                    None,
                    &HashMap::new(),
                    crate::output_image::name(),
                    crate::output_image::name(),
                    Ident::new("captured"),
                )
                .err()
                .unwrap();
            let error = error.downcast_ref::<SqlError>().unwrap();
            let expected = &result["reference"]["errors"][0];
            assert_eq!(
                i64::from(error.number),
                expected["number"].as_i64().unwrap()
            );
            assert_eq!(i64::from(error.state), expected["state"].as_i64().unwrap());
            assert_eq!(
                i64::from(error.severity),
                expected["class"].as_i64().unwrap()
            );
            assert_eq!(error.message, expected["message"].as_str().unwrap());
        }
        assert_eq!(
            session
                .db
                .query_row("SELECT n FROM typed_stage", [], |r| r.get::<_, i32>(0))
                .unwrap(),
            7
        );
    }

    #[test]
    fn typed_stages_convert_inserted_images_before_output() {
        let server = crate::server::Server::open(":memory:").unwrap();
        let mut session = Session::new(server.connection().unwrap()).unwrap();
        assert!(session.batch_response("CREATE TABLE typed_stage(id INT PRIMARY KEY,n INT,m MONEY,label NVARCHAR(12),r REAL DEFAULT 1.234567890,d DECIMAL(5,2) DEFAULT 3.456,tm TIME(2),v INT DEFAULT 9); INSERT INTO typed_stage VALUES(10,7,123.45,N'old',0,0,'00:00:00',1)", &HashMap::new(), false, None).1);
        let Statement::Query(query) = Parser::parse_sql(&crate::dialect::ServerDialect,
            "WITH s AS (SELECT @divisor AS divisor) UPDATE t SET id=t.id+100,n=(t.n/s.divisor)*2,label=t.m,m=t.m+0.005,r=1.234567890,d=3.456,tm='12:34:56.127',v=DEFAULT OUTPUT deleted.id,inserted.id,inserted.n,inserted.label,inserted.m,inserted.r,inserted.d,inserted.tm,inserted.v,inserted.n/4*4 AS whole FROM typed_stage t CROSS JOIN s")
            .unwrap().remove(0) else { unreachable!() };
        let SetExpr::Update(Statement::Update(update)) = *query.body else {
            unreachable!()
        };
        let parameters = [(
            "@divisor".into(),
            Parameter {
                data_type: SqlType::Int,
                value: ParameterValue::Int(2),
            },
        )]
        .into_iter()
        .collect();
        let candidate_name = crate::output_image::name();
        let image_name = crate::output_image::name();
        let plan = session
            .prepare_joined_output(
                &update,
                query.with.clone(),
                &parameters,
                candidate_name.clone(),
                image_name.clone(),
                Ident::new("captured"),
            )
            .unwrap();
        assert_eq!(plan.fields[3].info.as_ref().unwrap().max_length, Some(24));
        let mut default_update = update.clone();
        for assignment in &mut default_update.assignments {
            if matches!(&assignment.target, AssignmentTarget::ColumnName(name) if matches!(name.to_string().as_str(), "r" | "d"))
            {
                assignment.value = Expr::Identifier(Ident::new("DEFAULT"));
            }
        }
        // DEFAULT expansion also applies to types whose conversion is native.
        session
            .prepare_joined_output(
                &default_update,
                query.with,
                &parameters,
                crate::output_image::name(),
                crate::output_image::name(),
                Ident::new("captured"),
            )
            .unwrap();

        assert_eq!(
            session
                .db
                .query_row("SELECT id FROM typed_stage", [], |r| r.get::<_, i32>(0))
                .unwrap(),
            10
        );
        assert_eq!(session.db.query_row("SELECT count(*) FROM duckdb_tables() WHERE database_name='temp' AND table_name LIKE '__msduck_output_image_%'",[],|r|r.get::<_,i64>(0)).unwrap(),0);
        session.begin_transaction(0, "").unwrap();
        {
            let db = &session.db;
            let candidates = crate::output_image::Image::create_bound(
                db,
                candidate_name,
                &plan.candidates.query,
                &plan.candidates.values,
            )
            .unwrap();
            candidates
                .append(
                    db.prepare(&plan.candidates.query.to_string())
                        .unwrap()
                        .query_arrow(duckdb::params_from_iter(plan.candidates.values.iter()))
                        .unwrap(),
                )
                .unwrap();
            let images = crate::output_image::Image::create_bound(
                db,
                image_name,
                &plan.images.query,
                &plan.images.values,
            )
            .unwrap();
            images
                .append(
                    db.prepare(&plan.images.query.to_string())
                        .unwrap()
                        .query_arrow(duckdb::params_from_iter(plan.images.values.iter()))
                        .unwrap(),
                )
                .unwrap();
            db.execute(
                &plan.write.update.to_string(),
                duckdb::params_from_iter(plan.write.values.iter()),
            )
            .unwrap();
            let mut output = db.prepare(&plan.output.query.to_string()).unwrap();
            let batches = output
                .query_arrow(duckdb::params_from_iter(plan.output.values.iter()))
                .unwrap()
                .collect::<Vec<_>>();
            assert_eq!(batches.len(), 1);
            let batch = &batches[0];
            let values = (0..batch.num_columns())
                .map(|i| arrow_value(batch.column(i).as_ref(), 0, batch.schema().field(i)).unwrap())
                .collect::<Vec<_>>();
            println!("typed OUTPUT: {values:?}");
            assert_eq!(values[0], Value::Int(10));
            assert_eq!(values[1], Value::Int(110));
            assert_eq!(values[2], Value::Int(6));
            assert_eq!(
                crate::unicode_carrier::units(&values[3]).unwrap(),
                "123.45".encode_utf16().collect::<Vec<_>>()
            );
            assert_eq!(values[8], Value::Int(9));
            assert_eq!(values[9], Value::Int(4));
            assert_eq!(db.query_row("SELECT CAST(m AS VARCHAR), CAST(d AS VARCHAR), epoch_ns(tm), CAST(r AS DOUBLE) FROM typed_stage", [], |r| Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,i64>(2)?,r.get::<_,f64>(3)?))).unwrap(), ("123.4550".into(),"3.46".into(),45_296_130_000_000,1.2345678806304932));
            let mut stored = db
                .prepare("SELECT id,n,label,m,r,d,tm,v FROM typed_stage")
                .unwrap();
            let stored = stored.query_arrow([]).unwrap().next().unwrap();
            for (i, output_index) in [1, 2, 3, 4, 5, 6, 7, 8].into_iter().enumerate() {
                assert_eq!(
                    values[output_index],
                    arrow_value(stored.column(i).as_ref(), 0, stored.schema().field(i)).unwrap()
                );
            }
            drop(output);
            images.close().unwrap();
            candidates.close().unwrap();
        }
        session.rollback_transaction("").unwrap();
        assert_eq!(
            session
                .db
                .query_row("SELECT id FROM typed_stage", [], |r| r.get::<_, i32>(0))
                .unwrap(),
            10
        );
    }

    #[test]
    fn typed_capture_rejects_assignment_overflow_before_writing() {
        let server = crate::server::Server::open(":memory:").unwrap();
        let mut session = Session::new(server.connection().unwrap()).unwrap();
        assert!(session.batch_response("CREATE TABLE stage_overflow(id INT PRIMARY KEY,n INT); INSERT INTO stage_overflow VALUES(10,7)", &HashMap::new(), false, None).1);
        let Statement::Update(update) = Parser::parse_sql(
            &crate::dialect::ServerDialect,
            "UPDATE t SET n=@value OUTPUT deleted.n,inserted.n FROM stage_overflow t",
        )
        .unwrap()
        .remove(0) else {
            unreachable!()
        };
        let candidate_name = crate::output_image::name();
        let image_name = crate::output_image::name();
        let parameters = [(
            "@value".into(),
            Parameter {
                data_type: SqlType::BigInt,
                value: ParameterValue::BigInt(2147483648),
            },
        )]
        .into_iter()
        .collect();
        let plan = session
            .prepare_joined_output(
                &update,
                None,
                &parameters,
                candidate_name.clone(),
                image_name.clone(),
                Ident::new("captured"),
            )
            .unwrap();
        assert_eq!(
            plan.fields[1].info.as_ref().unwrap().system_type_id,
            Some(56)
        );
        session.begin_transaction(0, "").unwrap();
        {
            let db = &session.db;
            let candidates = crate::output_image::Image::create_bound(
                db,
                candidate_name,
                &plan.candidates.query,
                &plan.candidates.values,
            )
            .unwrap();
            candidates
                .append(
                    db.prepare(&plan.candidates.query.to_string())
                        .unwrap()
                        .query_arrow(duckdb::params_from_iter(plan.candidates.values.iter()))
                        .unwrap(),
                )
                .unwrap();
            let images = crate::output_image::Image::create_bound(
                db,
                image_name,
                &plan.images.query,
                &plan.images.values,
            )
            .unwrap();
            let mut capture = db.prepare(&plan.images.query.to_string()).unwrap();
            let error = capture
                .query_arrow(duckdb::params_from_iter(plan.images.values.iter()))
                .err()
                .unwrap();
            assert!(error.to_string().contains("Arithmetic overflow"), "{error}");
            drop(capture);
            drop(images);
            drop(candidates);
        }
        session.rollback_transaction("").unwrap();
        assert_eq!(
            session
                .db
                .query_row("SELECT n FROM stage_overflow WHERE id=10", [], |r| r
                    .get::<_, i32>(0))
                .unwrap(),
            7
        );
        assert_eq!(session.db.query_row("SELECT count(*) FROM duckdb_tables() WHERE database_name='temp' AND table_name LIKE '__msduck_output_image_%'",[],|r|r.get::<_,i64>(0)).unwrap(),0);
    }

    #[test]
    fn candidate_binding_still_validates_user_ranking_expressions() {
        let server = crate::server::Server::open(":memory:").unwrap();
        let session = Session::new(server.connection().unwrap()).unwrap();
        let Statement::Query(query) = Parser::parse_sql(
            &sqlparser::dialect::DuckDbDialect {},
            "SELECT ROW_NUMBER() OVER() FROM (SELECT 1 AS id) t QUALIFY ROW_NUMBER() OVER(PARTITION BY t.id)=1",
        ).unwrap().remove(0) else { unreachable!() };
        let error = session
            .bind_candidate_query(query, &HashMap::new())
            .err()
            .unwrap();
        assert!(error.to_string().contains("ORDER BY"), "{error}");
    }

    #[test]
    fn stage_parameters_are_independent_and_preparation_has_no_effects() {
        let server = crate::server::Server::open(":memory:").unwrap();
        let mut session = Session::new(server.connection().unwrap()).unwrap();
        assert!(session.batch_response("CREATE TABLE stage_target(id INT PRIMARY KEY,n INT); INSERT INTO stage_target VALUES(10,1),(20,2)", &HashMap::new(),false,None).1);
        session
            .db
            .execute_batch("CREATE SEQUENCE stage_evaluations")
            .unwrap();
        session.begin_transaction(0, "").unwrap();
        let sql = "WITH s AS (SELECT @id AS id,@extra AS extra) UPDATE t SET n=CAST(t.n+s.extra+(SELECT MAX(extra)+@delta FROM s)+CAST(nextval('stage_evaluations') AS INT) AS INT) OUTPUT deleted.id,inserted.n,s.extra,@label AS label,inserted.n+@delta AS plus_delta FROM stage_target t JOIN s ON t.id=s.id WHERE t.id>=@min";
        let Statement::Query(query) = Parser::parse_sql(&crate::dialect::ServerDialect, sql)
            .unwrap()
            .remove(0)
        else {
            unreachable!()
        };
        let SetExpr::Update(Statement::Update(update)) = *query.body else {
            unreachable!()
        };
        let mut parameters = [("@id", 10), ("@extra", 7), ("@delta", 3), ("@min", 0)]
            .into_iter()
            .map(|(name, value)| {
                (
                    name.to_owned(),
                    Parameter {
                        value: ParameterValue::Int(value),
                        data_type: SqlType::Int,
                    },
                )
            })
            .collect::<HashMap<_, _>>();
        let label = "🐤'); SELECT $1; --";
        parameters.insert(
            "@label".into(),
            Parameter {
                value: ParameterValue::Unicode(label.encode_utf16().collect()),
                data_type: SqlType::Character(
                    CharacterType::new(Family::Nvarchar, Length::Bounded(40)).unwrap(),
                ),
            },
        );
        {
            let db = &session.db;
            db.execute_batch("INSERT INTO stage_target VALUES(30,3)")
                .unwrap();
            let binding = crate::output_join::bind(db, &update, query.with, &parameters).unwrap();
            let candidate_name = crate::output_image::name();
            let image_name = crate::output_image::name();
            let plan = binding
                .images(
                    &update,
                    candidate_name.clone(),
                    image_name.clone(),
                    Ident::new("captured"),
                )
                .unwrap();
            let Some(OutputClause::Output { select_items, .. }) = &update.output else {
                unreachable!()
            };
            let prepared = session
                .bind_joined_output(&binding, &plan, select_items, &parameters)
                .unwrap();
            assert_eq!(
                prepared.candidates.values,
                vec![Value::Int(10), Value::Int(7), Value::Int(0)]
            );
            assert_eq!(
                prepared.images.values,
                vec![Value::Int(10), Value::Int(7), Value::Int(3)]
            );
            assert_eq!(prepared.output.values.len(), 2);
            assert_eq!(prepared.output.values[1], Value::Int(3));
            assert!(prepared.write.values.is_empty());
            assert!(!prepared.output.query.to_string().contains(label));
            assert_eq!(
                prepared.fields[3].info.as_ref().unwrap().max_length,
                Some(80)
            );
            parameters.get_mut("@delta").unwrap().value = ParameterValue::Int(9);
            let rebound = session
                .bind_joined_output(&binding, &plan, select_items, &parameters)
                .unwrap();
            assert_eq!(rebound.images.values[2], Value::Int(9));
            assert_eq!(prepared.images.values[2], Value::Int(3));
            parameters.get_mut("@delta").unwrap().data_type = SqlType::BigInt;
            assert!(
                session
                    .bind_joined_output(&binding, &plan, select_items, &parameters)
                    .is_err()
            );
            parameters.get_mut("@delta").unwrap().data_type = SqlType::Int;
            let mut nulls = parameters.clone();
            for parameter in nulls.values_mut() {
                parameter.value = ParameterValue::Null;
            }
            let null_plan = session
                .bind_joined_output(&binding, &plan, select_items, &nulls)
                .unwrap();
            assert!(
                null_plan
                    .output
                    .values
                    .iter()
                    .all(|value| *value == Value::Null)
            );
            assert_eq!(
                null_plan.fields[3].info.as_ref().unwrap().max_length,
                Some(80)
            );
            assert_eq!(
                db.query_row("SELECT count(*) FROM stage_target WHERE n=id/10", [], |r| r
                    .get::<_, i64>(0))
                    .unwrap(),
                3
            );
            assert_eq!(db.query_row("SELECT count(*) FROM duckdb_tables() WHERE database_name='temp' AND table_name LIKE '__msduck_output_image_%'",[],|r|r.get::<_,i64>(0)).unwrap(),0);
            assert_eq!(db.query_row("SELECT last_value FROM duckdb_sequences() WHERE sequence_name='stage_evaluations'",[],|r|r.get::<_,Option<i64>>(0)).unwrap(),None);
            assert_eq!(session.transactions, 1);

            let candidates = crate::output_image::Image::create_bound(
                db,
                candidate_name,
                &prepared.candidates.query,
                &prepared.candidates.values,
            )
            .unwrap();
            let mut capture = db.prepare(&prepared.candidates.query.to_string()).unwrap();
            candidates
                .append(
                    capture
                        .query_arrow(duckdb::params_from_iter(prepared.candidates.values.iter()))
                        .unwrap(),
                )
                .unwrap();
            let images = crate::output_image::Image::create_bound(
                db,
                image_name,
                &prepared.images.query,
                &prepared.images.values,
            )
            .unwrap();
            let mut capture = db.prepare(&prepared.images.query.to_string()).unwrap();
            images
                .append(
                    capture
                        .query_arrow(duckdb::params_from_iter(prepared.images.values.iter()))
                        .unwrap(),
                )
                .unwrap();
            db.execute(
                &prepared.write.update.to_string(),
                duckdb::params_from_iter(prepared.write.values.iter()),
            )
            .unwrap();
            let mut output = db.prepare(&prepared.output.query.to_string()).unwrap();
            let mut batches = output
                .query_arrow(duckdb::params_from_iter(prepared.output.values.iter()))
                .unwrap();
            let batch = batches.next().unwrap();
            assert_eq!(batch.num_rows(), 1);
            let value = |index| {
                arrow_value(batch.column(index).as_ref(), 0, batch.schema().field(index)).unwrap()
            };
            assert_eq!(value(0), Value::Int(10));
            assert_eq!(value(1), Value::Int(19));
            assert_eq!(value(2), Value::Int(7));
            assert_eq!(
                crate::unicode_carrier::units(&value(3)).unwrap(),
                label.encode_utf16().collect::<Vec<_>>()
            );
            assert_eq!(value(4), Value::Int(22));
            assert!(batches.next().is_none());
            drop(batches);
            drop(output);
            drop(capture);
            images.close().unwrap();
            candidates.close().unwrap();
            assert_eq!(
                db.query_row("SELECT currval('stage_evaluations')", [], |r| r
                    .get::<_, i64>(0))
                    .unwrap(),
                1
            );
        }
        session.rollback_transaction("").unwrap();
        assert_eq!(
            session
                .db
                .query_row(
                    "SELECT count(*) FROM stage_target WHERE (id=10 AND n=1) OR (id=20 AND n=2)",
                    [],
                    |r| r.get::<_, i64>(0)
                )
                .unwrap(),
            2
        );
        assert_eq!(
            session
                .db
                .query_row("SELECT count(*) FROM stage_target WHERE id=30", [], |r| r
                    .get::<_, i64>(
                    0
                ))
                .unwrap(),
            0
        );
    }
}
