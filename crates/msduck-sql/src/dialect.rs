//! SQL Server parser extensions absent from sqlparser's built-in dialect.
use sqlparser::{
    ast::{helpers::attached_token::AttachedToken, *},
    dialect::{Dialect, MsSqlDialect},
    keywords::Keyword,
    parser::{Parser, ParserError},
    tokenizer::{Token, TokenWithSpan},
};

#[derive(Debug)]
pub struct ServerDialect;

/// sqlparser recognizes PostgreSQL's `<@` and `^@` before checking the dialect.
/// In T-SQL these are operators followed by variable identifiers.
pub fn tokenize(sql: &str) -> Result<Vec<TokenWithSpan>, ParserError> {
    let tokens =
        sqlparser::tokenizer::Tokenizer::new(&ServerDialect, sql).tokenize_with_location()?;
    let mut tokens = tokens.into_iter();
    let mut result = Vec::new();
    while let Some(mut token) = tokens.next() {
        if matches!(token.token, Token::ArrowAt | Token::CaretAt) {
            let operator = if token.token == Token::ArrowAt {
                Token::Lt
            } else {
                Token::Caret
            };
            let Some(mut variable) = tokens.next() else {
                return Err(ParserError::ParserError("missing variable after <@".into()));
            };
            let Token::Word(word) = &mut variable.token else {
                return Err(ParserError::ParserError("invalid variable after <@".into()));
            };
            if word.quote_style.is_some() || token.span.end != variable.span.start {
                return Err(ParserError::ParserError("invalid variable after <@".into()));
            }
            word.value.insert(0, '@');
            word.keyword = Keyword::NoKeyword;
            token.token = operator;
            result.push(token);
            result.push(variable);
        } else {
            result.push(token);
        }
    }
    crate::openjson_path::path_tokens(&mut result);
    Ok(result)
}
macro_rules! forward_flags {
    ($($name:ident),* $(,)?) => { $(fn $name(&self) -> bool { MsSqlDialect {}.$name() })* };
}
fn control_token(token: &Token) -> Option<bool> {
    match token {
        Token::Word(word)
            if word.quote_style.is_none() && word.value.eq_ignore_ascii_case("BREAK") =>
        {
            Some(false)
        }
        Token::Word(word)
            if word.quote_style.is_none() && word.value.eq_ignore_ascii_case("CONTINUE") =>
        {
            Some(true)
        }
        _ => None,
    }
}

fn negated_comparison(parser: &Parser) -> Option<BinaryOperator> {
    let first = parser.peek_token();
    let second = parser.peek_nth_token(1);
    if first.token != Token::ExclamationMark || first.span.end != second.span.start {
        return None;
    }
    match second.token {
        Token::Lt => Some(BinaryOperator::GtEq),
        Token::Gt => Some(BinaryOperator::LtEq),
        _ => None,
    }
}
// These internal placeholders cannot be produced by the T-SQL tokenizer. Using
// existing AST nodes keeps sqlparser's recursive visitors and IF parser intact.
pub fn loop_control(statement: &Statement) -> Option<bool> {
    if let Statement::Return(ReturnStatement {
        value: Some(ReturnStatementValue::Expr(Expr::Value(value))),
    }) = statement
        && let Value::Placeholder(marker) = &value.value
    {
        return match marker.as_str() {
            "msduck:break" => Some(false),
            "msduck:continue" => Some(true),
            _ => None,
        };
    }
    None
}
pub fn is_with_values(option: &ColumnOption) -> bool {
    matches!(option, ColumnOption::DialectSpecific(tokens)
        if matches!(tokens.as_slice(), [Token::Word(a), Token::Word(b)]
            if a.keyword == Keyword::WITH && b.keyword == Keyword::VALUES))
}
impl Dialect for ServerDialect {
    fn supports_group_by_with_modifier(&self) -> bool {
        true
    }
    fn supports_group_by_expr(&self) -> bool {
        true
    }
    fn parse_prefix(&self, parser: &mut Parser) -> Option<Result<Expr, ParserError>> {
        // A scalar query can begin with a parenthesized set-operation branch.
        // The default prefix parser only recognizes an immediate SELECT.
        if parser.peek_token().token == Token::LParen
            && parser.peek_nth_token(1).token == Token::LParen
        {
            match parser.maybe_parse(|parser| {
                parser.expect_token(&Token::LParen)?;
                let query = parser.parse_query()?;
                parser.expect_token(&Token::RParen)?;
                Ok(Expr::Subquery(query))
            }) {
                Ok(Some(expr)) => return Some(Ok(expr)),
                Err(error) => return Some(Err(error)),
                Ok(None) => {}
            }
        }
        if matches!(parser.peek_token().token, Token::Word(w) if w.quote_style.is_none() && w.value.starts_with('@') && !w.value.starts_with("@@"))
            && parser.peek_nth_token(2).token == Token::Eq
        {
            let name = match parser.peek_nth_token(1).token {
                Token::Plus => "add",
                Token::Minus => "sub",
                Token::Mul => "mul",
                Token::Div => "div",
                Token::Mod => "mod",
                Token::Ampersand => "and",
                Token::Pipe => "or",
                Token::Caret => "xor",
                _ => return None,
            };
            return Some((|| {
                let variable = parser.parse_identifier()?;
                parser.next_token();
                parser.expect_token(&Token::Eq)?;
                let right = parser.parse_expr()?;
                Ok(Expr::BinaryOp {
                    left: Box::new(Expr::Identifier(variable)),
                    op: BinaryOperator::Custom(format!("msduck:select-{name}")),
                    right: Box::new(right),
                })
            })());
        }
        MsSqlDialect {}.parse_prefix(parser)
    }
    fn parse_column_option(
        &self,
        parser: &mut Parser,
    ) -> Result<Option<Result<Option<ColumnOption>, ParserError>>, ParserError> {
        if parser.peek_keyword(Keyword::WITH)
            && matches!(parser.peek_nth_token(1).token, Token::Word(w) if w.keyword == Keyword::VALUES)
        {
            let tokens = vec![parser.next_token().token, parser.next_token().token];
            return Ok(Some(Ok(Some(ColumnOption::DialectSpecific(tokens)))));
        }
        MsSqlDialect {}.parse_column_option(parser)
    }

