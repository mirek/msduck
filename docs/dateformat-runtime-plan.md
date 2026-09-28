# DATEFORMAT runtime plan

The [retained SQL Server 2025 capture](../reference/dateformat.json) has 43
observations from each of two independent containers. Its
[capture notes](dateformat.md) distinguish six orders, four target types,
execute-time changes, two sessions, `SET LANGUAGE`, invalid-setting recovery,
and reuse of one prepared handle. Microsoft's
[SET DATEFORMAT specification](https://learn.microsoft.com/en-us/sql/t-sql/statements/set-dateformat-transact-sql?view=sql-server-ver17)
defines the orders and states that the setting applies at execution time. The
fixture supplies the precise rows, descriptors, errors, and wire events used
below. This is an implementation plan, not a claim that msduck implements it.

## Current path and gap

| Boundary | Current source behavior | Consequence |
| --- | --- | --- |
| Batch syntax | [`msduck-sql::batch::parse`](../crates/msduck-sql/src/batch.rs) parses a whole batch into ordered SQL AST statements; [`Session::batch_response_inner`](../src/engine.rs) executes those leaves in order. | A setting can be applied per statement without splitting SQL text or changing the parser's statement boundaries. The exact AST shape of `SET DATEFORMAT @format` still needs a focused syntax test. |
| Setting validation | [`session_setting`](../src/engine.rs) accepts only `SET DATEFORMAT MDY` among DATEFORMAT forms and returns no state change. [`Session::execute`](../src/engine.rs) applies `DATEFIRST` at runtime; [`datepart::setting`](../src/datepart.rs) is its AST recognition precedent. | Current `MDY` acceptance is a no-op. Other orders and the captured local-variable form have no DATEFORMAT runtime path. Stringifying the AST is inadequate for variable evaluation and exact diagnostics. |
| Session lifetime | [`Session`](../src/engine.rs) owns `datefirst`, `nocount`, transaction state, and the DuckDB connection. `SET LANGUAGE US_ENGLISH` resets `datefirst` and emits information 5703; no DATEFORMAT field exists. | Add an explicit per-connection date order. `SET LANGUAGE us_english` must reset it to `mdy` as captured, without changing another connection. A failed setting must leave the prior order intact. |
| Preparation and RPC | [`validate_prepared_sql`](../src/engine.rs) checks SET syntax without executing it. [`RpcState`](../src/rpc.rs) stores prepared SQL and [`sp_execute`](../src/rpc.rs) calls `prepared_batch` later. [`batch_response_context`](../src/engine.rs) currently saves/restores some settings for RPC execution. | Validate shape at prepare time, but resolve the current date order when each statement executes. The fixture proves two executions of one handle under successive `dmy` and `mdy` settings; it does not prove how a DATEFORMAT change *inside* a prepared RPC should persist. |
| Temporal conversion | [`datetime2_cast::lower`](../crates/msduck-sql/src/datetime2_cast.rs) and [`datetimeoffset_cast::lower`](../crates/msduck-sql/src/datetimeoffset_cast.rs) select tagged cast functions. Root [`datetime2_cast`](../src/datetime2_cast.rs) and [`datetimeoffset_cast`](../src/datetimeoffset_cast.rs) parse text through ISO-oriented core functions; [`datetime2_date`](../src/datetime2_date.rs) delegates ordinary text DATE casts to DuckDB. [`DateTime2::parse_iso`](../crates/msduck-core/src/datetime2.rs) explicitly excludes locale-dependent forms. The root translator currently maps DATETIME to DuckDB TIMESTAMP. | Neither DuckDB's ambient text conversion nor an ISO-only parser can supply the captured session-sensitive interpretation. Legacy DATETIME needs its distinct source-aware path. Keep the SQL target declaration and offset typed independently of the input's value. |
| Errors and wire | [`SqlError`](../crates/msduck-core/src/diagnostic.rs), [`emit_error`](../src/engine.rs), and [`FailedQuery`](../src/query_error.rs) preserve typed diagnostics and optional result metadata; the engine emits DONE-family tokens after each leaf. | Invalid `SET DATEFORMAT xyz` must emit 2741/state 1/severity 16 with `ERROR, DONE` and raw DONE status 2/command 249. Strict DATE conversion under `ydm` must emit 241/state 1/severity 16 with `COLMETADATA, ERROR, DONE`, Date metadata, and raw DONE status 2/command 193. Do not replace either with a generic parse error or an untyped NULL. |

These observations are supported by current source and the retained fixture.
They do not establish other SQL Server languages, style codes, month names,
two-digit-year cutoff, RPC setting persistence, or every Unicode date spelling.

## Deterministic rule and effect boundary

Represent the order as a validated `DateOrder` value (`mdy`, `dmy`, `ymd`,
`ydm`, `myd`, `dym`). A pure rule should take the date text, explicit target
type, order, and any explicit style as inputs, and return a typed temporal
value, an invalid-conversion result, or an **unsupported shape**. `TRY_CAST`
maps only a known invalid conversion to typed NULL; an unimplemented spelling
must not silently become NULL or inherit DuckDB interpretation. Reuse the exact
calendar and offset types in `msduck-core`; avoid lossy text normalization,
hidden locale, clocks, environment reads, or process-global mutable state.
The captured ASCII numeric forms and ISO forms are a bounded first target.
Additional language-dependent forms need their own SQL Server probes.

The root `Session` owns the mutable setting. Parse the SET AST into an explicit
operation; evaluate a local `@format` value once from the caller-owned variable
map at execution time; validate it before assigning the new order. The captured
`xyz` error leaves `dmy` in force. `SET LANGUAGE us_english` updates its
language-dependent order and `DATEFIRST` together after validation. Other
languages remain explicit unsupported cases until captured. Preparation may
validate syntax and declarations, but must not mutate the session or freeze
an order into the prepared SQL.

At execution, pass the current order into the conversion adapter explicitly.
The adapter may own the DuckDB function and session bridge; the core rule must
not read the connection. Prove that DuckDB does not constant-fold or cache an
old order across `sp_execute` calls. A per-execution bound input is preferable;
if a connection-owned scalar reads session state, it must be marked and tested
for runtime evaluation. Preserve one evaluation of a volatile source operand.
The SQL target type fixes result metadata even for NULL and conversion errors,
and DATETIMEOFFSET must retain its original signed offset, not only its UTC
instant.

The type split is material. For `03/04/2024`, `mdy` yields March 4 and `dmy`
yields April 3 for all four captured types. Under `ydm`, the newer DATE,
DATETIME2 and DATETIMEOFFSET targets reject that slash spelling through
`TRY_CAST`, while legacy DATETIME accepts it. For `2024/05/04`, `ydm` yields
May 4 in the newer types but April 5 in DATETIME. ISO
`2024-03-04T05:06:07.1234567+02:00` is format-independent in the captured
orders. Do not implement this by globally swapping date substrings or by
making legacy DATETIME share the newer parser.

## Ordered successor scopes

These are **proposed** task scopes, not published claims. Publish each through
the protected registry only after its prerequisites and reserved-file handoffs
are checked. The file sets are disjoint from each other; the named export and
engine files are presently reserved by other workers.

| Stage | Proposed exact scope | Dependency and acceptance |
| --- | --- | --- |
| 1. Deterministic conversion | `crates/msduck-core/src/dateformat.rs`, `crates/msduck-core/src/lib.rs`, `crates/msduck-core/tests/dateformat.rs` | After `legacy-datetime-export-v1` and the shared core `lib.rs` claim are released. Export a public rule for the captured numeric and ISO forms; test every order, target-type split, offset, NULL/invalid/unsupported distinction, and bounds without DuckDB. Do not stage a private module as a completed integration. |
| 2. SET recognition | `crates/msduck-sql/src/dateformat.rs`, `crates/msduck-sql/src/lib.rs`, `crates/msduck-sql/tests/dateformat.rs` | After the SQL `lib.rs` reservation, including `string-agg-rules-v1`, is released. Recognize literal and local-variable AST forms, reject unsupported shapes, and return a typed setting operation without effects. Verify semicolon-free batches and preparation validation without executing SET. Depends on stage 1's order type. |
| 3. Conversion adapter | `src/dateformat_cast.rs`, `src/lib.rs`, `src/scalar.rs`, `tests/dateformat_cast.rs` | After the active root export and `src/scalar.rs` claims are released and stages 1–2 merge. Register target-specific, connection-owned native conversion functions that receive text and order explicitly, including a distinct legacy DATETIME path. Direct adapter tests must prove typed output, offset retention, bounded input and single evaluation; do not yet redirect public SQL CAST. |
| 4. Session and wire integration | `src/engine.rs`, `src/rpc.rs`, `src/datetime2_date.rs`, `src/datetime2_cast.rs`, `src/datetimeoffset_cast.rs`, `tests/dateformat.rs`, `tests/dateformat.test.mjs`, `docs/dateformat.md`, `README.md`, `ROADMAP.md` | After stages 1–3 and the active engine/RPC claims are released. Apply SET/SET LANGUAGE atomically per session, redirect text CAST/TRY_CAST for all four captured targets to the stage-3 adapter at execution, ensure `sp_prepare` does not mutate or freeze the setting, and replay rows, descriptors, diagnostics, ordered events, raw DONE status and command codes through the server. Confirm all required workspace/client/audit checks on the exact PR head. If `src/rpc.rs` needs no change, narrow the published scope before claim. |

The root engine is reserved by `result-alignment-v1`; `src/rpc.rs` by
`prepared-execution-context-v1`; `src/scalar.rs` by `unicode-trim-v1`; and
shared crate exports by other live claims. A blocked or completed *task state*
does not release a claim by itself. The queue publisher must follow the owner
handoff procedure before reassigning an overlapping file. Do not publish these
stages concurrently with conflicting scopes.

For stage 4, use the retained case names as differential assertions: all six
`ambiguous` and `same date` rows; `ydm strict date failure` and `ydm strict
datetime`; three ISO probes; `runtime changes in one batch`; A/B isolation;
`language resets format`; `format overrides language`; `invalid format` and
`after invalid format`; `format from variable`; and `prepared execute-time
setting`. Compare descriptors and ordered token events, not only JavaScript
date values. The fixture's DATETIMEOFFSET text and `TZOFFSET` columns are
needed because Tedious normalizes the typed value to a UTC `Date`.
