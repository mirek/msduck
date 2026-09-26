//! Pure binding and final-value selection for application RPC parameters.
//!
//! The caller parses declarations and decodes TDS values. This module only
//! resolves names, direction and declaration types; it performs no conversion
//! and does not infer metadata from a runtime value.
use std::collections::{HashMap, HashSet};

use crate::parameter::Parameter;
use msduck_core::{character::Length, types::Type, value::Value};

#[derive(Clone, Copy, Debug)]
pub struct Declaration<'a> {
    pub name: &'a str,
    pub data_type: Type,
    pub output: bool,
}

#[derive(Clone, Debug)]
pub struct Received<'a> {
    /// Empty means the declaration at this RPC ordinal. Otherwise the name may
    /// have a leading `@`; resolution is case-insensitive.
    pub name: &'a str,
    /// TDS RPC parameter status: 0 for input, 1 for OUTPUT.
    pub status: u8,
    pub parameter: Parameter,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutputSlot {
    pub name: String,
    pub data_type: Type,
    /// Zero-based ordinal among application RPC parameters, including inputs.
    pub ordinal: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub struct OutputValue {
    pub slot: OutputSlot,
    pub value: Value,
}

#[derive(Clone, Debug)]
pub struct Bound {
    pub bindings: HashMap<String, Parameter>,
    pub outputs: Vec<OutputSlot>,
}

/// Bind received RPC parameters to explicit, ordered SQL declarations. Named
/// parameters may arrive in any order; a positional parameter uses its *own*
/// ordinal rather than the next unmatched declaration.
pub fn bind(declarations: &[Declaration<'_>], received: &[Received<'_>]) -> Result<Bound, String> {
    if declarations.len() != received.len() {
        return Err("RPC parameter count does not match declaration".into());
    }
    let mut declared = HashMap::with_capacity(declarations.len());
    for (index, declaration) in declarations.iter().enumerate() {
        let name = canonical_name(declaration.name)?;
        if declared.insert(name.clone(), index).is_some() {
            return Err(format!("duplicate parameter declaration {name}"));
        }
        if declaration.output && !supported_output(declaration.data_type) {
            return Err(format!("unsupported RPC OUTPUT declaration {name}"));
        }
    }

    let mut bindings = HashMap::with_capacity(received.len());
    let mut outputs = Vec::new();
    let mut seen = HashSet::with_capacity(received.len());
    for (ordinal, parameter) in received.iter().enumerate() {
        if parameter.status & !1 != 0 {
            return Err("unsupported default/encrypted RPC parameter".into());
        }
        let name = if parameter.name.is_empty() {
            canonical_name(declarations[ordinal].name)?
        } else {
            canonical_name(parameter.name)?
        };
        let Some(&declaration_index) = declared.get(&name) else {
            return Err(format!("RPC parameter is not declared: {name}"));
        };
        if !seen.insert(declaration_index) {
            return Err(format!("duplicate RPC parameter {name}"));
        }
        let declaration = &declarations[declaration_index];
        if declaration.output != (parameter.status == 1) {
            return Err(format!(
                "RPC OUTPUT direction disagrees with declaration {name}"
            ));
        }
        bindings.insert(
            name.clone(),
            Parameter {
                value: parameter.parameter.value.clone(),
                data_type: declaration.data_type,
            },
        );
        if declaration.output {
            outputs.push(OutputSlot {
                name,
                data_type: declaration.data_type,
                ordinal,
            });
        }
    }
    Ok(Bound { bindings, outputs })
}

impl Bound {
    /// An uncaught RPC error produces no application RETURNVALUE tokens, even
    /// when an earlier statement assigned an output. A caught error that lets
    /// execution complete passes `completed = true` with its final variables.
    /// RETURNVALUE emits small outputs before MAX outputs, preserving original
    /// RPC order within each group; each slot retains its original ordinal.
    pub fn final_values(
        &self,
        final_variables: &HashMap<String, Parameter>,
        completed: bool,
    ) -> Result<Vec<OutputValue>, String> {
        if !completed {
            return Ok(Vec::new());
        }
        self.outputs
            .iter()
            .filter(|slot| !large_object(slot.data_type))
            .chain(
                self.outputs
                    .iter()
                    .filter(|slot| large_object(slot.data_type)),
            )
            .map(|slot| {
                let final_parameter = final_variables
                    .get(&slot.name)
                    .ok_or_else(|| format!("missing final RPC OUTPUT variable {}", slot.name))?;
                if final_parameter.data_type != slot.data_type {
                    return Err(format!(
                        "RPC OUTPUT variable {} changed its declared type",
                        slot.name
                    ));
                }
                Ok(OutputValue {
                    slot: slot.clone(),
                    value: final_parameter.value.clone(),
                })
            })
            .collect()
    }
}

fn canonical_name(name: &str) -> Result<String, String> {
    let bare = name.strip_prefix('@').unwrap_or(name);
    if bare.is_empty()
        || bare.starts_with('@')
        || !bare
            .chars()
            .next()
            .is_some_and(|c| matches!(c, '_' | '#' | '$') || c.is_alphabetic())
        || !bare
            .chars()
            .all(|c| matches!(c, '_' | '#' | '$') || c.is_alphanumeric())
    {
        return Err(format!("invalid RPC parameter name {name}"));
    }
    Ok(format!("@{}", bare.to_lowercase()))
}

fn supported_output(kind: Type) -> bool {
    matches!(
        kind,
        Type::TinyInt
            | Type::SmallInt
            | Type::Int
            | Type::BigInt
            | Type::Bit
            | Type::Character(_)
            | Type::Binary(_)
            | Type::Decimal(_)
    )
}

fn large_object(kind: Type) -> bool {
    match kind {
        Type::Character(character) => character.length() == Length::Max,
        Type::Binary(binary) => binary.length() == Length::Max,
        _ => false,
    }
}
