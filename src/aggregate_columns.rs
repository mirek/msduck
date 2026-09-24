//! Native catalog acquisition and integration checks for the pure operand binder.
mod catalog;
#[cfg(test)]
mod group_all;
#[cfg(test)]
mod variant_groups;
pub use catalog::annotate;
pub use catalog::annotate_relations;
