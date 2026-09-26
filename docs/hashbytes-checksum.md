# HASHBYTES, CHECKSUM and BINARY_CHECKSUM reference contract

The retained fixture in reference/hashbytes-checksum.json contains 162 SQL
Server observations, each captured in two fresh databases in each of two
independent containers. Both containers used the pinned SQL Server 2025 image
digest 86cc6144ef39bb0fbed2329e1ad79b13ee82e7b2e4739213a0db0800e668a74a. The
four raw captures matched exactly. The observations are:

- 141 ordinary batches, including 8 setup statements;
- 20 sp_executesql RPC calls;
- one prepared handle, with its sp_prepare, five sp_execute calls and its
  sp_unprepare retained separately.

They retain result rows, TDS column descriptors, error
number/state/class/line/text, information messages (none occurred) and DONE
tokens. Binary values are stored as hex. scripts/capture-hashbytes-checksum.mjs
regenerates an artifact and checks it against the retained fixture. With
`--write-fixture` it refuses to run when the fixture already exists, and it
writes the fixture with an exclusive-create flag. `--one-database` is a
diagnostic mode that cannot write the fixture.

msduck does not implement these functions. This document describes SQL Server
only.

## HASHBYTES

| Input | Captured behavior |
| --- | --- |
| Result descriptor | VarBinary, TDS length 8000, flags 33, for every successful case. This covers literal, MAX, >8000-byte, NULL and RPC inputs, and every algorithm. The one exception is `HASHBYTES(alg,txt)` over two table columns, which had flags 1. SELECT INTO created `varbinary`, max_length 8000, nullable. |
| MD4, MD5, SHA, SHA1, SHA2_256, SHA2_512 | The standard digests of the input bytes, with DATALENGTH 16, 20 and 64 for MD5, SHA1 and SHA2_512. SHA and SHA1 returned identical values. |
| MD2 | NULL for VARCHAR, NVARCHAR and VARBINARY input, with no error or message. |
| Algorithm spelling | Case-insensitive (`sha2_256` and `Md5` succeeded), and an NVARCHAR algorithm is accepted. A trailing space (`'MD5 '`) succeeded. A leading space (`' MD5'`) returned NULL. |
| Unknown algorithm (`SHA3_256`, `SHA2_384`, `''`) | NULL with no error, in batches, RPC and prepared executions. With a per-row algorithm column, the valid row hashed and the invalid row returned NULL. An invalid algorithm over an empty source returned metadata with no rows. |
| Typed NULL algorithm or input | NULL. This also holds for an RPC NULL algorithm or input. |
| Untyped `NULL` literal as either argument | Error 8116, state 1, class 16 ("Argument data type NULL is invalid for argument N of hashbytes function."), before any metadata. The batch then ends with DONE (rowCount null). |
| Integer algorithm | Error 8116 for argument 1. |
| Input families | VARCHAR, NVARCHAR, CHAR, NCHAR, VARBINARY and their MAX forms are accepted. INT, numeric literal, DATETIME, UNIQUEIDENTIFIER, XML and TEXT each raise 8116 (state 1) for argument 2, so there is no implicit conversion. An INT RPC parameter also raised 8116. |
| Argument count | One or three arguments: error 174, state 1, class 15 ("The hashbytes function requires 2 argument(s)."). |
| Encoding | VARCHAR under the default collation hashes code-page bytes (`'é'` hashes as 0xE9). VARCHAR under Latin1_General_100_CI_AS_SC_UTF8 (by cast plus COLLATE, and from a column) hashes UTF-8 bytes (0xC3A9). NVARCHAR hashes UTF-16LE, including the surrogate pair for U+1F600. VARBINARY 0x616263 hashes exactly as VARCHAR 'abc'. The hashes of NVARCHAR N'abc' differ from those of VARCHAR 'abc'. |
| Case, trailing spaces, padding | Hashing is byte-exact. 'ABC' and 'abc' differ. 'abc ' differs from 'abc'. CHAR(5) and NCHAR(5) hash the padded bytes. A `COLLATE Latin1_General_BIN2` clause on 'abc' did not change the digest. |
| Empty input | '', N'' and 0x all returned the SHA2_256 digest of zero bytes. |
| Long input | 8000-byte VARCHAR, 10000-byte VARCHAR(MAX), 10000-byte NVARCHAR(MAX) (5000 characters) and 10000-byte VARBINARY(MAX) all returned the full-input digest with no truncation or error. The same held for 9000-unit NVARCHAR, VARCHAR and VARBINARY RPC parameters, which tedious sends as MAX types. |

All captured HASHBYTES digests were verified independently against Node's
crypto module, except for MD4, whose 'abc' value matches the RFC 1320 test
vector.

## CHECKSUM

