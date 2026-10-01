//! CLUSTERED and NONCLUSTERED on PRIMARY KEY and UNIQUE constraints,
//! `CREATE [UNIQUE] NONCLUSTERED INDEX`, and column-level key constraints
//! with a column list.
//!
//! sqlparser has no place for these keywords in a constraint, and msduck
//! stores every table and index the same way, so the tokens are dropped
//! before parsing. The catalog does not record which index SQL Server would
//! make clustered, and two CLUSTERED constraints are not rejected with 8112
//! (see docs/gaps-keys.md). `CREATE [UNIQUE] CLUSTERED INDEX` keeps its
//! keyword for the keys feature's parser (`dialect::ext::keys::index`).
//!
//! SQL Server also accepts a table constraint written after a column without
//! a separating comma, such as `id int NOT NULL CONSTRAINT uq UNIQUE (id)` or
//! `b int PRIMARY KEY (a, b)`. sqlparser reads constraints after a column as
//! column options, which take no column list, so the missing comma is
//! inserted inside CREATE TABLE column lists.
use sqlparser::{
    keywords::Keyword,
    tokenizer::{Token, TokenWithSpan},
};

pub(crate) fn is_word(token: &Token, value: &str) -> bool {
    matches!(token, Token::Word(w) if w.quote_style.is_none() && w.value.eq_ignore_ascii_case(value))
}

/// The indexes of the tokens that are not whitespace or comments.
pub(crate) fn significant(tokens: &[TokenWithSpan]) -> Vec<usize> {
    tokens
        .iter()
        .enumerate()
        .filter(|(_, token)| !matches!(token.token, Token::Whitespace(_)))
        .map(|(index, _)| index)
        .collect()
}

pub fn strip(tokens: &mut Vec<TokenWithSpan>) {
    let significant = significant(tokens);
    let word = |position: usize, value: &str| {
        significant
            .get(position)
            .is_some_and(|&index| is_word(&tokens[index].token, value))
    };
    let keyword = |position: usize, keyword: Keyword| {
        significant.get(position).is_some_and(
            |&index| matches!(&tokens[index].token, Token::Word(w) if w.quote_style.is_none() && w.keyword == keyword),
        )
    };
    let mut remove = Vec::new();
    for (position, &token) in significant.iter().enumerate() {
        let clustering = word(position, "CLUSTERED") || word(position, "NONCLUSTERED");
        if !clustering || position == 0 {
            continue;
        }
        // Not CREATE UNIQUE CLUSTERED INDEX, which stays unsupported.
        let constraint = (keyword(position - 1, Keyword::UNIQUE)
            && !keyword(position + 1, Keyword::INDEX))
            || (position >= 2
                && keyword(position - 1, Keyword::KEY)
                && keyword(position - 2, Keyword::PRIMARY));
        // CREATE [UNIQUE] NONCLUSTERED INDEX
        let index = word(position, "NONCLUSTERED")
            && keyword(position + 1, Keyword::INDEX)
            && (keyword(position - 1, Keyword::CREATE)
                || (position >= 2
                    && keyword(position - 1, Keyword::UNIQUE)
                    && keyword(position - 2, Keyword::CREATE)));
        if constraint || index {
            remove.push(token);
        }
    }
    for index in remove.into_iter().rev() {
        tokens.remove(index);
    }
    separate_constraints(tokens);
}

/// Whether the significant token at `position` opens the definition list of
/// `CREATE TABLE name (`. sqlparser reads no table constraints in table
/// variable declarations, so those are left alone.
fn opens_table_list(tokens: &[TokenWithSpan], significant: &[usize], position: usize) -> bool {
    let token = |p: usize| &tokens[significant[p]].token;
    let keyword = |p: usize, k: Keyword| matches!(token(p), Token::Word(w) if w.quote_style.is_none() && w.keyword == k);
    // Walk back over a (possibly qualified) object name.
    let mut p = position;
    let mut expect_name = true;
    while p > 0 {
        p -= 1;
        match token(p) {
            Token::Word(_) if expect_name => expect_name = false,
            Token::Period if !expect_name => expect_name = true,
            _ => break,
        }
        if !expect_name && p >= 2 && keyword(p - 1, Keyword::TABLE) {
            return keyword(p - 2, Keyword::CREATE);
        }
    }
    false
}

