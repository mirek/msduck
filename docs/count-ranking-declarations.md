# COUNT and ROW_NUMBER declarations

Projection inference assigns COUNT an INT declaration and COUNT_BIG and
ROW_NUMBER a BIGINT declaration from an explicit catalog snapshot. These facts
are available to derived tables, CTEs and downstream arithmetic before backend
lowering. Parameter declarations supply types; bound values and result rows do
not influence them. Existing result-property inference supplies column flags.

The helper runs shared aggregate/window validation before inference, checks
source and parameter declarations, and preserves unknown barriers for missing
or ambiguous names, typeless NULL, legacy image/text/ntext operands, unsupported
functions, subqueries and nested aggregates/windows. Explicit casts retain their
source checks through the parser's integer conversion marker. COUNT over a
proven typed NULL is valid; COUNT(NULL) is not. Named windows and explicit frames
remain unresolved here pending coverage. Constant window keys remain unresolved
except the exact retained `(SELECT NULL)` subquery. This is type inference, not
complete statement binding or validation.

## Retained reference

`reference/count-ranking-declarations.json` contains two identical fresh pinned
SQL Server 2025 runs: 30 profiles in both batch and RPC modes, plus version and
setup records, giving 62 records per run. SHA-256:
`90e58f096efc179289404dfb29af02323ca6f44c1ba6745b704da59629fe2738`.
The reproducible generator is `scripts/capture-count-ranking-declarations.mjs`;
`--check` verifies the retained fixture and `--write-fixture` refuses replacement.
It reuses the existing bounded raw capture helpers and preserves full rows,
descriptors, errors, completion tokens and ORDER bytes/event placement.

Profiles cover star, columns, DISTINCT, literals, typed/typeless NULL, empty
and grouped/window results, derived/CTE consumers and decimal division;
ROW_NUMBER covers ordered, partitioned, empty, derived/CTE, constant subquery
and invalid inputs. Prepared COUNT/COUNT_BIG and ROW_NUMBER retain preparation
metadata and executions with @p=0,9,1, including empty input/result phases.

COUNT descriptors are IntN length 4; COUNT_BIG and ROW_NUMBER are IntN length 8.
COUNT-derived division has precision 18/scale 6 and COUNT_BIG-derived division
precision 27/scale 6. Decimal logical catalog storage widths (9 and 13) differ
from the captured wire carrier width (17). Tests compare precision/scale in
logical inference and complete wire descriptors in public client tests; these
are distinct representations and the fixture is never normalized to hide them.

Reference errors include 8117 for COUNT(NULL), 207 for missing columns, 208 for
a missing table, 4112 for ROW_NUMBER without ORDER BY and 4114 for an argument to
ROW_NUMBER. Invalid shapes must not receive fabricated known declarations.

## Verification and remaining runtime work

Pure tests replay successful descriptors and unknown barriers without backend
rows, preserve the query AST, and check prepared declarations against all
captured phases in both runs and transport modes. Dedicated public tests compare
full successful rows/descriptors and tedious prepared execution metadata.
Preparation metadata differences are retained separately rather than asserted
as compatible; msduck emits no metadata for the omitted option and rejects
explicit option 1.
They create the exact captured heap declarations and rows. The shared reference
setup also creates an unrelated PRIMARY KEY CLUSTERED table; msduck rejects that
DDL, so it is not used as setup for these heap-only execution comparisons.
This DDL limitation remains a separate compatibility gap.

`artifacts/count-ranking-declarations/root-errors.json` records complete raw
reference/server error-profile differences. Its diagnostic capture is explicitly
not a compatibility pass. Successful row/descriptor comparisons do not establish
raw completion-token or ORDER fidelity. Root ORDER emission remains separate
backlog work (#693). Wider aggregate acceptance, runtime validation, overflow
and every unproven window shape still need exact ground truth and integration.
No root source files or engine.rs are changed by this task.

The supplemental `--default-prepare` capture records omitted and explicit zero
options separately in two fresh containers, retaining transport-specific errors
and metadata rather than assuming they are equivalent. Tedious omits the option;
its preparation metadata requires separate reference verification.
The client diagnostic records preparation omissions and the full batch/RPC
responses when option 1 is requested. Execution comparisons remain strict.