    fn dialect(&self) -> std::any::TypeId {
        MsSqlDialect {}.dialect()
    }
    fn is_delimited_identifier_start(&self, ch: char) -> bool {
        MsSqlDialect {}.is_delimited_identifier_start(ch)
    }
    fn is_identifier_start(&self, ch: char) -> bool {
        MsSqlDialect {}.is_identifier_start(ch)
    }
    fn is_identifier_part(&self, ch: char) -> bool {
        MsSqlDialect {}.is_identifier_part(ch)
    }
    fn identifier_quote_style(&self, identifier: &str) -> Option<char> {
        MsSqlDialect {}.identifier_quote_style(identifier)
    }
    forward_flags!(
        convert_type_before_value,
        supports_outer_join_operator,
        supports_dollar_as_money_prefix,
        supports_connect_by,
        supports_eq_alias_assignment,
        supports_try_convert,
        supports_boolean_literals,
        supports_named_fn_args_with_colon_operator,
        supports_named_fn_args_with_expr_name,
        supports_named_fn_args_with_rarrow_operator,
        supports_start_transaction_modifier,
        supports_end_transaction_modifier,
        supports_set_stmt_without_operator,
        supports_table_versioning,
        supports_nested_comments,
        supports_object_name_double_dot_notation
    );
    fn get_reserved_grantees_types(&self) -> &[GranteesType] {
        MsSqlDialect {}.get_reserved_grantees_types()
    }
    fn is_select_item_alias(&self, explicit: bool, keyword: &Keyword, parser: &mut Parser) -> bool {
        if !explicit && matches!(keyword, Keyword::FOR | Keyword::OPTION) {
            return false;
        }
        // The alias candidate has already been consumed. WINDOW is also a
        // usable identifier, so recognize a definition by the following name
        // and AS rather than reserving it everywhere.
        if !explicit
            && *keyword == Keyword::WINDOW
            && matches!(parser.peek_token_ref().token, Token::Word(_))
            && matches!(parser.peek_nth_token(1).token, Token::Word(word) if word.keyword == Keyword::AS)
        {
            return false;
        }
        !matches!(
            keyword,
            Keyword::SET | Keyword::BEGIN | Keyword::END | Keyword::CONTINUE
        ) && control_token(&parser.peek_token_ref().token).is_none()
            && MsSqlDialect {}.is_select_item_alias(explicit, keyword, parser)
    }
    fn is_table_factor_alias(
        &self,
        explicit: bool,
        keyword: &Keyword,
        parser: &mut Parser,
    ) -> bool {
        if !explicit && matches!(keyword, Keyword::FOR | Keyword::OPTION) {
            return false;
        }
        !matches!(
            keyword,
            Keyword::SET | Keyword::BEGIN | Keyword::END | Keyword::CONTINUE
        ) && control_token(&parser.peek_token_ref().token).is_none()
            && MsSqlDialect {}.is_table_factor_alias(explicit, keyword, parser)
    }
    fn get_next_precedence(&self, parser: &Parser) -> Option<Result<u8, ParserError>> {
        if negated_comparison(parser).is_some() {
            return Some(Ok(self.prec_value(sqlparser::dialect::Precedence::Eq)));
        }
        MsSqlDialect {}.get_next_precedence(parser)
    }
    fn parse_infix(
        &self,
        parser: &mut Parser,
        expr: &Expr,
        precedence: u8,
    ) -> Option<Result<Expr, ParserError>> {
        let op = negated_comparison(parser)?;
        Some((|| {
            parser.next_token();
            parser.next_token();
            Ok(Expr::BinaryOp {
                left: Box::new(expr.clone()),
                op,
                right: Box::new(parser.parse_subexpr(precedence)?),
            })
        })())
    }
    fn parse_statement(&self, parser: &mut Parser) -> Option<Result<Statement, ParserError>> {
        if crate::raiserror::starts(parser) {
            return Some(crate::raiserror::parse(parser));
        }
        if parser.peek_keyword(Keyword::MERGE) {
            return Some(crate::merge::parse(parser));
        }
        if parser.peek_keyword(Keyword::DELETE) {
            return Some(parse_delete_statement(parser));
        }
        if parser.peek_keyword(Keyword::UPDATE) {
            return Some(parse_update_assignments(parser));
        }
        if parser.peek_keyword(Keyword::WITH)
            && let Ok(statement) = parser.try_parse(parse_cte_dml)
        {
            return Some((|| {
                let Statement::Query(mut query) = statement else {
                    unreachable!()
                };
                crate::query_options::parse(parser, &mut query)?;
                Ok(Statement::Query(query))
            })());
        }
        if parser.peek_keyword(Keyword::WITH) || parser.peek_keyword(Keyword::SELECT) {
            return Some(parse_cte_query(parser));
        }
        if parser.peek_keyword(Keyword::ALTER)
            && let Ok(statement) = parser.try_parse(parse_tsql_alter_column)
        {
            return Some(Ok(statement));
        }

        if parser.peek_keyword(Keyword::ALTER)
            && let Ok(statement) = parser.try_parse(parse_tsql_add_columns)
        {
            return Some(Ok(statement));
        }

        if parser.peek_keyword(Keyword::ALTER)
            && let Ok(statement) = parser.try_parse(parse_tsql_drop_columns)
        {
            return Some(Ok(statement));
        }

        if parser.peek_keyword(Keyword::BEGIN)
            && matches!(parser.peek_nth_token(1).token, Token::Word(word) if word.keyword == Keyword::TRY)
        {
            return Some(parse_try_catch(parser));
        }
        if parser.peek_keyword(Keyword::SET)
            && matches!(parser.peek_nth_token(1).token, Token::Word(word) if word.value.starts_with('@'))
            && parser.peek_nth_token(3).token == Token::Eq
        {
            let op = match parser.peek_nth_token(2).token {
                Token::Plus => Some(BinaryOperator::Plus),
                Token::Minus => Some(BinaryOperator::Minus),
                Token::Mul => Some(BinaryOperator::Multiply),
                Token::Div => Some(BinaryOperator::Divide),
                Token::Mod => Some(BinaryOperator::Modulo),
                Token::Ampersand => Some(BinaryOperator::BitwiseAnd),
                Token::Pipe => Some(BinaryOperator::BitwiseOr),
                Token::Caret => Some(BinaryOperator::BitwiseXor),
                _ => None,
            };
            if let Some(op) = op {
                return Some(parse_compound_set(parser, op));
            }
        }
        if let Some(is_continue) = control_token(&parser.peek_token_ref().token) {
            parser.next_token();
            let marker = if is_continue {
                "msduck:continue"
            } else {
                "msduck:break"
            };
            return Some(Ok(Statement::Return(ReturnStatement {
                value: Some(ReturnStatementValue::Expr(Expr::Value(
                    Value::Placeholder(marker.into()).into(),
                ))),
            })));
        }
        if parser.peek_keyword(Keyword::RETURN)
            && matches!(parser.peek_nth_token(1).token, Token::Word(word) if matches!(word.keyword, Keyword::END | Keyword::ELSE))
        {
            parser.next_token();
            return Some(Ok(Statement::Return(ReturnStatement { value: None })));
        }
        if parser.peek_keyword(Keyword::BEGIN) {
            let next = parser.peek_nth_token(1);
            let transaction = match next.token {
                Token::SemiColon | Token::EOF => true,
                Token::Word(word) => {
                    word.value.eq_ignore_ascii_case("DISTRIBUTED")
                        || matches!(
                            word.keyword,
                            Keyword::TRAN
                                | Keyword::TRANSACTION
                                | Keyword::WORK
                                | Keyword::TRY
                                | Keyword::CATCH
                        )
                }
                _ => false,
            };
            if !transaction {
                return Some(parse_block(parser));
            }
        }
        if parser.peek_keyword(Keyword::WHILE) {
            return Some(parse_while(parser));
        }
        MsSqlDialect {}.parse_statement(parser)
    }
}

