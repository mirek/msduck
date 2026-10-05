// This test imports the new pure module by path while lib.rs belongs to an
// independent worker. The eventual export uses this same production source.
#[path = "../src/rpc_output.rs"]
mod rpc_output;
mod parameter {
    pub use msduck_sql::parameter::Parameter;
}

use msduck_core::{
    character::{CharacterType, Family, Length},
    types::{DecimalType, Type},
    value::{Decimal, Value},
};
use parameter::Parameter;
use rpc_output::{Declaration, Received, bind};
use std::collections::HashMap;

fn declaration<'a>(name: &'a str, data_type: Type, output: bool) -> Declaration<'a> {
    Declaration {
        name,
        data_type,
        output,
    }
}
fn received<'a>(name: &'a str, status: u8, value: &Value) -> Received<'a> {
    Received {
        name,
        status,
        parameter: Parameter {
            value: value.clone(),
            data_type: Type::Int,
        },
    }
}

#[test]
fn named_outputs_follow_rpc_order_and_keep_declared_types() {
    let z = Value::Int(1);
    let n = Value::Int(5);
    let a = Value::Int(2);
    let declarations = [
        declaration("@a", Type::Int, true),
        declaration("@n", Type::Int, false),
        declaration("@z", Type::Int, true),
    ];
    let bound = bind(
        &declarations,
        &[
            received("z", 1, &z),
            received("@N", 0, &n),
            received("A", 1, &a),
        ],
    )
    .unwrap();
    assert_eq!(
        bound
            .outputs
            .iter()
            .map(|slot| slot.name.as_str())
            .collect::<Vec<_>>(),
        ["@z", "@a"]
    );
    assert_eq!(
        bound
            .outputs
            .iter()
            .map(|slot| slot.ordinal)
            .collect::<Vec<_>>(),
        [0, 2]
    );
    assert_eq!(bound.bindings["@n"].value, Value::Int(5));

    let mut final_variables = bound.bindings.clone();
    final_variables.get_mut("@a").unwrap().value = Value::Int(7);
    final_variables.get_mut("@z").unwrap().value = Value::Int(6);
    let values = bound.final_values(&final_variables, true).unwrap();
    assert_eq!(
        values
            .iter()
            .map(|entry| entry.value.clone())
            .collect::<Vec<_>>(),
        [Value::Int(6), Value::Int(7)]
    );
    assert!(
        bound
            .final_values(&final_variables, false)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn positional_slots_use_their_rpc_ordinal_and_mixed_aliases_cannot_steal_one() {
    let value = Value::Int(1);
    let declarations = [
        declaration("@first", Type::Int, false),
        declaration("@second", Type::Int, true),
    ];
    let bound = bind(
        &declarations,
        &[received("", 0, &value), received("", 1, &value)],
    )
    .unwrap();
    assert_eq!(bound.outputs[0].name, "@second");
    assert!(
        bind(
            &declarations,
            &[received("second", 1, &value), received("", 1, &value)]
        )
        .unwrap_err()
        .contains("duplicate RPC parameter")
    );
}

#[test]
fn max_outputs_follow_small_outputs_by_declaration_even_for_small_values() {
    let max_unicode = Type::Character(CharacterType::new(Family::Nvarchar, Length::Max).unwrap());
    let max_binary = Type::Binary(msduck_core::types::BinaryType::new(false, Length::Max).unwrap());
    let declarations = [
        declaration("@first", max_unicode, true),
        declaration("@middle", Type::Int, true),
        declaration("@last", max_binary, true),
    ];
    let unicode = Value::Text("a".into());
    let number = Value::Int(7);
    let bytes = Value::Blob(vec![1]);
    let bound = bind(
        &declarations,
        &[
            received("first", 1, &unicode),
            received("middle", 1, &number),
            received("last", 1, &bytes),
        ],
    )
    .unwrap();
    assert_eq!(
        bound
            .outputs
            .iter()
            .map(|slot| slot.name.as_str())
            .collect::<Vec<_>>(),
        ["@first", "@middle", "@last"]
    );
    let values = bound.final_values(&bound.bindings, true).unwrap();
    assert_eq!(
        values
            .iter()
            .map(|item| (item.slot.name.as_str(), item.slot.ordinal))
            .collect::<Vec<_>>(),
        [("@middle", 1), ("@first", 0), ("@last", 2)]
    );
}

#[test]
fn null_empty_raw_unicode_binary_and_decimal_survive_planning() {
    let unicode_type =
        Type::Character(CharacterType::new(Family::Nvarchar, Length::Bounded(4)).unwrap());
    let binary_type =
        Type::Binary(msduck_core::types::BinaryType::new(false, Length::Bounded(4)).unwrap());
    let decimal_type = Type::Decimal(DecimalType::new(9, 2).unwrap());
    let declarations = [
        declaration("@u", unicode_type, true),
        declaration("@b", binary_type, true),
        declaration("@d", decimal_type, true),
    ];
    let null = Value::Null;
    let bound = bind(
        &declarations,
        &[
            received("", 1, &null),
            received("", 1, &null),
            received("", 1, &null),
        ],
    )
    .unwrap();
    let mut final_variables = bound.bindings.clone();
    final_variables.get_mut("@u").unwrap().value = Value::Unicode(vec![0xd800, 90]);
    final_variables.get_mut("@b").unwrap().value = Value::Blob(vec![]);
    final_variables.get_mut("@d").unwrap().value =
        Value::Decimal(Decimal::new(9, 2, 123456789).unwrap());
    let values = bound.final_values(&final_variables, true).unwrap();
    assert_eq!(values[0].value, Value::Unicode(vec![0xd800, 90]));
    assert_eq!(values[1].value, Value::Blob(vec![]));
    assert_eq!(
        values[2].value,
        Value::Decimal(Decimal::new(9, 2, 123456789).unwrap())
    );
    assert_eq!(values[0].slot.data_type, unicode_type);
    final_variables.get_mut("@u").unwrap().value = Value::Null;
    assert_eq!(
        bound.final_values(&final_variables, true).unwrap()[0].value,
        Value::Null
    );
}

#[test]
fn bad_counts_names_directions_statuses_and_unsupported_output_types_fail_closed() {
    let value = Value::Int(1);
    let declarations = [declaration("@p", Type::Int, true)];
    assert!(bind(&declarations, &[]).is_err());
    assert!(
        bind(&declarations, &[received("other", 1, &value)])
            .unwrap_err()
            .contains("not declared")
    );
    assert!(
        bind(&declarations, &[received("p", 0, &value)])
            .unwrap_err()
            .contains("direction")
    );
    for status in [2, 4, 128, 255] {
        assert!(
            bind(&declarations, &[received("p", status, &value)])
                .unwrap_err()
                .contains("unsupported default/encrypted")
        );
    }
    assert!(
        bind(
            &[
                declaration("@p", Type::Int, false),
                declaration("p", Type::Int, true)
            ],
            &[received("p", 0, &value), received("p", 1, &value)]
        )
        .unwrap_err()
        .contains("duplicate parameter declaration")
    );
    assert!(
        bind(
            &[declaration("@p", Type::Float, true)],
            &[received("p", 1, &value)]
        )
        .unwrap_err()
        .contains("unsupported RPC OUTPUT declaration")
    );
}

#[test]
fn missing_or_retyped_final_variable_fails_but_uncaught_error_emits_nothing() {
    let value = Value::Null;
    let bound = bind(
        &[declaration("@p", Type::Int, true)],
        &[received("p", 1, &value)],
    )
    .unwrap();
    let mut final_variables = HashMap::new();
    assert!(
        bound
            .final_values(&final_variables, true)
            .unwrap_err()
            .contains("missing final")
    );
    assert!(
        bound
            .final_values(&final_variables, false)
            .unwrap()
            .is_empty()
    );
    final_variables.insert(
        "@p".into(),
        Parameter {
            value: Value::Int(3),
            data_type: Type::BigInt,
        },
    );
    assert!(
        bound
            .final_values(&final_variables, true)
            .unwrap_err()
            .contains("changed its declared type")
    );
}
