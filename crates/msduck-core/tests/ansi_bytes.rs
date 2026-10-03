use msduck_core::ansi_bytes::{
    AnsiBytes, AnsiView, ByteError, EncodingIdentity, checked_byte_total,
};

#[test]
fn every_byte_survives_borrowing_ownership_and_explicit_encoding_identity() {
    let bytes: Vec<u8> = (0..=255).collect();
    for encoding in [
        EncodingIdentity::Cp1252,
        EncodingIdentity::Cp1251,
        EncodingIdentity::Utf8,
        EncodingIdentity::Opaque(1252),
    ] {
        let borrowed = AnsiView::new(encoding, &bytes, 256).unwrap();
        assert_eq!(borrowed.encoding(), encoding);
        assert_eq!(borrowed.bytes().as_ptr(), bytes.as_ptr());
        let owned = borrowed.try_to_owned(256).unwrap();
        assert_eq!(owned.view(), borrowed);
        let (actual_encoding, actual_bytes) = owned.into_parts();
        assert_eq!(actual_encoding, encoding);
        assert_eq!(actual_bytes, bytes);
    }
    assert_ne!(EncodingIdentity::Opaque(1252), EncodingIdentity::Cp1252);
}

#[test]
fn retained_malformed_utf8_is_not_decoded_rejected_or_normalized() {
    // Original source bytes from reference/bulk-character-encoding.json.
    // SQL acceptance/errors and Unicode/client replacements are separate rules.
    for bytes in [
        &[0x80][..],
        &[0xc3, 0x28],
        &[0xc0, 0xaf],
        &[0xed, 0xa0, 0x80],
        &[0xf0, 0x9f, 0x92],
    ] {
        let owned = AnsiBytes::from_slice(EncodingIdentity::Utf8, bytes, bytes.len()).unwrap();
        assert_eq!(owned.view().bytes(), bytes);
        assert_eq!(owned.view().encoding(), EncodingIdentity::Utf8);
    }
}

#[test]
fn native_codepage_bytes_are_distinct_from_sql_units_and_client_text() {
    for (encoding, bytes) in [
        (EncodingIdentity::Cp1252, &[0x81, 0x8d, 0x90, 0xff][..]),
        (
            EncodingIdentity::Cp1251,
            &[0xcf, 0xf0, 0xe8, 0xe2, 0xe5, 0xf2],
        ),
        (EncodingIdentity::Utf8, "éΩ🦆".as_bytes()),
    ] {
        assert_eq!(
            AnsiBytes::from_slice(encoding, bytes, bytes.len())
                .unwrap()
                .into_parts(),
            (encoding, bytes.to_vec())
        );
    }
}

#[test]
fn null_and_empty_remain_distinct_at_zero_limits_even_for_opaque_identity() {
    for encoding in [EncodingIdentity::Utf8, EncodingIdentity::Opaque(u32::MAX)] {
        assert_eq!(AnsiView::nullable(encoding, None, 0).unwrap(), None);
        let empty = AnsiView::nullable(encoding, Some(&[]), 0).unwrap().unwrap();
        assert!(empty.bytes().is_empty());
        assert_eq!(empty.encoding(), encoding);
        assert_eq!(empty.try_to_owned(0).unwrap().view(), empty);
    }
}

#[test]
fn moving_owned_bytes_preserves_the_allocation_and_immutable_borrowed_view() {
    let bytes = vec![0xed, 0xa0, 0x80];
    let pointer = bytes.as_ptr();
    let owned = AnsiBytes::from_vec(EncodingIdentity::Utf8, bytes, 3).unwrap();
    let first = owned.view();
    let copied_view = first;
    assert_eq!(first.bytes().as_ptr(), pointer);
    assert_eq!(copied_view, first);
    let (encoding, bytes) = owned.into_parts();
    assert_eq!(encoding, EncodingIdentity::Utf8);
    assert_eq!(bytes.as_ptr(), pointer);
    assert_eq!(bytes, [0xed, 0xa0, 0x80]);
}

#[test]
fn copying_rechecks_current_limits_and_failures_leave_borrowed_input_intact() {
    let input = [0xf0, 0x9f, 0xa6, 0x86];
    let view = AnsiView::new(EncodingIdentity::Utf8, &input, 4).unwrap();
    let error = ByteError::Limit {
        requested: 4,
        maximum: 3,
    };
    assert_eq!(view.try_to_owned(3).unwrap_err(), error);
    assert_eq!(
        AnsiBytes::from_slice(EncodingIdentity::Utf8, &input, 3).unwrap_err(),
        error
    );
    assert_eq!(
        AnsiBytes::from_vec(EncodingIdentity::Utf8, input.to_vec(), 3).unwrap_err(),
        error
    );
    assert_eq!(
        AnsiView::new(EncodingIdentity::Utf8, &input, 0).unwrap_err(),
        ByteError::Limit {
            requested: 4,
            maximum: 0
        }
    );
    assert_eq!(view.bytes(), [0xf0, 0x9f, 0xa6, 0x86]);
    assert_eq!(view.try_to_owned(4).unwrap().view(), view);
}

#[test]
fn aggregate_accounting_handles_overflow_and_limit_boundaries_without_mutation() {
    assert_eq!(checked_byte_total(0, 0, 0), Ok(0));
    assert_eq!(checked_byte_total(7, 3, 10), Ok(10));
    assert_eq!(
        checked_byte_total(usize::MAX, 0, usize::MAX),
        Ok(usize::MAX)
    );
    assert_eq!(
        checked_byte_total(usize::MAX, 1, usize::MAX),
        Err(ByteError::LengthOverflow)
    );
    assert_eq!(
        checked_byte_total(7, 4, 10),
        Err(ByteError::Limit {
            requested: 11,
            maximum: 10
        })
    );
    assert_eq!(
        checked_byte_total(11, 0, 10),
        Err(ByteError::Limit {
            requested: 11,
            maximum: 10
        })
    );
}

#[test]
fn error_and_debug_output_are_bounded_for_large_native_payloads() {
    let bytes = vec![255; 24_000];
    let owned = AnsiBytes::from_slice(EncodingIdentity::Opaque(7), &bytes, bytes.len()).unwrap();
    let debug = format!("{owned:?}");
    assert!(debug.len() < 300);
    assert!(debug.contains("24000"));
    assert!(debug.contains("Opaque(7)"));
    assert_eq!(
        ByteError::LengthOverflow.to_string(),
        "native ANSI byte count overflow"
    );
    assert!(
        ByteError::Limit {
            requested: usize::MAX,
            maximum: 0
        }
        .to_string()
        .len()
            < 100
    );
}