/// Consume compound assignment markers only in SELECT projection positions.
/// Markers elsewhere are syntax errors, never ordinary comparison expressions.
pub fn canonicalize_select_assignments(statement: &mut Statement) -> Result<(), String> {
    use std::ops::ControlFlow;
    struct Assignments;
    impl VisitorMut for Assignments {
        type Break = String;
        fn pre_visit_select(&mut self, select: &mut Select) -> ControlFlow<String> {
            for item in &mut select.projection {
                let SelectItem::UnnamedExpr(Expr::BinaryOp {
                    left,
                    op: BinaryOperator::Custom(marker),
                    right,
                }) = item
                else {
                    continue;
                };
                let op = match marker.as_str() {
                    "msduck:select-add" => BinaryOperator::Plus,
                    "msduck:select-sub" => BinaryOperator::Minus,
                    "msduck:select-mul" => BinaryOperator::Multiply,
                    "msduck:select-div" => BinaryOperator::Divide,
                    "msduck:select-mod" => BinaryOperator::Modulo,
                    "msduck:select-and" => BinaryOperator::BitwiseAnd,
                    "msduck:select-or" => BinaryOperator::BitwiseOr,
                    "msduck:select-xor" => BinaryOperator::BitwiseXor,
                    _ => continue,
                };
                let Expr::Identifier(alias) = left.as_ref() else {
                    continue;
                };
                *item = SelectItem::ExprWithAlias {
                    alias: alias.clone(),
                    expr: Expr::BinaryOp {
                        left: left.clone(),
                        op,
                        right: Box::new(Expr::Nested(right.clone())),
                    },
                };
            }
            ControlFlow::Continue(())
        }
        fn pre_visit_expr(&mut self, expr: &mut Expr) -> ControlFlow<String> {
            if matches!(expr, Expr::BinaryOp { op: BinaryOperator::Custom(marker), .. } if marker.starts_with("msduck:select-"))
            {
                return ControlFlow::Break(
                    "compound assignment requires a SELECT projection".into(),
                );
            }
            ControlFlow::Continue(())
        }
    }
    match statement.visit(&mut Assignments) {
        ControlFlow::Continue(()) => Ok(()),
        ControlFlow::Break(error) => Err(error),
    }
}
fn parse_while(parser: &mut Parser) -> Result<Statement, ParserError> {
    let token: TokenWithSpan = parser.expect_keyword(Keyword::WHILE)?;
    let condition = parser.parse_expr()?;
    let statement = parser.parse_statement()?;
    Ok(Statement::While(WhileStatement {
        while_block: ConditionalStatementBlock {
            start_token: AttachedToken(token),
            condition: Some(condition),
            then_token: None,
            conditional_statements: ConditionalStatements::Sequence {
                statements: vec![statement],
            },
        },
    }))
}

