# OPENJSON

OPENJSON(document [, path]) returns key NVARCHAR(4000), value
NVARCHAR(MAX), and type INT. Objects expose every member, including duplicates;
arrays expose zero-based index strings. Values retain lexical number spelling,
decode JSON strings, retain object/array source fragments, and map JSON null to
SQL NULL. Type codes are 0/null, 1/string, 2/number, 3/boolean, 4/array, 5/object.
NULL input and empty containers produce no rows with the same metadata.

The scanner and path selection are shared with JSON_VALUE/JSON_QUERY. This
implementation follows upstream OPENJSON's full document validation and requires
an object or array root. Missing lax paths and selected scalars produce no rows;
missing strict paths report 13608/state 3. Document and path errors report
13609/state 4 and 13607/state 22. Known messages are canonicalized on the wire and
in TRY/CATCH. Detailed syntax positions are not implemented.

Native extraction returns a LIST of typed STRUCTs. Relational lowering uses
UNNEST in a lateral subquery, allowing CROSS/OUTER APPLY. No JSON extension or
serialization round trip is required. Each scalar invocation evaluates both
inputs once; native tests exercise 6000 input rows and a 6000-element child
vector. As with other correlated DuckDB execution, identical correlated values
may be deduplicated by the backend; volatile lateral evaluation needs further
reference testing.

Client tests cover default rows, aliases, integer type expressions, empty/NULL
metadata, long Unicode values, prepared failure/recovery, stored documents,
CROSS/OUTER APPLY and persisted view metadata. Audit probes retain lexical values
and capture outer-join NULL extension. Captures are not SQL Server comparisons.

Explicit WITH schemas now select one source row per array element, or one for an
object. Column names supply case-sensitive, quoted property paths by default;
explicit paths override them. Scalar columns decode strings and retain lexical
numbers before the existing cast pipeline applies the declared type. Long scalar
text is not limited to JSON_VALUE's 4000 UTF-16 units. AS JSON preserves the
original object/array slice and requires NVARCHAR(MAX), otherwise reporting
13618. TEXT, NTEXT, IMAGE and SQL_VARIANT declarations report 13614.

Strict missing column paths report 13608/state 6; wrong scalar/fragment kinds
report 13624/state 1, following upstream tests. Client tests cover numeric,
Boolean, date and exact temporal conversions, bounded strings, long values,
prepared recovery, nested APPLY, views and empty results. Conversion behavior
inherits the existing type implementation and still requires live reference
comparison, particularly numeric/Boolean coercion and less common types.

Top-level paths accept local variables and RPC/prepared parameters as well as
literals. A token adapter represents just that argument as a parser placeholder,
then lowering restores the normal variable expression and bound parameter path.
Batch validation sees those variables before execution, including unexecuted
branches; view validation rejects them to prevent capturing session values.
Tests cover changed paths, NULL, strict errors and recovery, default/WITH schemas,
comments and nested source arguments, and undeclared variables in prepared SQL.
Column paths inside WITH remain string literals.

Omitted WITH character lengths default to 1. The same normalized declarations
control conversion, source typing and result metadata for VARCHAR, CHAR,
NVARCHAR and NCHAR (including CHARACTER/CHARACTER VARYING aliases). Fixed-width
values are padded by the existing conversion functions. Ordinary CAST/CONVERT
omitted-length defaults remain 30. Tests cover empty strings, NULLs, explicit
widths and MAX, prepared input, views and empty results. Declared character
metadata now retains VARCHAR/CHAR/NCHAR families through catalog provenance;
this does not complete general character storage enforcement or collation work.

Binary schemas decode Base64 JSON strings to byte buffers. VARBINARY(n) preserves
the decoded length; BINARY(n) pads shorter values with trailing zero bytes.
Both report 13613 when the decoded value exceeds the declared size, and malformed
Base64 reports 13612. Omitted binary widths default to 1. Bounded binary metadata
uses the declared type and width, including empty results, views and imported
columns. VARBINARY(MAX) retains PLP transport for long values. Native tests cover
arbitrary bytes and vector NULLs; client tests cover padding, overflow, prepared
recovery, 20 KB payloads and stored/view metadata.

The decoder currently uses the standard alphabet, padded quartets and canonical
unused bits, ignoring ASCII space/tab/CR/LF. Exact SQL Server acceptance of
noncanonical encodings and whitespace remains unverified. Non-string scalar
binary conversion fails explicitly. Full ordinary BINARY/VARBINARY cast and
storage enforcement is separate remaining work. No upstream OPENJSON binary
conversion implementation was found.

Remaining work includes non-string binary coercion, Base64 edge-case comparison,
complete path grammar, key truncation/overflow semantics,
BIN2 key and inherited value collations, isolated UTF-16 surrogate handling,
compatibility-level gating, exact malformed-document search order and diagnostics,
and live SQL Server comparison.

Sources:

- [Microsoft OPENJSON reference](https://learn.microsoft.com/en-us/sql/t-sql/functions/openjson-transact-sql)
- Upstream `packages/engine/src/json.ts` and `json.test.ts`, at the revision
  recorded in [reference review](reference-review.md).

The deterministic implementation lives in `msduck-core::openjson`: default rows,
explicit-schema source and column selection, and Base64 conversion share the
core's JSON scanner and path rules. SQL declaration validation, coercion ASTs,
metadata and DuckDB vector marshalling remain root adapters. OPENJSON diagnostic
states stay distinct from JSON_VALUE/JSON_QUERY states. Pure tests run without
DuckDB; native vector and independent client tests still cover the adapters.
