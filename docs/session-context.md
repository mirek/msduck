# SESSION_CONTEXT reference

`reference/session-context.json` retains 227 ordered SQL Server observations of
`SESSION_CONTEXT` and `sp_set_session_context`. Each one keeps its decoded rows,
full column descriptors, error and info tokens, DONE tokens and return status.
`scripts/capture-session-context.mjs` ran against the pinned SQL Server 2025
image `sha256:86cc6144ef39bb0fbed2329e1ad79b13ee82e7b2e4739213a0db0800e668a74a`.
It used two fresh databases in each of two independent containers, and all
four captures were identical. The fixture SHA-256 is
`587fa27b8ec62b641369c7c3a61cf562e02c45cfc4f75a80f4903ec0fe9d37e6`.
The fixture contains tedious-decoded values and TDS descriptors, not raw TDS
bytes. tedious turns a `sql_variant` into a JavaScript value, so every probe
also reads `SQL_VARIANT_PROPERTY` (`BaseType`, `Precision`, `Scale`,
`TotalBytes`, `Collation`, `MaxLength`). Exact numeric and temporal text is
retained through `CONVERT`.

## Capture design

Session context belongs to one connection. The capture therefore has 14
scenarios, and each opens its own labelled tedious connections (`a`, `b`, ...)
to the fresh database and closes them at the end, so no scenario sees state
left by another. The step kinds are:

- 192 SQL batches
- 18 RPC procedure calls to `sp_set_session_context` by name
  (`callProcedure`)
- 9 parameterized `sp_executesql` calls
- 3 prepared sequences with 16 executions in total
- 3 tedious `connection.reset` calls
- 1 explicit close and 1 explicit open

The prepared capture mirrors the fixed `capturePrepared` semantics of PR #312
(`docs/reference-captures.md` on `work/prepared-capture-helper-v1`), which is
not on main yet:

- Preparation completes through the `prepared`/`error` events.
- `request.error` is cleared before each phase, and an error object already
  reported for an earlier phase is not reported again.
- Rows and messages are bounded.

When that helper lands on main, this script should import it instead of
keeping its own copy.

Batches and RPCs use a local capture with the record shape of
`lib/compatibility.mjs` `capture`. It also stores `status` on every DONEPROC
entry: the RETURNSTATUS value that preceded that token, or null when there was
none. tedious passes that value with each DONEPROC and then clears it, so every
`EXEC` in a batch keeps its own status. The top-level `returnStatus` is only
the last one. Rows are bounded at 10000 per result set, and messages plus DONE
tokens at 2000 per batch (200 per prepared phase). The fill loops emit about
760 DONE tokens, and validation rejects any truncated record.

The script refuses, before any container starts, an output path that resolves
to the retained fixture. Symlinks in the existing part of the path are
resolved, so the check also covers a file that does not exist yet. This mirrors
`refuseFixtureOutput` from PR #312.

Every RPC, `sp_executesql` and prepared statement text ends with a unique
`/*case name*/` comment. Named constraints use explicit names (`pk_ctx_rows`,
`df_ctx_rows_tenant`, `df_ctx_rows_raw`). The script compares captures with
`assertSameCapture` and never passes a whole capture to `node:assert`.
`--write-fixture` calls `refuseExistingFixture` before any container starts and
writes with `writeNewFixture`. `--one-database` is a diagnostic mode that never
writes or compares the fixture. Runs used `node --max-old-space-size=2048`
under an RSS watchdog with a 4 GB kill limit. Peak RSS was about 134 MB.

The server and database collation were `SQL_Latin1_General_CP1_CI_AS`.

## Function surface

- `SESSION_CONTEXT(key)` returns a nullable `sql_variant`. The TDS descriptor
  is `Variant` with length 8009, flags 33 and no collation, including unnamed
  columns. `sp_describe_first_result_set` reports `sql_variant`,
  `system_type_id` 98 and `max_length` 8016. `SELECT ... INTO` creates a
  nullable `sql_variant` column with `max_length` 8016.
- A missing key returns NULL for the value and for every `SQL_VARIANT_PROPERTY`.
  `N''` and 129-character keys can be read and return NULL.
- The argument must be a Unicode character type. `nvarchar` literals,
  `sysname`, `nchar(10)` and `nvarchar(max)` variables are accepted, and so are
  an `nvarchar(max)` RPC parameter and a NULL `nvarchar` RPC parameter.
  `NULL`, `varchar` (as a literal, variable or RPC parameter) and `int` raise
  8116/state 1/class 16 with the message `Argument data type <type> is invalid
  for argument 1 of session_context function.` In `sp_executesql` the RPC
  return status is 8116.