fn parse_block(parser: &mut Parser) -> Result<Statement, ParserError> {
    parser.expect_keyword(Keyword::BEGIN)?;
    let mut statements = Vec::new();
    while !parser.peek_keyword(Keyword::END) {
        while parser.consume_token(&Token::SemiColon) {}
        if parser.peek_keyword(Keyword::END) {
            break;
        }
        statements.push(parser.parse_statement()?);
    }
    parser.expect_keyword(Keyword::END)?;
    Ok(Statement::StartTransaction {
        begin: true,
        statements,
        exception: None,
        has_end_keyword: true,
        transaction: None,
        modifier: None,
        modes: vec![],
    })
}

fn parse_compound_set(parser: &mut Parser, op: BinaryOperator) -> Result<Statement, ParserError> {
    parser.expect_keyword(Keyword::SET)?;
    let variable = parser.parse_identifier()?;
    parser.next_token();
    parser.expect_token(&Token::Eq)?;
    let right = parser.parse_expr()?;
    Ok(Statement::Set(Set::SingleAssignment {
        scope: None,
        hivevar: false,
        variable: ObjectName::from(vec![variable.clone()]),
        values: vec![Expr::BinaryOp {
            left: Box::new(Expr::Identifier(variable)),
            op,
            right: Box::new(Expr::Nested(Box::new(right))),
        }],
    }))
}

fn parse_tsql_drop_columns(parser: &mut Parser) -> Result<Statement, ParserError> {
    parser.expect_keywords(&[Keyword::ALTER, Keyword::TABLE])?;
    let name = parser.parse_object_name(false)?;
    parser.expect_keywords(&[Keyword::DROP, Keyword::COLUMN])?;
    let mut if_exists = parser.parse_keywords(&[Keyword::IF, Keyword::EXISTS]);
    let mut operations = Vec::new();
    loop {
        if operations.len() >= 10000 {
            return Err(ParserError::ParserError("too many dropped columns".into()));
        }
        if matches!(parser.peek_token().token, Token::Word(w) if matches!(w.keyword, Keyword::ADD | Keyword::ALTER | Keyword::DROP | Keyword::COLUMN | Keyword::CONSTRAINT))
        {
            return Err(ParserError::ParserError("expected column name".into()));
        }
        operations.push(AlterTableOperation::DropColumn {
            has_column_keyword: true,
            column_names: vec![parser.parse_identifier()?],
            if_exists,
            drop_behavior: None,
        });
        if !parser.consume_token(&Token::Comma) {
            break;
        }
        if parser.parse_keyword(Keyword::COLUMN) {
            if_exists = parser.parse_keywords(&[Keyword::IF, Keyword::EXISTS]);
        }
    }
    Ok(Statement::AlterTable(AlterTable {
        name,
        if_exists: false,
        only: false,
        operations,
        location: None,
        on_cluster: None,
        table_type: None,
        end_token: AttachedToken(parser.peek_token()),
    }))
}

