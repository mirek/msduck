# COUNT typeless NULL compilation evidence

The server currently accepts COUNT(NULL), whereas SQL Server reports8117.
`reference/count-null-compilation.json` retains two identical fresh pinned
SQL Server2025 runs covering35 profiles in batch and RPC modes plus version
and setup:72 records per run. Every profile additionally retains its complete
reset and follow-up response, including rows, descriptors, errors, DONE and
ORDER/event positions. The independent follow-up checks table rows, transaction
depth and connection recovery. Reference SHA-256:
`d08c691dab34e91de2d49aae575d91976419cf666a394b0509fb47ee14e5f530`.

Generate a new artifact with `node scripts/capture-count-null-compilation.mjs`;
`--write-fixture` refuses to replace the retained fixture, and `--check` verifies
its checksum, complete request plans, reset/follow-up phases and raw token
invariants. Capture uses two freshly owned containers and databases with the
existing bounded wire helpers and finally cleanup. Use the Linux runner lock.

## Captured behavior

Bare/parenthesized/DISTINCT NULL, empty-input and window/frame NULL calls report
8117, state1, class16, naming count or count_big. Unary positive NULL also fails.
There is no result metadata or row set. Explicit INT/BIGINT/NVARCHAR casts,
CONVERT(INT,NULL), NULL+1 and declared INT/BIGINT/NVARCHAR NULL parameters are
valid: COUNT returns INT0 and COUNT_BIG returns BIGINT0. A NULL value is not
an unknown declaration, and its value must not supply a compile-time type.

Error precedence is significant. The all-NULL CASE reports8133; a scalar
subquery argument reports130. COUNT(SUM(NULL)) reports8117 naming sum, and
SUM(COUNT(NULL)) reports8117 naming count, rather than the generic nested
aggregate error. Missing table and predicate column errors208 and207 take
precedence over COUNT(NULL). UPDATE SET a=COUNT(NULL) reports8117 before the
ordinary aggregate-in-UPDATE restriction. A blanket early NULL rejection would
therefore get some of these results wrong; binding and operand validation order
need explicit treatment.

Compilation rejects the entire same-level batch: a prior SELECT emits no result,
a prior INSERT has no effect, and invalid calls still fail after RETURN or in an
unreachable IF. Same-level TRY/CATCH does not catch8117. A higher-level TRY/CATCH
around sp_executesql does catch it and returns number/state/severity/message as
ordinary rows with no outgoing ERROR token. Raw completion records for both
forms remain in the fixture; equal error numbers alone are not wire agreement.

Every follow-up sees four heap rows and three non-NULL labels, transaction depth0
and recovery value42, except the valid typed-NULL prior-write control, which sees
five rows. That control demonstrates that the side-effect probe detects a real
INSERT rather than merely asserting an invariant reset state.

Preparing a typeless COUNT/COUNT_BIG NULL call reports8117 followed by8180 and
leaves the selected handle NULL. Preparing COUNT(@p)/COUNT_BIG(@p) with an INT
declaration returns fixed metadata without executing, then yields0/0 for NULL
and4/4 for value0 over the same four rows. Preparation and both executions have
identical complete descriptors. These captures explicitly request metadata
option1 and do not substitute ordinary execute-time metadata for preparation.

## Implementation follow-up

This task changes no production Rust and proves reference observations only.
The future validator must read original logical syntax and explicit declarations,
preserve binding and nested-operand precedence, and reject same-level invalid
batches before writes or prior results. It must preserve higher-level dynamic
catchability and the ordered preparation diagnostics, while leaving typed NULL
and parameter value independence intact. Shared aggregate/preflight logic may
supply deterministic rules, but catalog acquisition, staged batch binding and
execution/catch/prepared framing remain root adapter responsibilities.

Broader aggregate NULL inputs, other modifiers, missing sources alongside more
complex syntax and unproven error combinations need additional ground truth.
No general rule for every aggregate, NULL expression or compilation boundary is
claimed. Root runtime integration remains backlog #696 and engine.rs is not
reserved by this reference work.
