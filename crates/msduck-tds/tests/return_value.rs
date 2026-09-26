use msduck_tds::{
    COLLATION,
    collation::Collation,
    return_value::{self, Declaration, Parameter, Status, Value},
};

fn parameter<'a>(declaration: Declaration, value: Value<'a>) -> Parameter<'a> {
    Parameter {
        ordinal: 0,
        name: &[],
        status: Status::Output,
        user_type: 0,
        flags: 1,
        declaration,
        value,
    }
}
fn encode(parameter: &Parameter<'_>) -> Vec<u8> {
    let mut out = Vec::new();
    return_value::encode(&mut out, parameter).unwrap();
    out
}

#[test]
fn prepared_handle_remains_the_exact_int_returnvalue_vector() {
    let mut old = Vec::new();
    msduck_tds::return_handle(&mut old, "@handle", 42);
    let expected = [
        0xac, 0, 0, 6, b'h', 0, b'a', 0, b'n', 0, b'd', 0, b'l', 0, b'e', 0, 1, 0, 0, 0, 0, 1, 0,
        0x26, 4, 4, 42, 0, 0, 0,
    ];
    assert_eq!(old, expected);
    let name: Vec<_> = "handle".encode_utf16().collect();
    let mut p = parameter(Declaration::Int(4), Value::Int(42));
    p.name = &name;
    assert_eq!(encode(&p), expected);
}

#[test]
fn ordinal_status_flags_and_null_integer_are_encoded_in_spec_order() {
    let mut p = parameter(Declaration::Int(8), Value::Null);
    p.ordinal = 0x1234;
    p.status = Status::UdfReturn;
    p.user_type = 0x0102_0304;
    p.flags = 0x0041;
    p.name = &[0xd83d];
    assert_eq!(
        encode(&p),
        [
            0xac, 0x34, 0x12, 1, 0x3d, 0xd8, 2, 4, 3, 2, 1, 0x41, 0, 0x26, 8, 0
        ]
    );
}

#[test]
fn integer_widths_and_decimal_sign_magnitude_are_exact() {
    for (width, number, payload) in [
        (1, 255, vec![255]),
        (2, -2, vec![254, 255]),
        (4, -2, vec![254, 255, 255, 255]),
        (8, i64::MIN, i64::MIN.to_le_bytes().to_vec()),
    ] {
        let bytes = encode(&parameter(Declaration::Int(width), Value::Int(number)));
        assert_eq!(&bytes[11..14], &[0x26, width, width]);
        assert_eq!(&bytes[14..], payload);
    }
    let bytes = encode(&parameter(
        Declaration::Decimal {
            precision: 5,
            scale: 2,
            max_length: 17,
        },
        Value::Decimal(-12_345),
    ));
    assert_eq!(
        bytes,
        [
            0xac, 0, 0, 0, 1, 0, 0, 0, 0, 1, 0, 0x6a, 17, 5, 2, 5, 0, 0x39, 0x30, 0, 0
        ]
    );
    let compact = encode(&parameter(
        Declaration::Decimal {
            precision: 5,
            scale: 2,
            max_length: 5,
        },
        Value::Decimal(-12_345),
    ));
    assert_eq!(compact[12], 5);
    assert_eq!(&compact[13..], &bytes[13..]);
    let zero = encode(&parameter(
        Declaration::Decimal {
            precision: 1,
            scale: 0,
            max_length: 17,
        },
        Value::Decimal(0),
    ));
    assert_eq!(&zero[15..], &[5, 1, 0, 0, 0, 0]);
}