fn parse_tsql_add_columns(parser: &mut Parser) -> Result<Statement, ParserError> {
    parser.expect_keywords(&[Keyword::ALTER, Keyword::TABLE])?;
    let name = parser.parse_object_name(false)?;
    parser.expect_keyword(Keyword::ADD)?;
    let mut operations = Vec::new();
    loop {
        if operations.len() >= 10000 {
            return Err(ParserError::ParserError("too many added columns".into()));
        }
        let column_def = parser.parse_column_def()?;
        operations.push(AlterTableOperation::AddColumn {
            column_keyword: false,
            if_not_exists: false,
            column_def,
            column_position: None,
        });
        if !parser.consume_token(&Token::Comma) {
            break;
        }
    }
    Ok(Statement::AlterTable(AlterTable {
        name,
        if_exists: false,
        only: false,
        operations,
        location: None,
        on_cluster: None,
        table_type: None,
        end_token: AttachedToken(parser.peek_token()),
    }))
}

fn parse_tsql_alter_column(parser: &mut Parser) -> Result<Statement, ParserError> {
    parser.expect_keywords(&[Keyword::ALTER, Keyword::TABLE])?;
    let name = parser.parse_object_name(false)?;
    parser.expect_keywords(&[Keyword::ALTER, Keyword::COLUMN])?;
    let column_name = parser.parse_identifier()?;
    if matches!(parser.peek_token().token, Token::Word(w) if matches!(w.keyword, Keyword::SET | Keyword::DROP | Keyword::TYPE | Keyword::ADD))
    {
        return Err(ParserError::ParserError(
            "expected T-SQL column data type".into(),
        ));
    }
    let data_type = parser.parse_data_type()?;
    let not_null = parser.parse_keywords(&[Keyword::NOT, Keyword::NULL]);
    if !not_null {
        let _ = parser.parse_keyword(Keyword::NULL);
    }
    Ok(Statement::AlterTable(AlterTable {
        name,
        if_exists: false,
        only: false,
        operations: vec![
            AlterTableOperation::AlterColumn {
                column_name: column_name.clone(),
                op: AlterColumnOperation::SetDataType {
                    data_type,
                    using: None,
                    had_set: true,
                },
            },
            AlterTableOperation::AlterColumn {
                column_name,
                op: if not_null {
                    AlterColumnOperation::SetNotNull
                } else {
                    AlterColumnOperation::DropNotNull
                },
            },
        ],
        location: None,
        on_cluster: None,
        table_type: None,
        end_token: AttachedToken(parser.peek_token()),
    }))
}

fn parse_try_catch(parser: &mut Parser) -> Result<Statement, ParserError> {
    parser.expect_keywords(&[Keyword::BEGIN, Keyword::TRY])?;
    let statements = parse_until_end(parser, Keyword::TRY)?;
    while parser.consume_token(&Token::SemiColon) {}
    parser.expect_keywords(&[Keyword::BEGIN, Keyword::CATCH])?;
    let caught = parse_until_end(parser, Keyword::CATCH)?;
    Ok(Statement::StartTransaction {
        begin: true,
        transaction: None,
        modes: vec![],
        modifier: Some(TransactionModifier::Try),
        statements,
        exception: Some(vec![ExceptionWhen {
            idents: vec![],
            statements: caught,
        }]),
        has_end_keyword: true,
    })
}
fn parse_until_end(parser: &mut Parser, kind: Keyword) -> Result<Vec<Statement>, ParserError> {
    let mut statements = Vec::new();
    loop {
        while parser.consume_token(&Token::SemiColon) {}
        if parser.peek_keyword(Keyword::END) {
            parser.expect_keywords(&[Keyword::END, kind])?;
            return Ok(statements);
        }
        statements.push(parser.parse_statement()?);
    }
}

fn parse_output_clause(parser: &mut Parser) -> Result<Option<OutputClause>, ParserError> {
    if !parser.parse_keyword(Keyword::OUTPUT) {
        return Ok(None);
    }
    let output_token = parser.get_current_token().clone().into();
    let select_items = parser.parse_projection()?;
    let into_table = if parser.parse_keyword(Keyword::INTO) {
        Some(SelectInto {
            temporary: false,
            unlogged: false,
            table: false,
            // Match sqlparser's INSERT representation of a destination with
            // a column list; logical binding must decode this as a sink.
            targets: vec![parser.parse_expr()?],
        })
    } else {
        None
    };
    Ok(Some(OutputClause::Output {
        output_token,
        select_items,
        into_table,
    }))
}

