#[path = "../src/merge_action.rs"]
mod merge_action;

use merge_action::{
    ActionKind, Arm, ArmEvaluation, Candidate, EvaluatedCandidate, PlanError, Relation, Row,
    Selection, select_actions,
};
use serde_json::{Value, json};

#[derive(Debug, PartialEq, Eq)]
struct Image {
    id: i32,
    n: i32,
}

type Input = EvaluatedCandidate<u64, u64, Image, Image>;
type TestCandidate = Candidate<u64, u64, Image, Image>;

fn image(id: i32, n: i32) -> Image {
    Image { id, n }
}

fn target(identity: u64, id: i32, n: i32) -> Row<u64, Image> {
    Row {
        id: identity,
        image: image(id, n),
    }
}

fn source(identity: u64, id: i32, n: i32) -> Row<u64, Image> {
    Row {
        id: identity,
        image: image(id, n),
    }
}

fn applied(new_target: Option<Image>) -> ArmEvaluation<Image> {
    ArmEvaluation::Apply { new_target }
}

fn fixture_case(name: &str) -> Value {
    let fixture: Value =
        serde_json::from_str(include_str!("../../../reference/merge-execution.json")).unwrap();
    fixture["runs"][0]
        .as_array()
        .unwrap()
        .iter()
        .find(|case| case["name"] == name)
        .unwrap()
        .clone()
}

#[test]
fn three_basic_action_families_keep_prewrite_and_postwrite_images() {
    let cases: [(ActionKind, TestCandidate, Option<Image>, &str); 3] = [
        (
            ActionKind::Update,
            Candidate::Matched {
                target: target(101, 1, 10),
                source: source(201, 1, 11),
            },
            Some(image(1, 11)),
            "matched update",
        ),
        (
            ActionKind::Insert,
            Candidate::SourceOnly {
                source: source(204, 4, 40),
            },
            Some(image(4, 40)),
            "unmatched insert",
        ),
        (
            ActionKind::Delete,
            Candidate::TargetOnly {
                target: target(103, 3, 30),
            },
            None,
            "by source delete",
        ),
    ];
    for (kind, candidate, new_target, reference) in cases {
        let relation = candidate.relation();
        let plan = select_actions(
            &[Arm {
                relation,
                action: kind,
            }],
            vec![Input {
                candidate,
                arms: vec![applied(new_target)],
            }],
            Selection::All,
        )
        .unwrap();
        let captured = fixture_case(reference);
        assert_eq!(
            plan.affected_rows(),
            captured["result"]["rowCount"].as_u64().unwrap() as usize
        );
        assert_eq!(
            plan.actions[0].kind.sql_name(),
            captured["result"]["sets"][0]["rows"][0][0]
        );
        let action = &plan.actions[0];
        match kind {
            ActionKind::Update => {
                assert_eq!(action.target_id, Some(101));
                assert_eq!(action.source_id, Some(201));
                assert_eq!(action.old_target, Some(image(1, 10)));
                assert_eq!(action.new_target, Some(image(1, 11)));
                assert_eq!(action.source_image, Some(image(1, 11)));
            }
            ActionKind::Insert => {
                assert_eq!(action.target_id, None);
                assert_eq!(action.old_target, None);
                assert_eq!(action.new_target, Some(image(4, 40)));
            }
            ActionKind::Delete => {
                assert_eq!(action.source_id, None);
                assert_eq!(action.old_target, Some(image(3, 30)));
                assert_eq!(action.new_target, None);
            }
        }
    }
}

