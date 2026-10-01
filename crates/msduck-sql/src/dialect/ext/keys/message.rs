//! DuckDB duplicate-key diagnostics and SQL Server's duplicate-key messages.
//!
//! DuckDB reports a violated unique index or constraint in two forms:
//!
//! - `Duplicate key "expr: value, expr: value" violates primary key constraint.`
//!   (or `... violates unique constraint.`) when the key already exists;
//! - `PRIMARY KEY or UNIQUE constraint violation: duplicate key "value, value"`
//!   when one statement inserts the same key twice.
//!
//! Neither names the index, so keys-managed indexes start with a tag
//! expression whose value identifies them.

/// What a DuckDB duplicate-key message says.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Duplicate {
    /// `Some(true)` for a primary key, `Some(false)` for a unique constraint
    /// or index, `None` when DuckDB does not say.
    pub primary: Option<bool>,
    /// Each key expression as DuckDB prints it, when the message has them.
    pub expressions: Option<Vec<String>>,
    /// Each key value as DuckDB prints it, read left to right; reliable for
    /// native keys, whose expressions are column names.
    pub values: Vec<String>,
    /// The quoted text between the message's double quotes.
    pub body: String,
}

impl Duplicate {
    /// The tag of a keys-managed index: the value of its first expression,
    /// `CASE WHEN ... END`. The filter inside that expression can contain
    /// any text, so the tag is read after the expression's closing `END`.
    pub fn tag(&self) -> Option<i64> {
        let text = match self.expressions {
            None => self.values.first()?.as_str(),
            Some(_) => {
                let start = self.body.find(" END: ")? + " END: ".len();
                let rest = &self.body[start..];
                &rest[..rest.find(", ").unwrap_or(rest.len())]
            }
        };
        text.parse().ok()
    }

    /// The `count` values after the tag of a keys-managed index. Read from
    /// the right: values never contain ", " or ": ", and the key column
    /// expressions after the tag contain no ": ".
    pub fn managed_values(&self, count: usize) -> Option<Vec<String>> {
        if self.expressions.is_none() {
            return (self.values.len() == count + 1).then(|| self.values[1..].to_vec());
        }
        let mut values = Vec::with_capacity(count);
        let mut rest = self.body.as_str();
        for _ in 0..count {
            let colon = rest.rfind(": ")?;
            let tail = &rest[colon + 2..];
            values.push(tail[..tail.find(", ").unwrap_or(tail.len())].to_owned());
            rest = &rest[..colon];
        }
        values.reverse();
        Some(values)
    }
}

const EXISTING: &str = "Duplicate key \"";
const STATEMENT: &str = "PRIMARY KEY or UNIQUE constraint violation: duplicate key \"";

/// Recognize a DuckDB duplicate-key message.
pub fn duplicate(message: &str) -> Option<Duplicate> {
    if let Some(start) = message.find(STATEMENT) {
        let body = &message[start + STATEMENT.len()..];
        let body = &body[..body.rfind('"')?];
        return Some(Duplicate {
            primary: None,
            expressions: None,
            values: body.split(", ").map(str::to_owned).collect(),
            body: body.to_owned(),
        });
    }
    let start = message.find(EXISTING)?;
    let rest = &message[start + EXISTING.len()..];
    let (body, primary) = if let Some(end) = rest.find("\" violates primary key constraint") {
        (&rest[..end], true)
    } else if let Some(end) = rest.find("\" violates unique constraint") {
        (&rest[..end], false)
    } else {
        return None;
    };
    // Values never contain ", " in keys-managed indexes (they are numbers,
    // booleans, hexadecimal strings or temporal text); a mismatched count is
    // detected by the caller.
    let mut expressions = vec![];
    let mut values = vec![];
    let mut rest = body;
    while let Some(colon) = rest.find(": ") {
        expressions.push(rest[..colon].to_owned());
        let after = &rest[colon + 2..];
        let end = after.find(", ").unwrap_or(after.len());
        values.push(after[..end].to_owned());
        rest = after[end..].strip_prefix(", ").unwrap_or("");
    }
    Some(Duplicate {
        primary: Some(primary),
        expressions: Some(expressions),
        values,
        body: body.to_owned(),
    })
}

/// The single-column value of a native key, which may itself contain ", ".
pub fn single_value(message: &str) -> Option<String> {
    if let Some(start) = message.find(STATEMENT) {
        let body = &message[start + STATEMENT.len()..];
        return Some(body[..body.rfind('"')?].to_owned());
    }
    let start = message.find(EXISTING)?;
    let rest = &message[start + EXISTING.len()..];
    let end = rest.rfind("\" violates ")?;
    let (_, value) = rest[..end].split_once(": ")?;
    Some(value.to_owned())
}

/// The kind of object whose uniqueness was violated.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    PrimaryKey,
    UniqueConstraint,
    UniqueIndex,
}