fn parse_update_assignments(parser: &mut Parser) -> Result<Statement, ParserError> {
    parser.expect_keyword(Keyword::UPDATE)?;
    let update_token = parser.get_current_token().clone().into();
    let table = parser.parse_table_and_joins()?;
    parser.expect_keyword(Keyword::SET)?;
    let assignments = parser.parse_comma_separated(|parser| {
        let target = parser.parse_assignment_target()?;
        let marker = match parser.peek_token().token {
            Token::Plus => Some("add"),
            Token::Minus => Some("sub"),
            Token::Mul => Some("mul"),
            Token::Div => Some("div"),
            Token::Mod => Some("mod"),
            Token::Ampersand => Some("and"),
            Token::Pipe => Some("or"),
            Token::Caret => Some("xor"),
            _ => None,
        };
        if marker.is_some() {
            parser.next_token();
        }
        parser.expect_token(&Token::Eq)?;
        let right = parser.parse_expr()?;
        let value = if let Some(marker) = marker {
            let AssignmentTarget::ColumnName(name) = &target else {
                return Err(ParserError::ParserError(
                    "compound UPDATE requires one column".into(),
                ));
            };
            let names = name
                .0
                .iter()
                .map(|p| {
                    p.as_ident()
                        .cloned()
                        .ok_or_else(|| ParserError::ParserError("unsupported UPDATE column".into()))
                })
                .collect::<Result<Vec<_>, _>>()?;
            let left = if names.len() == 1 {
                Expr::Identifier(names[0].clone())
            } else {
                Expr::CompoundIdentifier(names)
            };
            crate::expr::binary_function(&format!("__msduck_compound_{marker}"), left, right)
        } else {
            right
        };
        Ok(Assignment { target, value })
    })?;
    let output = parse_output_clause(parser)?;
    let from = if parser.parse_keyword(Keyword::FROM) {
        Some(UpdateTableFromKind::AfterSet(
            parser.parse_comma_separated(Parser::parse_table_and_joins)?,
        ))
    } else {
        None
    };
    let selection = if parser.parse_keyword(Keyword::WHERE) {
        Some(parser.parse_expr()?)
    } else {
        None
    };
    Ok(Statement::Update(Update {
        update_token,
        optimizer_hints: vec![],
        table,
        assignments,
        from,
        selection,
        returning: None,
        output,
        or: None,
        order_by: vec![],
        limit: None,
    }))
}

fn parse_delete_statement(parser: &mut Parser) -> Result<Statement, ParserError> {
    let delete_token = parser.expect_keyword(Keyword::DELETE)?.into();
    let _ = parser.parse_keyword(Keyword::FROM);
    let target = parser.parse_table_and_joins()?;
    let output = parse_output_clause(parser)?;
    let using = if parser.parse_keyword(Keyword::FROM) {
        Some(parser.parse_comma_separated(Parser::parse_table_and_joins)?)
    } else {
        None
    };
    let selection = if parser.parse_keyword(Keyword::WHERE) {
        Some(parser.parse_expr()?)
    } else {
        None
    };
    Ok(Statement::Delete(Delete {
        delete_token,
        optimizer_hints: vec![],
        tables: vec![],
        from: FromTable::WithFromKeyword(vec![target]),
        using,
        selection,
        returning: None,
        output,
        order_by: vec![],
        limit: None,
    }))
}

fn parse_cte_query(parser: &mut Parser) -> Result<Statement, ParserError> {
    let mut query = parser.parse_query()?;
    crate::query_options::parse(parser, &mut query)?;
    // sqlparser parses MERGE query bodies directly, bypassing the dialect's
    // statement hook. Apply the same validation before leaving this scope.
    if let SetExpr::Merge(Statement::Merge(merge)) = query.body.as_ref() {
        crate::merge::finish(parser, merge)?;
    }
    Ok(Statement::Query(query))
}

fn parse_cte_dml(parser: &mut Parser) -> Result<Statement, ParserError> {
    parser.expect_keyword(Keyword::WITH)?;
    let with_token = parser.get_current_token().clone().into();
    let recursive = parser.parse_keyword(Keyword::RECURSIVE);
    let cte_tables = parser.parse_comma_separated(Parser::parse_cte)?;
    let body = if parser.peek_keyword(Keyword::DELETE) {
        SetExpr::Delete(parse_delete_statement(parser)?)
    } else {
        SetExpr::Update(parse_update_assignments(parser)?)
    };
    let mut stub = Parser::parse_sql(&MsSqlDialect {}, "SELECT 1")?;
    let Statement::Query(mut query) = stub.remove(0) else {
        unreachable!()
    };
    query.with = Some(With {
        with_token,
        recursive,
        cte_tables,
    });
    query.body = Box::new(body);
    Ok(Statement::Query(query))
}

