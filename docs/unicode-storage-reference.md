# Unicode storage reference evidence

`reference/unicode-storage.json` records 180 SQL Server 2025 CU7 cases
(17.0.4065.4), using the exact image digest retained in the fixture. It covers
NVARCHAR(3), NCHAR(3), and NVARCHAR(MAX) through INSERT VALUES, INSERT SELECT,
UPDATE, defaults, and variable-to-table assignment. Each case retains its setup,
write, subsequent read, and identical read after closing and reopening the
connection. Reconnect verifies retained database state, not a server restart.

The twelve input shapes cover legacy and version-100 casing, ASCII casing,
empty and NULL values, supplementary characters, isolated high/low surrogates,
casing adjacent to an isolated surrogate, overflow, and excess trailing spaces.
Rows retain both the client string and SQL Server's VARBINARY conversion, so
UTF-16 code units are independently visible. DATALENGTH and column descriptors
remain in the capture, including BIGINT results for MAX. Errors and completion
tokens are preserved without removing flags or messages.

The capture establishes these storage rules:

- Isolated surrogate units survive assignment and reconnect. For example, the
  high unit from LEFT(N'🦆',1) is stored as bytes `3ed8`, not replacement text.
- NCHAR(3) pads to three UTF-16 units. An isolated high surrogate receives two
  spaces; the supplementary character followed by `x` exactly fills the column.
- Bounded targets accept excess trailing spaces: `N'ab   '` becomes `N'ab '`.
  MAX retains all five units.
- Non-space overflow raises error 2628. All twenty failing cases are atomic:
  failed multirow inserts leave no rows; failed updates retain both old rows.
- UPPER of `ƀ` remains `ƀ` under the default SQL collation and becomes `Ƀ` under
  Latin1_General_100_CI_AS before storage.

Regenerate on a Docker-capable machine with Node.js and installed dependencies:

```sh
node scripts/capture-unicode-storage.mjs reference/unicode-storage.json
```

The script uses the shared owned-container helper with a loopback-only ephemeral
port, validates expected error numbers and row counts, checks string code units
against stored binary bytes, compares reads across reconnect, and removes only
its own container. Two independent container runs produced byte-identical JSON.
No credentials are stored in the artifact.

This is reference evidence, not a claim that msduck implements every case.
At f30dd32, the ordinary Unicode target layout still passes carrier structs
through a VARCHAR conversion, which can stringify the carrier and cause false
truncation. UPDATE expressions also need target-scope casing binding. Correcting
those paths must retain the captured raw units, width semantics, and atomicity;
converting invalid Unicode to replacement characters would not satisfy them.

A diagnostic replay at f30dd32 attempted all 180 programs. Six setups failed;
none matched the complete fixture. This is not a count of 180 independent
storage defects: the common read projection uses CONVERT(VARBINARY(MAX),s),
which that revision does not lower and therefore fails for every completed
setup. Write failures independently expose mixed VARCHAR/carrier VALUES or
UNION inputs, carrier stringification, and unbound casing on some expression
paths. Keep the full fixture intact when implementing these missing boundaries.
