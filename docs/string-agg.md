# STRING_AGG reference contract

The retained fixture in reference/string-agg.json contains 42 SQL Server
programs, each captured in two fresh databases in each of two independent
containers. Both containers used the pinned SQL Server 2025 image digest
86cc6144ef39bb0fbed2329e1ad79b13ee82e7b2e4739213a0db0800e668a74a.
The four raw captures matched exactly. They retain result rows, TDS column
descriptors, error number/state/class/text, information messages and DONE
tokens. scripts/capture-string-agg.mjs regenerates an artifact and checks the
retained fixture; it refuses to overwrite an existing fixture.

Observed value and declaration rules:

| Input | Captured behavior |
| --- | --- |
| NVARCHAR with non-NULL separator | NULL inputs are skipped, without an extra separator. Empty or all-NULL groups return NULL. |
| NULL separator | Non-NULL inputs concatenate without a separator. This also held for a bound NULL separator. |
| VARCHAR input | Output is VARCHAR(8000), represented by a VarChar TDS column with 8000-byte length. |
| NVARCHAR input | Output is NVARCHAR(4000), represented by an NVarChar TDS column with 8000-byte length. |
| VARCHAR(MAX) or NVARCHAR(MAX) input | The corresponding output is MAX, represented by a TDS length of 65535. |
| Separator expression or MAX variable | Inline CAST expressions are rejected with error 8733: the separator must be a literal or variable. VARCHAR(MAX) and NVARCHAR(MAX) separator variables are rejected with error 8734. |
| INT, DECIMAL or DATETIME2 input | SQL Server formats the input as text and returns NVARCHAR(4000). The retained rows include exact decimal scale and DATETIME2 text. |
| VARCHAR input with NVARCHAR separator | Error 8116 before any result metadata. NVARCHAR input with VARCHAR separator succeeds. |
| Bounded output past 8000 bytes | Error 9829 after the result descriptor and before a row, followed by a non-row-count DONE. The captured VARCHAR and NVARCHAR cases differ in error state (0 and 1). Casting the input to MAX permits the long result. |

The captured scalar result descriptors are nullable (flags 1) and carry the
server collation. The output width follows the declared expression family,
not the actual number of rows or final string length. The fixture retains
the complete descriptors and long values rather than reducing them to
type names or hashes.

WITHIN GROUP sorts before concatenation: the fixture has ascending,
descending and NULL-key cases, plus grouped results and a bound separator.
Its tied-key case uses identical values, so the captured output is stable
without implying an order between distinguishable ties. Unordered
concatenation was also observed but must not be treated as an ordering
guarantee. The captured unsupported forms are DISTINCT (syntax error 102),
OVER (error 4113), and incompatible WITHIN GROUP order lists in one scope
(error 8711). Binary input and integer separators produce error 8116.

An implementation successor should put declaration-to-output family/width
selection, NULL elimination and bounded-length checks in the deterministic
core. The SQL syntax layer must preserve each ordered aggregate's WITHIN
GROUP keys and reject unsupported forms before writes. The root adapter
must bind source declarations and collations, supply evaluated values to the
aggregate once, and emit the captured metadata, diagnostic and DONE
ordering. DuckDB's STRING_AGG spelling alone is not evidence that these
SQL Server rules hold. Shared parser, engine, aggregate and metadata files
were outside this reference task's scope.
