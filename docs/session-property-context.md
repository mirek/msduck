# SESSIONPROPERTY and session context

The owner's "Sql wrapper" application (tedious, `encrypt: true`, database
`master`) issues these statements after it connects:

1. `select 42`
2. `select cast(sessionproperty('ANSI_NULLS') as int) as ansiNulls, ...` for
   ANSI_NULLS, ANSI_PADDING, ANSI_WARNINGS, ARITHABORT,
   CONCAT_NULL_YIELDS_NULL, QUOTED_IDENTIFIER and NUMERIC_ROUNDABORT
3. `exec sys.sp_set_session_context @key = N'email', @value = null`
4. `select @p as v` with an Int parameter

tedious `execSql` sends each one as an `sp_executesql` RPC. Before this
change, statement 2 failed with 208 (no SESSIONPROPERTY function) and
statement 3 with 137 (the EXEC argument names were read as undeclared
variables). All four now return the same rows, column descriptors and DONE
tokens as SQL Server, both as RPCs and as batches.

## Reference capture

`scripts/capture-session-property-context.mjs` runs 76 observations through
tedious against the pinned SQL Server 2025 image
(`sha256:86cc6144ef39bb0fbed2329e1ad79b13ee82e7b2e4739213a0db0800e668a74a`)
in two fresh databases. The runs must match each other, and the fixture is
retained only then, in `reference/session-property-context.json`. Each record
keeps the decoded rows, column descriptors (type, length, flags, collation),
errors and info messages with their procedure name, and DONE tokens. A
DONEPROC also keeps its RETURNSTATUS, which tedious passes as the third
`doneProc` argument. The capture uses `withReferenceContainer`, so its
containers carry the `msduck.owner` label, and it compares with
`assertSameCapture`. `--write-fixture` never overwrites an existing fixture.
The script exports `observe`, which the client test replays against msduck.

## Observed behavior

### SESSIONPROPERTY

- The result is a nullable `sql_variant` (TDS length 8009, flags 33) with
  base type `int`: 1 for ON and 0 for OFF.
- After tedious logs in, `@@OPTIONS` is 5496. Every option reports 1 except
  NUMERIC_ROUNDABORT, which reports 0.
- The name is case-insensitive and ignores trailing spaces, but not leading
  ones. `N'...'`, variables and RPC parameters work.
- Unknown names, other SET options (`ANSI_NULL_DFLT_ON`), `NULL` and `1`
  return NULL.
- Zero or two arguments raise 174, state 1, class 15:
  `The sessionproperty function requires 1 argument(s).`
- Each `SET <option> OFF` (`ON` for NUMERIC_ROUNDABORT) changes the value in
  the same batch and in later batches. `SET ... ON` restores it.
- A SET inside `sp_executesql` is reverted when the call returns.
- `SET QUOTED_IDENTIFIER` produces no DONE token of its own.
- `CAST(SESSIONPROPERTY(...) AS INT)` is a nullable IntN(4) with flags 33.

### sp_set_session_context

- Arguments bind by position and their names are ignored.
  `@value = N'x', @key = N'named'` stores key `x`.
- In a batch, a successful call returns RETURNSTATUS 0 and a DONEPROC. Inside
  `sp_executesql` it ends with a DONEINPROC, with no row count.
- A failure raises a class 16 error with procedure name
  `sys.sp_set_session_context` and returns status 1. The batch continues,
  `@@ERROR` holds the number, and a failing `sp_executesql` returns the error
  number as its status (15664). A failure caught by TRY/CATCH has no
  RETURNSTATUS. `@@ROWCOUNT` survives the call.
- Captured errors:

  | Case | Error |
  |---|---|
  | NULL key | 225 `The parameters supplied for the procedure "sp_set_session_context" are not valid.` |
  | Empty or 129-character key | 15666 `Cannot set key '<key>' in the session context. The size of the key cannot exceed 256 bytes.` |
  | One argument | 16903 `The "sp_set_connection_context" procedure was called with an incorrect number of parameters.` |
  | Four arguments | 16914 `... was called with too many parameters.` |
  | `@read_only = NULL`, `nvarchar(max)` value | 15600 `An invalid parameter or option was specified for procedure 'sp_set_connection_context'.` |
  | Write to a read_only key | 15664 `Cannot set key '<stored key>' in the session context. The key has been set as read_only for this session.` |
  | `1+1` argument | 102 `Incorrect syntax near '+'.`, class 15. This is a compile error, so the batch does not run. |

- `@read_only = 2` locks the key, since the value converts to bit. Values are
  not transactional: a set inside a rolled-back transaction remains.

### SESSION_CONTEXT

- The result is a nullable `sql_variant` (length 8009, flags 33) that keeps
  the value's base type:
  - `42` is `int`.
  - A `bigint` variable is `bigint`.
  - `N'owner@example.com'` is `nvarchar` with MaxLength 34.
  - A tedious NVarChar parameter of 14 characters has MaxLength 28.
  - `'abc'` is `varchar`.
