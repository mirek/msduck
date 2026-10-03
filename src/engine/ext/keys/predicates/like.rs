//! SQL Server's Unicode LIKE over UTF-16 code units, under the database's
//! case-insensitive default collation: units the collation ignores are
//! dropped from the value and the pattern, units match when their
//! case-folded forms agree ([`super::key::fold`]), and a set range holds the
//! units that sort between its ends (base letter first, so `[a-c]` holds
//! `B` and `á`). Other ranges compare code units, not linguistic weights.
//!
//! Behavior captured from SQL Server (reference/gaps-unicode-predicates.json):
//!
//! - `%` matches any run of units, `_` exactly one unit (a surrogate pair
//!   is two units);
//! - `[...]` matches one unit from a set of units and `a-z` ranges, `[^...]`
//!   one unit outside it; `[]` matches nothing and `[^]` any unit; a `-`
//!   first or right after a range is literal, while `x-` takes the next
//!   unit (even `]`) as the range end;
//! - the escape character makes the next unit literal everywhere, including
//!   inside a set, and takes precedence over `]`, `^` and `-`;
//! - a pattern with an unterminated set, or ending in the escape character,
//!   matches nothing;
//! - trailing spaces are significant in both the value and the pattern.

use super::key::{base, fold, ignorable};

/// The collation order of one unit: its base letter, then its case-folded
/// form (see [`super::key`]).
fn weight(unit: u16) -> (u16, u16) {
    let folded = fold(unit);
    (fold(base(folded)), folded)
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Token {
    /// `%`
    Any,
    /// `_`
    One,
    Unit(u16),
    Set {
        negated: bool,
        ranges: Vec<(u16, u16)>,
    },
}

impl Token {
    fn matches(&self, unit: u16) -> bool {
        match self {
            Token::Any => true,
            Token::One => true,
            Token::Unit(expected) => fold(*expected) == fold(unit),
            Token::Set { negated, ranges } => {
                ranges.iter().any(|(low, high)| {
                    weight(*low) <= weight(unit) && weight(unit) <= weight(*high)
                }) != *negated
            }
        }
    }
}

/// A parsed pattern; `None` when it can match nothing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Pattern(Option<Vec<Token>>);

impl Pattern {
    pub(crate) fn parse(pattern: &[u16], escape: Option<u16>) -> Self {
        let pattern: Vec<u16> = pattern.iter().copied().filter(|u| !ignorable(*u)).collect();
        Self(tokens(&pattern, escape))
    }

    pub(crate) fn matches(&self, value: &[u16]) -> bool {
        let Some(tokens) = &self.0 else {
            return false;
        };
        let value: Vec<u16> = value.iter().copied().filter(|u| !ignorable(*u)).collect();
        let value = value.as_slice();
        // Greedy matching that backtracks only to the last `%`, which is
        // complete for patterns whose other tokens match single units.
        let (mut t, mut v) = (0, 0);
        let mut star: Option<(usize, usize)> = None;
        while v < value.len() {
            match tokens.get(t) {
                Some(Token::Any) => {
                    star = Some((t, v));
                    t += 1;
                }
                Some(token) if token.matches(value[v]) => {
                    t += 1;
                    v += 1;
                }
                _ => match star {
                    Some((star_token, star_value)) => {
                        t = star_token + 1;
                        v = star_value + 1;
                        star = Some((star_token, star_value + 1));
                    }
                    None => return false,
                },
            }
        }
        tokens[t..].iter().all(|token| *token == Token::Any)
    }
}

fn tokens(pattern: &[u16], escape: Option<u16>) -> Option<Vec<Token>> {
    const PERCENT: u16 = b'%' as u16;
    const UNDERSCORE: u16 = b'_' as u16;
    const OPEN: u16 = b'[' as u16;
    let mut tokens = Vec::new();
    let mut i = 0;
    while i < pattern.len() {
        let unit = pattern[i];
        i += 1;
        let token = if Some(unit) == escape {
            let literal = *pattern.get(i)?;
            i += 1;
            Token::Unit(literal)
        } else {
            match unit {
                PERCENT => Token::Any,
                UNDERSCORE => Token::One,
                OPEN => set(pattern, &mut i, escape)?,
                unit => Token::Unit(unit),
            }
        };
        // Consecutive `%` are one `%`.
        if !(token == Token::Any && tokens.last() == Some(&Token::Any)) {
            tokens.push(token);
        }
    }
    Some(tokens)
}

/// A set after its `[`; `None` when unterminated or the escape dangles.
fn set(pattern: &[u16], i: &mut usize, escape: Option<u16>) -> Option<Token> {
    const CLOSE: u16 = b']' as u16;
    const CARET: u16 = b'^' as u16;
    const DASH: u16 = b'-' as u16;
    // One member, literal when escaped; `None` for the closing bracket.
    let member = |i: &mut usize| -> Option<Option<u16>> {
        let unit = *pattern.get(*i)?;
        *i += 1;
        if Some(unit) == escape {
            let literal = *pattern.get(*i)?;
            *i += 1;
            Some(Some(literal))
        } else if unit == CLOSE {
            Some(None)
        } else {
            Some(Some(unit))
        }
    };
    let mut negated = false;
    if pattern.get(*i) == Some(&CARET) && escape != Some(CARET) {
        negated = true;
        *i += 1;
    }
    let mut ranges = Vec::new();
    loop {
        let Some(low) = member(i)? else {
            return Some(Token::Set { negated, ranges });
        };
        if pattern.get(*i) == Some(&DASH) && escape != Some(DASH) {
            *i += 1;
            // The range end is the next unit, even a `]`.
            let high = *pattern.get(*i)?;
            *i += 1;
            let high = if Some(high) == escape {
                let literal = *pattern.get(*i)?;
                *i += 1;
                literal
            } else {
                high
            };
            ranges.push((low, high));
        } else {
            ranges.push((low, low));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn like(value: &str, pattern: &str, escape: Option<char>) -> bool {
        let value: Vec<u16> = value.encode_utf16().collect();
        let pattern: Vec<u16> = pattern.encode_utf16().collect();
        Pattern::parse(&pattern, escape.map(|c| c as u16)).matches(&value)
    }

    /// (pattern, value, escape, SQL Server's result), from the reference
    /// capture. That capture used a BIN2 database; the two case-only cases
    /// marked below follow the case-insensitive default instead
    /// (reference/default-collation.json).
    const CASES: &[(&str, &str, Option<char>, bool)] = &[
        ("[]]", "abc", None, false),
        ("[]]", "]", None, false),
        ("[abc", "[abc", None, false),
        ("[abc", "a", None, false),
        ("ab", "ab ", None, false),
        ("ab ", "ab", None, false),
        ("a!%", "a%", Some('!'), true),
        ("a!", "a!", Some('!'), false),
        ("a!b", "ab", Some('!'), true),
        ("_", "\u{1F986}", None, false),
        ("__", "\u{1F986}", None, false), // default collation
        ("[a-c]", "b", None, true),
        ("[a-c]", "B", None, true), // default collation
        ("[^a-c]", "b", None, false),
        ("[a-]", "-", None, false),
        ("[c-a]", "b", None, false),
        ("[%]", "a", None, false),
        ("[%]", "%", None, true),
        ("[_]", "_", None, true),
        ("[[]", "[", None, true),
        ("[^]", "^", None, true),
        ("[]", "a", None, false),
        ("[]", "", None, false),
        ("[!a]", "a", Some('!'), true),
        ("[!a]", "!", Some('!'), false),
        ("[!]]", "]", Some('!'), true),
        ("a]", "a]", None, true),
        ("[^]", "a", None, true),
        ("[^]", "", None, false),
        ("[a-]", "a", None, false),
        ("[-a]", "-", None, true),
        ("[-a]", "a", None, true),
        ("[a-c-e]", "d", None, false),
        ("[a-c-e]", "-", None, true),
        ("[a-c-e]", "e", None, true),
        ("[^^]", "^", None, false),
        ("[^^]", "a", None, true),
        ("[]a]", "a", None, false),
        ("[]a]", "]", None, false),
        ("a[", "a[", None, false),
        ("a[", "a", None, false),
        ("[ab]]", "a]", None, true),
        ("%[", "x", None, false),
        ("x%", "x", None, true),
        ("%", "", None, true),
        ("_", "", None, false),
        ("[a-a]", "a", None, true),
        ("[z-a]", "a", None, false),
        ("[z-a]", "z", None, false),
        ("[--/]", ".", None, true),
        ("[a^]", "^", None, true),
        ("[^a^]", "^", None, false),
        ("[[]]", "[]", None, true),
        ("[a", "[", None, false),
        ("ab%%", "ab", None, true),
        ("%b%", "abc", None, true),
        ("a%c", "abbbc", None, true),
        ("a%c", "abbbcd", None, false),
        ("a%%", "a%", Some('%'), true),
        ("a%_", "a_", Some('%'), true),
        ("a%b", "ab", Some('%'), true),
        ("a%", "a", Some('%'), false),
        ("a[[", "a[", Some('['), true),
        ("a[%", "a%", Some('['), true),
        ("[[", "[", Some('['), true),
        ("[a]", "a]", Some('['), true),
        ("[a]]]", "a", Some(']'), false),
        ("[a]]]", "]", Some(']'), false),
        ("a]%", "a%", Some(']'), true),
        ("[a]", "a", Some(']'), false),
        ("[a!-c]", "b", Some('!'), false),
        ("[a!-c]", "-", Some('!'), true),
        ("[!^a]", "^", Some('!'), true),
        ("[!^a]", "b", Some('!'), false),
        ("[^!a]", "a", Some('!'), false),
        ("[^!a]", "b", Some('!'), true),
        ("[a-!c]", "b", Some('!'), true),
        ("!", "", Some('!'), false),
        ("a!", "a", Some('!'), false),
        ("[a!", "a", Some('!'), false),
        ("!!", "!", Some('!'), true),
        ("[^a]", "a", Some('^'), true),
        ("[^a]", "b", Some('^'), false),
        ("[^a]", "^", Some('^'), false),
        ("[a-c]", "b", Some('-'), false),
        ("[a-c]", "-", Some('-'), false),
        ("[a-c]", "c", Some('-'), true),
        ("a", "A", None, true), // default collation
        ("[A-C]", "b", None, true),
        ("[^A-C]", "b", None, false),
        ("F_O%", "foo bar", None, true),
    ];

    #[test]
    fn matches_sql_server_unicode_like() {
        for (pattern, value, escape, expected) in CASES {
            assert_eq!(
                like(value, pattern, *escape),
                *expected,
                "{value:?} LIKE {pattern:?} ESCAPE {escape:?}"
            );
        }
    }

    #[test]
    fn ignorable_units_vanish_and_ranges_follow_base_letters() {
        // Surrogates, NUL and U+FEFF are ignored in values and patterns.
        assert!(like("\u{1F986}", "", None));
        assert!(!like("\u{1F986}", "_", None));
        assert!(like("a\u{0}b", "ab", None));
        assert!(like("ab", "a\u{FEFF}b", None));
        assert!(like("x\u{1F986}", "%\u{1F986}", None));
        // Letters with diacritics sort next to their base letter.
        assert!(like("\u{100}", "[a-x]", None));
        assert!(like("é", "[a-f]", None));
        assert!(!like("é", "[f-z]", None));
        assert!(like("\u{E000}", "[^a-x]", None));
    }

    #[test]
    fn backtracking_handles_repeated_prefixes() {
        assert!(like("aaab", "%aab", None));
        assert!(like("abcabcabd", "%abc%abd", None));
        assert!(!like("abcabcab", "%abc%abd", None));
        assert!(like("a", "%%%a%%", None));
    }
}