#[cfg(test)]
mod tests {
    #[test]
    fn speculative_scalar_queries_preserve_recursion_limits() {
        use super::*;
        for value in ["1", "SELECT 1", "SELECT 1 UNION SELECT 2"] {
            let sql = format!("SELECT {}{}{}", "(".repeat(32), value, ")".repeat(32));
            let result = Parser::new(&ServerDialect)
                .with_recursion_limit(8)
                .try_with_sql(&sql)
                .unwrap()
                .parse_statements();
            assert!(matches!(result, Err(ParserError::RecursionLimitExceeded)));
        }
    }
    #[test]
    fn tsql_drop_columns_preserves_lists_and_statement_boundaries() {
        let statements = sqlparser::parser::Parser::parse_sql(
            &super::ServerDialect,
            "ALTER TABLE dbo.t DROP COLUMN IF EXISTS [a,b],c,COLUMN d; SELECT 1",
        )
        .unwrap();
        assert_eq!(statements.len(), 2);
        let sqlparser::ast::Statement::AlterTable(table) = &statements[0] else {
            panic!()
        };
        assert_eq!(table.operations.len(), 3);
        for (op, (expected, exists)) in
            table
                .operations
                .iter()
                .zip([("a,b", true), ("c", true), ("d", false)])
        {
            let sqlparser::ast::AlterTableOperation::DropColumn {
                column_names,
                if_exists,
                ..
            } = op
            else {
                panic!()
            };
            assert_eq!(column_names[0].value, expected);
            assert_eq!(*if_exists, exists);
        }
        for sql in [
            "ALTER TABLE t DROP COLUMN",
            "ALTER TABLE t DROP COLUMN a,",
            "ALTER TABLE t DROP COLUMN a,,b",
        ] {
            assert!(sqlparser::parser::Parser::parse_sql(&super::ServerDialect, sql).is_err());
        }
    }
    use super::*;
    #[test]
    fn window_clause_after_projection_is_not_an_implicit_alias() {
        for sql in [
            "SELECT ROW_NUMBER() OVER w WINDOW w AS (ORDER BY (SELECT 1))",
            "SELECT SUM(1) OVER [w] WINDOW [w] AS ()",
            "SELECT 1 WINDOW /* definition */ w AS ()",
            "select sum(1) over w window w as ()",
        ] {
            let statements = Parser::parse_sql(&ServerDialect, sql).unwrap();
            let Statement::Query(query) = &statements[0] else {
                panic!("expected query")
            };
            let SetExpr::Select(select) = query.body.as_ref() else {
                panic!("expected select")
            };
            assert!(select.from.is_empty());
            assert_eq!(select.named_window.len(), 1);
            assert!(matches!(select.projection[0], SelectItem::UnnamedExpr(_)));
        }
        for sql in ["SELECT 1 WINDOW", "SELECT 1 AS WINDOW", "SELECT 1 [WINDOW]"] {
            let statements = Parser::parse_sql(&ServerDialect, sql).unwrap();
            let Statement::Query(query) = &statements[0] else {
                panic!("expected query")
            };
            let SetExpr::Select(select) = query.body.as_ref() else {
                panic!("expected select")
            };
            assert!(select.named_window.is_empty());
            assert!(
                matches!(&select.projection[0], SelectItem::ExprWithAlias { alias, .. } if alias.value == "WINDOW")
            );
        }
    }
    #[test]
    fn multi_column_add_parses_decimal_commas_and_statement_boundaries() {
        let statements = Parser::parse_sql(&ServerDialect,
            "ALTER TABLE dbo.t ADD a DECIMAL(10,2) NULL DEFAULT 1.25, b NVARCHAR(10) NOT NULL DEFAULT N'x'; SELECT 1;").unwrap();
        assert_eq!(statements.len(), 2);
        let Statement::AlterTable(table) = &statements[0] else {
            panic!("expected table alteration")
        };
        assert_eq!(table.operations.len(), 2);
        for sql in [
            "ALTER TABLE t ADD a INT,",
            "ALTER TABLE t ADD a INT, b",
            "ALTER TABLE t ADD",
        ] {
            assert!(Parser::parse_sql(&ServerDialect, sql).is_err(), "{sql}");
        }
    }
    #[test]
    fn tsql_alter_column_preserves_statement_boundary_and_nullability() {
        let statements = Parser::parse_sql(
            &ServerDialect,
            "ALTER TABLE [dbo].[t] ALTER COLUMN [n] BIGINT NOT NULL; SELECT 1;",
        )
        .unwrap();
        assert_eq!(statements.len(), 2);
        let Statement::AlterTable(table) = &statements[0] else {
            panic!("expected ALTER TABLE")
        };
        assert!(matches!(
            &table.operations[0],
            AlterTableOperation::AlterColumn {
                op: AlterColumnOperation::SetDataType {
                    data_type: DataType::BigInt(_),
                    ..
                },
                ..
            }
        ));
        assert!(matches!(
            &table.operations[1],
            AlterTableOperation::AlterColumn {
                op: AlterColumnOperation::SetNotNull,
                ..
            }
        ));
        assert!(
            Parser::parse_sql(&ServerDialect, "ALTER TABLE t ALTER COLUMN n INT NOT;").is_err()
        );
    }
    #[test]
    fn output_preserves_projection_sink_and_statement_boundaries() {
        for (sql, count, sink) in [
            (
                "UPDATE t SET n=n+1 OUTPUT deleted.n AS old_n,inserted.n AS new_n WHERE id=1; SELECT 9",
                2,
                false,
            ),
            (
                "UPDATE a SET n=b.n OUTPUT deleted.n,inserted.n INTO sink(a,b) FROM t a JOIN s b ON a.id=b.id WHERE b.n>0; SELECT 9",
                2,
                true,
            ),
            (
                "DELETE FROM t OUTPUT deleted.* WHERE id=1; SELECT 9",
                1,
                false,
            ),
            (
                "DELETE a OUTPUT deleted.id,b.n INTO sink(a,b) FROM t a JOIN s b ON a.id=b.id WHERE b.n>0; SELECT 9",
                2,
                true,
            ),
            (
                "INSERT INTO t(id) OUTPUT inserted.id INTO sink(a) VALUES(1); SELECT 9",
                1,
                true,
            ),
        ] {
            let statements = crate::batch::parse(sql).unwrap();
            assert_eq!(statements.len(), 2, "{sql}");
            assert!(matches!(&statements[1], Statement::Query(_)));
            let output = match &statements[0] {
                Statement::Update(update) => {
                    assert!(update.selection.is_some());
                    &update.output
                }
                Statement::Delete(delete) => {
                    assert!(delete.selection.is_some());
                    &delete.output
                }
                Statement::Insert(insert) => &insert.output,
                other => panic!("unexpected statement: {other}"),
            };
            let Some(OutputClause::Output {
                select_items,
                into_table,
                ..
            }) = output
            else {
                panic!("missing OUTPUT: {sql}")
            };
            assert_eq!(select_items.len(), count, "{sql}");
            assert_eq!(into_table.is_some(), sink, "{sql}");
            if let Some(into) = into_table {
                assert_eq!(into.targets.len(), 1);
                assert!(matches!(&into.targets[0], Expr::Function(_)));
            }
        }
        for sql in [
            "UPDATE t SET n=1 OUTPUT WHERE id=1",
            "DELETE FROM t OUTPUT deleted.id, WHERE id=1",
            "UPDATE t SET n=1 WHERE id=1 OUTPUT inserted.n",
            "DELETE FROM t OUTPUT deleted.id INTO",
        ] {
            assert!(crate::batch::parse(sql).is_err(), "{sql}");
        }
    }