- `CAST(SESSION_CONTEXT(N'email') AS NVARCHAR(100))` is a nullable
  NVarChar(200) with flags 33.
- Key matching: `email` finds `Email` and `email `, but not `EMAIL`. The
  final non-space character must match exactly (see `keys_match`).
- A varchar key or a NULL argument raises 8116, state 1, class 16:
  `Argument data type varchar is invalid for argument 1 of session_context function.`
  No argument raises 174.
- RESETCONNECTION (tedious `connection.reset`) clears every key and
  read_only lock and restores the login SET state.

## Implementation

- `crates/msduck-sql/src/session_function.rs` declares SESSIONPROPERTY and
  SESSION_CONTEXT as one-argument functions with a `sql_variant` result.
- `session_function/session_context.rs` holds the deterministic rules:
  - `SessionOptions::property` for option names
  - `SessionContext`, the per-session store with the key-matching,
    read_only, key-size and byte-bound rules
  - `set_call` and `bind`, which read `EXEC sp_set_session_context`
    positionally, check arity and key/value/read_only types, and return the
    captured errors
  - `positional_arguments`, which drops argument names at parse time
  - `validate_set_calls`, which raises the compile-time 102
- `src/engine/session_context.rs` is the root adapter. `Session` owns the
  store, and RESETCONNECTION replaces the session, which clears it. Calls are
  lowered to synthetic, typed, bound variables, so values never enter SQL
  text.
  - Under an explicit CAST or CONVERT to a non-variant type, the stored
    base-type value is bound directly, as SQL Server converts from the base
    type. A missing value becomes an untyped NULL.
  - Elsewhere the value is wrapped in `CAST(... AS SQL_VARIANT)`.
- `src/engine.rs` runs `sp_set_session_context` in the batch loop with the
  completion tokens above. It lowers the functions wherever it already
  lowers `DB_NAME`: statements, prepared validation and scalar evaluation.
  SESSIONPROPERTY reads the live ANSI_WARNINGS state. msduck refuses every
  other `SET <option> OFF`, so their login values are the live state.

## Tests

- `crates/msduck-sql/tests/session_property_context.rs` covers the pure rules.
- `tests/session_property_context.rs` runs the owner's statements and the
  session state over TDS with tiberius, both as batches and as
  `sp_executesql`.
- `tests/session_property_context.test.mjs` runs the owner's probe through
  tedious `execSql`. It also replays every fixture observation against
  msduck:
  - 23 of the 76 records, including all seven owner-statement records (four RPCs and three batches), must be identical.
  - Each record with a known difference names it. Only that difference is
    patched, and the rest must still match.
  - Refused cases must fail with an explicit error.
  - A known gap that starts to match fails the test, so the list stays
    current.

  CI does not run this file yet: `.github/workflows/ci.yml` belongs to
  another claimed task. The Rust tests run in CI.

## Remaining gaps

- **Bare nvarchar/varchar session values.** msduck's `sql_variant` carrier
  holds only integer base types. `SELECT SESSION_CONTEXT(N'email')` over an
  nvarchar value, and every `varchar` value, raise an explicit
  "unsupported" error. The value can be read with an explicit
  CAST/CONVERT. Integer values and NULL work everywhere.
- **Flags on `sql_variant` results.** If a SELECT has a bare `sql_variant`
  column, msduck's projection inference drops that result's column facts, so
  every column reports flags 1 instead of 33. This also happens on main for
  `CAST(1 AS SQL_VARIANT)`. The owner's statement 2 casts to INT and is not
  affected.
- **Other SET options.** `@@OPTIONS`, and `SET <option> OFF` for options
  other than ANSI_WARNINGS (and NUMERIC_ROUNDABORT ON), are unsupported, so
  SESSIONPROPERTY never reports those states.
- **Existing defect.** `CAST(SQL_VARIANT_PROPERTY(v, 'BaseType') AS NVARCHAR)`
  returns the internal struct text instead of `int`.
- **Procedure name.** msduck diagnostics have no procedure name, so the
  procName is empty where SQL Server sends `sys.sp_set_session_context`, and
  `ERROR_PROCEDURE()` is NULL.
- **Not supported:**
  - `EXEC @status = procedure` (sqlparser rejects it)
  - `DEFAULT` arguments
  - numeric constants outside `int`
  - value types other than bit, the integer types and bounded nvarchar
  - `sp_set_session_context` as a direct RPC by name
  - session functions inside views, defaults, routines and triggers (refused)
- **Extra DONE token.** msduck sends a DONE for `SET QUOTED_IDENTIFIER`,
  which SQL Server does not.
- **Approximate size limit.** SQL Server's 1 MB budget uses internal
  allocation sizes that were not derived. msduck uses a plain 1 MiB byte sum
  and raises 15665 with the captured message.
- **Collations.** Key matching was captured only under
  `SQL_Latin1_General_CP1_CI_AS`, and msduck applies Unicode lowercase for
  non-ASCII keys.

Other session-context reference evidence is on the unmerged
`work/session-context-reference-v1` branch (`session-context-reference-v1`).
This task does not use or edit its files.
