//! Native functions the catalog views use.
//!
//! - `__msduck_catalog_definition(source)`: SQL Server's catalog text of a
//!   stored T-SQL expression (`msduck_sql::dialect::ext::catalog::definition`),
//!   or NULL when it is not known.
//! - `__msduck_module_text(text)`: a module's stored text as SQL Server keeps
//!   it (`ALTER` becomes `CREATE`).
//! - `__msduck_type_shape(type)`: the `sys.types` name and the declared
//!   length, precision and scale of a type as written, separated by `|`
//!   (empty when the type's own value applies).
use crate::unicode_carrier::bytes;
use duckdb::{
    core::{DataChunkHandle, Inserter, LogicalTypeId as Id},
    vscalar::{ScalarFunctionSignature, VScalar},
    vtab::arrow::WritableVector,
};
use msduck_sql::dialect::ext::catalog::definition;
use sqlparser::ast::{DataType, Expr};

/// A VARCHAR to VARCHAR function over `F`.
struct Text<F: Function>(std::marker::PhantomData<F>);

trait Function {
    fn apply(input: &str) -> Option<String>;
}

impl<F: Function + 'static> VScalar for Text<F> {
    type State = ();
    fn invoke(
        _: &(),
        input: &mut DataChunkHandle,
        output: &mut dyn WritableVector,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let len = input.len();
        let source = input.flat_vector(0);
        let mut results = Vec::with_capacity(len);
        for row in 0..len {
            if source.row_is_null(row as u64) {
                results.push(None);
                continue;
            }
            let text = String::from_utf8(bytes(&source, row, len)?)?;
            results.push(F::apply(&text));
        }
        let mut result = output.flat_vector();
        for (row, value) in results.into_iter().enumerate() {
            match value {
                Some(value) => result.insert(row, value.as_str()),
                None => result.set_null(row),
            }
        }
        Ok(())
    }
    fn signatures() -> Vec<ScalarFunctionSignature> {
        vec![ScalarFunctionSignature::exact(
            vec![Id::Varchar.into()],
            Id::Varchar.into(),
        )]
    }
}

struct Definition;
impl Function for Definition {
    fn apply(input: &str) -> Option<String> {
        definition::source_definition(input)
    }
}

struct Module;
impl Function for Module {
    fn apply(input: &str) -> Option<String> {
        Some(definition::module_text(input))
    }
}

struct Shape;
impl Function for Shape {
    fn apply(input: &str) -> Option<String> {
        let kind = data_type(input)?;
        let text = |value: Option<String>| value.unwrap_or_default();
        Some(match msduck_sql::catalog_shape::declaration(&kind) {
            Some(shape) => format!(
                "{}|{}|{}|{}",
                shape.name,
                text(shape.length.map(|v| v.to_string())),
                text(shape.precision.map(|v| v.to_string())),
                text(shape.scale.map(|v| v.to_string()))
            ),
            // Alias and table types: by name, without a schema.
            None => {
                let name = match &kind {
                    DataType::Custom(name, arguments) if arguments.is_empty() => name
                        .0
                        .last()
                        .and_then(|part| part.as_ident())
                        .map(|ident| ident.value.clone())?,
                    _ => return None,
                };
                format!("{name}|||")
            }
        })
    }
}

/// A type as written, parsed.
fn data_type(text: &str) -> Option<DataType> {
    match definition::parse_expression(&format!("CAST(NULL AS {text})"))? {
        Expr::Cast { data_type, .. } => Some(data_type),
        _ => None,
    }
}

pub(super) fn register(db: &duckdb::Connection) -> anyhow::Result<()> {
    db.register_scalar_function::<Text<Definition>>("__msduck_catalog_definition")?;
    db.register_scalar_function::<Text<Shape>>("__msduck_type_shape")?;
    db.register_scalar_function::<Text<Module>>("__msduck_module_text")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn type_shapes_follow_declarations() {
        assert_eq!(Shape::apply("int").as_deref(), Some("int|||"));
        assert_eq!(
            Shape::apply("nvarchar(20)").as_deref(),
            Some("nvarchar|40||")
        );
        assert_eq!(
            Shape::apply("varchar(max)").as_deref(),
            Some("varchar|-1||")
        );
        assert_eq!(
            Shape::apply("decimal(10,2)").as_deref(),
            Some("decimal|9|10|2")
        );
        assert_eq!(Shape::apply("dbo.tvp").as_deref(), Some("tvp|||"));
    }
}
