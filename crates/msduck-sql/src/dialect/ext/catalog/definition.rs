//! Catalog text is a logical SQL Server declaration, not backend SQL.
use sqlparser::ast::{Expr, Value};

/// Integer defaults use SQL Server's two enclosing parentheses. Other
/// expression families remain unknown until their serialization is verified;
/// never expose native SQL or parser annotations as a SQL Server definition.
pub fn literal_default_definition(expr: &Expr) -> Option<String> {
    let expr = match expr {
        Expr::Nested(inner) => return literal_default_definition(inner),
        expr => expr,
    };
    let Expr::Value(value) = expr else {
        return None;
    };
    let Value::Number(number, false) = &value.value else {
        return None;
    };
    if number.is_empty() || !number.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let number = number.parse::<i128>().ok()?;
    Some(format!("(({number}))"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlparser::{ast::Statement, parser::Parser};

    #[test]
    fn integer_default_matches_retained_sql_server_definition() {
        let statements = Parser::parse_sql(
            &crate::dialect::ServerDialect,
            "CREATE TABLE dbo.t(state BIT CONSTRAINT DF_t_state DEFAULT(1))",
        )
        .unwrap();
        let Statement::CreateTable(table) = &statements[0] else {
            panic!()
        };
        let sqlparser::ast::ColumnOption::Default(expr) = &table.columns[0].options[0].option
        else {
            panic!()
        };
        assert_eq!(literal_default_definition(expr), Some("((1))".into()));
        assert_eq!(
            literal_default_definition(&Expr::Value(Value::Number("1.25".into(), false).into())),
            None
        );
        assert_eq!(
            literal_default_definition(&Expr::Identifier(sqlparser::ast::Ident::new("x"))),
            None
        );
    }
}
