# JSON extraction array wildcards

`reference/json-extraction-wildcard.json` retains 106 `JSON_VALUE` and
`JSON_QUERY` requests against the pinned SQL Server 2025 image, twice in fresh
databases with equal rows, descriptors, diagnostics and ordered events. An
independent recapture matched the retained fixture. The capture includes the
source document and path as data alongside each SQL request, so the core test
replays both observed runs without parsing SQL text.

For the captured `[*]` paths, SQL Server validates the entire document before
returning an extraction result. An invalid later array element therefore raises
13609 even if an earlier element matches. A wildcard path with exactly one
selected value returns that value if it has the requested kind. Zero or multiple
selected values return NULL in lax mode; in strict mode `JSON_VALUE` reports
13608/state 1, while `JSON_QUERY` reports 13608/state 1 for zero or one
wrong-kind match and 13624/state 3 for multiple matches. A selected JSON null
produces SQL NULL even in strict mode and even when another branch matches in
the captured cases. A path such as `$[*].a` can select one value from a
multi-element array when other elements lack `a`.

The deterministic core now accepts `[*]` for extraction while leaving the
ordinary public path parser and `JSON_PATH_EXISTS` rules unchanged. It validates
the full document, then walks wildcard branches in source order with an
explicit stack holding one array iterator per nesting level. It retains source
slices for containers and lexical numbers, decodes strings only when selected,
and uses the same UTF-16 selection logic for both UTF-8 and raw UTF-16 inputs.
The core fixture test matches the result or diagnostic identity of all 212
records across the two runs, including advanced non-wildcard paths now handled
by the range evaluator.

Known differences remain explicit:

- The captured range results and 13660 states for `[last]` and `[0,1]` now
  match in the extraction APIs; this fixture alone does not cover the full
  advanced path grammar.
- The malformed `'$[*].'` path has SQL error 13607/state 14 with an offset;
  the detailed core APIs and TDS adapter preserve its captured character,
  UTF-16 position, state and message. The legacy core API still returns `PATH`.
- Malformed JSON reports SQL 13609 with an unexpected character and position;
  the core's `DOCUMENT` diagnostic does not retain that detail. Root adapters,
  descriptors and event ordering have not been compared against this fixture.

This implements the captured wildcard subset, not the full SQL Server 2025
advanced JSON path grammar or wire-level parity.