fn separate_constraints(tokens: &mut Vec<TokenWithSpan>) {
    let significant = significant(tokens);
    let token = |p: usize| &tokens[significant[p]].token;
    let keyword = |p: usize, k: Keyword| {
        p < significant.len()
            && matches!(token(p), Token::Word(w) if w.quote_style.is_none() && w.keyword == k)
    };
    let mut stack: Vec<Option<usize>> = Vec::new();
    let mut insert = Vec::new();
    for position in 0..significant.len() {
        match token(position) {
            Token::LParen => {
                stack.push(opens_table_list(tokens, &significant, position).then_some(position));
                continue;
            }
            Token::RParen => {
                stack.pop();
                continue;
            }
            _ => {}
        }
        let Some(Some(open)) = stack.last() else {
            continue;
        };
        let list = if keyword(position, Keyword::PRIMARY) && keyword(position + 1, Keyword::KEY) {
            position + 2
        } else if keyword(position, Keyword::UNIQUE) {
            position + 1
        } else {
            continue;
        };
        if list >= significant.len() || token(list) != &Token::LParen {
            continue;
        }
        let start = if position >= 2 && keyword(position - 2, Keyword::CONSTRAINT) {
            position - 2
        } else {
            position
        };
        if start == 0 || start - 1 == *open || token(start - 1) == &Token::Comma {
            continue;
        }
        insert.push(significant[start]);
    }
    for index in insert.into_iter().rev() {
        let span = tokens[index].span;
        tokens.insert(index, TokenWithSpan::new(Token::Comma, span));
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn clustering_keywords_are_dropped_only_after_keys() {
        for (sql, expected) in [
            (
                "create table T (v varchar(64) not null constraint T_Pk primary key nonclustered)",
                "CREATE TABLE T (v VARCHAR(64) NOT NULL CONSTRAINT T_Pk PRIMARY KEY)",
            ),
            (
                "CREATE TABLE T (id int PRIMARY KEY CLUSTERED, code int UNIQUE NONCLUSTERED)",
                "CREATE TABLE T (id INT PRIMARY KEY, code INT UNIQUE)",
            ),
            (
                "CREATE TABLE T (a int, b int, CONSTRAINT PK_T PRIMARY KEY NONCLUSTERED (a, b DESC), CONSTRAINT UQ_T UNIQUE /* c */ CLUSTERED (b ASC))",
                "CREATE TABLE T (a INT, b INT, CONSTRAINT PK_T PRIMARY KEY (a, b DESC), CONSTRAINT UQ_T UNIQUE (b ASC))",
            ),
            (
                "ALTER TABLE T ADD PRIMARY KEY NONCLUSTERED (a)",
                "ALTER TABLE T ADD PRIMARY KEY(a)",
            ),
            (
                "CREATE UNIQUE NONCLUSTERED INDEX UX ON T (a)",
                "CREATE UNIQUE INDEX UX ON T(a)",
            ),
            (
                "CREATE NONCLUSTERED INDEX IX ON T (a)",
                "CREATE INDEX IX ON T(a)",
            ),
            (
                "SELECT clustered, nonclustered FROM T",
                "SELECT clustered, nonclustered FROM T",
            ),
        ] {
            let statements = crate::batch::parse(sql).unwrap();
            assert_eq!(statements[0].to_string(), expected, "{sql}");
        }
        assert!(crate::batch::parse("CREATE CLUSTERED INDEX CIX ON T (a)").is_ok());
        assert!(crate::batch::parse("CREATE UNIQUE CLUSTERED INDEX CIX ON T (a)").is_ok());
    }

    #[test]
    fn column_constraints_with_lists_become_table_constraints() {
        for (sql, expected) in [
            (
                "CREATE TABLE items (id int NOT NULL CONSTRAINT uq_items UNIQUE(id))",
                "CREATE TABLE items (id INT NOT NULL, CONSTRAINT uq_items UNIQUE (id))",
            ),
            (
                "CREATE TABLE things (id int NOT NULL CONSTRAINT pk_things PRIMARY KEY(id), v int)",
                "CREATE TABLE things (id INT NOT NULL, v INT, CONSTRAINT pk_things PRIMARY KEY (id))",
            ),
            (
                "CREATE TABLE dbo.pairs (a int NOT NULL, b int NOT NULL CONSTRAINT pk_pairs PRIMARY KEY CLUSTERED (a, b))",
                "CREATE TABLE dbo.pairs (a INT NOT NULL, b INT NOT NULL, CONSTRAINT pk_pairs PRIMARY KEY (a, b))",
            ),
            (
                "CREATE TABLE plain (id int PRIMARY KEY (id), other int UNIQUE NONCLUSTERED (other))",
                "CREATE TABLE plain (id INT, other INT, PRIMARY KEY (id), UNIQUE (other))",
            ),
            (
                "CREATE TABLE ok (id int PRIMARY KEY, v int UNIQUE, CONSTRAINT u UNIQUE (v))",
                "CREATE TABLE ok (id INT PRIMARY KEY, v INT UNIQUE, CONSTRAINT u UNIQUE (v))",
            ),
            (
                "CREATE TABLE c (id int CHECK (id IN (1, 2)) UNIQUE (id))",
                "CREATE TABLE c (id INT CHECK (id IN (1, 2)), UNIQUE (id))",
            ),
        ] {
            let statements = crate::batch::parse(sql).unwrap();
            assert_eq!(statements[0].to_string(), expected, "{sql}");
        }
    }
}