- Zero or two arguments raise 174/class 15: `The session_context function
  requires 1 argument(s).`
- `sp_set_session_context` is an `X`/`EXTENDED_STORED_PROCEDURE` with no rows
  in `sys.all_parameters`. `sys.sp_set_session_context` and
  `master.sys.sp_set_session_context` both work.

## Setting values

- Arguments bind by position, and parameter names are ignored.
  `@name=N'bad', @value=1` stores key `bad`. `@value=6, @key=N'reordered'`
  passes 6 as the key and fails with 225.
- Success returns status 0 (`EXEC @r = ...` gives 0). Every failure returns 1,
  and the batch continues after the statement-level error (`@@ERROR` is the
  error number). Each `EXEC` has its own DONEPROC status. For example,
  `read_only NULL` records `[1, 0]` and `promote writable key` records
  `[0, 0, 1]`. When a failure is caught by TRY/CATCH, the failing call's
  DONEPROC has no RETURNSTATUS (`status` null).
- `EXEC` arguments must be constants or variables. `1+1` and `GETDATE()` fail
  with 102 (syntax).
- The value keeps the declared base type of its variable, with type facets:
  - `int` 42: precision 10, TotalBytes 6, MaxLength 4
  - `decimal(10,3)`: TotalBytes 9, MaxLength 5
  - `numeric(38,0)`: TotalBytes 21, MaxLength 17. The exact text
    `12345678901234567890123456789012345678` is preserved.
  - `real` and `float`: precision 24 and 53
  - `money` and `smallmoney`: precision 19 and 10, scale 4
  - `nvarchar(20)`: MaxLength 40 and the database collation
  - `char(5)` and `nchar(5)`: padded
  - `binary(4)`: zero-padded
  - `date`, `time(3)`, `datetime`, `smalldatetime`, `datetime2(7)`,
    `datetimeoffset(2)` and `uniqueidentifier`: their own types. The
    `datetime2(7)` value keeps all 7 digits, and `datetimeoffset` keeps its
    `+05:30` offset.
  - A `sql_variant` variable holding an `int` stores base type `int`.
  - `nvarchar(4000)`, `varchar(8000)` and `varbinary(8000)` at full length are
    accepted.
- A `COLLATE` clause on a variable initializer does not survive. The variable,
  and therefore the stored value, has the database collation.
- `nvarchar(max)`, `varchar(max)`, `varbinary(max)`, `xml`, `json`,
  `hierarchyid` and `vector(2)` values fail with 15600/state 1/class 16. The
  message is `An invalid parameter or option was specified for procedure
  'sp_set_connection_context'.` It names the internal procedure. An RPC
  `nvarchar(max)` parameter fails the same way.
- Constant typing:

  | Constant | Stored type |
  |---|---|
  | `42`, `-5` | `int` |
  | `2147483648` | `numeric(10,0)` |
  | `1.50` | `numeric(3,2)` |
  | `1e1` | `float` |
  | `$1.5` | `money` |
  | `'abc'` | `varchar`, MaxLength 3 |
  | `N'abc'` | `nvarchar`, MaxLength 6 |
  | `0x0A0B` | `varbinary`, MaxLength 2 |

  `NULL` and `DEFAULT` values succeed and read back as NULL.
- RPC parameter types become the stored base type:

  | tedious parameter | Stored type |
  |---|---|
  | `BigInt` | `bigint` |
  | `NVarChar` | `nvarchar` with the declared length |
  | `VarChar` | `varchar` |
  | `Decimal(10,3)` | `decimal` |
  | `Float` | `float` |
  | `Bit` | `bit` |
  | `VarBinary` | `varbinary` |
  | `UniqueIdentifier` | `uniqueidentifier` |
  | `Date` | `date` |
  | `DateTime2` scale 3 | `datetime2`, precision 23, scale 3 |
  | `DateTimeOffset` scale 3 | `datetimeoffset`, precision 30, scale 3 |

  A prepared tedious `NVarChar` declares `nvarchar(4000)`, and its value is
  stored with MaxLength 8000.
- Overwriting changes the base type (`int` 1, then `nvarchar` `two`). A NULL
  can be written and then overwritten.
- Missing arguments raise 16903 and a fourth argument raises 16914. Both
  messages name `sp_set_connection_context`.
