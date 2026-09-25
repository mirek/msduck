#[path = "../src/merge_output.rs"]
mod merge_output;

use merge_output::{
    ActionKind, Cell, Descriptor, Destination, Expression, OutputColumn, Projected, Projection,
    ProjectionError, SelectedAction, SinkColumn, project,
};
use serde_json::{Value, json};

#[derive(Clone, Debug, Eq, PartialEq)]
struct Decl {
    ty: String,
    max_bytes: Option<u64>,
    nullable: bool,
    collation: Option<Value>,
}

fn fixture() -> Value {
    serde_json::from_str(include_str!("../../../reference/merge-execution.json")).unwrap()
}

fn observed(name: &str) -> Value {
    fixture()["runs"][0]
        .as_array()
        .unwrap()
        .iter()
        .find(|case| case["name"] == name)
        .unwrap()["result"]
        .clone()
}

fn decl(column: &Value) -> Decl {
    Decl {
        ty: column["type"].as_str().unwrap().to_owned(),
        max_bytes: column["length"].as_u64(),
        nullable: column["flags"].as_u64().unwrap() & 1 == 1,
        collation: column["collation"]
            .as_object()
            .map(|_| column["collation"].clone()),
    }
}

fn col(
    name: &str,
    expression: Expression,
    descriptor: Descriptor<Decl, Value>,
) -> OutputColumn<Decl, Value> {
    OutputColumn {
        name: name.to_owned(),
        expression,
        descriptor,
    }
}

fn action_col(reference: &Value, name: &str) -> OutputColumn<Decl, Value> {
    assert_eq!(reference["type"], "NVarChar");
    assert_eq!(reference["length"], 20);
    col(
        name,
        Expression::Action,
        Descriptor::Action {
            max_utf16_units: 10,
            collation: reference["collation"].clone(),
        },
    )
}

fn field_col(reference: &Value, expression: Expression) -> OutputColumn<Decl, Value> {
    col(
        reference["name"].as_str().unwrap(),
        expression,
        Descriptor::Field(decl(reference)),
    )
}

fn row(
    kind: ActionKind,
    deleted: Option<Vec<Option<Value>>>,
    inserted: Option<Vec<Option<Value>>>,
) -> SelectedAction<Value> {
    SelectedAction {
        kind,
        deleted,
        inserted,
        source: None,
        evaluated: vec![],
    }
}

fn image(id: i32, n: i32) -> Vec<Option<Value>> {
    vec![Some(json!(id)), Some(json!(n))]
}

fn sql_rows(rows: &[Vec<Cell<Value>>]) -> Vec<Vec<Value>> {
    rows.iter()
        .map(|row| {
            row.iter()
                .map(|cell| match cell {
                    Cell::Value(value) => value.clone(),
                    Cell::Action(value) => json!(value),
                    Cell::Null | Cell::Unavailable => Value::Null,
                })
                .collect()
        })
        .collect()
}

fn assert_direct(name: &str, expressions: Vec<Expression>, actions: &[SelectedAction<Value>]) {
    let observation = observed(name);
    let set = &observation["sets"][0];
    let columns = set["columns"].as_array().unwrap();
    let spec = columns
        .iter()
        .zip(expressions)
        .map(|(reference, expression)| match expression {
            Expression::Action => action_col(reference, reference["name"].as_str().unwrap()),
            _ => field_col(reference, expression),
        })
        .collect();
    let Projected::Direct(output) = project(
        Projection {
            columns: spec,
            destination: Destination::Direct,
        },
        actions,
    )
    .unwrap() else {
        panic!("direct OUTPUT must return a result set");
    };
    assert_eq!(
        sql_rows(&output.rows),
        set["rows"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| row.as_array().unwrap().to_vec())
            .collect::<Vec<_>>(),
        "{name}"
    );
    assert_eq!(output.columns.len(), columns.len());
    for ((name, descriptor), reference) in output.columns.iter().zip(columns) {
        assert_eq!(name, reference["name"].as_str().unwrap());
        match descriptor {
            Descriptor::Action {
                max_utf16_units,
                collation,
            } => {
                assert_eq!(*max_utf16_units, 10);
                assert_eq!(reference["type"], "NVarChar");
                assert_eq!(reference["length"], 20);
                assert_eq!(collation, &reference["collation"]);
            }
            Descriptor::Field(field) => assert_eq!(field, &decl(reference)),
        }
    }
}