    #[test]
    fn try_catch_requires_paired_delimiters_and_preserves_following_statements() {
        let statements = Parser::parse_sql(
            &ServerDialect,
            "BEGIN TRY BEGIN SELECT 1 END END TRY BEGIN CATCH RETURN END CATCH; SELECT 2;",
        )
        .unwrap();
        assert_eq!(statements.len(), 2);
        for sql in [
            "BEGIN TRY SELECT 1 END TRY",
            "BEGIN TRY SELECT 1 END CATCH BEGIN CATCH END CATCH",
            "BEGIN TRY END TRY BEGIN CATCH SELECT 1",
            "BEGIN TRY END TRY SELECT 1 BEGIN CATCH END CATCH",
        ] {
            assert!(Parser::parse_sql(&ServerDialect, sql).is_err(), "{sql}");
        }
    }
    #[test]
    fn blocks_and_single_statement_loops_preserve_statement_boundaries() {
        let statements =
            Parser::parse_sql(&ServerDialect, "WHILE 1=0 SELECT 1; SELECT 2;").unwrap();
        assert_eq!(statements.len(), 2);
        let Statement::While(while_statement) = &statements[0] else {
            panic!("expected loop")
        };
        assert_eq!(while_statement.while_block.statements().len(), 1);
        Parser::parse_sql(&ServerDialect, "BEGIN SELECT 1 SET NOCOUNT ON END").unwrap();
        assert!(Parser::parse_sql(&ServerDialect, "BEGIN SELECT 1").is_err());
        assert!(Parser::parse_sql(&ServerDialect, "WHILE 1=1").is_err());
        let statements = Parser::parse_sql(&ServerDialect, "BEGIN TRAN; COMMIT;").unwrap();
        assert!(matches!(
            statements[0],
            Statement::StartTransaction {
                has_end_keyword: false,
                ..
            }
        ));
    }
}