| Input | Captured behavior |
| --- | --- |
| Result descriptor | IntN, length 4, flags 33. SELECT INTO created `int`, max_length 4, nullable. The unnamed column name is empty. |
| Typed NULL | A single typed NULL returns 2147483647 for each of INT, VARCHAR, NVARCHAR, DATE and VARBINARY, and also for an RPC NULL INT and a prepared NULL NVARCHAR. For mixed arguments, `(NULL,1)` returns -10, `(1,NULL)` returns 2147483631 and `(NULL,NULL)` returns -2147483640. For a table row whose INT and VARCHAR columns are both NULL, each column alone returned 2147483647 and the pair returned -2147483640. |
| Untyped `NULL` literal | Error 8116, **state 4**, class 16, for whichever argument position holds it. The error occurs before any metadata, so a later valid column in the same SELECT is not returned. |
| Integers | INT returns the value (1, 0, -1, 2147483647). TINYINT, SMALLINT, BIGINT and BIT 1 all return 1. BIGINT 4294967296 returns 1. |
| Exact numerics | 1.5 returns 1768927692 for DECIMAL(10,1), DECIMAL(10,2), NUMERIC(38,10) and the literal. That is, the result was independent of scale and precision in these cases. MONEY and SMALLMONEY 1.5 both return 15000. |
| Approximate numerics | FLOAT 1.5 returns 1073217536 and REAL 1.5 returns 1069547520. FLOAT 0 and -0.0 both return 0. |
| Date and time | DATE 2024-01-02 returns 45291. DATETIME returns 3293111 and SMALLDATETIME returns -1326776136. DATETIME2(7) returns -1217878027, TIME(7) returns -1217915106 and DATETIMEOFFSET(7) with +02:00 returns -204685213. DATETIME2(0) and DATETIME2(7) at a whole second are equal (-1219104654). DATETIMEOFFSET values for the same instant with offsets +00:00 and +02:00 are equal. |
| UNIQUEIDENTIFIER, binary | The GUID returns -1916545669. VARBINARY 0x616263 and BINARY(5) 0x616263 (zero-padded) both return 26435. 0x returns 0. |
| Character, default collation (SQL_Latin1_General_CP1_CI_AS) | 'abc' and 'ABC' both return 34400. '', N'' and 0x return 0. Trailing spaces and CHAR(10) padding are ignored ('abc', 'abc ' and 'abc   ' all return 34400), but a leading space is not (165472). N'abc' and N'ABC' both return 1132495864. VARCHAR(MAX) and NVARCHAR(MAX) of short values equal their bounded forms. |
| Character, other collations | SQL_Latin1_General_CP1_CS_AS: 'abc' returns 169824 and 'ABC' returns 34400. Latin1_General_CS_AS, as either VARCHAR or NVARCHAR, returns 1132495864 for **both** 'abc' and 'ABC'. The Latin1_General_CI_AS, CS_AS and BIN2 VARCHAR columns gave 1132495864 for 'abc', 'ABC' and 'abc ' under CI_AS and CS_AS. Under BIN2 they gave 26435, 17763 and 26435, which are the BINARY_CHECKSUM values. N'é' and N'e' each returned 81 under both Latin1_General_CI_AI and Latin1_General_CI_AS. |
| Long character input | REPLICATE to 10000 characters as VARCHAR(MAX) returns 0. 5000 characters as NVARCHAR(MAX) returns -1912959494, and a 9000-character NVARCHAR RPC parameter returned the same value. |
| Multiple arguments | The result depends on order: `(1,2)` returns 18 and `(2,1)` returns 33. `('a','b')` returns 2159, the same as `('ab')`, while `('b','a')` returns 2174. A 20-argument call succeeded (-2003203156). The RPC call `(@a INT=1, @b NVARCHAR='abc')` returned 1132495848. |
| `CHECKSUM(*)` | Equals CHECKSUM over the explicit column list in table order for every captured row. The qualified form `CHECKSUM(s.*)` is error 102 ("Incorrect syntax near '*'."). Over a table with TEXT and XML columns, `CHECKSUM(*)` raises two 8116 state 4 errors, one for argument 2 (text) and one for argument 3 (xml). |
| Noncomparable types | TEXT, NTEXT, IMAGE and XML, as expressions or columns, each raise 8116 state 4. SQL_VARIANT is accepted: `CAST(1 AS SQL_VARIANT)` returns -1374215287 (not 1), and `CAST('abc' AS SQL_VARIANT)` returns 1132495867. |
| No arguments | Error 1076, state 1, class 15 ("Function 'checksum' requires at least 1 argument(s)."). |

## BINARY_CHECKSUM

