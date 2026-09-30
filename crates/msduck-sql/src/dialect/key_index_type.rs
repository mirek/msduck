//! CLUSTERED and NONCLUSTERED on PRIMARY KEY and UNIQUE constraints, and
//! `CREATE [UNIQUE] NONCLUSTERED INDEX`.
//!
//! sqlparser has no place for these keywords in a constraint, and msduck
//! stores every table and index the same way, so the tokens are dropped
//! before parsing. The catalog does not record which index SQL Server would
//! make clustered, and two CLUSTERED constraints are not rejected with 8112
//! (see docs/tedious-compat-gaps.md). `CREATE CLUSTERED INDEX` still fails.
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
        let constraint = keyword(position - 1, Keyword::UNIQUE)
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
        assert!(crate::batch::parse("CREATE CLUSTERED INDEX CIX ON T (a)").is_err());
    }
}
