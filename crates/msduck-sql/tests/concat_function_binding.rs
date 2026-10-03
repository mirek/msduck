use msduck_core::{
    catalog::TypeMetadata,
    character::{Family, Length},
    collation::Label,
    result::Properties,
};
use msduck_sql::{
    binding_scope::{Field, Scope, Source},
    catalog_snapshot::CatalogSnapshot,
    concat_ws::{Collation, Encoding},
    projection::concat_functions::{self as binding, Context, Error},
    temporal_guid_text::Language,
};
use sqlparser::{
    ast::{Expr, Query, SelectItem, SetExpr, Statement},
    parser::Parser,
};
use std::num::NonZeroUsize;
const CI: &str = "Latin1_General_100_CI_AS";
const CS: &str = "Latin1_General_100_CS_AS";
fn collations() -> Vec<Collation> {
    [CI, CS]
        .into_iter()
        .map(|name| Collation {
            name: name.into(),
            supplementary: false,
            case_sensitive: name == CS,
            encoding: Encoding::Cp1252,
        })
        .collect()
}
fn catalog() -> CatalogSnapshot {
    let mut c = CatalogSnapshot {
        default_collation: Some(CI.into()),
        ..Default::default()
    };
    for (name, id) in [
        ("varchar", 167),
        ("nvarchar", 231),
        ("char", 175),
        ("nchar", 239),
        ("int", 56),
        ("bigint", 127),
        ("binary", 173),
        ("varbinary", 165),
        ("xml", 241),
        ("sql_variant", 98),
    ] {
        c.types.insert(
            name.into(),
            TypeMetadata {
                system_type_id: Some(id),
                user_type_id: Some(i32::from(id)),
                max_length: Some(4),
                precision: Some(0),
                scale: Some(0),
                collation_name: Some(CI.into()),
            },
        );
    }
    c
}
fn query(sql: &str) -> Box<Query> {
    let statements = Parser::parse_sql(&msduck_sql::dialect::ServerDialect, sql).unwrap();
    let [Statement::Query(q)] = statements.as_slice() else {
        panic!("query")
    };
    q.clone()
}
fn expression(q: &Query) -> &Expr {
    let SetExpr::Select(s) = q.body.as_ref() else {
        panic!()
    };
    match &s.projection[0] {
        SelectItem::UnnamedExpr(e) | SelectItem::ExprWithAlias { expr: e, .. } => e,
        _ => panic!(),
    }
}
fn field(c: &CatalogSnapshot, name: &str) -> Field {
    Field {
        name: name.into(),
        info: c.types.get("varchar").cloned(),
        collation: Some(Ok(Label::Implicit(CI.into()))),
        properties: Properties::expression(false),
        json_fragment: false,
    }
}
#[test]
fn literal_and_typed_null_allocations_are_declaration_only() {
    let c = catalog();
    let collations = collations();
    let context = Context {
        collations: &collations,
        language: Language::UsEnglish,
    };
    for (sql, family, length, nullable) in [
        (
            "SELECT CONCAT_WS(NULL,NULL,NULL)",
            Family::Varchar,
            Length::Bounded(1),
            false,
        ),
        (
            "SELECT CONCAT_WS('', '', '')",
            Family::Varchar,
            Length::Bounded(3),
            false,
        ),
        (
            "SELECT CONCAT_WS(NULL,CAST(NULL AS NVARCHAR(10)),NULL)",
            Family::Nvarchar,
            Length::Bounded(10),
            false,
        ),
        (
            "SELECT TRANSLATE(CAST(NULL AS NVARCHAR(10)),N'A',N'Z')",
            Family::Nvarchar,
            Length::Bounded(4000),
            true,
        ),
    ] {
        let q = query(sql);
        let original = q.clone();
        let plans = binding::query(&c, &q, &Scope::default(), &context).unwrap();
        let plan = &plans[0].1;
        assert_eq!(
            plan.conversion().result().declaration.family(),
            family,
            "{sql}"
        );
        assert_eq!(
            plan.conversion().result().declaration.length(),
            length,
            "{sql}"
        );
        assert_eq!(plan.properties(), Properties::expression(nullable));
        assert_eq!(q, original);
        assert_eq!(
            plan.info().user_type_id,
            plan.info().system_type_id.map(i32::from)
        );
    }
}
#[test]
fn quoted_parameter_columns_ctes_derived_and_apply_share_scope_binding() {
    let mut c = catalog();
    c.tables
        .insert("t".into(), vec![field(&c, "@p"), field(&c, "s")]);
    let mut scope = Scope::default();
    let mut parameter = c.types["nvarchar"].clone();
    parameter.max_length = Some(-1);
    scope.parameters.insert("@p".into(), parameter);
    let col = collations();
    let context = Context {
        collations: &col,
        language: Language::UsEnglish,
    };
    for sql in [
        "SELECT CONCAT_WS(NULL,[@p],'x') FROM t",
        "WITH c AS (SELECT [@p] AS x FROM t) SELECT CONCAT_WS(NULL,c.x,'x') FROM c",
        "SELECT CONCAT_WS(NULL,d.x,'x') FROM (SELECT [@p] AS x FROM t) d",
        "SELECT CONCAT_WS(NULL,d.x,'x') FROM t CROSS APPLY (SELECT t.[@p] AS x) d",
    ] {
        let q = query(sql);
        let plans = binding::query(&c, &q, &scope, &context).unwrap();
        assert_eq!(
            plans[0].1.conversion().result().declaration.family(),
            Family::Varchar,
            "{sql}"
        );
        assert_eq!(
            plans[0].1.conversion().result().declaration.length(),
            Length::Bounded(5),
            "{sql}"
        );
    }
    let q = query("SELECT CONCAT_WS(NULL,@p,'x') FROM t");
    assert_eq!(
        binding::query(&c, &q, &scope, &context).unwrap()[0]
            .1
            .conversion()
            .result()
            .declaration
            .length(),
        Length::Max
    );
}
#[test]
fn borrowed_original_operands_remain_once_and_saved_plan_rejects_replacement() {
    let c = catalog();
    let cols = collations();
    let context = Context {
        collations: &cols,
        language: Language::UsEnglish,
    };
    let q = query(
        "SELECT CONCAT_WS(NULL,CAST(volatile_value() AS VARCHAR(10)) COLLATE Latin1_General_100_CI_AS,'x')",
    );
    let plans = binding::query(&c, &q, &Scope::default(), &context).unwrap();
    let plan = &plans[0].1;
    assert!(std::ptr::eq(plan.original(), expression(&q)));
    assert_eq!(plan.operands().len(), 3);
    assert_eq!(
        plan.original()
            .to_string()
            .matches("volatile_value()")
            .count(),
        1
    );
    plan.validate(expression(&q)).unwrap();
    assert_eq!(
        plan.validate(expression(&query("SELECT CONCAT_WS(NULL,'changed','x')"))),
        Err(Error::PlanMismatch)
    );
}
#[test]
fn unknown_shadow_and_collation_context_do_not_guess() {
    let c = catalog();
    let col = collations();
    let context = Context {
        collations: &col,
        language: Language::UsEnglish,
    };
    let q = query("SELECT CONCAT_WS(NULL,s,'x')");
    let mut scope = Scope::default();
    scope.rows.push(Some(vec![Source {
        qualifiers: vec!["outer".into()],
        fields: vec![field(&c, "s")],
    }]));
    assert!(
        binding::bind(
            &c,
            expression(&q),
            &scope,
            &context,
            NonZeroUsize::new(1).unwrap()
        )
        .unwrap()
        .is_some()
    );
    let mut unknown = field(&c, "s");
    unknown.info = None;
    scope.rows.push(Some(vec![Source {
        qualifiers: vec!["inner".into()],
        fields: vec![unknown],
    }]));
    assert!(matches!(
        binding::bind(
            &c,
            expression(&q),
            &scope,
            &context,
            NonZeroUsize::new(1).unwrap()
        ),
        Err(Error::UnknownOperand)
    ));
    let q = query("SELECT CONCAT_WS(NULL,'x','y')");
    let unknown = Context {
        collations: &[],
        language: Language::UsEnglish,
    };
    assert!(matches!(
        binding::query(&c, &q, &Scope::default(), &unknown),
        Err(Error::Function(_))
    ));
}

