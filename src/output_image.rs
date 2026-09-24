//! Transaction-owned physical row images for evaluating OUTPUT projections.
use anyhow::{Result, ensure};
use duckdb::{Connection, arrow::record_batch::RecordBatch};
use sqlparser::ast::{Ident, ObjectName, Query};
use std::sync::atomic::{AtomicU64, Ordering};
static NEXT_IMAGE: AtomicU64 = AtomicU64::new(1);

pub(crate) fn name() -> ObjectName {
    ObjectName::from(vec![
        Ident::new("temp"),
        Ident::new("main"),
        Ident::with_quote(
            '"',
            format!(
                "__msduck_output_image_{}",
                NEXT_IMAGE.fetch_add(1, Ordering::Relaxed)
            ),
        ),
    ])
}

pub(crate) struct Image<'a> {
    db: &'a Connection,
    name: ObjectName,
    active: bool,
}
pub(crate) fn close(image: Option<Image<'_>>) -> Result<()> {
    if let Some(image) = image {
        image.close()?;
    }
    Ok(())
}
impl<'a> Image<'a> {
    pub fn create_bound(
        db: &'a Connection,
        name: ObjectName,
        source: &Query,
        values: &[duckdb::types::Value],
    ) -> Result<Self> {
        let mut empty = source.clone();
        let sqlparser::ast::SetExpr::Select(select) = empty.body.as_mut() else {
            anyhow::bail!("unsupported paired image description")
        };
        // Preserve placeholders in the predicate and assignments while making
        // acquisition empty; actual expressions execute in capture below.
        let condition = select
            .selection
            .take()
            .unwrap_or(sqlparser::ast::Expr::Value(
                sqlparser::ast::Value::Boolean(true).into(),
            ));
        select.selection = Some(sqlparser::ast::Expr::BinaryOp {
            left: Box::new(sqlparser::ast::Expr::Nested(Box::new(condition))),
            op: sqlparser::ast::BinaryOperator::And,
            right: Box::new(sqlparser::ast::Expr::Value(
                sqlparser::ast::Value::Boolean(false).into(),
            )),
        });
        db.execute(
            &format!("CREATE TEMP TABLE {name} AS {empty}"),
            duckdb::params_from_iter(values.iter()),
        )?;
        Ok(Self {
            db,
            name,
            active: true,
        })
    }
    pub fn create(db: &'a Connection, name: ObjectName, source: &Query) -> Result<Self> {
        db.execute_batch(&format!("CREATE TEMP TABLE {name} AS {source}"))?;
        Ok(Self {
            db,
            name,
            active: true,
        })
    }
    pub fn append(&self, batches: impl Iterator<Item = RecordBatch>) -> Result<()> {
        let mut captured = Vec::new();
        let mut bytes = 0usize;
        for batch in batches {
            bytes = bytes.saturating_add(batch.get_array_memory_size());
            ensure!(
                bytes <= 64 * 1024 * 1024,
                "OUTPUT image materialization exceeds the configured 64 MiB limit"
            );
            captured.push(batch);
        }
        let table = &self.name.0[2]
            .as_ident()
            .expect("generated image name")
            .value;
        let mut appender = self.db.appender_to_catalog_and_db(table, "temp", "main")?;
        for batch in captured {
            appender.append_record_batch(batch)?;
        }
        appender.flush()?;
        Ok(())
    }
    pub fn close(mut self) -> Result<()> {
        self.db
            .execute_batch(&format!("DROP TABLE {}", self.name))?;
        self.active = false;
        Ok(())
    }
}
impl Drop for Image<'_> {
    fn drop(&mut self) {
        if self.active {
            // After a native transaction abort this can fail; the enclosing
            // rollback also removes the relation created in that transaction.
            let _ = self.db.execute_batch(&format!("DROP TABLE {}", self.name));
        }
    }
}
