//! Static result properties over explicit lexical fields; never inspect row values.
use crate::binding_scope::{self, Source};
use msduck_core::result::{Origin, Properties};
use sqlparser::ast::*;

pub fn expression(expr: &Expr, sources: &[Source], outer: &[Option<Vec<Source>>]) -> Properties {
    expression_with(expr, sources, outer, &|_| None)
}
pub fn expression_with(
    expr: &Expr,
    sources: &[Source],
    outer: &[Option<Vec<Source>>],
    context: &dyn Fn(&Expr) -> Option<Properties>,
) -> Properties {
    if let Some(properties) = context(expr) {
        return properties;
    }
    let recurse = |expr| expression_with(expr, sources, outer, context);
    match expr {
        Expr::Nested(e) => recurse(e),
        Expr::Value(v) => {
            Properties::expression(matches!(v.value, Value::Null | Value::Placeholder(_)))
        }
        Expr::Identifier(id) if crate::session_function::counter_type(&id.value).is_some() => {
            Properties::expression(false)
        }
        Expr::Identifier(id) if id.value.starts_with('@') => Properties::expression(true),
        Expr::Identifier(id) => binding_scope::resolve(&[id], sources, outer)
            .map(|f| f.properties)
            .unwrap_or_default(),
        Expr::CompoundIdentifier(ids) => {
            binding_scope::resolve(&ids.iter().collect::<Vec<_>>(), sources, outer)
                .map(|f| f.properties)
                .unwrap_or_default()
        }
        Expr::UnaryOp {
            op: UnaryOperator::Plus,
            expr,
        } => recurse(expr),
        Expr::UnaryOp {
            op: UnaryOperator::Minus,
            expr,
        } => Properties::expression(!numeric_literal(expr)),
        Expr::Case {
            conditions,
            else_result,
            ..
        } => {
            let nullable = else_result
                .as_ref()
                .is_none_or(|e| recurse(e).nullable != Some(false))
                || conditions
                    .iter()
                    .any(|c| recurse(&c.result).nullable != Some(false));
            Properties::expression(nullable)
        }
        Expr::Function(f) => {
            let name = f.name.to_string().to_ascii_uppercase();
            if f.over.is_some()
                || matches!(
                    name.as_str(),
                    "COUNT"
                        | "COUNT_BIG"
                        | "SUM"
                        | "AVG"
                        | "MIN"
                        | "MAX"
                        | "STDEV"
                        | "STDEVP"
                        | "VAR"
                        | "VARP"
                        | "STRING_AGG"
                )
            {
                return Properties {
                    nullable: Some(true),
                    origin: Origin::Derived,
                };
            }
            if matches!(name.as_str(), "GROUPING" | "GROUPING_ID") {
                return Properties::expression(false);
            }
            if matches!(
                name.as_str(),
                "REPLICATE"
                    | "UNICODE"
                    | "__MSDUCK_CARRIER_UNICODE"
                    | "DATALENGTH"
                    | "LEN"
                    | "LEFT"
                    | "RIGHT"
                    | "__MSDUCK_LEFT_UNICODE_TYPED"
                    | "__MSDUCK_RIGHT_UNICODE_TYPED"
            ) {
                return Properties::expression(true);
            }
            if name == "CHOOSE" {
                // Even a non-null selected arm may yield NULL for an invalid index.
                return Properties::expression(true);
            }
            if name == "COALESCE"
                && let FunctionArguments::List(args) = &f.args
            {
                let first = args
                    .args
                    .iter()
                    .filter_map(|arg| match arg {
                        FunctionArg::Unnamed(FunctionArgExpr::Expr(e)) => Some(e),
                        _ => None,
                    })
                    .find(|e| !literal_null(e));
                return Properties::expression(
                    first.is_none_or(|e| recurse(e).nullable != Some(false)),
                );
            }
            if name == "IIF"
                && let FunctionArguments::List(args) = &f.args
            {
                let nonnull = args.args.len() == 3 && args.args.iter().skip(1).all(|arg| matches!(arg, FunctionArg::Unnamed(FunctionArgExpr::Expr(e)) if recurse(e).nullable == Some(false)));
                return Properties::expression(!nonnull);
            }
            if name == "ISNULL"
                && let FunctionArguments::List(args) = &f.args
            {
                let nonnull = args.args.iter().any(|arg| matches!(arg, FunctionArg::Unnamed(FunctionArgExpr::Expr(e)) if non_null_input(e, sources, outer, context)));
                return Properties::expression(!nonnull);
            }
            if crate::session_function::result_type(f).is_some() {
                return Properties::expression(true);
            }
            // Function provenance is not universally fComputed in SQL Server.
            // Preserve unknown rather than guessing from scalar syntax alone.
            Properties::default()
        }
        Expr::Cast { .. } | Expr::BinaryOp { .. } => Properties::expression(true),
        Expr::Subquery(query) => Properties {
            nullable: Some(true),
            origin: if matches!(query.for_clause, Some(ForClause::Json { .. })) {
                Origin::Derived
            } else {
                Origin::Expression
            },
        },
        _ => Properties::default(),
    }
}

