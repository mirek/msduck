# RAISERROR implementation work

The server now executes ad-hoc RAISERROR messages with informational/error
delivery, formatting, counters, continuation and TRY/CATCH integration. Full
RAISERROR compatibility remains unfinished. `reference/raiserror.json` records
14 live SQL Server probes, with exact error/information events, result metadata,
counters and completion events. They establish:

- Ad-hoc messages use number 50000. Severity 0 through 10 is informational;
  severity 9 stays 9 on the wire, while severity 10 is transmitted as 0.
- Informational messages normally leave @@ERROR at zero. WITH SETERROR sets
  it to 50000 even at severity zero. @@ROWCOUNT becomes zero.
- Severity 11 and 16 errors continue to the next statement outside TRY.
  Severity 16 inside TRY transfers to CATCH with the original message/state.
- Negative ad-hoc severity becomes zero; negative state becomes one. The
  captured state 256 wraps to zero. Microsoft advises against states above 255.
- `%s`, `%d`, `%%`, zero-padded hexadecimal, signed field width and string
  precision are demonstrated. Explicit NULL and missing substitutions format
  as `(null)` in these probes.
- Missing message ID 50001 reports error 18054 with a catalog-lookup diagnostic;
  inventing a message for that number would be incorrect.

`msduck_core::raiserror::ad_hoc` now implements deterministic event attributes:
wire severity/state, information versus error delivery, whether @@ERROR should
be set, and catchable/fatal/LOG-required classification. Catalog-message lookup,
formatting and effectful permissions/lifecycle are deliberately outside that
function. The execution adapter now uses these rules.

The upstream mssqlite implementation was inspected at
`packages/tsql/src/parse/statement.ts` and `packages/engine/src/execute.ts` in the
reference checkout. Its formatter recognizes a subset of printf substitutions
but does not apply the captured width/padding/precision behavior, and its generic
informational message path loses severity/state. It also fabricates text for
unknown message IDs. These parts cannot be reused unchanged.

