//! SQL Server-compatible server and DuckDB adapters.
//! Deterministic values, syntax and codecs live in the three workspace libraries.
pub use msduck_core::{datetime2, datetimeoffset};
pub mod engine;
pub use msduck_sql::parameter;
pub(crate) use msduck_sql::{
    apply, dialect, function_args, grouping, grouping_syntax, merge, named_windows, sql_type, top,
    window_frame, window_placement,
};
pub(crate) use msduck_sql::{case_types, choose, nullif, percentile, predicate, ranking};
pub mod rpc;
pub mod server;
pub mod tds;

mod aggregate;
mod aggregate_columns;
mod assignment;
mod backend_value;
mod bitwise;
mod calendar_parts;
mod character;
mod character_storage;
mod column_catalog;
mod datalength;
mod dateadd;
mod datediff;
mod datepart;
mod datetime2_add;
mod datetime2_cast;
mod datetime2_compare;
mod datetime2_date;
mod datetime2_results;
mod datetime2fromparts;
mod datetimeoffset_cast;
mod datetimeoffset_compare;
mod datetimeoffsetfromparts;
mod declared_columns;
mod output_image;
pub mod output_join;
mod output_sink;
mod output_update;
mod query_error;
mod raiserror;
mod replicate;
mod unicode_carrier;
mod unicode_case;
mod unicode_trim;
pub(crate) use msduck_sql::delete;
mod ansi_binary;
mod binary_unicode;
mod character_aggregate;
mod decimal_aggregate;
mod decimal_division;
mod eomonth;
mod for_json;
mod identity;
mod identity_metadata;
mod insert;
mod integer_aggregate;
mod integer_conversion;
mod isjson;
mod isnull;
mod json_extract;
mod nchar_sets;
mod ncharacter;
mod ntile;
mod nvarchar;
mod object_catalog;
mod openjson;
mod percentile_input;
mod query_catalog;
mod result_types;
mod scalar;
mod schema_catalog;
mod select_into;
mod space;
mod string_escape;
mod switchoffset;
mod table_alter;
mod temporal_precision;
mod time_add;
mod time_results;
mod timefromparts;
mod truncate;
mod type_catalog;
mod unicode;
mod update;
mod value_window;
mod varchar;
mod variant;
mod variant_cast;
mod variant_compare;
mod variant_extreme;
#[cfg(test)]
mod variant_order;
mod variant_pack;
mod variant_results;
#[cfg(test)]
mod variant_sets;
mod views;

#[cfg(test)]
mod temporal_wire_tests;

mod money_range;

mod money_format;

mod money_arithmetic;

pub mod tls;

pub mod authentication;

mod concat_lower;