fn numeric_literal(expr: &Expr) -> bool {
    match expr {
        Expr::Value(value) => matches!(value.value, Value::Number(_, _)),
        Expr::Nested(expr)
        | Expr::UnaryOp {
            op: UnaryOperator::Plus | UnaryOperator::Minus,
            expr,
        } => numeric_literal(expr),
        _ => false,
    }
}

// ISNULL reasons about NULL propagation through successful ordinary casts,
// even though a standalone CAST's result metadata is nullable. TRY_CAST may
// introduce NULL. Grouping context takes precedence over this scalar rule.
fn non_null_input(
    expr: &Expr,
    sources: &[Source],
    outer: &[Option<Vec<Source>>],
    context: &dyn Fn(&Expr) -> Option<Properties>,
) -> bool {
    if let Some(properties) = context(expr) {
        return properties.nullable == Some(false);
    }
    match expr {
        Expr::Nested(inner) => non_null_input(inner, sources, outer, context),
        Expr::Cast {
            kind: CastKind::Cast | CastKind::DoubleColon,
            expr,
            ..
        } => non_null_input(
            crate::variant_cast::source(expr).unwrap_or(expr),
            sources,
            outer,
            context,
        ),
        _ => expression_with(expr, sources, outer, context).nullable == Some(false),
    }
}

/// Whether COALESCE must convert its first non-NULL candidate into a different
/// primitive family. Unknown/alias families do not prove a non-null result.
/// This is metadata inference, not a general conversion or overload resolver.
pub(crate) fn coalesce_conversion(
    first: &msduck_core::catalog::TypeMetadata,
    arguments: &[msduck_core::catalog::TypeMetadata],
) -> Option<bool> {
    fn rank(info: &msduck_core::catalog::TypeMetadata) -> Option<usize> {
        let id = info.system_type_id?;
        if info.user_type_id.is_some_and(|user| user != i32::from(id)) {
            return None;
        }
        // DECIMAL and NUMERIC share precedence despite their catalog IDs.
        let id = if id == 108 { 106 } else { id };
        // Increasing SQL Server precedence for the supported primitive families.
        [
            175, 167, 239, 231, 104, 48, 52, 56, 127, 122, 60, 106, 59, 62,
        ]
        .iter()
        .position(|candidate| *candidate == id)
    }
    let decimal_conversion = matches!(first.system_type_id, Some(106 | 108))
        && arguments.iter().any(|argument| {
            matches!(argument.system_type_id, Some(106 | 108))
                && (argument.precision != first.precision || argument.scale != first.scale)
        });
    let max_conversion = matches!(first.system_type_id, Some(167 | 231))
        && first.max_length != Some(-1)
        && arguments.iter().any(|argument| {
            argument.system_type_id == first.system_type_id && argument.max_length == Some(-1)
        });
    let first = rank(first)?;
    let highest = arguments
        .iter()
        .map(rank)
        .collect::<Option<Vec<_>>>()?
        .into_iter()
        .max()?;
    Some(highest > first || decimal_conversion || max_conversion)
}