#[test]
fn unicode_units_and_character_collation_are_lossless() {
    let p = parameter(
        Declaration::Unicode {
            units: Some(2),
            fixed: false,
            collation: Collation::default(),
        },
        Value::Unicode(&[0xd800, 0x0061]),
    );
    let bytes = encode(&p);
    assert_eq!(
        &bytes[11..],
        &[
            0xe7,
            4,
            0,
            COLLATION[0],
            COLLATION[1],
            COLLATION[2],
            COLLATION[3],
            COLLATION[4],
            4,
            0,
            0,
            0xd8,
            0x61,
            0
        ]
    );
    let null = encode(&parameter(
        Declaration::Unicode {
            units: Some(2),
            fixed: false,
            collation: Collation::default(),
        },
        Value::Null,
    ));
    assert_eq!(&null[19..], &[255, 255]);
    let ansi = encode(&parameter(
        Declaration::Ansi {
            bytes: Some(2),
            fixed: false,
            collation: Collation::default(),
        },
        Value::Bytes(&[0xe9, b'x']),
    ));
    assert_eq!(
        &ansi[11..],
        &[
            0xa7,
            2,
            0,
            COLLATION[0],
            COLLATION[1],
            COLLATION[2],
            COLLATION[3],
            COLLATION[4],
            2,
            0,
            0xe9,
            b'x'
        ]
    );
}

#[test]
fn plp_binary_and_unicode_distinguish_null_empty_and_bytes() {
    let binary = Declaration::Binary {
        bytes: None,
        fixed: false,
    };
    let null = encode(&parameter(binary, Value::Null));
    assert_eq!(
        &null[11..],
        &[0xa5, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255]
    );
    let empty = encode(&parameter(binary, Value::Bytes(&[])));
    assert_eq!(
        &empty[11..],
        &[0xa5, 255, 255, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]
    );
    let value = encode(&parameter(binary, Value::Bytes(&[1, 2])));
    assert_eq!(
        &value[11..],
        &[
            0xa5, 255, 255, 2, 0, 0, 0, 0, 0, 0, 0, 2, 0, 0, 0, 1, 2, 0, 0, 0, 0
        ]
    );
    let unicode = Declaration::Unicode {
        units: None,
        fixed: false,
        collation: Collation::default(),
    };
    let value = encode(&parameter(unicode, Value::Unicode(&[0xd800])));
    assert_eq!(
        &value[19..],
        &[2, 0, 0, 0, 0, 0, 0, 0, 2, 0, 0, 0, 0, 0xd8, 0, 0, 0, 0]
    );
}

#[test]
fn invalid_declarations_and_values_leave_existing_output_unchanged() {
    let invalid = [
        parameter(Declaration::Int(3), Value::Int(1)),
        parameter(Declaration::Int(1), Value::Int(-1)),
        parameter(Declaration::Int(4), Value::Int(i64::from(i32::MAX) + 1)),
        parameter(Declaration::Bit, Value::Int(1)),
        parameter(
            Declaration::Decimal {
                precision: 2,
                scale: 3,
                max_length: 17,
            },
            Value::Decimal(1),
        ),
        parameter(
            Declaration::Decimal {
                precision: 2,
                scale: 0,
                max_length: 17,
            },
            Value::Decimal(100),
        ),
        parameter(
            Declaration::Decimal {
                precision: 10,
                scale: 2,
                max_length: 5,
            },
            Value::Decimal(1),
        ),
        parameter(
            Declaration::Ansi {
                bytes: Some(0),
                fixed: false,
                collation: Collation::default(),
            },
            Value::Bytes(b"x"),
        ),
        parameter(
            Declaration::Binary {
                bytes: Some(1),
                fixed: true,
            },
            Value::Bytes(b""),
        ),
        parameter(
            Declaration::Unicode {
                units: Some(1),
                fixed: false,
                collation: Collation::default(),
            },
            Value::Unicode(&[1, 2]),
        ),
        parameter(
            Declaration::Unicode {
                units: None,
                fixed: true,
                collation: Collation::default(),
            },
            Value::Null,
        ),
    ];
    for p in invalid {
        let mut out = vec![0xaa, 0xbb];
        assert!(return_value::encode(&mut out, &p).is_err(), "{p:?}");
        assert_eq!(out, [0xaa, 0xbb]);
    }
    let long_name = vec![b'a' as u16; 256];
    let mut p = parameter(Declaration::Int(4), Value::Int(1));
    p.name = &long_name;
    let mut out = vec![0xaa];
    assert!(return_value::encode(&mut out, &p).is_err());
    assert_eq!(out, [0xaa]);
    p.name = &[];
    p.flags = 0x0800;
    assert!(return_value::encode(&mut out, &p).is_err());
    assert_eq!(out, [0xaa]);
}
