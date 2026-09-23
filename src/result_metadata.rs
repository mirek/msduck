//! Align logical facts with a physical result before constructing wire columns.
use crate::{
    query_catalog::Field,
    tds::{Column, Type},
};

/// Each logical list describes the whole result. A mismatched list cannot
/// establish the identity of even its first column, so none of it is applied.
/// Field facts and type overrides can be independently unavailable.
pub(crate) struct Aligned<'a> {
    fields: &'a [Field],
    declared: &'a [Option<Type>],
}

impl<'a> Aligned<'a> {
    pub(crate) fn new(width: usize, fields: &'a [Field], declared: &'a [Option<Type>]) -> Self {
        Self {
            fields: if fields.len() == width { fields } else { &[] },
            declared: if declared.len() == width {
                declared
            } else {
                &[]
            },
        }
    }

    pub(crate) fn declared(&self, index: usize) -> Option<&Type> {
        self.declared.get(index).and_then(Option::as_ref)
    }

    pub(crate) fn column(&self, index: usize, physical_name: String, kind: Type) -> Column {
        let field = self.fields.get(index);
        Column {
            name: field.map_or(physical_name, |field| field.name.clone()),
            kind,
            properties: field.map(|field| field.properties).unwrap_or_default(),
            collation: crate::query_catalog::wire_collation(self.fields, self.fields.len(), index),
        }
    }
}