| Input | Captured behavior |
| --- | --- |
| Result descriptor | Same as CHECKSUM: IntN, length 4, flags 33, and SELECT INTO creates a nullable `int`. |
| Numeric, date, GUID and binary | Identical to the CHECKSUM values above for every captured non-character value. |
| Character | Case-sensitive and independent of collation. VARCHAR 'abc', NVARCHAR N'abc', 0x616263 and every collation's 'abc' return 26435, and 'ABC' returns 17763. Trailing spaces and CHAR padding are ignored for both VARCHAR and NVARCHAR ('abc ', N'abc ' and CHAR(10) all return 26435). '' and N'' return 0. |
| Long character input | 255 VARCHAR(MAX) characters return 268435462. 256 characters and 10000 characters both return 0. A 9000-character NVARCHAR RPC parameter returned 268435462. |
| Multiple arguments | `(1,2)` returns 18 and `(2,1)` returns 33. `('a','b')` returns 1650, the same as `('ab')`, while `('b','a')` returns 1601. |
| NULLs | Typed NULLs match CHECKSUM: 2147483647 alone, -10 for `(NULL,1)` and 2147483631 for `(1,NULL)`. An untyped `NULL` as the only argument raises **8184**, state 1, class 16 ("Error in binarychecksum. There are no comparable columns in the binarychecksum input."). Untyped NULL next to 1, in either position, is ignored and returns 1, which equals `BINARY_CHECKSUM(1)`. |
| Noncomparable types | These are ignored rather than rejected. TEXT or XML as the only argument raises 8184. `BINARY_CHECKSUM(CAST('abc' AS TEXT),1)` returns 1. `BINARY_CHECKSUM(*)` over (id INT, TEXT, XML) returns 1, which equals `BINARY_CHECKSUM(id)`. SQL_VARIANT 1 returns -1374215287. |
| `BINARY_CHECKSUM(*)` | Equals the explicit column list. It differs from `CHECKSUM(*)` on rows with character data, and equals it on the all-NULL row (2145351543). |
| No arguments | Error 1076, state 1, class 15. |

## Completion tokens and RPC shape

- A successful batch SELECT ends with DONE(rowCount n, more false).
- A compile-time error (8116, 174, 1076, 102, 8184) produces no metadata and
  one DONE with a null row count.
- An sp_executesql call ends with DONEINPROC(n, more true) and DONEPROC.
- The failing INT RPC ended with DONEPROC only.
- Batches with DECLARE or SELECT INTO carry extra DONE tokens with more=true.
- sp_prepare returned the three-column descriptor (VarBinary 8000, IntN 4,
  IntN 4) with DONEINPROC(0) and no rows.
- Each sp_execute returned the same descriptor and one row.
- sp_unprepare returned a single DONEPROC.
- Parameter values did not change the descriptors, including the invalid
  algorithm, the NULL input and the replay of the first value set.

## Not captured

- Other server or database default collations, and other code pages. All
  captures used the server default SQL_Latin1_General_CP1_CI_AS; only
  expression and column collations varied.
- CHECKSUM over computed and indexed columns, and CHECKSUM_AGG.
- Hash-index usage patterns and HASHBYTES in persisted computed columns.
- TVP, bulk-copy and output-parameter paths.
- SQL_VARIANT holding a noncomparable base type, CLR, spatial and hierarchyid
  types, and cursors.
- Bounds on argument counts beyond 20.
- The internal CHECKSUM or BINARY_CHECKSUM algorithm. The fixture records
  values only. The regularities noted above, such as order dependence and the
  long-input zero results, are observations, not a derived formula.
- Whether MD2's NULL result depends on build or configuration. It was observed
  only on the pinned image.

## Proposed successors

**Deterministic core (msduck-core, with the msduck-sql binding rules).** This
successor covers:

- HASHBYTES algorithm-name resolution: case-insensitive, trailing-space
  trimming, a leading space or unknown name gives NULL, and MD2 gives NULL.
- Digest computation over the exact input bytes: the code page or UTF-8 for
  VARCHAR according to collation, UTF-16LE for NVARCHAR, raw bytes for
  VARBINARY, and CHAR/NCHAR padding preserved.
- The fixed VARBINARY(8000) result declaration.
- Argument-family validation: 8116 with the captured states (1 for HASHBYTES,
  4 for CHECKSUM), 174 and 1076.
- The BINARY_CHECKSUM rules: 8184 when no comparable argument remains, and
  noncomparable arguments ignored.
- CHECKSUM and BINARY_CHECKSUM value functions per family, including the
  typed-NULL constants, trailing-space and collation handling, and the
  long-input behavior. These must be validated value-for-value against every
  row in this fixture before any claim.

Suggested scope:

- a new msduck-core module with unit tests driven by
  reference/hashbytes-checksum.json;
- msduck-sql function-signature and binding entries for the three functions
  (arity, argument families and untyped NULL rejection), without DuckDB
  dependencies.

**Root integration (root crate).** This successor covers:

- lowering the functions to the deterministic implementations, either as
  registered DuckDB scalar functions or as adapter evaluation;
- expanding `CHECKSUM(*)` and `BINARY_CHECKSUM(*)` from the bound catalog
  column order, rejecting `alias.*`, and dropping or rejecting noncomparable
  columns as captured;
- propagating collations so that CHECKSUM and VARCHAR HASHBYTES see the
  captured collation-dependent bytes;
- emitting the captured descriptors (VarBinary 8000 and IntN 4 with flags),
  diagnostics and DONE sequences for batches, sp_executesql and prepared
  handles.

Suggested scope:

- root lowering and scalar-function registration;
- a reference-comparison test against this fixture.

Shared parser, engine, catalog, metadata and client-test files were outside
this reference task's scope.