- `@read_only` is converted to `bit`, so 2 means read-only. `NULL` and `N'yes'`
  raise 15600 and the key is not set.

## Keys

- Keys up to 128 characters work. `N''` and 129-character keys, whether passed
  as a literal or through an `nvarchar(200)` variable, fail with 15666. There
  is no truncation. The message quotes the whole key and says the key size
  cannot exceed 256 bytes. A `varchar` literal key is accepted for storage (its
  value reads back through `N'vk'`), and so is an RPC `VarChar` key.
- A NULL key (literal, variable or prepared parameter), an `int` key or a
  `DEFAULT` key fails with 225/state 1/class 16: `The parameters supplied for
  the procedure "sp_set_session_context" are not valid.`
- The key comparison is not a plain collation comparison. Every
  key-comparison probe fits this rule: two keys match when they are equal
  under the database
  collation (case- and trailing-space-insensitive, accent-sensitive) **and**
  their final non-space character is identical by case. Examples for a stored
  key:

  | Stored key | Matches | Does not match |
  |---|---|---|
  | `abc` | `Abc`, `aBc`, `ABc`, `abc `, `ABc  ` | `ABC`, `abC`, `abC `, ` abc`, `ábc` |
  | `CaseKey` | `casekey`, `caseKey` | `CASEKEY` |
  | `hello` | `HELLo`, `HeLLo` | `hellO`, `HELLO` |
  | `q` | | `Q` |
  | `aB` | `AB` | `Ab`, `ab` |
  | `key1` | `KEY1`, `Key1` | |
  | `XYZ` | | `xyz`, `Xyz` |

  Non-matching spellings are separate keys. With `ro` read-only, setting `RO`
  succeeds, while `Ro` and `ro ` fail with 15664. `rO` then reads the `RO`
  value. The 15664 message quotes the stored spelling (`'ro'`), not the one
  supplied. The rule is inferred from these probes. It suggests a
  case-sensitive hash of the final character in front of a collation
  comparison, but that mechanism was not observed directly.

## read_only

- After `@read_only = 1`, any later set of that key fails with 15664/state
  1/class 16: `Cannot set key '<stored key>' in the session context. The key
  has been set as read_only for this session.` This covers a new value, the
  same value, `@read_only = 0` and NULL, and the value is unchanged. A key
  first set to NULL with `@read_only = 1` is also locked. A writable key can be
  promoted to read-only by a later call with `@read_only = 1`.
- In TRY/CATCH: `ERROR_NUMBER` 15664, severity 16, state 1,
  `ERROR_PROCEDURE` `sp_set_session_context`, line 1.
- Under `XACT_ABORT ON` inside a transaction, 15664 aborts the batch and rolls
  back the transaction (`@@TRANCOUNT` 0). The read-only value is unchanged.
- In `sp_executesql` the RPC return status is 15664. For a prepared
  `sp_execute` the return status is 0. A direct RPC or `EXEC` returns 1.

## Size limit

- Each connection holds about 1 MB of keys and values. With `varchar(8000)`
  values and keys `fill0`...`fill124`, 125 values are stored. The 126th fails
  with 15665/state 1/class 16: `The value was not set for key 'fill125' because
  the total size of keys and values in the session context would exceed the 1
  MB limit.` With `nvarchar(4000)` values in TRY/CATCH the cutoff is also 125.
- At the limit a small `int` key still fits. Shrinking a key and then setting
  another key to NULL frees enough for a new 8000-byte value, and an existing
  key can then grow back to 8000 bytes.
- Accounting after 125 × 8000 values with 4-character keys:
  - A 1-character key fits a `varchar` of at most 3976 bytes, and this is
    repeatable.
  - A 10-character key fits 3940 bytes, which is 4 bytes per extra key
    character.
  - An `nvarchar` value fits 1988 characters (3976 bytes).
  - After an extra `int` key the maximum drops to 3624. Setting that key to
    NULL drops it further to 3276, and adding a new NULL key raises it again to
    3616.

  So the accounting is internal allocation, not a simple sum. It was
  identical in all four captures, but no formula is derived.

## Scope and transactions

- Values persist across batches. They are not transactional: a set inside a
  rolled-back transaction remains, and a set before `THROW` in a failed batch
  remains, while statements after the THROW do not run.
- Procedures, `sp_executesql`, triggers and scalar functions (which may `EXEC`
  the extended procedure) set values that the caller sees. They also read the
  caller's values. Views, inline functions and scalar functions read the
  current session's value.