#[test]
fn function_results_propagate_through_cte_derived_apply_and_nested_calls() {
    let c = catalog();
    let col = collations();
    let context = Context {
        collations: &col,
        language: Language::UsEnglish,
    };
    for sql in [
        "WITH c AS (SELECT CONCAT_WS(NULL,CAST(NULL AS VARCHAR(4)),'x') AS v) SELECT CONCAT_WS(NULL,c.v,'y') FROM c",
        "SELECT CONCAT_WS(NULL,d.v,'y') FROM (SELECT CONCAT_WS(NULL,CAST(NULL AS VARCHAR(4)),'x') AS v) d",
        "SELECT CONCAT_WS(NULL,d.v,'y') FROM (SELECT CAST(NULL AS VARCHAR(4)) AS s) t CROSS APPLY (SELECT CONCAT_WS(NULL,t.s,'x') AS v) d",
        "SELECT CONCAT_WS(NULL,CONCAT_WS(NULL,CAST(NULL AS VARCHAR(4)),'x'),'y')",
    ] {
        let q = query(sql);
        let original = q.clone();
        let plans = binding::query(&c, &q, &Scope::default(), &context).unwrap();
        assert_eq!(
            plans[0].1.conversion().result().declaration.length(),
            Length::Bounded(6),
            "{sql}"
        );
        let fields = binding::fields(&c, &q, &Scope::default(), &context).unwrap();
        assert_eq!(fields[0].info.as_ref().unwrap().max_length, Some(6));
        assert_eq!(fields[0].properties, Properties::expression(false));
        assert_eq!(q, original);
    }
    let q = query(
        "WITH c AS (SELECT TRANSLATE(CAST(NULL AS NVARCHAR(10)),N'A',N'Z') AS v) SELECT c.v FROM c WHERE 1=0",
    );
    let fields = binding::fields(&c, &q, &Scope::default(), &context).unwrap();
    assert_eq!(fields[0].info.as_ref().unwrap().max_length, Some(8000));
    assert_eq!(fields[0].properties.nullable, Some(true));
    assert_eq!(
        fields[0].properties.origin,
        msduck_core::result::Origin::Derived
    );
}
#[test]
fn real_select_position_and_original_diagnostic_precedence_are_retained() {
    let mut c = catalog();
    let mut a = field(&c, "a");
    a.collation = Some(Ok(Label::Implicit(CI.into())));
    let mut b = field(&c, "b");
    b.collation = Some(Ok(Label::Implicit(CS.into())));
    c.tables.insert("t".into(), vec![a, b]);
    let cols = collations();
    let context = Context {
        collations: &cols,
        language: Language::UsEnglish,
    };
    let q = query("SELECT t.*,CONCAT_WS(NULL,a,b) FROM t");
    let error = match binding::query(&c, &q, &Scope::default(), &context) {
        Err(Error::Function(msduck_sql::concat_conversion::Error::Function(
            msduck_sql::concat_ws::Error::Sql(error),
        ))) => error,
        _ => panic!("expected diagnostic"),
    };
    assert_eq!((error.number, error.state), (451, 1));
    assert!(error.message.contains("column 3"), "{}", error.message);
    let q = query("SELECT TRANSLATE(unknown)");
    let error = match binding::query(&c, &q, &Scope::default(), &context) {
        Err(Error::Function(msduck_sql::concat_conversion::Error::Function(
            msduck_sql::concat_ws::Error::Sql(error),
        ))) => error,
        _ => panic!("expected arity"),
    };
    assert_eq!(error.number, 174);
    let q = query("SELECT CONCAT_WS(DISTINCT NULL,'a','b')");
    assert!(matches!(
        binding::query(&c, &q, &Scope::default(), &context),
        Err(Error::UnsupportedSyntax)
    ));
}