Remaining work includes complete argument compatibility, sys.messages lookup,
LOG permissions and storage, NOWAIT delivery, high-severity connection
termination, and complete TRY/CATCH metadata and completion sequences. The high
severity flags in the pure model follow Microsoft's
[RAISERROR documentation](https://learn.microsoft.com/en-us/sql/t-sql/language-elements/raiserror-transact-sql?view=sql-server-ver17);
fatal delivery and logging have not yet been exercised against the server.

## Statement parsing

The SQL crate now intercepts RAISERROR statements and retains message, severity,
state, substitution expressions and LOG/NOWAIT/SETERROR options. A marked
`Statement::Raise` expression keeps arguments visible to existing AST visitors;
`raiserror::call` exposes the decoded statement without reparsing rendered SQL.
The marked statement must be handled by the engine, not rendered for DuckDB.

The parser rejects missing required arguments and unknown/missing WITH options.
Its regression verifies option preservation, following-statement boundaries,
substitution values and mutation through a generic expression visitor. Complete
argument-form/type validation, substitution-count limits remain unfinished; runtime execution is described below. In particular, successfully parsing an argument expression
does not yet establish that SQL Server accepts that argument form.

The prior attribute-only snapshot passed all 365 workspace Rust tests, strict
Clippy and formatting. The parser regression and full parser-snapshot
verification passed (366 Rust tests, strict Clippy and formatting).

## Bounded message formatting

`msduck_core::raiserror::format::message` now formats explicit typed arguments
into UTF-16 code units. It supports signed/unsigned decimal, octal and hexadecimal,
strings, literal percent signs, flags, static/dynamic width and precision, and
h/l/I64 modifiers. SMALLINT/TINYINT/INT/BIGINT retain their distinctions: `%hd`
accepts the two small integer types, `%d` accepts integers through INT, and
`%I64d` requires BIGINT. Invalid specifications and mismatched substitutions
return typed 2787/2786 diagnostics. Missing and explicit NULL substitutions render
`(null)` without field padding. Negative dynamic width becomes zero; negative
precision means unspecified.

Four new reference captures (`reference/raiserror-format*.json`) retain 42 live
SQL Server probes, including invalid formats, integer widths, NULL dimensions,
negative dimensions, boundary lengths and supplementary Unicode characters.
The output bound is 2047 UTF-16 units; overflow retains 2044 units plus `...`.
Both direct messages and `%s` substitutions preserve exactly 2047 units without
ellipsis. Allocation stays bounded for huge field widths, and formatting still
validates subsequent substitutions after the output fills.

The formatter deliberately returns UTF-16 units: live `%.1s` with a duck emoji
returns an unpaired high surrogate. Converting that output through Rust `String`
would change the message. The UTF-16 diagnostic encoder described below now preserves this representation
on the wire; the execution and caught-error adapters still need integration.
Complete argument eligibility/count validation, catalog messages and runtime
RAISERROR delivery remain unfinished. Binary substitutions and full modifier
combinations are not yet represented or exhaustively compared.

Parser-snapshot validation completed successfully: 366 workspace Rust tests,
strict Clippy and formatting. Formatter-snapshot validation also passed: 368 workspace Rust tests, strict
Clippy and formatting. Full remote verification passed against that synchronized formatter snapshot:
368 Rust tests, 368 client/harness tests and 303 completed audit cases.
The earlier arithmetic-continuation snapshot also completed remote Rust/client/
audit verification; it does not include RAISERROR execution.

## UTF-16 diagnostic transport

The TDS crate now exposes `diagnostic_utf16` with an explicit ERROR/INFO token
kind, severity, state, number and UTF-16 message units. It preserves unpaired
surrogates without converting them to replacement characters. Existing String
entry points delegate to this encoder and retain their previous token policy,
server/procedure/line fields and 16,000-unit transport bound. A raw-byte vector
checks a split surrogate and informational severity/state; additional assertions
cover ERROR attributes and bounded token/message lengths. All 16 TDS tests pass.

This removes the wire-encoding barrier but does not implement statement delivery.
The batch executor's current success path clears @@ERROR, and its generic failure
path stops the batch. RAISERROR needs an explicit raised-message outcome to apply
its distinct information/error, SETERROR, CATCH and continuation rules. Caught
error messages currently use Rust String values as well; preserving split
surrogates through ERROR_MESSAGE and bare THROW remains an additional boundary
problem. NOWAIT also requires access to transport flushing rather than only the
current complete-response byte buffer.

Wire-snapshot validation passed all 369 workspace Rust tests, strict Clippy and
formatting. Full local client verification subsequently passed all 368 tests with no
failures, cancellations or skips. The completed remote verification used the
earlier formatter snapshot and did not include this wire API.

`reference/raiserror-rpc.json` adds ten live RPC captures. Informational severity
zero completes with DONEINPROC, RETURNSTATUS 0 and DONEPROC. A terminal severity
16 message returns status 50000; a following successful SELECT resets return
status to zero while seeing @@ERROR 50000 and @@ROWCOUNT zero. SETERROR at
severity 9 sets @@ERROR without transferring to CATCH. Severity 19 without LOG
reports 2754 and continues. WITH LOG at severity 16 succeeds for the reference
sysadmin login; this does not establish permission behavior for other principals
or verify log contents. An explicit top-level RETURN 42 is rejected at compile
time with 178; the current server's acceptance is a separate compatibility gap.

`reference/raiserror-failure-counter.json` isolates six further RPC probes, each
on a fresh connection. Formatting failures at requested severity 16 emit 2787
(invalid format) or 2786 (wrong substitution type), but set @@ERROR to the
requested ad-hoc number 50000. A terminal invalid format returns RPC status
50000. At requested severity zero, invalid formatting sets @@ERROR to 2787;
WITH SETERROR changes it to 50000. Missing catalog message 50001 emits 18054
but leaves @@ERROR 50001. All these failures continue to a following SELECT,
which sees rowcount zero and then resets RPC return status to zero. In CATCH,
the earlier RPC probe sees the actual diagnostic identity 2787/16/1. The runtime
outcome must therefore carry diagnostic identity separately from the counter/
status number and retain the phase of failure (for example, the prior missing
LOG probe uses counter 2754 rather than 50000).


## Initial batch execution integration

The batch interpreter now binds constant and variable arguments without running
DuckDB queries. The formatter preserves their integer widths. A raised-message
outcome carries the emitted diagnostic separately from the @@ERROR/RPC status
number. INFO messages preserve severity/state and SETERROR behavior. Uncaught
nonfatal errors emit a completion and continue; caught errors transfer to the
handler with their diagnostic identity. Later successful statements reset the
RPC return status while overall batch failure remains visible to native callers.
Preparation binds arguments without formatting or delivering the message.

SQL diagnostics can retain original UTF-16 alongside display text. Wire emission
uses original units, and CATCH followed by bare THROW preserves them. The existing
ERROR_MESSAGE expression still uses display text, so an unpaired surrogate is
replaced in that expression; general SQL string storage needs a separate solution.

A native regression verifies that an application error preserves both writes
inside an explicit transaction, commit succeeds, counters distinguish formatting
errors, CATCH sees 2787 and bare THROW preserves a split surrogate. It passes.
The deterministic suites, strict Clippy and formatting pass as well. Both focused client regressions pass, including transaction preservation,
RPC counters/status, surrogate rethrow, preflight write prevention and a
2100-character bound message truncated to 2044 units plus ellipsis. Two audit
probes are added; full audit and remote verification are running.

WITH LOG, NOWAIT and fatal delivery remain explicit unsupported operations.
Severity above 18 without LOG returns 2754 and continues, as observed. User
message IDs at least 50000 currently report missing-catalog error 18054 with the
requested number retained in @@ERROR; sys.messages/sp_addmessage support and
system message lookup are unfinished. The preflight section below adds argument form, count and declared-type checks;
complete argument compatibility still needs work. Unsupported argument forms
currently fail explicitly during binding instead of being evaluated by DuckDB.

Ten argument probes in `reference/raiserror-arguments.json` distinguish syntax,
compile-time type checks and formatting. Preflight now reports 156 for literal
NULL in required positions, 102 for the captured expression/plus/parenthesis
forms, 2747 for more than 20 substitutions and 2748 for disallowed declared
substitution types. These checks cover nested statements before execution, so
preceding writes are not committed by a malformed later RAISERROR. A numeric
literal such as 1.0 is retained as decimal and mismatches `%d` during formatting;
a declared DECIMAL substitution fails earlier even if the template has no
substitution. Broader severity/state conversion and argument cases remain open.


## Integrated reference comparison

`artifacts/compatibility/raiserror-runtime-comparison.json` retains unmodified
captures and raw differences for all 82 queries in eight reference groups.
69 complete captures match exactly: 13/14 initial messages, 14/16 format probes,
7/13 extra format probes, 9/9 typed formats, 4/4 integer-width probes, 6/10 RPC
control-flow probes, 6/6 isolated failure-counter probes and 10/10 argument probes.
The remaining thirteen captures differ for these reasons:

- Eight boundary-length queries fail during their REPLICATE setup because that
  SQL function is not implemented. Core formatting tests and the independent
  bound-parameter client test verify truncation without claiming these captures
  match.
- Three TRY/CATCH captures retain completion-sequence differences; two also
  expose ERROR_* metadata flags and ERROR_MESSAGE width differences. Their
  diagnostic/counter values match for the ordinary strings tested.
- WITH LOG remains unsupported in one RPC capture.
- One top-level RETURN with a value is accepted by the current interpreter but
  rejected by SQL Server during compilation (178).

The latest native preflight regression and strict Clippy passed after clearing
regenerable Rust incremental caches. A preceding link attempt exhausted disk
space and produced a non-executable test artifact; it was removed and rebuilt.
The DuckDB native cache and the binary used by the then-running client suite
were preserved. The final runtime snapshot passed all 370 workspace Rust tests, strict Clippy
and formatting, plus both focused client tests. Full local audit and remote
Rust/client/audit verification are running separately. No full compatibility claim follows from the matching captures.


The subsequent ERROR_* declaration change removes the CATCH column flags and
message-width differences above. Its rerun preserves the remaining completion
sequence differences and still matches 69/82 whole captures; see
[ERROR_* declarations](try-catch.md) for the seven additional
reference probes and verification scope. The prior local audit spanned a binary
rebuild and is retained as a mixed-version capture, not a final baseline.


The frozen runtime snapshot's full Linux verification completed successfully:
370 Rust tests, 370 client/harness tests, strict Clippy, formatting and all 305
audit cases. Against the preserved 303-case pre-RAISERROR audit, all 303 existing
captures are unchanged and only the two RAISERROR probes are added. Raw evidence
is retained in `artifacts/compatibility/raiserror-runtime-audit-diff.json`. This
snapshot precedes the ERROR_* metadata and REPLICATE adapter work.