/// SQL Server's duplicate-key error: number, state, severity and message.
pub fn violation(kind: Kind, name: &str, object: &str, values: &[String]) -> (i32, u8, u8, String) {
    let values = values.join(", ");
    match kind {
        Kind::PrimaryKey | Kind::UniqueConstraint => (
            2627,
            1,
            14,
            format!(
                "Violation of {} constraint '{name}'. Cannot insert duplicate key in object '{object}'. The duplicate key value is ({values}).",
                if kind == Kind::PrimaryKey {
                    "PRIMARY KEY"
                } else {
                    "UNIQUE KEY"
                }
            ),
        ),
        Kind::UniqueIndex => (
            2601,
            1,
            14,
            format!(
                "Cannot insert duplicate key row in object '{object}' with unique index '{name}'. The duplicate key value is ({values})."
            ),
        ),
    }
}

/// SQL Server's error when a new unique index finds existing duplicates.
pub fn creation(name: &str, object: &str, values: &[String]) -> (i32, u8, u8, String) {
    (
        1505,
        1,
        16,
        format!(
            "The CREATE UNIQUE INDEX statement terminated because a duplicate key was found for the object name '{object}' and the index name '{name}'. The duplicate key value is ({}).",
            values.join(", ")
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn existing_key_messages_keep_expressions_and_values() {
        let d = duplicate("Constraint Error: Duplicate key \"CASE  WHEN ((v > CAST(0 AS INTEGER))) THEN (CAST(18 AS INTEGER)) ELSE CAST(NULL AS INTEGER) END: 18, (v IS NULL): false, COALESCE(v, CAST(0 AS INTEGER)): 1\" violates unique constraint.").unwrap();
        assert_eq!(d.primary, Some(false));
        assert_eq!(d.values, ["18", "false", "1"]);
        assert_eq!(d.expressions.unwrap()[1], "(v IS NULL)");
        let d =
            duplicate("Constraint Error: Duplicate key \"id: 1\" violates primary key constraint.")
                .unwrap();
        assert_eq!(d.primary, Some(true));
        assert_eq!(d.values, ["1"]);
        let d = duplicate("Constraint Error: Duplicate key \"hex(rtrim(a, ' ')): 782C, b: 12.50, e: 2024-01-02 03:04:05.678\" violates unique constraint.").unwrap();
        assert_eq!(d.values, ["782C", "12.50", "2024-01-02 03:04:05.678"]);
    }

    #[test]
    fn managed_tags_and_values_survive_filter_text() {
        let d = duplicate("Constraint Error: Duplicate key \"CASE  WHEN ((status = 'a: b, c: d') AND (n > 0)) THEN (CAST(9000000000000000003 AS BIGINT)) ELSE CAST(NULL AS BIGINT) END: 9000000000000000003, (code IS NULL): false, COALESCE(code, CAST(0 AS INTEGER)): 5, hex(rtrim(v, ' ')): 61\" violates unique constraint.").unwrap();
        assert_eq!(d.tag(), Some(9_000_000_000_000_000_003));
        assert_eq!(d.managed_values(3).unwrap(), ["false", "5", "61"]);
        let d = duplicate(
            "Constraint Error: PRIMARY KEY or UNIQUE constraint violation: duplicate key \"9000000000000000001, 7\"",
        )
        .unwrap();
        assert_eq!(d.tag(), Some(9_000_000_000_000_000_001));
        assert_eq!(d.managed_values(1).unwrap(), ["7"]);
        assert!(d.managed_values(2).is_none());
    }

    #[test]
    fn statement_duplicates_keep_values() {
        let d = duplicate(
            "Constraint Error: PRIMARY KEY or UNIQUE constraint violation: duplicate key \"18, false, 7\"",
        )
        .unwrap();
        assert_eq!(d.primary, None);
        assert_eq!(d.values, ["18", "false", "7"]);
        assert_eq!(
            single_value(
                "Constraint Error: Duplicate key \"v: a, b: c\" violates unique constraint."
            )
            .as_deref(),
            Some("a, b: c")
        );
        assert!(duplicate("Constraint Error: NOT NULL constraint failed: t.id").is_none());
        assert!(duplicate("Constraint Error: Violates foreign key constraint because key \"id: 1\" does not exist in the referenced table").is_none());
    }

    #[test]
    fn sql_server_messages_match_captures() {
        assert_eq!(
            violation(Kind::PrimaryKey, "pk_items", "dbo.items", &["a".into(), "x".into()]),
            (2627, 1, 14, "Violation of PRIMARY KEY constraint 'pk_items'. Cannot insert duplicate key in object 'dbo.items'. The duplicate key value is (a, x).".into())
        );
        assert_eq!(
            violation(
                Kind::UniqueConstraint,
                "uq_items_name",
                "dbo.items",
                &["<NULL>".into()]
            )
            .3,
            "Violation of UNIQUE KEY constraint 'uq_items_name'. Cannot insert duplicate key in object 'dbo.items'. The duplicate key value is (<NULL>)."
        );
        assert_eq!(
            violation(Kind::UniqueIndex, "ux_items_code", "dbo.items", &["x   ".into()]),
            (2601, 1, 14, "Cannot insert duplicate key row in object 'dbo.items' with unique index 'ux_items_code'. The duplicate key value is (x   ).".into())
        );
        assert_eq!(
            creation("ux_items_name", "dbo.items", &["<NULL>".into()]).3,
            "The CREATE UNIQUE INDEX statement terminated because a duplicate key was found for the object name 'dbo.items' and the index name 'ux_items_name'. The duplicate key value is (<NULL>)."
        );
    }
}
