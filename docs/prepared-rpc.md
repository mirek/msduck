# Prepared statement RPCs

The dispatcher in `src/rpc.rs` accepts sp_prepare (11), sp_execute (12),
sp_prepexec (13), and sp_unprepare (15), by numeric ID or procedure name.
It also retains the existing sp_executesql path. Handles belong to the physical
connection and are discarded on disconnect.

Preparation parses parameter declarations and T-SQL, translates a temporary
AST, and calls DuckDB prepare without stepping the statement. SELECT, INSERT,
UPDATE and DELETE are supported within the current engine's language coverage.
Scalar DECLARE and local SET (including compound SET) are also supported.
Batch variable preflight checks declarations and references before preparation.
Initializers and SET expressions are cast to their declared type and compiled
without evaluation. Runtime errors and volatile expressions remain execution-time
work, and each execution recreates its local bindings.
IF/ELSE, WHILE, BEGIN/END, TRY/CATCH, BREAK/CONTINUE, RETURN, THROW, PRINT,
and supported transaction statements can also be prepared. A work stack visits
all branches and each loop body once; it never follows runtime conditions.
Expressions are compiled without evaluation, while ordinary DML retains its
nonexecuting preparation path. Infinite loops can therefore be prepared safely.
Condition truth remains execution-time work. Preparation reads the bound
expression's logical type directly from DuckDB's prepared statement, without
stepping it, and rejects non-Boolean IF/WHILE conditions with error 4145. This
also checks conditions inside skipped branches. Tests cover integer and text
conditions, deferred division errors, NULL predicates, EXISTS and fresh data
between executions. A shared [search-condition validator](predicates.md) also
rejects scalar BIT values as conditions before translation. Full T-SQL operand
conversion and specialized predicate contexts remain unfinished.
Supported session SET statements now share a pure validator with execution.
Preparing NOCOUNT ON/OFF does not change the session; only executing the
statement applies it. Fixed-default startup settings use the same existing
allowlist in both paths, while unsupported settings still fail explicitly.
Tests verify prepare-time nonexecution, rejected-batch state preservation,
conditional settings, repeated execution and named sp_prepexec.
T-SQL tokenization also separates adjacent less-than/variable expressions such
as `@n<@limit`; the upstream tokenizer's PostgreSQL `<@` token must not absorb
the variable prefix. The prepared-loop regression exercises this form.
No data changes occur during sp_prepare. The retained entry contains the source
SQL and declaration types; execution translates anew with current session
state and binds fresh values. This is not a native execution-plan cache.

Ordinary and compound SELECT assignments to declared input parameters use the
execution path's assignment lowering during preparation. Preparation binds the
translated query without evaluating assignments or preceding DML. Assigned
values are visible to later statements in the same execution; the next
execution starts from its new input values. Missing targets and mixed
assignment/result projections fail validation. Tests also cover CTEs, empty
results, NULLs, truncation to integer targets, overflow recovery and named
sp_prepexec reuse. Application OUTPUT parameters are still separate unfinished work.

The handle is returned through a typed integer RETURNVALUE before final RPC
completion. sp_prepexec also returns the query results. Failed validation or
failed prepexec execution does not retain a handle. Invalid/released handles
return 8179. Handles increase without reuse or integer wrap. Successful
unprepare reclaims retained text accounting. A connection accepts at most 1024
live handles and 16 MiB of SQL/declaration text.

Verification:

- Tedious prepare/execute/unprepare with inserts, repeated values, typed NULLs,
  Unicode/SQL-looking strings, errors and reuse.
- Named sp_prepexec produces results and a handle usable by sp_execute.
- Two clients on one listener cannot access or release one another's handles.
- Truncated requests, invalid SQL, duplicate declarations, missing variables,
  exhausted handle space, and failed prepexec do not allocate live handles.
- Exact integer OUTPUT token vector and storage reclamation checks.

The test harness clears tedious's retained Request.error before repeating an
execution on the same Request. This is driver state, not a server error-token
workaround; ordinary errors remain asserted by their SQL error number.

Still required: prepare-time result metadata (option 1), native plan caching,
application OUTPUT parameters, additional RPC data types, DDL
preparation and full SQL Server differential token/error validation. Option 1
and unsupported parameter status flags are rejected explicitly.
Complete SET semantics and option-scope restoration for nested execution remain
unfinished; accepting the existing fixed defaults does not establish them.

References inspected:

- mssqlite server connection dispatch and TDS RETURNVALUE encoder, at
  `7f71f2081602f8e3051998f5c11f058e65fe24ec`.
- Copied TDS protocol and tedious skills.
- [Microsoft sp_prepexec contract](https://learn.microsoft.com/en-us/sql/relational-databases/system-stored-procedures/sp-prepexec-transact-sql?view=sql-server-ver17).