- `DEFAULT CAST(SESSION_CONTEXT(N'tenant') AS INT)` and a `sql_variant`
  default evaluate per insert: NULL before the key is set, then 12 as `int`.
- `sql_variant` arithmetic (`SESSION_CONTEXT(N'k') + 1`) and implicit
  assignment to `int` or `nvarchar` variables raise 257/state 3/class 16.
  Comparisons with `sql_variant` values work: `int` 42 equals 42, does not
  equal `N'42'`, and a `bigint` value compares greater than an `int` value.
  An explicit `CAST` of `nvarchar` text to `int` raises 245.

## RPC and prepared calls on one connection

- A direct RPC call to `sp_set_session_context` sets values that a later
  `sp_executesql` read and a later SQL batch both see. Its return status is 0,
  or 1 with the same error tokens as `EXEC`.
- The first prepared sequence runs 7 executions:
  1. `p1` = 1
  2. `p1` = 2 with `@ro = 1`
  3. `p1` = 3, which fails with 15664
  4. `p2` = NULL
  5. `p3` = 4
  6. a NULL key, which fails with 225
  7. `p4` = 6

  Only executions 3 and 6 carry error tokens, and later executions have none,
  so no error is stale. `sp_prepare` of that `EXEC` text returns status 8182,
  while `sp_prepare` of the `SELECT` read returns 0 with its column descriptor
  and no rows. A prepared read sees values set by earlier prepared, RPC and
  batch calls.

## Connections and reset

- A second connection sees none of the first connection's keys. It can set the
  same key, even as read-only, without affecting the first. The first
  connection's read-only flag does not block the second, and the reverse also
  holds. A connection opened after another closes starts empty.
- tedious `connection.reset` sets the RESETCONNECTION status bit on its
  initial SET-options batch. The reset emitted one `resetConnection` event and
  info 5703 (`Changed language setting to us_english.`). Afterwards:
  - every key reads NULL
  - a former read-only key can be set again
  - an open transaction is gone (`@@TRANCOUNT` 0)
  - the full 1 MB budget is available again (125 × 8000 bytes)
  - other connections are unaffected

## Gaps

- Only `SQL_Latin1_General_CP1_CI_AS` was observed. Key matching under other
  server or database collations, supplementary characters and combining marks
  was not captured. The final-character rule is an inference.
- The size accounting has no derived formula. Only the observed maxima are
  retained.
- The capture does not cover:
  - `USE` to another database
  - `EXECUTE AS` and impersonation
  - row-level security predicates
  - MARS
  - parallel plans
  - `sp_reset_connection` from other clients or connection pools
  - `CONTEXT_INFO`
- tedious cannot send `sql_variant` RPC parameters, so the capture has no RPC
  call whose value parameter is typed `sql_variant`.
- Raw TDS `sql_variant` bytes are not retained, only decoded values plus
  `SQL_VARIANT_PROPERTY`.
- The meaning of `sp_prepare` status 8182 is unexplained. It is retained raw.

## Proposed successors

This task changes no runtime behavior. A search of `src/` and `crates/` found
no `SESSION_CONTEXT`, `sp_set_session_context` or RESETCONNECTION handling.
msduck implements none of the behavior above.

1. **session-context-core (deterministic crates).**
   - In `msduck-core`, add a pure per-session context store. It holds typed
     `sql_variant` values with base type facets and read-only flags, and
     implements the observed key-matching rule behind an explicit collation
     input.
   - The core also validates value and key types and returns exact error
     values: 15600, 15664 with the stored key spelling, 15665, 15666, 225,
     16903 and 16914.
   - Size accounting stays an explicit, documented policy until a formula is
     captured. It must not be presented as exact.
   - In `msduck-sql`, bind `EXEC sp_set_session_context` arguments by
     position (ignoring names, rejecting expressions and handling `DEFAULT`),
     type constants as in the table above, and bind `SESSION_CONTEXT`
     arguments (8116, 174) with a nullable `sql_variant` descriptor of length
     8009.
2. **session-context-root.**
   - In the root crate, own one store per `Session` in `src/engine.rs`. It
     survives batches, transaction rollback and batch abort, and is shared
     with procedures, triggers, functions and dynamic SQL.
   - Serve `sp_set_session_context` through batch `EXEC`, RPC-by-name,
     `sp_executesql` and prepared calls with the observed return statuses.
   - Clear the store on the TDS RESETCONNECTION status bit and on close.
   - Encode values as TDS `sql_variant`.
   - Replay this fixture through tedious as the client check.