fn reference_catalog() -> (CatalogSnapshot, Vec<Collation>) {
    let mut c = catalog();
    c.default_collation = Some("SQL_Latin1_General_CP1_CI_AS".into());
    for (name, id) in [
        ("tinyint", 48),
        ("smallint", 52),
        ("bit", 104),
        ("real", 59),
        ("float", 62),
        ("decimal", 106),
        ("numeric", 108),
        ("money", 60),
        ("smallmoney", 122),
        ("date", 40),
        ("time", 41),
        ("datetime2", 42),
        ("datetimeoffset", 43),
        ("datetime", 61),
        ("smalldatetime", 58),
        ("uniqueidentifier", 36),
        ("text", 35),
        ("ntext", 99),
        ("image", 34),
    ] {
        c.types.insert(
            name.into(),
            TypeMetadata {
                system_type_id: Some(id),
                user_type_id: Some(i32::from(id)),
                max_length: Some(4),
                precision: Some(18),
                scale: Some(0),
                collation_name: None,
            },
        );
    }
    let properties = [
        (
            "SQL_Latin1_General_CP1_CI_AS",
            false,
            false,
            Encoding::Cp1252,
        ),
        (CI, false, false, Encoding::Cp1252),
        (CS, false, true, Encoding::Cp1252),
        ("Latin1_General_100_BIN2", false, true, Encoding::Cp1252),
        ("Latin1_General_100_CI_AS_SC", true, false, Encoding::Cp1252),
        (
            "Latin1_General_100_CI_AS_SC_UTF8",
            true,
            false,
            Encoding::Utf8,
        ),
    ];
    let cols = properties
        .into_iter()
        .map(
            |(name, supplementary, case_sensitive, encoding)| Collation {
                name: name.into(),
                supplementary,
                case_sensitive,
                encoding,
            },
        )
        .collect();
    let mut fields = Vec::new();
    for (name, kind, label) in [
        ("id", "INT", None),
        ("sep", "NVARCHAR(3)", None),
        ("a", "VARCHAR(10)", None),
        ("u", "NVARCHAR(10)", None),
        ("n", "INT", None),
        ("bin", "VARCHAR(5)", Some("Latin1_General_100_BIN2")),
        ("cs", "VARCHAR(5)", Some(CS)),
    ] {
        let q = query(&format!("SELECT CAST(NULL AS {kind})"));
        let Expr::Cast { data_type, .. } = expression(&q) else {
            panic!()
        };
        let info = c.cast_info(data_type).unwrap();
        let character = matches!(info.system_type_id, Some(167 | 231));
        fields.push(Field {
            name: name.into(),
            info: Some(info),
            collation: character.then(|| {
                Ok(Label::Implicit(
                    label.unwrap_or("SQL_Latin1_General_CP1_CI_AS").into(),
                ))
            }),
            properties: Properties {
                nullable: Some(true),
                origin: msduck_core::result::Origin::Stored,
            },
            json_fragment: false,
        });
    }
    c.tables.insert("dbo.cwt_src".into(), fields.clone());
    c.tables.insert("cwt_src".into(), fields);
    (c, cols)
}
fn prepared_scope(c: &CatalogSnapshot, record: &serde_json::Value) -> Scope {
    let mut scope = Scope::default();
    for declaration in record
        .get("declarations")
        .or_else(|| record.get("parameters"))
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
    {
        let name = declaration["type"].as_str().unwrap();
        let key = match name {
            "NVarChar" => "nvarchar",
            "VarChar" => "varchar",
            "NChar" => "nchar",
            "Char" => "char",
            "VarBinary" => "varbinary",
            "Binary" => "binary",
            "UniqueIdentifier" => "uniqueidentifier",
            other => other,
        };
        let Some(mut info) = c.types.get(&key.to_ascii_lowercase()).cloned() else {
            continue;
        };
        if let Some(n) = declaration["options"]["length"].as_i64() {
            let unicode = matches!(info.system_type_id, Some(231 | 239));
            let varying = matches!(info.system_type_id, Some(167 | 231 | 165));
            // Explicit declaration options above the bounded type cap generate
            // SQL MAX; this never consults the current parameter value.
            info.max_length = Some(if varying && n > if unicode { 4000 } else { 8000 } {
                -1
            } else {
                i16::try_from(n * if unicode { 2 } else { 1 }).unwrap()
            });
        } else if matches!(info.system_type_id, Some(167 | 231 | 165))
            && declaration["options"].get("length").is_some()
        {
            info.max_length = Some(-1);
        }
        if matches!(info.system_type_id, Some(167 | 175 | 231 | 239 | 165 | 173))
            && declaration["options"].get("length").is_none()
        {
            // Old RPC observer has no emitted TYPEINFO for inferred lengths.
            // Never recover compile declarations from the current bound value.
            info.max_length = None;
        }
        if let Some(p) = declaration["options"]["precision"].as_u64() {
            info.precision = Some(p as u8)
        }
        if let Some(s) = declaration["options"]["scale"].as_u64() {
            info.scale = Some(s as u8)
        }
        scope
            .parameters
            .insert(format!("@{}", declaration["name"].as_str().unwrap()), info);
    }
    scope
}
#[test]
fn retained_four_run_compile_descriptors_and_diagnostics_replay_without_values() {
    let (c, cols) = reference_catalog();
    let context = Context {
        collations: &cols,
        language: Language::UsEnglish,
    };
    let mut known = 0;
    let mut diagnostics = 0;
    let mut unknown = std::collections::BTreeMap::new();
    for (fixture, bytes) in [
        (
            "807",
            include_str!("../../../reference/concat-ws-translate.json"),
        ),
        (
            "815",
            include_str!("../../../reference/concat-text-conversion.json"),
        ),
        (
            "833",
            include_str!("../../../reference/concat-legacy-family.json"),
        ),
    ] {
        // Lossless typed carrier for the four exact isolated UTF16 results;
        // no row/error/SQL/descriptor is normalized or removed.
        let carrier;
        let bytes = if fixture == "807" {
            let raw = r#""\ud83d-a""#;
            assert_eq!(bytes.matches(raw).count(), 4);
            carrier = bytes.replace(raw, r#"{"utf16":[55357,45,97]}"#);
            carrier.as_str()
        } else {
            bytes
        };
        let reference: serde_json::Value = serde_json::from_str(bytes).unwrap();
        for container in reference["containers"].as_array().unwrap() {
            for run in container["runs"].as_array().unwrap() {
                for record in run.as_array().unwrap() {
                    let Some(sql) = record["sql"].as_str() else {
                        continue;
                    };
                    let Ok(statements) =
                        Parser::parse_sql(&msduck_sql::dialect::ServerDialect, sql)
                    else {
                        *unknown.entry(format!("{fixture} parser")).or_insert(0) += 1;
                        continue;
                    };
                    let [Statement::Query(q)] = statements.as_slice() else {
                        continue;
                    };
                    let scope = prepared_scope(&c, record);
                    let plans = match binding::query(&c, q, &scope, &context) {
                        Ok(plans) => plans,
                        Err(Error::Function(msduck_sql::concat_conversion::Error::Function(
                            msduck_sql::concat_ws::Error::Sql(error),
                        ))) => {
                            let expected = &record["result"]["errors"][0];
                            assert!(
                                !expected.is_null(),
                                "unexpected compile diagnostic {error:?}: {fixture} {}",
                                record["name"]
                            );
                            assert_eq!(
                                expected["number"], error.number,
                                "{} {}",
                                fixture, record["name"]
                            );
                            assert_eq!(
                                expected["state"], error.state,
                                "{} {}",
                                fixture, record["name"]
                            );
                            assert_eq!(
                                expected["message"], error.message,
                                "{} {}",
                                fixture, record["name"]
                            );
                            diagnostics += 1;
                            continue;
                        }
                        Err(error) => {
                            *unknown.entry(format!("{fixture} {error:?}")).or_insert(0) += 1;
                            continue;
                        }
                    };
                    for (position, plan) in plans {
                        let check = |column: &serde_json::Value| {
                            let result = plan.conversion().result();
                            let unicode = matches!(result.declaration.family(), Family::Nvarchar);
                            assert_eq!(
                                column["type"],
                                if unicode { "NVarChar" } else { "VarChar" },
                                "{} {}",
                                fixture,
                                record["name"]
                            );
                            let length = match result.declaration.length() {
                                Length::Max => 65535,
                                Length::Bounded(n) => u64::from(n) * if unicode { 2 } else { 1 },
                            };
                            assert_eq!(column["length"], length, "{} {}", fixture, record["name"]);
                            assert_eq!(
                                column["flags"], result.flags,
                                "{} {}",
                                fixture, record["name"]
                            );
                            assert_eq!(plan.properties().nullable, Some(result.flags & 1 != 0));
                        };
                        if let Some(column) =
                            record["result"]["sets"][0]["columns"].get(position.get() - 1)
                        {
                            check(column);
                            known += 1
                        }
                        if let Some(executions) = record["prepared"]["executions"].as_array() {
                            for execution in executions {
                                if let Some(column) = execution["result"]["sets"][0]["columns"]
                                    .get(position.get() - 1)
                                {
                                    check(column);
                                    known += 1
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    eprintln!("known {known} diagnostics {diagnostics} unknown {unknown:?}");
    assert_eq!((known, diagnostics), (3632, 1212));
    assert_eq!(
        unknown,
        std::collections::BTreeMap::from([("807 UnknownOperand".into(), 4)])
    );
}

#[test]
fn correlated_scalar_scopes_wrapped_diagnostics_and_alias_identity_remain_explicit() {
    let mut c = catalog();
    c.tables.insert("t".into(), vec![field(&c, "@p")]);
    c.types.get_mut("varchar").unwrap().user_type_id = Some(1001);
    let cols = collations();
    let context = Context {
        collations: &cols,
        language: Language::UsEnglish,
    };
    let q = query("SELECT CONCAT_WS(NULL,(SELECT CONCAT_WS(NULL,t.[@p],'x')),'y') FROM t");
    let plans = binding::query(&c, &q, &Scope::default(), &context).unwrap();
    assert_eq!(plans[0].1.info().max_length, Some(6));
    assert_eq!(plans[0].1.info().user_type_id, Some(167));
    let q =
        query("SELECT CONCAT_WS(NULL,(SELECT CONCAT_WS(NULL,t.[@p],'x') FROM missing),'y') FROM t");
    assert!(matches!(
        binding::query(&c, &q, &Scope::default(), &context),
        Err(Error::UnknownOperand)
    ));
    let q = query("SELECT LEN(CONCAT_WS('x'))");
    let Err(Error::Function(msduck_sql::concat_conversion::Error::Function(
        msduck_sql::concat_ws::Error::Sql(error),
    ))) = binding::query(&c, &q, &Scope::default(), &context)
    else {
        panic!("nested arity")
    };
    assert_eq!(error.number, 189);
    let q = query("SELECT LEN(CONCAT_WS(NULL,missing,'x'))");
    assert!(matches!(
        binding::query(&c, &q, &Scope::default(), &context),
        Err(Error::UnknownOperand)
    ));
    // Ordinary inference remains unchanged, even when the function context is unavailable.
    let q = query("SELECT CAST(NULL AS VARCHAR(4))");
    let empty = Context {
        collations: &[],
        language: Language::Unknown,
    };
    let ordinary = msduck_sql::projection::query_fields(&c, &q, &Scope::default()).unwrap();
    let bound = binding::fields(&c, &q, &Scope::default(), &empty).unwrap();
    assert_eq!(ordinary[0].info, bound[0].info);
    assert_eq!(ordinary[0].properties, bound[0].properties);
}
#[test]
fn invalid_deep_ast_and_modifiers_are_rejected_without_annotations() {
    let c = catalog();
    let cols = collations();
    let context = Context {
        collations: &cols,
        language: Language::UsEnglish,
    };
    let q = query("SELECT CONCAT_WS(NULL,'a','b')");
    let mut deep = expression(&q).clone();
    for _ in 0..65 {
        deep = Expr::Nested(Box::new(deep));
    }
    assert!(matches!(
        binding::bind(
            &c,
            &deep,
            &Scope::default(),
            &context,
            NonZeroUsize::new(1).unwrap()
        ),
        Err(Error::UnsupportedSyntax)
    ));
    for sql in [
        "SELECT CONCAT_WS(NULL,'a','b') OVER ()",
        "SELECT TRANSLATE(DISTINCT 'a','a','b')",
    ] {
        let q = query(sql);
        assert!(matches!(
            binding::query(&c, &q, &Scope::default(), &context),
            Err(Error::UnsupportedSyntax)
        ));
    }
}

#[test]
fn function_result_wildcards_retain_grouping_set_null_extension() {
    let c = catalog();
    let cols = collations();
    let context = Context {
        collations: &cols,
        language: Language::UsEnglish,
    };
    for select in ["*", "c.*", "c.v"] {
        let q = query(&format!(
            "WITH c AS (SELECT CONCAT_WS(NULL,'a','b') AS v) SELECT {select} FROM c GROUP BY GROUPING SETS ((v),())"
        ));
        let fields = binding::fields(&c, &q, &Scope::default(), &context).unwrap();
        assert_eq!(fields[0].properties.nullable, Some(true));
        assert_eq!(fields[0].info.as_ref().unwrap().max_length, Some(2));
    }
}
