//! SQL Server's Unicode LIKE over UTF-16 code units, under msduck's default
//! binary comparison (case-sensitive, code-unit ranges).
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
            Token::Unit(expected) => *expected == unit,
            Token::Set { negated, ranges } => {
                ranges
                    .iter()
                    .any(|(low, high)| (*low..=*high).contains(&unit))
                    != *negated
            }
        }
    }
}

/// A parsed pattern; `None` when it can match nothing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Pattern(Option<Vec<Token>>);

impl Pattern {
    pub(crate) fn parse(pattern: &[u16], escape: Option<u16>) -> Self {
        Self(tokens(pattern, escape))
    }

    pub(crate) fn matches(&self, value: &[u16]) -> bool {
        let Some(tokens) = &self.0 else {
            return false;
        };
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

    /// (pattern, value, escape, SQL Server's result), from the reference capture.
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
        ("__", "\u{1F986}", None, true),
        ("[a-c]", "b", None, true),
        ("[a-c]", "B", None, false),
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
        ("a", "A", None, false),
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
    fn sets_and_wildcards_see_surrogate_units() {
        let duck: Vec<u16> = "\u{1F986}".encode_utf16().collect();
        let set = |units: &[u16]| {
            let mut pattern = vec![b'[' as u16];
            pattern.extend_from_slice(units);
            pattern.push(b']' as u16);
            pattern
        };
        assert!(Pattern::parse(&set(&duck), None).matches(&duck[..1]));
        assert!(!Pattern::parse(&set(&duck), None).matches(&duck));
        let range = set(&[0xD800, b'-' as u16, 0xDFFF]);
        assert!(Pattern::parse(&range, None).matches(&[0xDAC0]));
        let mut suffix = vec![b'%' as u16];
        suffix.push(duck[1]);
        assert!(Pattern::parse(&suffix, None).matches(&duck));
    }

    #[test]
    fn backtracking_handles_repeated_prefixes() {
        assert!(like("aaab", "%aab", None));
        assert!(like("abcabcabd", "%abc%abd", None));
        assert!(!like("abcabcab", "%abc%abd", None));
        assert!(like("a", "%%%a%%", None));
    }
}
