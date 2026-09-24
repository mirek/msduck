//! Deterministic T-SQL syntax, logical bindings and AST transformations.
//! Database/catalog access, native vectors, sessions and I/O belong to adapters.
#![forbid(unsafe_code)]
pub mod apply;
pub mod binding_scope;
pub mod catalog_shape;
pub mod catalog_snapshot;
pub mod checked_expression;
pub mod checked_projection;
pub mod concat;
pub mod cte_columns;
pub mod cte_recursion;
pub mod dialect;
pub mod expr;
pub mod expression_metadata;
pub mod for_json;
pub mod function_args;
pub mod generate_series;
pub mod grouping;
pub mod grouping_syntax;
pub mod merge;
pub mod money_cast;
pub mod named_windows;
pub mod openjson_path;
pub mod parameter;
pub mod projection;
pub mod query_options;
pub mod recursive_lower;
pub mod set_coercion;
pub mod sql_type;
pub mod temporal_scale;
pub mod top;
pub mod window_frame;
pub mod window_placement;

pub mod choose;
pub mod nullif;
pub mod predicate;
pub mod ranking;

pub mod aggregate;
pub mod case_types;
pub mod percentile;

pub mod datetime2_cast;
pub mod datetime2_compare;
pub mod datetimeoffset_cast;
pub mod datetimeoffset_compare;

pub mod variant_compare;
pub mod variant_order;
pub mod variant_pack;
pub mod variant_results;
pub mod variant_sets;

pub mod nchar_sets;
pub mod result_types;

pub mod aggregate_columns;
pub mod character_storage;
pub mod datalength;
pub mod datetime2_sets;
pub mod delete;
pub mod group_all;
pub mod update;

pub mod money_format;

pub mod money_results;

pub mod batch;
pub mod variant_cast;

pub mod ddl_syntax;
pub mod preflight;
pub mod view_definition;

pub mod money_arithmetic;
pub mod money_compare;

pub mod result_properties;

pub mod grouping_properties;

pub mod session_function;

pub mod unary_operator;

pub mod decimal_division;
pub mod raiserror;

pub mod replicate;

pub mod left_right;

pub mod aggregate_diagnostics;
pub mod output;
pub mod output_bind;
pub mod output_join;
pub mod output_target;
pub mod output_update;