#[test]
fn mixed_conditional_arms_reproduce_four_captured_actions_without_using_output_order() {
    let arms = [
        Arm {
            relation: Relation::Matched,
            action: ActionKind::Update,
        },
        Arm {
            relation: Relation::Matched,
            action: ActionKind::Delete,
        },
        Arm {
            relation: Relation::SourceOnly,
            action: ActionKind::Insert,
        },
        Arm {
            relation: Relation::TargetOnly,
            action: ActionKind::Delete,
        },
    ];
    let candidates = vec![
        Input {
            candidate: Candidate::Matched {
                target: target(101, 1, 10),
                source: source(201, 1, 11),
            },
            arms: vec![
                applied(Some(image(1, 11))),
                ArmEvaluation::NotQualified,
                ArmEvaluation::NotQualified,
                ArmEvaluation::NotQualified,
            ],
        },
        Input {
            candidate: Candidate::Matched {
                target: target(102, 2, 20),
                source: source(202, 2, 22),
            },
            arms: vec![
                ArmEvaluation::NotQualified,
                applied(None),
                ArmEvaluation::NotQualified,
                ArmEvaluation::NotQualified,
            ],
        },
        Input {
            candidate: Candidate::SourceOnly {
                source: source(204, 4, 44),
            },
            arms: vec![
                ArmEvaluation::NotQualified,
                ArmEvaluation::NotQualified,
                applied(Some(image(4, 44))),
                ArmEvaluation::NotQualified,
            ],
        },
        Input {
            candidate: Candidate::TargetOnly {
                target: target(103, 3, 30),
            },
            arms: vec![
                ArmEvaluation::NotQualified,
                ArmEvaluation::NotQualified,
                ArmEvaluation::NotQualified,
                applied(None),
            ],
        },
    ];
    let plan = select_actions(&arms, candidates, Selection::All).unwrap();
    let captured = fixture_case("mixed actions");
    assert_eq!(
        plan.affected_rows(),
        captured["result"]["rowCount"].as_u64().unwrap() as usize
    );
    assert_eq!(plan.affected_rows(), 4);
    assert_eq!(
        plan.actions.iter().map(|a| a.kind).collect::<Vec<_>>(),
        [
            ActionKind::Update,
            ActionKind::Delete,
            ActionKind::Insert,
            ActionKind::Delete
        ]
    );
    let mut output: Vec<Value> = plan
        .actions
        .iter()
        .map(|action| {
            json!([
                action.kind.sql_name(),
                action.new_target.as_ref().map(|row| row.id),
                action.old_target.as_ref().map(|row| row.id)
            ])
        })
        .collect();
    output.sort_by_key(|row| {
        (
            row[0].as_str().unwrap().to_owned(),
            row[1].as_i64().unwrap_or(0),
            row[2].as_i64().unwrap_or(0),
        )
    });
    assert_eq!(
        json!(output),
        fixture_case("mixed output rows")["result"]["sets"][0]["rows"]
    );
    assert_eq!(plan.actions[0].old_target, Some(image(1, 10)));
    assert_eq!(plan.actions[0].new_target, Some(image(1, 11)));
    assert_eq!(plan.actions[3].old_target, Some(image(3, 30)));
    assert_eq!(
        fixture_case("mixed final rows")["result"]["sets"][0]["rows"],
        json!([[1, 11], [4, 44]])
    );
}

#[test]
fn duplicate_source_target_write_is_error_8672_before_any_plan_is_returned() {
    let arm = [Arm {
        relation: Relation::Matched,
        action: ActionKind::Update,
    }];
    let candidates = vec![
        Input {
            candidate: Candidate::Matched {
                target: target(101, 1, 10),
                source: source(201, 1, 11),
            },
            arms: vec![applied(Some(image(1, 11)))],
        },
        Input {
            candidate: Candidate::Matched {
                target: target(101, 1, 10),
                source: source(202, 1, 12),
            },
            arms: vec![applied(Some(image(1, 12)))],
        },
    ];
    let error = select_actions(&arm, candidates, Selection::All).unwrap_err();
    assert_eq!(error, PlanError::DuplicateTarget { target_id: 101 });
    let (number, state, class, message) = error.sql_diagnostic().unwrap();
    let captured = fixture_case("duplicate source error");
    let expected = &captured["result"]["errors"][0];
    assert_eq!(i64::from(number), expected["number"]);
    assert_eq!(u64::from(state), expected["state"]);
    assert_eq!(u64::from(class), expected["class"]);
    assert_eq!(message, expected["message"]);
}

