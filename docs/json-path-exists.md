# JSON_PATH_EXISTS

JSON_PATH_EXISTS returns INT 1 when a path selects a JSON value and INT 0 when it
selects none. JSON null, empty strings and empty objects/arrays count as present;
SQL NULL in either argument propagates NULL. It does not impose JSON_VALUE's
4000 UTF-16-unit limit on selected strings.

The deterministic rule lives in `msduck_core::json_path::exists`. It supports
case-sensitive property names, quoted/escaped keys, zero-based indexes, optional
lax/strict mode, and array wildcards. A wildcard matches when any selected branch
completes the remaining path, including nested wildcards. The walker uses an
explicit work stack and the lexical scanner, retaining support for deep nesting
and numbers outside machine numeric ranges. Existing JSON_VALUE/JSON_QUERY and
OPENJSON path parsing does not gain wildcard syntax through this change.

Malformed documents and invalid paths return 0 rather than a JSON evaluation
error. The current implementation validates the whole source before searching.
The adapter casts inputs to text, handles SQL NULLs, evaluates each argument once,
and returns an exact native INTEGER. Ordinary SQL argument evaluation may still
raise errors independently of JSON path evaluation.

Tests cover the Microsoft wildcard examples, missing versus JSON null, empty
containers, quoted keys, case sensitivity, long strings and deep paths. Native
6000-row tests cover chunk boundaries, NULLs, invalid documents and volatile
source/path evaluation. Independent client tests cover prepared parameters,
stored inputs, predicates, views, CTEs and empty INT metadata. The audit corpus
records values and metadata for later SQL Server comparison.

Remaining work includes live SQL Server comparison (especially validation order
for malformed suffixes, duplicate-key descent, scalar roots and strict wildcard
branches), exact argument/type diagnostics and coercion, isolated UTF-16 surrogate
keys, and newer path ranges/index lists/`last` and native JSON type behavior.

References:

- [Microsoft JSON_PATH_EXISTS reference](https://learn.microsoft.com/en-us/sql/t-sql/functions/json-path-exists-transact-sql)
- [Microsoft JSON path expressions](https://learn.microsoft.com/en-us/sql/relational-databases/json/json-path-expressions-sql-server)
