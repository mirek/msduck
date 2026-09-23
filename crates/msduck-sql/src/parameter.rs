//! Shared bindings use core values and logical SQL declarations.
use msduck_core::{types::Type, value::Value};

#[derive(Clone, Debug)]
pub struct Parameter {
    pub value: Value,
    pub data_type: Type,
}
impl Parameter {
    pub fn ast_type(&self) -> sqlparser::ast::DataType {
        crate::sql_type::ast(self.data_type)
    }
}
