# ALTER TABLE column changes

ALTER TABLE ADD translates column types and supports nullable columns,
NOT NULL, and unnamed DEFAULT expressions. Nullable additions leave existing
rows NULL and install the default afterward. NOT NULL additions with a default
populate existing rows before adding the constraint. Without a default,
NOT NULL additions succeed only when the backend can enforce the constraint
(for example, an empty table). DROP COLUMN supports multiple names and IF EXISTS.

An internal transaction groups the generated DDL in autocommit mode. Failure
rolls back every step, including prior drops in a multi-column operation. Inside
an explicit transaction, the caller controls commit/rollback; DuckDB errors
can abort that transaction. Bound/session values in defaults are rejected to
avoid persisting statement-specific constants. Dynamic NEWID defaults remain
backend expressions.

Tedious tests cover old-row NULLs versus new-row defaults, BIT and UUID types,
NOT NULL default population, empty/populated tables, rollback, failed additions,
multi-column failure atomicity, IF EXISTS and unsupported option recovery.
The Rust restart test verifies an added column and its default after reopen.

Remaining work includes named/default/check/foreign-key constraints,
changing identity columns, computed columns, catalog
completeness, dependency validation and SQL Server locking/error/transaction fidelity.
These forms fail explicitly. Existing type-width, collation and temporal limits
also apply to added columns. Backend behavior is not a SQL Server differential
compatibility result.

The mssqlite reference transpiler splits ADD/DROP column operations into SQL
statements. msduck uses the same operation-level approach but adds transaction
grouping and explicitly handles SQL Server's nullable DEFAULT behavior.

Reference: [Microsoft ALTER TABLE](https://learn.microsoft.com/en-us/sql/t-sql/statements/alter-table-transact-sql).

## ALTER COLUMN

T-SQL `ALTER TABLE ... ALTER COLUMN name type [NULL | NOT NULL]` is parsed
into type and nullability operations and executed in the same atomic DDL group.
Omitted nullability makes the column nullable. Explicit NOT NULL checks existing
rows; failure in autocommit mode rolls back the earlier type change. Normal
query type translation also applies here, including BIGINT and NVARCHAR.

Tests verify widening metadata, omitted/explicit nullability, rejection of NULL
rows, restoration after failed conversion/constraint checks, explicit rollback,
missing columns, parser boundaries and recovery. Changes to TINYINT, SMALLINT,
INT and BIGINT use the shared integer conversion path through a generated
USING expression. Existing decimal/float values truncate before range checks;
character integer syntax and empty-space handling match explicit casts. Tests
cover all four widths, quoted column names, exact BIGINT boundaries, NULLs,
failed range/conversion checks, restoration of the original decimal values and
metadata after failure, explicit rollback and persistence after reopening.

Other conversions still use DuckDB rules: SQL Server width checks,
conversion precedence, indexed
column restrictions and all dependency rules are not yet emulated. Transaction
errors inside an explicit user transaction may require caller rollback. These
limits prevent a full ALTER COLUMN compatibility claim.
Stored MONEY provenance and conversion of existing default expressions remain
gaps, as described in [integer conversion](integer-conversion.md).

## Multiple column additions

The dialect parser accepts `ADD a type, b type, ...` as one alteration, using
column-definition parsing so decimal precision/scale and expression commas are
not mistaken for column boundaries. It feeds all additions into the existing
DDL transaction group. Parser tests cover malformed/trailing definitions and
statement boundaries. Client tests cover mixed nullable/NOT NULL defaults,
new-row defaults, duplicate names, a later constraint failure, and explicit
rollback. Autocommit failures leave none of the earlier columns added.

Inside an explicit user transaction, the backend may abort the transaction or
leave prior DDL pending after an error; callers must roll back. SQL Server's
statement-level rollback within a still-usable transaction remains a gap because
DuckDB does not provide native savepoints.

## WITH VALUES

A nullable added column with `DEFAULT ... WITH VALUES` populates existing rows
as well as providing the default for future inserts. The parser retains this
option on its column, including when NULL follows WITH VALUES. Execution removes
the T-SQL clause and keeps the default on DuckDB's ADD COLUMN so existing rows
receive it. Without WITH VALUES, the default is still deferred until after ADD.

Client tests compare filled and unfilled columns in the same statement, future
inserts, per-row NEWID defaults, duplicate/missing-default validation, later
column failure cleanup and rollback. Named default constraints remain unsupported.


## Tables with IDENTITY

Existing ordinary-column ADD, ALTER and DROP operations also work on identity
tables without resetting allocation. DROP of the identity column cleans its
private sequence and definition within the same DDL transaction. Rollback restores
the column, its values and allocator. Failed multi-column drops and external
sequence dependencies roll back in autocommit mode. Metadata functions return
NULL once the identity column is gone; prepared inserts re-resolve the remaining
columns. Native and client tests verify cleanup, rollback, allocation continuity,
primary-key dependency rejection, defaults and prepared reuse.

ADD IDENTITY now shares CREATE TABLE validation and allocates a value for each
existing row. The allocator default remains active for future inserts. Empty
tables start at the seed; existing-row assignment order is unspecified. The
sequence and original definition join the DDL transaction, so overflow during
population or a later column failure rolls back all additions in autocommit mode.
Tests also cover explicit rollback, negative increments, exact BIGINT seeds and
cleanup after failed additions. Adding PRIMARY KEY/named constraints with the
identity column remains unsupported, as does changing an identity column's
existing type/nullability. See [identity coverage](identity.md).


For ADD IDENTITY, a temporary constant seed default allows NOT NULL enforcement
before population. After all schema changes, one UPDATE allocates the final IDs;
all steps share the DDL transaction. This avoids DuckDB's restrictions on NOT NULL
checks over uncommitted updates and keeps populated values materialized in WAL.
A subprocess recovery test verifies stored values and the next allocation after
an unclean exit. No intermediate seed placeholders escape a committed operation.


The T-SQL DROP parser accepts comma-separated column names, quoted names containing
commas, repeated COLUMN groups and IF EXISTS. Missing-column failures roll back
prior drops in autocommit mode, including catalog IDs and declaration metadata.
ADD, DROP and ALTER COLUMN cannot be mixed in one statement; validation rejects
mixed actions before execution. The type and nullability operations generated
for a single ALTER COLUMN remain one valid action. Separate DROP and ADD statements
assign fresh IDs, and explicit rollback restores the original IDs and declarations.
