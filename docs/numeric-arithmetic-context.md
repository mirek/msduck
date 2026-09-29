# Contextual numeric arithmetic in SQL Server

[`reference/numeric-arithmetic-context.json`](../reference/numeric-arithmetic-context.json)
retains 34 batch observations and four executions of one prepared statement from
each of two independent pinned SQL Server 2025 containers. Both reported product
version `17.0.4065.4`. The two canonical captures are byte-identical (SHA-256
`53b4db12496c6c038e07ccb77c3d7fec20fd02ee6144e8effe47f6d84fc79519`).
The fixture retains rows, exact `VARCHAR(100)` renderings, column descriptors,
errors, informational messages, completion events, decoded DONE fields, and raw
DONE status bits and command identifiers without normalizing them. The failed
conversion ends with command 193 and status `0x0002` (`DONE_ERROR`); an ordinary
SELECT ends with command 193 and status `0x0010` (`DONE_COUNT`).

| Expression | First descriptor | Exact rendering |
| --- | --- | --- |
| `2147483649/2` | `NumericN(16,6)` | `1073741824.500000` |
| `2147483649/0002` | `NumericN(16,6)` | `1073741824.500000` |
| `2147483649/99` | `NumericN(16,6)` | `21691754.030303` |
| `2147483649/100` | `NumericN(16,6)` | `21474836.490000` |
| `2147483649/1000000` | `NumericN(18,8)` | `2147.48364900` |
| `2147483649/1000000000` | `NumericN(21,11)` | `2.14748364900` |
| `2147483649/2147483647` | `NumericN(21,11)` | `1.00000000093` |
| `10/2147483649` | `NumericN(13,11)` | `0.00000000465` |
| `CAST(2147483649 AS DECIMAL(10,0))/2` | `DecimalN(16,6)` | `1073741824.500000` |
| `CAST(2147483649 AS DECIMAL(10,0))/CAST(2 AS INT)` | `DecimalN(21,11)` | `1073741824.50000000000` |
| `CAST(2 AS DECIMAL(5,2))/3` | `DecimalN(9,6)` | `0.666666` |
| `CAST(2 AS DECIMAL(5,2))/CAST(3 AS INT)` | `DecimalN(16,13)` | `0.6666666666666` |
| `(2147483649/2)/2` | `NumericN(18,8)` | `536870912.25000000` |
| `2147483649/(2/2)` | `NumericN(21,11)` | `2147483649.00000000000` |

The bare integer divisor's written magnitude matters: a seven-digit literal
increases the result scale, while two- and three-digit literals do not because
division has a minimum scale of six. Leading zeros do not change the captured
result. An explicit `INT` cast or a bound `INT` parameter has a different
result declaration even for the same runtime value. The prepared
`CAST(2147483649 AS DECIMAL(10,0))/@divisor` remains `DecimalN(21,11)` for
`2`, `3`, `NULL`, and `2` again. The NULL execution returns two typed NULL cells.
These observations are consistent with a minimal precision for a bare integer
literal converted in a decimal expression and fixed precision ten for a declared
`INT`; they do not prove the SQL Server compiler's internal rewrite.

The nominal descriptor also follows the explicitly cast operand: bare decimal
literals produce `NumericN`, explicit `DECIMAL` casts produce `DecimalN`, and
explicit `NUMERIC` casts produce `NumericN` in these probes. The capture includes
multiplication, addition, nesting, empty result metadata, a failed conversion
(8114/state 5/class 16), and successful reuse of the same connection. See the
fixture for the complete descriptors and completion sequence.

Msduck's decimal division lowering and projection inference now use the
minimum written precision of a bare integer operand, including when a decimal
division is nested inside another division. An inner integer division still
truncates before its result is converted for the outer decimal division.
Explicit and bound `INT` operands retain precision ten. The root regression expects
`DECIMAL(9,6)` and `0.666666` for `CAST(2 AS DECIMAL(5,2))/3` rather than the
previous `DECIMAL(16,13)` and longer quotient. Integer/integer division remains
separate. Only nested division trees with known declarations receive this
contextual lowering; other expression shapes still rely on shared metadata
rules. The TDS codec still cannot distinguish `NumericN` from `DecimalN`, so
the fixture remains reference evidence, not a claim that every expression or
full wire capture matches.

Run `node scripts/capture-numeric-arithmetic-context.mjs --check` to verify the
retained fixture checksum, plan and two independent captures without starting
SQL Server. On a Docker-capable host, run the script without flags to replay
the plan in two new isolated pinned containers and compare against the fixture;
the new raw capture goes under `artifacts/compatibility/numeric-arithmetic-context/`.
The script rejects output aliases to the fixture and an unpinned image override.