#[test]
fn row_identity_not_equal_values_controls_duplicate_detection() {
    let arms = [Arm {
        relation: Relation::Matched,
        action: ActionKind::Delete,
    }];
    let candidates = vec![
        Input {
            candidate: Candidate::Matched {
                target: target(101, 1, 10),
                source: source(201, 1, 11),
            },
            arms: vec![applied(None)],
        },
        Input {
            candidate: Candidate::Matched {
                target: target(102, 1, 10),
                source: source(202, 1, 12),
            },
            arms: vec![applied(None)],
        },
    ];
    assert_eq!(
        select_actions(&arms, candidates, Selection::All)
            .unwrap()
            .affected_rows(),
        2
    );
}

#[test]
fn explicit_top_subset_cte_source_and_nonqualifying_arm() {
    let arms = [Arm {
        relation: Relation::SourceOnly,
        action: ActionKind::Insert,
    }];
    let make = || {
        vec![
            Input {
                candidate: Candidate::SourceOnly {
                    source: source(201, 1, 10),
                },
                arms: vec![applied(Some(image(1, 10)))],
            },
            Input {
                candidate: Candidate::SourceOnly {
                    source: source(205, 5, 50),
                },
                arms: vec![applied(Some(image(5, 50)))],
            },
        ]
    };
    let zero = select_actions(&arms, make(), Selection::Indices(&[])).unwrap();
    assert_eq!(
        zero.affected_rows(),
        fixture_case("top zero")["result"]["rowCount"]
            .as_u64()
            .unwrap() as usize
    );
    let one = select_actions(&arms, make(), Selection::Indices(&[0])).unwrap();
    assert_eq!(
        one.affected_rows(),
        fixture_case("top one")["result"]["rowCount"]
            .as_u64()
            .unwrap() as usize
    );
    assert_eq!(one.actions[0].new_target, Some(image(1, 10)));
    let cte = select_actions(&arms, make(), Selection::Indices(&[1])).unwrap();
    assert_eq!(
        cte.affected_rows(),
        fixture_case("cte source")["result"]["rowCount"]
            .as_u64()
            .unwrap() as usize
    );
    assert_eq!(cte.actions[0].source_image, Some(image(5, 50)));
    let skipped = select_actions(
        &arms,
        vec![Input {
            candidate: Candidate::SourceOnly {
                source: source(205, 5, 50),
            },
            arms: vec![ArmEvaluation::NotQualified],
        }],
        Selection::All,
    )
    .unwrap();
    assert_eq!(skipped.affected_rows(), 0);
}

#[test]
fn ordered_arms_choose_first_qualifying_action_once() {
    let arms = [
        Arm {
            relation: Relation::Matched,
            action: ActionKind::Update,
        },
        Arm {
            relation: Relation::Matched,
            action: ActionKind::Delete,
        },
    ];
    let plan = select_actions(
        &arms,
        vec![Input {
            candidate: Candidate::Matched {
                target: target(101, 1, 10),
                source: source(201, 1, 11),
            },
            arms: vec![applied(Some(image(1, 11))), applied(None)],
        }],
        Selection::All,
    )
    .unwrap();
    assert_eq!(plan.affected_rows(), 1);
    assert_eq!(plan.actions[0].arm_index, 0);
    assert_eq!(plan.actions[0].kind, ActionKind::Update);
}

#[test]
fn malformed_binder_inputs_are_not_sql_diagnostics() {
    let arms = [Arm {
        relation: Relation::SourceOnly,
        action: ActionKind::Insert,
    }];
    let make = || {
        vec![Input {
            candidate: Candidate::SourceOnly {
                source: source(1, 1, 10),
            },
            arms: vec![applied(Some(image(1, 10)))],
        }]
    };
    assert_eq!(
        select_actions(&arms, make(), Selection::Indices(&[1])).unwrap_err(),
        PlanError::InvalidSelection { candidate_index: 1 }
    );
    assert_eq!(
        select_actions(&arms, make(), Selection::Indices(&[0, 0])).unwrap_err(),
        PlanError::RepeatedSelection { candidate_index: 0 }
    );
    let wrong = select_actions(
        &arms,
        vec![Input {
            candidate: Candidate::SourceOnly {
                source: source(1, 1, 10),
            },
            arms: vec![],
        }],
        Selection::All,
    )
    .unwrap_err();
    assert_eq!(wrong, PlanError::WrongArmCount { candidate_index: 0 });
    assert_eq!(wrong.sql_diagnostic(), None);
}