pub(crate) fn literal_null(expr: &Expr) -> bool {
    if let Some(source) = crate::variant_cast::source(expr) {
        return literal_null(source);
    }
    match expr {
        Expr::Value(v) => matches!(v.value, Value::Null),
        Expr::Nested(e) | Expr::Cast { expr: e, .. } => literal_null(e),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unicode_retains_nullable_expression_origin_for_values_and_empty_results() {
        for sql in [
            "SELECT UNICODE(N'🦆'),UNICODE(N''),UNICODE(NULL),UNICODE(12)",
            "SELECT UNICODE(s) FROM (VALUES(N'x')) t(s) WHERE 1=0",
            "SELECT UNICODE(LEFT(N'🦆',1)),UNICODE(RIGHT(N'🦆',1))",
        ] {
            let actual = fields(&CatalogSnapshot::default(), sql);
            assert!(!actual.is_empty());
            assert!(
                actual.iter().all(|p| *p == Properties::expression(true)),
                "{sql}: {actual:?}"
            );
        }
    }
    use crate::{
        binding_scope::{Field, Scope},
        catalog_snapshot::CatalogSnapshot,
    };
    fn fields(catalog: &CatalogSnapshot, sql: &str) -> Vec<Properties> {
        let Statement::Query(query) = crate::batch::parse(sql).unwrap().remove(0) else {
            panic!()
        };
        crate::projection::query_fields(catalog, &query, &Scope::default())
            .unwrap()
            .into_iter()
            .map(|f| f.properties)
            .collect()
    }
    #[test]
    fn grouping_keys_and_post_group_expressions_have_distinct_properties() {
        let mut catalog = CatalogSnapshot::default();
        catalog.types.insert(
            "int".into(),
            msduck_core::catalog::TypeMetadata {
                system_type_id: Some(56),
                ..Default::default()
            },
        );
        let derived = |nullable| Properties {
            nullable: Some(nullable),
            origin: Origin::Derived,
        };
        for key in ["CASE WHEN n=1 THEN 10 ELSE 20 END", "ISNULL(n,0)"] {
            for (group, nullable) in [(key.to_owned(), false), (format!("ROLLUP({key})"), true)] {
                assert_eq!(
                    fields(
                        &catalog,
                        &format!("SELECT {key} AS k FROM (VALUES(1),(2)) q(n) GROUP BY {group}")
                    ),
                    vec![derived(nullable)]
                );
            }
        }
        for group in ["n,ROLLUP(m)", "GROUPING SETS((n),(n,m))", "CUBE(n,m)"] {
            assert_eq!(
                fields(
                    &catalog,
                    &format!("SELECT n,m FROM (VALUES(1,2)) q(n,m) GROUP BY {group}")
                ),
                vec![derived(true), derived(true)]
            );
        }
        assert_eq!(
            fields(
                &catalog,
                "SELECT ISNULL(n,0),COALESCE(n,NULL),n FROM (VALUES(1),(2)) q(n) GROUP BY ROLLUP(n)"
            ),
            vec![
                Properties::expression(false),
                Properties::expression(true),
                derived(true)
            ]
        );
        for star in ["*", "q.*"] {
            assert_eq!(
                fields(
                    &catalog,
                    &format!("SELECT {star} FROM (VALUES(1)) q(n) GROUP BY ROLLUP(n)")
                ),
                vec![derived(true)]
            );
        }
        let sql = "SELECT (CASE WHEN q.n=1 THEN 10 ELSE 20 END) FROM (VALUES(1),(2)) q(n) GROUP BY ROLLUP(CASE WHEN n=1 THEN 10 ELSE 20 END)";
        let Statement::Query(query) = crate::batch::parse(sql).unwrap().remove(0) else {
            panic!()
        };
        let before = query.clone();
        let result = crate::projection::query_fields(&catalog, &query, &Scope::default()).unwrap();
        assert_eq!(result[0].properties, derived(true));
        assert_eq!(query, before);
        assert_eq!(catalog.types.len(), 1);
        assert!(catalog.tables.is_empty());
    }
    #[test]
    fn unary_minus_distinguishes_numeric_literals_from_runtime_operands() {
        let catalog = CatalogSnapshot::default();
        assert_eq!(
            fields(
                &catalog,
                "SELECT -1,-(1),-(-1),-@n,-@@ROWCOUNT,-CAST(1 AS INT)"
            ),
            vec![
                Properties::expression(false),
                Properties::expression(false),
                Properties::expression(false),
                Properties::expression(true),
                Properties::expression(true),
                Properties::expression(true)
            ]
        );
    }

    #[test]
    fn choose_and_max_conversions_keep_reference_nullable_properties() {
        use msduck_core::catalog::TypeMetadata;
        let mut catalog = CatalogSnapshot::default();
        for (name, id) in [("nvarchar", 231), ("varchar", 167)] {
            catalog.types.insert(
                name.into(),
                TypeMetadata {
                    system_type_id: Some(id),
                    ..Default::default()
                },
            );
            let bounded = TypeMetadata {
                system_type_id: Some(id),
                max_length: Some(6),
                ..Default::default()
            };
            let max = TypeMetadata {
                max_length: Some(-1),
                ..bounded.clone()
            };
            assert_eq!(
                coalesce_conversion(&bounded, &[max.clone(), bounded.clone()]),
                Some(true)
            );
            assert_eq!(
                coalesce_conversion(&max, &[max.clone(), bounded.clone()]),
                Some(false)
            );
            assert_eq!(
                coalesce_conversion(
                    &bounded,
                    &[TypeMetadata {
                        max_length: Some(40),
                        ..bounded.clone()
                    }]
                ),
                Some(false)
            );
        }
        catalog.types.insert(
            "int".into(),
            TypeMetadata {
                system_type_id: Some(56),
                ..Default::default()
            },
        );
        for sql in [
            "SELECT CHOOSE(1,10,20)",
            "SELECT CHOOSE(0,10,20)",
            "SELECT CHOOSE(NULL,10,20)",
            "SELECT COALESCE(CAST(NULL AS NVARCHAR(MAX)),N'abc')",
            "SELECT COALESCE('abc',CAST(NULL AS VARCHAR(MAX)))",
        ] {
            assert_eq!(
                fields(&catalog, sql),
                [Properties::expression(true)],
                "{sql}"
            );
        }
        assert_eq!(
            fields(
                &catalog,
                "SELECT ISNULL(CAST(NULL AS NVARCHAR(MAX)),N'abc')"
            ),
            [Properties::expression(false)]
        );
    }
    #[test]
    fn coalesce_conversions_and_isnull_casts_keep_distinct_nullability() {
        use msduck_core::catalog::TypeMetadata;
        let mut catalog = CatalogSnapshot::default();
        for (name, id) in [
            ("int", 56),
            ("bigint", 127),
            ("smallmoney", 122),
            ("money", 60),
            ("varchar", 167),
        ] {
            catalog.types.insert(
                name.into(),
                TypeMetadata {
                    system_type_id: Some(id),
                    ..Default::default()
                },
            );
        }
        let decimal = TypeMetadata {
            system_type_id: Some(106),
            precision: Some(3),
            scale: Some(2),
            ..Default::default()
        };
        let numeric = TypeMetadata {
            system_type_id: Some(108),
            ..decimal.clone()
        };
        assert_eq!(coalesce_conversion(&decimal, &[numeric]), Some(false));
        assert_eq!(
            coalesce_conversion(
                &decimal,
                &[TypeMetadata {
                    precision: Some(10),
                    ..decimal.clone()
                }]
            ),
            Some(true)
        );
        for (sql, nullable) in [
            ("SELECT COALESCE(CAST(NULL AS INT),1)", false),
            ("SELECT COALESCE(CAST(NULL AS BIGINT),1)", true),
            ("SELECT COALESCE(CAST(NULL AS SMALLMONEY),1)", true),
            ("SELECT COALESCE(CAST(NULL AS MONEY),'$2')", true),
            (
                "SELECT ISNULL(CAST(NULL AS SMALLMONEY),CAST(2 AS MONEY))",
                false,
            ),
            ("SELECT ISNULL(CAST(NULL AS INT),CAST(1 AS INT))", false),
            (
                "SELECT ISNULL(CAST(NULL AS INT),TRY_CAST('x' AS INT))",
                true,
            ),
            ("SELECT ISNULL(CAST(NULL AS INT),CAST(NULL AS INT))", true),
        ] {
            assert_eq!(fields(&catalog, sql)[0].nullable, Some(nullable), "{sql}");
        }
    }
    #[test]
    fn declarations_survive_aliases_and_null_extension_without_mutating_catalog() {
        let id = Properties {
            nullable: Some(false),
            origin: Origin::Identity,
        };
        let required = Properties {
            nullable: Some(false),
            origin: Origin::Stored,
        };
        let optional = Properties {
            nullable: Some(true),
            origin: Origin::Stored,
        };
        let mut catalog = CatalogSnapshot::default();
        catalog.tables.insert(
            "t".into(),
            [("id", id), ("n", required), ("v", optional)]
                .into_iter()
                .map(|(name, properties)| Field {
                    collation: None,
                    name: name.into(),
                    info: None,
                    json_fragment: false,
                    properties,
                })
                .collect(),
        );
        assert_eq!(
            fields(
                &catalog,
                "WITH q AS (SELECT * FROM t) SELECT id AS renamed,n,v FROM q WHERE 1=0"
            ),
            vec![id, required, optional]
        );
        let mut null_id = id;
        null_id.null_extend();
        let mut null_required = required;
        null_required.null_extend();
        assert_eq!(
            fields(
                &catalog,
                "SELECT a.id,a.n,b.id,b.n FROM t a LEFT JOIN t b ON 1=0"
            ),
            vec![id, required, null_id, null_required]
        );
        assert_eq!(
            fields(
                &catalog,
                "SELECT a.id,a.n,b.id,b.n FROM t a RIGHT JOIN t b ON 1=0"
            ),
            vec![null_id, null_required, id, required]
        );
        assert_eq!(
            fields(&catalog, "SELECT a.id,b.id FROM t a FULL JOIN t b ON 1=0"),
            vec![null_id, null_id]
        );
        assert_eq!(
            fields(&catalog, "SELECT id FROM t UNION ALL SELECT id FROM t"),
            vec![Properties {
                nullable: Some(false),
                origin: Origin::Derived
            }]
        );
        assert_eq!(
            fields(&catalog, "SELECT id,n,v FROM t"),
            vec![id, required, optional]
        );
        assert_eq!(
            fields(&catalog, "SELECT missing FROM t"),
            vec![Properties::default()]
        );
    }
    #[test]
    fn expression_metadata_does_not_use_runtime_values() {
        let mut catalog = CatalogSnapshot::default();
        catalog.types.insert(
            "int".into(),
            msduck_core::catalog::TypeMetadata {
                system_type_id: Some(56),
                ..Default::default()
            },
        );
        assert_eq!(
            fields(
                &catalog,
                "SELECT 1,NULL,N'x',CAST(1 AS INT),1+2,@@TRANCOUNT,@p,@@rowcount"
            ),
            vec![
                Properties::expression(false),
                Properties::expression(true),
                Properties::expression(false),
                Properties::expression(true),
                Properties::expression(true),
                Properties::expression(false),
                Properties::expression(true),
                Properties::expression(false)
            ]
        );
        assert_eq!(
            fields(
                &catalog,
                "SELECT ISNULL(CAST(NULL AS INT),1),COUNT(*),(SELECT 1 WHERE 1=0)"
            ),
            vec![
                Properties::expression(false),
                Properties {
                    nullable: Some(true),
                    origin: Origin::Derived
                },
                Properties::expression(true)
            ]
        );
        assert_eq!(
            fields(&catalog, "SELECT COALESCE(CAST(NULL AS INT),1)"),
            vec![Properties::expression(false)]
        );
    }
    #[test]
    fn derived_columns_and_grouping_sets_do_not_inherit_computed_flags() {
        let catalog = CatalogSnapshot::default();
        assert_eq!(
            fields(&catalog, "SELECT x FROM (SELECT 1+2 AS x) q"),
            vec![Properties {
                nullable: Some(true),
                origin: Origin::Derived
            }]
        );
        assert_eq!(
            fields(&catalog, "WITH q AS (SELECT 1 AS x) SELECT x FROM q"),
            vec![Properties {
                nullable: Some(false),
                origin: Origin::Derived
            }]
        );
        assert_eq!(
            fields(
                &catalog,
                "SELECT x,GROUPING(x) FROM (VALUES(1)) q(x) GROUP BY ROLLUP(x)"
            ),
            vec![
                Properties {
                    nullable: Some(true),
                    origin: Origin::Derived
                },
                Properties::expression(false)
            ]
        );
        assert_eq!(
            fields(&catalog, "SELECT unknown_function(1)"),
            vec![Properties::default()]
        );
        assert_eq!(
            fields(
                &catalog,
                "SELECT CAST(x AS INT) FROM (SELECT unknown_value AS x) q"
            ),
            vec![Properties::default()]
        );
        assert_eq!(
            fields(
                &catalog,
                "SELECT CAST(GROUPING(x) AS INT) FROM (VALUES(1)) q(x) GROUP BY ROLLUP(x)"
            ),
            vec![Properties {
                nullable: Some(true),
                origin: Origin::Derived
            }]
        );
        assert_eq!(
            fields(&catalog, "SELECT ISNULL(CAST(NULL AS INT),1)"),
            vec![Properties::default()]
        );
        let mut catalog = catalog;
        catalog.types.insert(
            "sql_variant".into(),
            msduck_core::catalog::TypeMetadata {
                system_type_id: Some(98),
                ..Default::default()
            },
        );
        assert_eq!(
            fields(&catalog, "SELECT CAST(1 AS SQL_VARIANT)"),
            vec![Properties {
                nullable: Some(true),
                origin: Origin::Derived
            }]
        );
    }
}
