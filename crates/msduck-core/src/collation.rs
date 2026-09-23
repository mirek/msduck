//! Collation coercion over explicit, caller-supplied catalog names.
//! Names and comparison weights are resolved by the binder, not by this module.

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Label {
    CoercibleDefault(String),
    Implicit(String),
    Explicit(String),
    /// A deferred conflict. An enclosing explicit COLLATE can resolve it.
    NoCollation {
        left: String,
        right: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Conflict {
    /// A failure within a collation-sensitive operation cannot be repaired by
    /// assigning a different collation to that operation's result.
    Operation(crate::diagnostic::SqlError),
    Explicit {
        left: String,
        right: String,
    },
    /// Different database defaults need the caller's conversion/context rule.
    /// Do not choose one using operand order or confuse this with an implicit
    /// conflict. The current single-database binder normally supplies one name.
    DefaultChoiceRequired {
        left: String,
        right: String,
    },
}

#[derive(Clone, Copy, Debug)]
pub enum Operation {
    Equal,
    NotEqual,
    Less,
    Greater,
    LessEqual,
    GreaterEqual,
    Replace,
    Stuff,
}

impl Operation {
    pub fn resolve(self, labels: &[Label]) -> Result<Option<Label>, Conflict> {
        let name = match self {
            Self::Equal => "equal to",
            Self::NotEqual => "not equal to",
            Self::Less => "less than",
            Self::Greater => "greater than",
            Self::LessEqual => "less than or equal to",
            Self::GreaterEqual => "greater than or equal to",
            Self::Replace => "replace",
            Self::Stuff => "stuff",
        };
        let conflict = |left: &str, right: &str| {
            Conflict::Operation(crate::diagnostic::SqlError::new(
                468,
                9,
                format!(
                    "Cannot resolve the collation conflict between \"{right}\" and \"{left}\" in the {name} operation."
                ),
            ))
        };
        let mut combined: Option<Label> = None;
        for label in labels {
            combined = Some(match combined {
                None => label.clone(),
                Some(previous) => match previous.combine(label) {
                    Err(Conflict::Explicit { left, right }) => return Err(conflict(&left, &right)),
                    result => result?,
                },
            });
        }
        if let Some(Label::NoCollation { left, right }) = &combined {
            if labels
                .iter()
                .any(|label| matches!(label, Label::NoCollation { .. }))
            {
                return Err(Conflict::Operation(crate::diagnostic::SqlError::new(
                    4191,
                    9,
                    format!("Cannot resolve collation conflict for {name} operation."),
                )));
            }
            return Err(conflict(left, right));
        }
        Ok(combined)
    }
}

impl Label {
    /// Combine labels for an expression. A NoCollation result is not itself an
    /// error: the enclosing operator decides whether it can consume that label.
    pub fn combine(&self, other: &Self) -> Result<Self, Conflict> {
        use Label::*;
        match (self, other) {
            (Explicit(left), Explicit(right)) if !same(left, right) => Err(Conflict::Explicit {
                left: left.clone(),
                right: right.clone(),
            }),
            (Explicit(_), _) => Ok(self.clone()),
            (_, Explicit(_)) => Ok(other.clone()),
            (NoCollation { .. }, _) => Ok(self.clone()),
            (_, NoCollation { .. }) => Ok(other.clone()),
            (Implicit(left), Implicit(right)) if !same(left, right) => Ok(NoCollation {
                left: left.clone(),
                right: right.clone(),
            }),
            (Implicit(_), _) => Ok(self.clone()),
            (_, Implicit(_)) => Ok(other.clone()),
            (CoercibleDefault(left), CoercibleDefault(right)) if !same(left, right) => {
                Err(Conflict::DefaultChoiceRequired {
                    left: left.clone(),
                    right: right.clone(),
                })
            }
            (CoercibleDefault(_), CoercibleDefault(_)) => Ok(self.clone()),
        }
    }

    /// The pinned SQL Server accepts nested explicit COLLATE, including a
    /// different outer name. Do not reject this based on the old documentation.
    pub fn collate(&self, name: String) -> Self {
        Self::Explicit(name)
    }

    /// NoCollation has no usable comparison name until an explicit override.
    pub fn name(&self) -> Option<&str> {
        match self {
            Self::CoercibleDefault(name) | Self::Implicit(name) | Self::Explicit(name) => {
                Some(name)
            }
            Self::NoCollation { .. } => None,
        }
    }
}

fn same(left: &str, right: &str) -> bool {
    left.eq_ignore_ascii_case(right)
}

#[cfg(test)]
mod tests {
    use super::*;
    const CI: &str = "Latin1_General_100_CI_AS";
    const CS: &str = "Latin1_General_100_CS_AS";
    const BIN2: &str = "Latin1_General_100_BIN2";

    #[test]
    fn precedence_preserves_deferred_conflicts_and_explicit_overrides() {
        let default = Label::CoercibleDefault("SQL_Latin1_General_CP1_CI_AS".into());
        let ci = Label::Implicit(CI.into());
        let cs = Label::Implicit(CS.into());
        let explicit = Label::Explicit(BIN2.into());
        let conflict = Label::NoCollation {
            left: CI.into(),
            right: CS.into(),
        };
        assert_eq!(ci.combine(&cs), Ok(conflict.clone()));
        assert_eq!(conflict.name(), None);
        for label in [&default, &ci, &cs, &conflict] {
            assert_eq!(label.combine(&explicit), Ok(explicit.clone()));
            assert_eq!(explicit.combine(label), Ok(explicit.clone()));
        }
        for label in [&default, &ci, &cs] {
            assert_eq!(label.combine(&conflict), Ok(conflict.clone()));
            assert_eq!(conflict.combine(label), Ok(conflict.clone()));
        }
        assert_eq!(ci.combine(&default), Ok(ci.clone()));
        assert_eq!(default.combine(&ci), Ok(ci.clone()));
        assert_eq!(
            ci.combine(&Label::Implicit(CI.to_lowercase())),
            Ok(ci.clone())
        );
        assert_eq!(explicit.collate(CS.into()), Label::Explicit(CS.into()));
        assert_eq!(conflict.collate(BIN2.into()), explicit);
    }

    #[test]
    fn explicit_conflicts_keep_operand_names_and_do_not_become_no_collation() {
        let left = Label::Explicit(BIN2.into());
        let right = Label::Explicit(CS.into());
        assert_eq!(
            left.combine(&right),
            Err(Conflict::Explicit {
                left: BIN2.into(),
                right: CS.into()
            })
        );
        assert_eq!(
            left.combine(&Label::Explicit(BIN2.to_lowercase())),
            Ok(left.clone())
        );
        assert_eq!(
            Label::CoercibleDefault(CI.into()).combine(&Label::CoercibleDefault(CS.into())),
            Err(Conflict::DefaultChoiceRequired {
                left: CI.into(),
                right: CS.into()
            })
        );
    }

    #[test]
    fn label_decisions_match_live_precedence_programs() {
        let reference: serde_json::Value =
            serde_json::from_str(include_str!("../../../reference/collation-precedence.json"))
                .unwrap();
        let cases = reference["results"].as_array().unwrap();
        let case = |name: &str| cases.iter().find(|c| c["name"] == name).unwrap();
        let ci = Label::Implicit(CI.into());
        let cs = Label::Implicit(CS.into());
        let unresolved = ci.combine(&cs).unwrap();
        assert!(matches!(unresolved, Label::NoCollation { .. }));
        assert_eq!(
            case("implicit conflict")["reference"]["errors"][0]["number"],
            468
        );
        assert_eq!(
            case("case no collation")["reference"]["errors"][0]["number"],
            451
        );
        assert_eq!(
            case("case comparison conflict")["reference"]["errors"][0]["number"],
            4191
        );
        assert_eq!(unresolved.collate(BIN2.into()).name(), Some(BIN2));
        assert!(
            case("case explicit restores collation")["reference"]["errors"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        // Derived/CTE fields retain the same label; turning it into Implicit
        // would incorrectly allow the outer explicit CS operand to override it.
        for name in [
            "derived explicit versus outer explicit",
            "cte explicit versus outer explicit",
        ] {
            let projected = Label::Explicit(BIN2.into());
            assert!(matches!(
                projected.combine(&Label::Explicit(CS.into())),
                Err(Conflict::Explicit { .. })
            ));
            assert_eq!(case(name)["reference"]["errors"][0]["number"], 468);
        }
        for name in ["nested explicit same", "nested explicit different"] {
            assert!(
                case(name)["reference"]["errors"]
                    .as_array()
                    .unwrap()
                    .is_empty()
            );
        }
    }
}