#[test]
fn three_direct_actions_match_owner_reference_rows_and_logical_descriptors() {
    assert_direct(
        "matched update",
        vec![
            Expression::Action,
            Expression::Inserted(0),
            Expression::Deleted(1),
            Expression::Inserted(1),
        ],
        &[row(
            ActionKind::Update,
            Some(image(1, 10)),
            Some(image(1, 11)),
        )],
    );
    assert_direct(
        "unmatched insert",
        vec![
            Expression::Action,
            Expression::Inserted(0),
            Expression::Inserted(1),
        ],
        &[row(ActionKind::Insert, None, Some(image(4, 40)))],
    );
    assert_direct(
        "by source delete",
        vec![
            Expression::Action,
            Expression::Deleted(0),
            Expression::Deleted(1),
        ],
        &[row(ActionKind::Delete, Some(image(3, 30)), None)],
    );
}

#[test]
fn mixed_output_into_assigns_all_four_actions_without_a_direct_result_set() {
    let reference = observed("mixed output rows");
    let sink = reference["sets"][0]["columns"].as_array().unwrap();
    let update = observed("matched update");
    let deleted = observed("by source delete");
    let columns = vec![
        action_col(&update["sets"][0]["columns"][0], "$action"),
        field_col(&update["sets"][0]["columns"][1], Expression::Inserted(0)),
        field_col(&deleted["sets"][0]["columns"][1], Expression::Deleted(0)),
    ];
    let sink_columns = sink
        .iter()
        .map(|column| SinkColumn {
            name: column["name"].as_str().unwrap().to_owned(),
            declaration: decl(column),
        })
        .collect();
    let actions = [
        row(ActionKind::Update, Some(image(1, 10)), Some(image(1, 11))),
        row(ActionKind::Delete, Some(image(2, 20)), None),
        row(ActionKind::Insert, None, Some(image(4, 44))),
        row(ActionKind::Delete, Some(image(3, 30)), None),
    ];
    let Projected::Into(output) = project(
        Projection {
            columns,
            destination: Destination::Into(sink_columns),
        },
        &actions,
    )
    .unwrap() else {
        panic!("OUTPUT INTO must not yield a direct result set");
    };
    assert_eq!(output.columns.len(), 3);
    for (column, reference) in output.columns.iter().zip(sink) {
        assert_eq!(column.name, reference["name"]);
        assert_eq!(column.declaration, decl(reference));
    }
    let mut actual = output
        .assignments
        .iter()
        .map(|assignment| {
            assert_eq!(
                assignment
                    .values
                    .iter()
                    .map(|(name, _)| name.as_str())
                    .collect::<Vec<_>>(),
                ["action", "inserted_id", "deleted_id"]
            );
            sql_rows(&[assignment
                .values
                .iter()
                .map(|(_, value)| value.clone())
                .collect()])
            .remove(0)
        })
        .collect::<Vec<_>>();
    actual.sort_by_key(|row| {
        (
            row[0].as_str().unwrap().to_owned(),
            row[2].as_i64().unwrap_or_default(),
        )
    });
    let expected = reference["sets"][0]["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row.as_array().unwrap().to_vec())
        .collect::<Vec<_>>();
    assert_eq!(actual, expected);
    assert!(matches!(
        output.assignments[1].values[1].1,
        Cell::Unavailable
    ));
    assert!(matches!(
        output.assignments[2].values[2].1,
        Cell::Unavailable
    ));
}

#[test]
fn empty_results_keep_declarations_and_nullable_images_stay_distinct() {
    let action = OutputColumn {
        name: "action".into(),
        expression: Expression::Action,
        descriptor: Descriptor::<Decl, Value>::Action {
            max_utf16_units: 10,
            collation: json!("SQL_Latin1_General_CP1_CI_AS"),
        },
    };
    let Projected::Direct(empty): Projected<Decl, Value, Value> = project(
        Projection {
            columns: vec![action],
            destination: Destination::Direct,
        },
        &[],
    )
    .unwrap() else {
        panic!("expected direct output")
    };
    assert!(empty.rows.is_empty());
    assert!(matches!(
        empty.columns[0].1,
        Descriptor::Action {
            max_utf16_units: 10,
            ..
        }
    ));

    let declaration = Decl {
        ty: "IntN".into(),
        max_bytes: Some(4),
        nullable: true,
        collation: None,
    };
    let projected = project(
        Projection {
            columns: vec![
                col(
                    "null_inserted",
                    Expression::Inserted(1),
                    Descriptor::Field(declaration.clone()),
                ),
                col(
                    "absent_deleted",
                    Expression::Deleted(1),
                    Descriptor::Field(declaration),
                ),
            ],
            destination: Destination::Direct,
        },
        &[row(
            ActionKind::Insert,
            None,
            Some(vec![Some(json!(1)), None]),
        )],
    )
    .unwrap();
    let Projected::Direct(output) = projected else {
        panic!("expected direct output")
    };
    assert_eq!(output.rows[0], [Cell::Null, Cell::Unavailable]);
    assert!(output.rows[0].iter().all(Cell::is_sql_null));
}

#[test]
fn ordered_evaluated_slots_and_storage_converted_image_are_used_as_supplied() {
    let text_decl = Decl {
        ty: "NVarChar".into(),
        max_bytes: Some(4),
        nullable: false,
        collation: Some(json!("database-collation")),
    };
    let selected = SelectedAction {
        kind: ActionKind::Update,
        deleted: Some(vec![Some(json!("abcdef"))]),
        inserted: Some(vec![Some(json!("ab"))]),
        source: Some(vec![Some(json!("source"))]),
        evaluated: vec![Some(json!("expression-1")), Some(json!("expression-2"))],
    };
    let Projected::Direct(output) = project(
        Projection {
            columns: vec![
                col(
                    "second",
                    Expression::Evaluated(1),
                    Descriptor::Field(text_decl.clone()),
                ),
                col(
                    "inserted",
                    Expression::Inserted(0),
                    Descriptor::Field(text_decl.clone()),
                ),
                col(
                    "source",
                    Expression::Source(0),
                    Descriptor::Field(text_decl.clone()),
                ),
                col(
                    "first",
                    Expression::Evaluated(0),
                    Descriptor::Field(text_decl),
                ),
            ],
            destination: Destination::Direct,
        },
        &[selected],
    )
    .unwrap() else {
        panic!("expected direct output")
    };
    assert_eq!(
        sql_rows(&output.rows),
        vec![vec![
            json!("expression-2"),
            json!("ab"),
            json!("source"),
            json!("expression-1")
        ]]
    );
}

#[test]
fn malformed_bindings_fail_before_exposing_partial_assignments() {
    let int_decl = Decl {
        ty: "Int".into(),
        max_bytes: None,
        nullable: false,
        collation: None,
    };
    let spec = || Projection {
        columns: vec![col(
            "id",
            Expression::Inserted(0),
            Descriptor::Field(int_decl.clone()),
        )],
        destination: Destination::Into(vec![SinkColumn {
            name: "id".into(),
            declaration: int_decl.clone(),
        }]),
    };
    assert_eq!(
        project(spec(), &[row(ActionKind::Update, None, Some(image(1, 2)))]).err(),
        Some(ProjectionError::WrongImage {
            row: 0,
            action: ActionKind::Update
        })
    );
    assert_eq!(
        project(spec(), &[row(ActionKind::Insert, None, Some(vec![]))]).err(),
        Some(ProjectionError::MissingField { row: 0, column: 0 })
    );
    let wrong_arity = Projection {
        columns: spec().columns,
        destination: Destination::<Decl>::Into(vec![]),
    };
    assert_eq!(
        project::<_, Value, Value>(wrong_arity, &[]).err(),
        Some(ProjectionError::SinkArity {
            projected: 1,
            sink: 0
        })
    );
    let wrong_width: Projection<Decl, Value> = Projection {
        columns: vec![col(
            "action",
            Expression::Action,
            Descriptor::Action {
                max_utf16_units: 5,
                collation: json!("database"),
            },
        )],
        destination: Destination::Direct,
    };
    assert_eq!(
        project::<_, _, Value>(wrong_width, &[]).err(),
        Some(ProjectionError::WrongActionWidth { column: 0 })
    );
}
