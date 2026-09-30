use msduck_core::{
    character::{CharacterType, Family, Length},
    types::{DecimalType, Type},
    value::{Decimal, Value},
};
use msduck_sql::percentile::{RANGE, runtime_fraction};

fn character(family: Family) -> Type {
    Type::Character(CharacterType::new(family, Length::Bounded(32)).unwrap())
}

#[test]
fn bound_float_and_real_bits_do_not_follow_scientific_literal_underflow() {
    // Two pinned SQL Server containers retained wire FLOAT/REAL subnormals;
    // negative subnormals produce 8727 rather than becoming a valid zero.
    for value in [
        f64::from_bits(1),
        1e-308,
        0.5,
        f64::from_bits(1.0f64.to_bits() - 1),
        -0.0,
    ] {
        assert_eq!(
            runtime_fraction(&Value::Double(value), Type::Float)
                .unwrap()
                .unwrap()
                .to_bits(),
            value.to_bits()
        );
    }
    for value in [f32::from_bits(1), 1e-40, 0.5, -0.0] {
        assert_eq!(
            runtime_fraction(&Value::Float(value), Type::Real)
                .unwrap()
                .unwrap()
                .to_bits(),
            f64::from(value).to_bits()
        );
    }
    for value in [
        -f64::from_bits(1),
        -1e-308,
        f64::from_bits(1.0f64.to_bits() + 1),
    ] {
        let error = runtime_fraction(&Value::Double(value), Type::Float)
            .unwrap()
            .unwrap_err();
        assert_eq!(
            (
                error.number,
                error.state,
                error.severity,
                error.message.as_str()
            ),
            (8727, 1, 16, RANGE)
        );
    }
}

#[test]
fn exact_decimal_endpoint_rounds_once_before_range_validation() {
    let declaration = Type::Decimal(DecimalType::new(38, 20).unwrap());
    for (coefficient, bits) in [
        (100_000_000_000_000_000_001, 1.0f64.to_bits()),
        (99_999_999_999_999_990_000, 1.0f64.to_bits() - 1),
    ] {
        let value = Value::Decimal(Decimal::new(38, 20, coefficient).unwrap());
        assert_eq!(
            runtime_fraction(&value, declaration)
                .unwrap()
                .unwrap()
                .to_bits(),
            bits
        );
    }
}

#[test]
fn captured_character_source_diagnostics_and_nul_prefix_are_retained() {
    for family in [Family::Varchar, Family::Nvarchar] {
        let declaration = character(family);
        for (source, number, state) in [("abc", 8114, 5), ("1e309", 8115, 2), ("1.1", 8727, 1)] {
            let error = runtime_fraction(&Value::Text(source.into()), declaration)
                .unwrap()
                .unwrap_err();
            assert_eq!(
                (error.number, error.state, error.severity),
                (number, state, 16)
            );
            if number == 8114 {
                assert_eq!(
                    error.message,
                    format!(
                        "Error converting data type {} to float.",
                        if family == Family::Varchar {
                            "varchar"
                        } else {
                            "nvarchar"
                        }
                    )
                );
            }
        }
        for source in ["", "  ", "\u{180e}.5", ".5\0ignored"] {
            assert!(
                runtime_fraction(&Value::Text(source.into()), declaration)
                    .unwrap()
                    .is_ok()
            );
        }
        let error = runtime_fraction(&Value::Null, declaration)
            .unwrap()
            .unwrap_err();
        assert_eq!((error.number, error.state), (8727, 1));
    }
    assert_eq!(
        runtime_fraction(
            &Value::Unicode(vec![48, 46, 53, 0, 0xd800]),
            character(Family::Nvarchar)
        )
        .unwrap()
        .unwrap(),
        0.5
    );
    assert!(runtime_fraction(&Value::Unicode(vec![0xd800]), character(Family::Nvarchar)).is_none());
}

#[test]
fn unknown_declarations_and_mismatched_carriers_are_barriers() {
    for (value, declaration) in [
        (Value::Double(0.5), Type::Real),
        (Value::Float(0.5), Type::Float),
        (Value::Text(".5".into()), Type::Float),
        (Value::Double(f64::NAN), Type::Float),
        (Value::Null, Type::Variant),
        (Value::TinyInt(-1), Type::TinyInt),
        (Value::Unicode(vec![48]), character(Family::Varchar)),
        (
            Value::Decimal(Decimal::new(8, 4, 5000).unwrap()),
            Type::Decimal(DecimalType::new(9, 4).unwrap()),
        ),
    ] {
        assert!(
            runtime_fraction(&value, declaration).is_none(),
            "{value:?} {declaration:?}"
        );
    }
}

#[test]
fn captured_ordering_diagnostics_precede_execution_fraction_conversion() {
    use msduck_sql::{
        binding_scope::Scope,
        percentile::{PlanError, runtime_plan},
    };
    use sqlparser::parser::Parser;
    for (kind, ordering, number, message) in [
        (
            "CONT",
            "1",
            5308,
            "Windowed functions, aggregates and NEXT VALUE FOR functions do not support integer indices as ORDER BY clause expressions.",
        ),
        (
            "DISC",
            "1",
            5308,
            "Windowed functions, aggregates and NEXT VALUE FOR functions do not support integer indices as ORDER BY clause expressions.",
        ),
        (
            "CONT",
            "CAST(NULL AS VARCHAR(10))",
            402,
            "The data types numeric and varchar are incompatible in the percentile_cont operator.",
        ),
        (
            "CONT",
            "CAST(NULL AS DATETIME)",
            402,
            "The data types numeric and datetime are incompatible in the percentile_cont operator.",
        ),
        (
            "CONT",
            "CAST(NULL AS DATETIME2)",
            402,
            "The data types numeric and datetime2 are incompatible in the percentile_cont operator.",
        ),
        (
            "CONT",
            "CAST(NULL AS INT)",
            5309,
            "Windowed functions, aggregates and NEXT VALUE FOR functions do not support constants as ORDER BY clause expressions.",
        ),
    ] {
        let sql = format!("PERCENTILE_{kind}(.5) WITHIN GROUP(ORDER BY {ordering}) OVER()");
        let expr = Parser::new(&msduck_sql::dialect::ServerDialect)
            .try_with_sql(&sql)
            .unwrap()
            .parse_expr()
            .unwrap();
        let before = expr.clone();
        let Err(PlanError::Diagnostic(error)) = runtime_plan(&expr, &Scope::default()) else {
            panic!("{sql}")
        };
        assert_eq!(
            (
                error.number,
                error.state,
                error.severity,
                error.message.as_str()
            ),
            (number, 1, 16, message)
        );
        assert_eq!(expr, before);
    }
}
