//! Deterministic SQL value rules. No database, transport, clock or session access.
#![forbid(unsafe_code)]
pub mod bin2;
pub mod binary_unicode;
pub mod bounded_aggregate;
pub mod case_mapping;
pub mod catalog;
pub mod character;
pub mod checked_integer;
pub mod collation;
pub mod concat;
pub mod datetime2;
pub mod datetimeoffset;
pub mod decimal_aggregate;
pub mod decimal_arithmetic;
pub mod diagnostic;
pub mod encoding;
pub mod for_json;
pub mod json;
pub mod json_escape;
pub mod json_path;
pub mod left_right;
pub mod legacy_datetime;
pub mod money;
pub mod openjson;
pub mod types;
pub mod value;

pub mod raiserror;
pub mod replicate;
pub mod result;

pub mod print;

pub mod completion;
