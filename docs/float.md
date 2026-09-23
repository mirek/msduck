# FLOAT and REAL precision

FLOAT without a precision and FLOAT(25) through FLOAT(53) translate to DuckDB
DOUBLE and use eight-byte floating-point results. FLOAT(1) through FLOAT(24)
translate to REAL and use four-byte results. REAL stays single precision;
DOUBLE PRECISION stays double precision. Precision outside 1 through 53, or
precision with a scale, is rejected before executing the affected statement.

The shared type translator applies these rules to casts, typed local variables,
prepared parameter declarations, CREATE TABLE, ALTER TABLE ADD and ALTER COLUMN.
NULL and empty results retain the chosen width. Stored values and defaults
retain it after reopening the database. Widening an existing REAL column cannot
recover precision already lost when its values were stored.

The copied mssqlite data-type skill supplied the precision buckets. Tedious
tests use 16777217, which distinguishes single from double precision, to check
values and wire widths across these paths. Tiberius independently reads f32
and f64 results. A restart test verifies a double-precision stored value.

Existing databases created with the former FLOAT mapping are not migrated:
those columns retain their stored backend type until explicitly altered.
Full SQL Server overflow/underflow rules, non-finite value restrictions,
conversion formatting, exact diagnostics and differential validation remain
unfinished. Floating-point values are approximate.

Reference: [Microsoft FLOAT and REAL](https://learn.microsoft.com/en-us/sql/t-sql/data-types/float-and-real-transact-sql).
