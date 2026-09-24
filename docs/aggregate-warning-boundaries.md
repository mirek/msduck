# Aggregate warning execution boundaries

`reference/aggregate-warning-boundaries.json` captures 50 programs against the
pinned SQL Server 17.0.4065.4 image. Two fresh containers produced byte-identical
captures. Each program retains setup, SQL, rows, descriptors, errors, information
messages, event order, subsequent session state and resulting table contents.
The stored-view case also repeats execution in the same connection.

The fixture records initial @@OPTIONS=5496, ANSI_WARNINGS=1 and ARITHABORT=1.
Only ANSI_WARNINGS changes between the ON/OFF groups. Arithmetic behavior in this
capture must therefore not be generalized to ARITHABORT OFF sessions.

## Observations

- Aggregates inside stored views warn on each execution. Two SELECT statements
  produce two warnings. An outer false predicate that prunes execution produces
  none.
- INSERT SELECT, UPDATE scalar subqueries, DELETE subqueries, SELECT INTO and
  variable assignment consumers can warn without returning a result set. The
  warning precedes their statement DONE token. An UPDATE with no target rows
  still warns in the captured scalar-subquery plan.
- An empty aggregate source inserts NULL without warning. Grouped INSERT emits
  only one warning despite multiple aggregate groups.
- A window warns only when its frames consume NULL. The following-only frame
  over `(1,NULL),(2,2)` produces no warning, nor does a single NULL row whose
  following-only frame is empty. The preceding-only counterpart consumes NULL
  and warns. COUNT follows the same distinction. LAG does not warn.
- The captured conversion and division failures emit warning 8153 after the
  error and before DONE. SUM overflow emits error 8115/state 2 without warning.
  A failed CHECK emits error 547, information 3621, then warning 8153; the sink
  remains empty. Warning state cannot simply be discarded for all errors.
- When conversion failure enters CATCH, the error is caught and no warning is
  emitted. A warning from an earlier successful statement remains visible when
  a later statement fails. Error handling must preserve statement boundaries.
- ANSI_WARNINGS OFF suppresses 8153 in all these programs. It does not suppress
  information 3621 for the failed CHECK. Arithmetic errors remain under the
  captured ARITHABORT policy.

## Local replay at e10a8cd

A replay against the owner-built executable exactly matched 25/50 programs.
Two CHECK cases stopped at unsupported ALTER ADD CONSTRAINT setup. The other
23 programs retain 129 exact differences; no fields were normalized away.

The replay confirms three false window warnings for NULL inputs outside every
frame. Missing warnings remain in stored views, ordinary INSERT/UPDATE/DELETE,
SET and DECLARE scalar subqueries, and partial failures. SELECT INTO and SELECT
assignment already match the captured execution behavior. Additional differences
include stored-view descriptor flags (1 versus 9), integer conversion message
text and SUM overflow state (1 versus 2).

This is reference evidence, not an implementation or completion claim. The
initial 126-program warning matrix can pass while these boundaries fail. Next
integration work must observe actual frame consumption without evaluating
volatile operands twice, propagate diagnostics through every execution consumer,
and preserve warning ordering through error/catch paths. Persisted view
bindings must never store a transient execution ticket.
