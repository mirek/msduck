# Percentile ordering source types

`node scripts/capture-percentile-order-type.mjs --check` verifies the immutable
fixture, its full capture plan and equality of two fresh SQL Server 2025
containers. Both used pinned image digest
`86cc6144ef39bb0fbed2329e1ad79b13ee82e7b2e4739213a0db0800e668a74a`,
ProductVersion `17.0.4065.4`. Fixture SHA-256:
`878046d815625a00225430c816b2b16f822290b2b1f014c289881d64f6b010e0`.

Each run contains 194 records: version and reuse, 29 declared source types
across CONT/DISC and ordinary/all-NULL/empty VALUES inputs (174 batches),
and 18 prepared requests with explicit RPC declarations. Every successful
prepared request includes non-NULL, NULL and rebound non-NULL executions plus
unprepare. Failed preparations retain their complete diagnostic sequence and
have no fabricated execution observations.

To reproduce, run the script without flags with a new output path. It creates
two fresh containers and requires exact equality with the retained fixture.
`--write-fixture` creates a fixture only when none exists. The default output
is `artifacts/compatibility/percentile-order-type/capture.json`. Existing output
paths and aliases of the fixture are refused before container creation. A
failure leaves an incomplete output marker. Use the shared remote runner lock;
these captures require no native DuckDB build.

## Captured rules

For the numeric families tested, CONT returns nullable `FloatN(8)`, including
BIT, integer, decimal, money, REAL and both FLOAT precisions. DISC preserves
the declared source family and capacity. The percentile column has nullable
flag 1 in every successful batch, including empty input. All-NULL input emits
three NULL percentile values; empty input emits no rows with the same descriptor.
Ordinary input ignores the NULL ordering value while retaining its source row.

| Ordering source | DISC descriptor |
| --- | --- |
| BIT | BitN(1) |
| TINYINT / SMALLINT / INT / BIGINT | IntN(1 / 2 / 4 / 8) |
| DECIMAL(10,2) / DECIMAL(38,8) | DecimalN, precision/scale 10/2 or 38/8 |
| SMALLMONEY / MONEY | MoneyN(4 / 8) |
| REAL / FLOAT(24) / FLOAT(53) | FloatN(4 / 4 / 8) |
| CHAR(8) / VARCHAR(8) | Char / VarChar, 8 bytes |
| NCHAR(8) / NVARCHAR(8) | NChar / NVarChar, 16 bytes |
| VARCHAR(MAX) / NVARCHAR(MAX) | VarChar / NVarChar, PLP length marker 65535 |
| DATE / TIME(3) | Date / Time, TIME scale 3 |
| SMALLDATETIME / DATETIME | DateTimeN(4 / 8) |
| DATETIME2(7) / DATETIMEOFFSET(7) | DateTime2 / DateTimeOffset, scale 7 |
| BINARY(4) / VARBINARY(4) | Binary / VarBinary, 4 bytes |
| UNIQUEIDENTIFIER | UniqueIdentifier(16) |
| SQL_VARIANT containing INT | Variant, capacity 8009 |

Character descriptors retain the captured CP1252 collation fields. Fixed CHAR
and NCHAR values retain their padding; MAX descriptors remain PLP even though
the selected values are short. XML cannot be sorted: DISC reports error
305/state 1/class 16 for ordinary, all-NULL and empty inputs.

CONT rejects character, temporal, binary, uniqueidentifier, XML and sql_variant
ordering declarations with 402/state 1/class 16. Its message identifies the
source type, including `varchar(max)` and `nvarchar(max)`. Empty input does not
suppress binding errors. Prepared incompatible CONT ordering operands report
402 followed by 8180, return status 8180 and DONEPROC status 2; they produce no
result descriptors or executions. Successful preparations expose descriptors
before any binding value is supplied, including COLMETADATA and ORDER events.
Their descriptors remain identical across non-NULL, NULL and rebound values.

## Implementation requirements and limits

Infer eligible ordering declarations from explicit source/catalog snapshots,
independently of runtime parameter values. CONT's declaration is FLOAT(53);
DISC's is the source declaration with nullable logical result properties,
character collation/capacity, decimal precision/scale and temporal scale intact.
Apply source-specific CONT binding errors even to empty input, preserve XML
sort rejection and the separate prepared failure sequence. Root adapters own
catalog acquisition, actual execution and wire completion phases.

The fixture preserves decoded rows/descriptors, errors, warnings, ordered token
names, and decoded DONE fields with raw status/command words. It does not
capture arbitrary token payload bytes. Tedious decodes DECIMAL and MONEY as
JavaScript numbers: the DECIMAL(38,8) selected value loses decimal digits and
cannot prove exact decimal arithmetic or original value bytes. DATE/TIME values
use the client's UTC Date representation and nanosecondsDelta where available;
DATETIMEOFFSET's original offset is not preserved by that decoder. SQL_VARIANT
rows expose the decoded scalar, not its entire embedded descriptor. These
limitations remain explicit rather than treating decoded equality as wire parity.

This is reference evidence, not an msduck replay or compatibility pass. The
existing complete replay in `docs/percentile-execution.md` retains current DISC
nullability/width and CONT diagnostic differences. This matrix does not prove
all collations, arbitrary types, scalar expressions, ties, partitions, physical
order, overflow/interpolation precision or runtime fraction evaluation. Other
workers' reserved implementation files and the blocked character-fraction
client gate are unchanged.
