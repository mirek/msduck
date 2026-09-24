//! Parse-time MERGE validation, before any batch statements can execute.
use sqlparser::{
    ast::*,
    parser::{Parser, ParserError},
    tokenizer::Token,
};

const TERMINATOR: &str = "A MERGE statement must be terminated by a semi-colon (;).";

pub fn parse(parser: &mut Parser) -> Result<Statement, ParserError> {
    let token = parser.next_token();
    let merge = parser.parse_merge(token)?;
    finish(parser, &merge)?;
    Ok(Statement::Merge(merge))
}

pub fn finish(parser: &Parser, merge: &Merge) -> Result<(), ParserError> {
    // Leave the terminator for the enclosing batch/block parser. Comments and
    // whitespace are skipped by peek_token; punctuation inside strings is not.
    if parser.peek_token().token != Token::SemiColon {
        return Err(ParserError::ParserError(TERMINATOR.into()));
    }
    validate(merge).map_err(ParserError::ParserError)
}

fn family(kind: &MergeClauseKind) -> usize {
    match kind {
        MergeClauseKind::Matched => 0,
        MergeClauseKind::NotMatched | MergeClauseKind::NotMatchedByTarget => 1,
        MergeClauseKind::NotMatchedBySource => 2,
    }
}

fn validate(merge: &Merge) -> Result<(), String> {
    let labels = [
        "WHEN MATCHED",
        "WHEN NOT MATCHED",
        "WHEN NOT MATCHED BY SOURCE",
    ];
    let mut seen = [[false; 3]; 3];
    for clause in &merge.clauses {
        let (action, name) = match clause.action {
            MergeAction::Insert(_) => (0, "INSERT"),
            MergeAction::Update(_) => (1, "UPDATE"),
            MergeAction::Delete { .. } => (2, "DELETE"),
            MergeAction::DoNothing { .. } => {
                return Err("unsupported MERGE DO NOTHING action".into());
            }
        };
        let kind = family(&clause.clause_kind);
        if seen[kind][action] {
            return Err(format!(
                "An action of type '{name}' cannot appear more than once in a '{}' clause of a MERGE statement.",
                labels[kind]
            ));
        }
        seen[kind][action] = true;
    }
    let mut unconditional = [false; 3];
    for clause in &merge.clauses {
        let kind = family(&clause.clause_kind);
        if unconditional[kind] {
            return Err(format!(
                "In a MERGE statement, a '{}' clause with a search condition cannot appear after a '{}' clause with no search condition.",
                labels[kind], labels[kind]
            ));
        }
        unconditional[kind] = clause.predicate.is_none();
    }
    Ok(())
}

pub fn error_number(message: &str) -> Option<i32> {
    let message = message
        .strip_prefix("sql parser error: ")
        .unwrap_or(message);
    if message == TERMINATOR {
        Some(10713)
    } else if message.starts_with("An action of type '")
        && message.ends_with("clause of a MERGE statement.")
    {
        Some(10714)
    } else if message.starts_with("In a MERGE statement, a '")
        && message.ends_with("clause with no search condition.")
    {
        Some(5324)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn terminator_is_a_token_and_valid_arms_parse() {
        let head = "MERGE t USING s ON t.id=s.id ";
        for tail in [
            "WHEN MATCHED THEN DELETE;",
            "WHEN MATCHED AND s.id>0 THEN UPDATE SET n=1 WHEN MATCHED THEN DELETE;",
            "WHEN NOT MATCHED BY SOURCE AND t.id>0 THEN DELETE WHEN NOT MATCHED BY SOURCE THEN UPDATE SET n=1;",
            "WHEN NOT MATCHED THEN INSERT(n) VALUES (';') /* ; */ ;",
        ] {
            assert!(
                Parser::parse_sql(&crate::dialect::ServerDialect, &format!("{head}{tail}")).is_ok(),
                "{tail}"
            );
        }
        for tail in [
            "WHEN MATCHED THEN DELETE",
            "WHEN MATCHED THEN DELETE -- ;",
            "WHEN NOT MATCHED THEN INSERT(n) VALUES (';')",
        ] {
            let error = Parser::parse_sql(&crate::dialect::ServerDialect, &format!("{head}{tail}"))
                .unwrap_err();
            assert_eq!(error_number(&error.to_string()), Some(10713));
        }
    }
}
