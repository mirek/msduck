# Character storage conversion

INSERT and UPDATE recover VARCHAR, CHAR, NVARCHAR and NCHAR declarations from
catalog metadata after DuckDB erases their widths. Conversion occurs on target
values before storage. VARCHAR/CHAR use the current Windows-1252 code page;
NVARCHAR/NCHAR count UTF-16 units. Fixed-width types pad shorter values with
spaces. NULL stays NULL. MAX preserves the complete value.

Under the supported ANSI_WARNINGS ON setting, excess non-space text raises
legacy error 8152. Excess trailing ASCII spaces may be discarded. A failed
multi-row INSERT or UPDATE leaves no partial writes. Explicit expression casts
continue to use their own truncating rules; local-variable assignment is not
routed through the storage converter.

CREATE/ALTER ADD defaults and ALTER COLUMN conversions use the same native
functions. Failed narrowing preserves the prior declaration and values.
Persisted catalog types and default expressions work after database reopening.
ALTER ADD can install a converted literal while creating the column, then restore
the executable default. This avoids DuckDB's NOT NULL restriction after expression
default updates. Nullable ADD without WITH VALUES keeps deferred evaluation.
The backing type is VARCHAR; declared families and widths remain in sys.columns
and client result metadata.

Native tests cover byte/UTF-16 widths, fixed padding, trailing spaces, 6000-row
NULL vectors, single evaluation and file restart. Client tests cover all four
families, multi-row atomicity, UPDATE, prepared recovery, omitted/default values,
ALTER ADD WITH VALUES, failed narrowing and TRY/CATCH. Audit captures preserve
legacy error numbers rather than pretending they match modern verbose errors.

The upstream `character.ts` storage converter and `engine.test.ts` cases informed
the implementation. Upstream reports 2628 and rejects any excess bytes; this
implementation currently uses 8152 and follows Microsoft's documented trailing
space exception.

Remaining work:

- Configurable verbose 2628 diagnostics with table/column/value context and exact states.
- ANSI_WARNINGS OFF and legacy ANSI_PADDING behavior; only ON is supported.
- Exact noncharacter source coercion, additional code pages and collation rules.
- Binary-column storage conversion and complete general character type propagation.
- Reference testing for error precedence and less common DDL paths, including
  nonconstant ALTER ADD defaults combined with NOT NULL.
- Control-flow evaluation after a failed statement has aborted DuckDB's transaction:
  direct ROLLBACK works, but evaluating IF through a backend query can still fail.
- Live SQL Server comparison.

References:

- [Microsoft SET ANSI_WARNINGS](https://learn.microsoft.com/en-us/sql/t-sql/statements/set-ansi-warnings-transact-sql)
- [Upstream revision](reference-review.md)

Implementation boundary: validated character types, padding, storage overflow and
CAST conversion rules now live in `msduck-core::character`; Windows-1252 encoding
lives in `msduck-core::encoding`. DuckDB callbacks and AST/catalog adaptation stay
in the root crate. Pure tests run without DuckDB; existing vector, atomicity and
restart tests continue to verify the adapters. See [architecture](architecture.md).
