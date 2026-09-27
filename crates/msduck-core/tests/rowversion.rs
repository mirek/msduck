#[path = "../src/rowversion.rs"]
mod rowversion;

use rowversion::{DatabaseCounter, RowVersion, RowVersionError};

fn allocate(counter: &mut DatabaseCounter) -> RowVersion {
    let (next, value) = counter.allocate().unwrap();
    *counter = next;
    value
}

#[test]
fn captured_cross_table_sequence_keeps_the_counter_outside_row_transactions() {
    // PR #383, reference/rowversion.json: four identical SQL Server 2025 runs.
    let initial = [0, 0, 0, 0, 0, 0, 0x07, 0xd0];
    let mut counter = DatabaseCounter::from_bytes(&initial).unwrap();
    assert_eq!(counter.current().bytes(), initial); // DDL and reads

    let first_table = allocate(&mut counter);
    assert_eq!(first_table.bytes(), [0, 0, 0, 0, 0, 0, 0x07, 0xd1]);
    let second_table = allocate(&mut counter);
    assert_eq!(second_table.bytes(), [0, 0, 0, 0, 0, 0, 0x07, 0xd2]);
    let noop_update = allocate(&mut counter);
    assert_eq!(noop_update.bytes(), [0, 0, 0, 0, 0, 0, 0x07, 0xd3]);
    assert_eq!(counter.current(), noop_update); // zero-row UPDATE does not allocate

    assert_eq!(allocate(&mut counter).bytes()[7], 0xd4); // nullable declaration
    assert_eq!(allocate(&mut counter).bytes()[7], 0xd5); // TIMESTAMP synonym
    assert_eq!(counter.current().bytes()[7], 0xd5); // rejected explicit writes

    let rolled_back_row = allocate(&mut counter);
    assert_eq!(rolled_back_row.bytes()[7], 0xd6);
    // The row is discarded by the effectful transaction adapter; counter is not.
    assert_eq!(counter.current(), rolled_back_row);
    assert_eq!(allocate(&mut counter).bytes()[7], 0xd7);
    assert_eq!(allocate(&mut counter).bytes()[7], 0xd8); // RPC no-op UPDATE

    let copied_rowversion = noop_update; // SELECT INTO copies bytes without allocation
    assert_eq!(copied_rowversion.bytes()[7], 0xd3);
    assert_eq!(counter.current().bytes()[7], 0xd8);
}

#[test]
fn rejects_wrong_width_without_truncating_or_padding() {
    for width in [0, 7, 9, 16] {
        let bytes = vec![0xff; width];
        assert_eq!(
            RowVersion::from_bytes(&bytes),
            Err(RowVersionError::WrongLength { actual: width })
        );
        assert_eq!(
            DatabaseCounter::from_bytes(&bytes),
            Err(RowVersionError::WrongLength { actual: width })
        );
    }
    assert_eq!(
        RowVersion::from_bytes(&[0xff; 8]).unwrap().bytes(),
        [0xff; 8]
    );
}

#[test]
fn counter_uses_big_endian_checked_arithmetic() {
    let zero = DatabaseCounter::from_bytes(&[0; 8]).unwrap();
    assert_eq!(zero.allocate().unwrap().1.bytes(), [0, 0, 0, 0, 0, 0, 0, 1]);

    let before_carry = DatabaseCounter::from_bytes(&[0, 0, 0, 0, 0, 0, 0xff, 0xff]).unwrap();
    assert_eq!(
        before_carry.allocate().unwrap().1.bytes(),
        [0, 0, 0, 0, 0, 1, 0, 0]
    );

    let maximum = DatabaseCounter::from_bytes(&[0xff; 8]).unwrap();
    assert_eq!(maximum.allocate(), Err(RowVersionError::CounterExhausted));
    assert_eq!(maximum.current().bytes(), [0xff; 8]);
}
