# STRING_SPLIT reference contract

`reference/string-split.json` retains 49 SQL Server programs per run. The
generator repeated every program in two fresh databases in each of two
independent containers, all using the pinned SQL Server 2025 image digest
`86cc6144ef39bb0fbed2329e1ad79b13ee82e7b2e4739213a0db0800e668a74a`.
All four raw captures matched. Each record keeps the query, any RPC parameter
declarations and values, rows, TDS descriptors, error number/state/class/text,
information messages, and DONE tokens. Run
`node scripts/capture-string-split.mjs OUTPUT` to recapture and compare; the
generator refuses to overwrite an existing fixture. `--one-database` is only
a faster diagnostic probe and cannot write the retained fixture.

Observed value and declaration behavior:

| Input or form | Captured behavior |
| --- | --- |
| `STRING_SPLIT('alpha,beta', ',')` | Two rows, `alpha` and `beta`. The result is `VARCHAR(10)` for this literal's declared width. |
| Empty input | One row containing the empty string; a typed NULL input has zero rows. Both retain a result descriptor. |
| Repeated or edge separator | Empty tokens are retained. `a,,b` yields an empty token; `,a,` yields two. |
| Empty, NULL, two-character or supplementary separator | Error 214, state 11, after the `value` result descriptor and before any row. The bound NULL separator has the same error and RPC completion tokens. |
| `VARCHAR`/`NVARCHAR` source | The `value` column follows the source family and declared capacity, including MAX (TDS length 65535). A `VARCHAR` source with an `NVARCHAR` separator produces an `NVARCHAR` result. |
| Third argument `0` / `1` | `0` returns only `value`; `1` adds a non-null `BIGINT` `ordinal` column counting from 1. Tedious exposes these BIGINT values as strings. An empty input still has an ordinal-1 empty token. |
| Invalid third argument | Constants `2` and `-1` raise 4199; a variable or RPC parameter raises 8748; a decimal raises 8116. A NULL constant behaves like disabled ordinal: the value-only query returns `a` and `b`, while selecting the absent `ordinal` column raises 207. |
| Invalid source/separator type | Integer or binary source and integer separator raise 8116 before result metadata. Missing or excess arguments raise 313 and 8144. |
| Table column and `APPLY` | The fixture retains source-derived widths and nullability, including `OUTER APPLY`'s null-extended row and the ordered ordinal values. |

The fixture also retains the source collation on an explicitly binary-collated
expression. Unordered output was captured to expose the actual wire result,
but SQL Server does not promise token order without an outer `ORDER BY`.
Ordering by `ordinal` is the portable way to request input order when the
third argument is enabled. The capture's equality across runs is evidence of
repeatability for these probes, not a general ordering contract.

This is reference evidence only: msduck does not yet implement `STRING_SPLIT`.
A successor should put tokenization, NULL and separator checks, ordinal
construction, and declaration-to-result metadata rules in deterministic code.
The SQL layer must bind the table-valued function and enforce the constant
ordinal argument; the root adapter must apply it to source rows and emit the
captured descriptors, errors and completions. The implementation must replay
these cases through the public Tedious path before claiming compatibility.
