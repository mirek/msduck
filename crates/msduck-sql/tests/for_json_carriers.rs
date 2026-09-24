use msduck_core::for_json::Options;
use msduck_sql::{dialect::ServerDialect, for_json};
use sqlparser::{ast::*, parser::Parser};

fn source(sql: &str) -> Box<Query> {
    let Statement::Query(query) = Parser::parse_sql(&ServerDialect, sql).unwrap().remove(0) else {
        panic!("query")
    };
    query
}

#[test]
fn list_lowering_keeps_order_limits_ctes_and_alias_collision_boundaries() {
    for sql in [
        "SELECT TOP (3) n AS n FROM t ORDER BY n DESC",
        "WITH q AS (SELECT n FROM t) SELECT n AS n FROM q ORDER BY n OFFSET 2 ROWS FETCH NEXT 3 ROWS ONLY",
        "SELECT __msduck_json_source.n AS n FROM t AS __msduck_json_source ORDER BY n",
    ] {
        let mut query = source(sql);
        let original = query.clone();
        for_json::lower(
            &mut query,
            vec!["n".into()],
            "spec".into(),
            &Options::default(),
        )
        .unwrap();
        let SetExpr::Select(select) = query.body.as_ref() else {
            panic!("select")
        };
        let TableFactor::Derived {
            subquery, alias, ..
        } = &select.from[0].relation
        else {
            panic!("derived")
        };
        assert_eq!(subquery, &original);
        if sql.contains("AS __msduck_json_source") {
            assert_eq!(alias.as_ref().unwrap().name.value, "__msduck_json_source_");
        }
        let mut functions = Vec::new();
        struct Calls<'a>(&'a mut Vec<String>);
        impl Visitor for Calls<'_> {
            type Break = ();
            fn pre_visit_expr(&mut self, expr: &Expr) -> std::ops::ControlFlow<()> {
                if let Expr::Function(f) = expr {
                    self.0.push(f.name.to_string());
                }
                std::ops::ControlFlow::Continue(())
            }
        }
        let _ = query.visit(&mut Calls(&mut functions));
        assert_eq!(
            functions
                .iter()
                .filter(|name| name.as_str() == "__msduck_json_row")
                .count(),
            1
        );
        assert_eq!(
            functions
                .iter()
                .filter(|name| name.as_str() == "list")
                .count(),
            1
        );
        assert!(!functions.iter().any(|name| name == "string_agg"));
        assert!(query.to_string().contains(", [])"));
    }
}

#[test]
fn list_wrappers_preserve_options_and_fragment_markers() {
    for (root, without, name, fragment) in [
        (None, false, "__msduck_json_array", true),
        (Some("r/雪"), false, "__msduck_json_array", true),
        (None, true, "__msduck_json_unwrapped", false),
    ] {
        let mut query = source("SELECT 1 AS n WHERE 1=0");
        for_json::lower(
            &mut query,
            vec!["n".into()],
            "spec".into(),
            &Options {
                root: root.map(str::to_owned),
                without_array_wrapper: without,
                ..Options::default()
            },
        )
        .unwrap();
        assert!(
            query
                .to_string()
                .starts_with(&format!("SELECT {name}(coalesce(list("))
        );
        assert!(query.to_string().contains(&format!(
            ", '{}')",
            root.map(|r| format!("1{r}")).unwrap_or_else(|| "0".into())
        )));
        assert_eq!(for_json::fragment(&Expr::Subquery(query)), fragment);
    }
}
