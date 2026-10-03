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
