# RPC procedure calls and RPC OUTPUT parameters

Issue #753 (task `gaps-rpc-procedures-v1`, with its companion
`gaps-rpc-procedures-v1-runtime`). Before this work:

- An RPC request that names a procedure (tedious `callProcedure`, mssql
  `request.execute('name')`) failed with 40515 "unsupported RPC procedure".
  This covered user procedures and system procedures such as
  sp_getapplock and sp_set_session_context.
- sp_executesql over RPC with OUTPUT parameters (tedious
  `addOutputParameter`) failed with 40515 "unsupported output value
  parameter".
- A duplicate key inside an RPC request ended the whole request, and
  sp_prepexec then returned no prepared handle.

The SQL batch side of procedures is described in
[gaps-procedures.md](gaps-procedures.md).

## Evidence

`scripts/capture-gaps-rpc-procedures.mjs` sends 115 requests through tedious,
each time in a fresh SQL Server container. The retained runs in
`reference/gaps-rpc-procedures.json` come from
`mcr.microsoft.com/mssql/server:2022-latest` (16.0.4236.2,
`sha256:0ec7739e1c5ec2f57861facbe1f2b74f1d3e147c7c97edf91eeea920c5944d9c`).
They were captured with `MSSQL_REFERENCE_IMAGE` set to that image, since the
2025 image pinned in `scripts/lib/reference-container.mjs` is not available
on this host. Two independent containers produced identical streams.
`node scripts/capture-gaps-rpc-procedures.mjs --check` revalidates the
retained runs without a container.

Each observation records, in order:

- the DONE, DONEPROC, DONEINPROC, RETURNSTATUS, RETURNVALUE and COLMETADATA
  tokens, byte for byte;
- the kind of each ENVCHANGE (transaction descriptors differ between
  servers);
- ERROR and INFO messages through tedious events, since their server name
  differs;
- result rows and returned values.

`tests/compat/rpc_procedures.test.mjs` replays every observation against
msduck through tedious on one connection. It requires the same rows, error
and info numbers, returned values and token bytes, except for the
differences listed at the end of this document. Each of those must still
differ, so a fix is noticed. The file also has focused tests.

`tests/gaps_rpc_procedures.rs` sends hand-encoded requests and checks exact
token bytes, including forms tedious cannot send, such as the fDefaultValue
status flag. Unit tests in `src/rpc.rs`, `src/rpc/procedures.rs` and
`src/engine/ext/procedures/rpc.rs` cover the RETURNVALUE encodings, handle
bookkeeping and argument construction.

## Procedure calls by name

An RPC whose name is not one of the prepared-statement procedures
(sp_executesql, sp_prepare, sp_prepexec, sp_execute and sp_unprepare, by
name or id) runs as the call `EXEC name arguments` would in a SQL batch.
`Session::rpc_call` (`src/engine/ext/procedures/rpc.rs`) does this:

- **The name** is parsed as a T-SQL object name: one to three parts,
  quoted or not (`]]` escapes `]` inside brackets), case-insensitive. A
  name that does not parse, such as `p; SELECT 1`, fails with 2812. The
  parsed name goes into the statement as is; it never becomes SQL text.
- **Arguments** are bound to synthetic caller variables typed as the RPC
  parameters. Values are never rendered as SQL. Arguments are passed in
  request order:
  - named (`@a`, or `a`, which names the same parameter);
  - positional (an empty name), bound by its ordinal even after a named
    argument, as SQL Server does (the T-SQL grammar would reject that with
    119);
  - `OUTPUT` for the fByRefValue status flag (1);
  - `DEFAULT` for the fDefaultValue status flag (2).

  The fEncrypted flag is refused.
- **Dispatch** follows the SQL `EXEC` path:
  - sp_set_session_context first;
  - then the features' exec hooks: user procedures and sp_executesql
    (procedures), sp_getapplock and sp_releaseapplock (applock), and any
    later feature such as sp_rename or sp_pkeys;
  - then, as in a batch, the engine's statement path.
- **Batch scope.** The request is its own batch: `batch_begin` and
  `batch_end` run around it. A transaction left doomed is rolled back with
  3998, as at the end of any batch. The features' `batch` hooks, which
  inspect batch text, do not run: the request has no text.

Binding, conversion, defaults, return status, frames and errors inside the
procedure are those of the procedure runtime ([gaps-procedures.md](gaps-procedures.md)).

### Token stream

As captured:

- **A completed call** sends:
  - the procedure's own tokens, with DONEINPROC completions;
  - RETURNSTATUS: the RETURN value, or the procedure status rule;
  - one RETURNVALUE per OUTPUT parameter, in request order;
  - DONEPROC with status 0 and CurCmd 224.

  Statement-terminating errors inside the procedure (RAISERROR, a duplicate
  key, divide by zero) do not change this. The status reflects them, for
  example -6 after RAISERROR severity 16 and -4 after a duplicate key.
- **A call error** sends only the error and DONEPROC with the error flag
  (status 2, CurCmd 224), with no RETURNSTATUS or RETURNVALUE. Call errors
  are 201, 8144, 8145, 8143, 8162, 8114 and 2812. The same applies to:
  - a compilation error found while the procedure runs (208 for a missing
    table);
  - an error that ends the batch (THROW);
  - an OUTPUT value that does not fit its parameter's type (8114 state 2).
- **A changed `@@TRANCOUNT`** sends RETURNSTATUS 0, then 266, then DONEPROC
  with the error flag.

### RETURNVALUE

Each RETURNVALUE echoes its request parameter:

- **Ordinal:** the parameter's position in the request. For sp_executesql
  the statement and the declarations count, so the first value is 2.
- **Name:** as sent, `@` included, or empty for a positional parameter.
- **Status, user type and flags:** status 1, user type 0 and flags 0.
- **TYPE_INFO:** the parameter's declared type, as the client sent it, with
  two exceptions. Character types carry the server collation, and decimal
  and numeric declare a capacity of 17 bytes.

The value is converted to that declared type, not to the procedure's
parameter type:

- A `bigint` parameter returns a bigint. An `nvarchar` parameter declared
  for an `int` procedure parameter returns `N'6'`.
- Character values are truncated to the declared length: `nvarchar(2)`
  returns `ab` for `abcdefgh`.
- A value that does not fit fails with 8114 (state 2), and the call returns
  nothing.

An OUTPUT parameter whose procedure parameter is not assigned returns the
value it was sent with. A parameter sent without the OUTPUT flag returns
nothing, even when the procedure declares it OUTPUT.

### System procedures

- sp_set_session_context works with named or positional parameters, with
  or without the `sys.` prefix. It sends RETURNSTATUS 0 and DONEPROC.
- sp_getapplock and sp_releaseapplock run through the applock feature, with
  the same values and errors as in a batch.
- sp_executesql by name behaves as by id.

## sp_executesql and prepared statements with OUTPUT parameters

An sp_executesql request with at least one OUTPUT value runs as
`EXEC sp_executesql @stmt, @params, values...` through the procedure
runtime, and then returns the values. Requests without OUTPUT values keep
the engine's existing RPC path. The same applies to sp_prepexec and
sp_execute with OUTPUT values. Their statement and declaration text are
bound as `nvarchar(max)`.

As captured:

- **Completed.** The statement's tokens, then RETURNSTATUS, the
  RETURNVALUEs and DONEPROC. The status is the last error number, reset by
  a later successful statement. So it is 0 after
  `SET @x = 1; RAISERROR(...); SET @x = 2`, and `@x` returns 2.
- **Failed.** A compilation error (208, 102) or a call error (8144, 8162,
  214, 8114) sends:
  - the error;
  - RETURNSTATUS with the error number, except 1 for a missing value
    (8178);
  - each OUTPUT parameter with the value it was sent with;
  - DONEPROC with the error flag.
- **Aborted.** THROW or a conversion error that ends the batch sends only
  DONEPROC with the error flag.

## Prepared handles

- sp_prepexec returns its handle after RETURNSTATUS: RETURNSTATUS, the
  handle's RETURNVALUE, then any other OUTPUT values. This holds even when a
  statement failed with a statement-terminating error such as a duplicate
  key. The handle then stays valid.
- When the batch is aborted (THROW), sp_prepexec returns no handle. The
  handle number is not consumed: the next preparation receives it.
- sp_execute and sp_unprepare with an unknown handle send:
  - 8179, with state 4 for sp_execute and state 8 for sp_unprepare;
  - RETURNSTATUS 8179;
  - any OUTPUT parameter with the value it was sent with;
  - DONEPROC with the error flag.

A duplicate key inside any RPC request now ends only its statement, as in a
SQL batch: 2627 or 2601, 3621, and DONEINPROC with the error flag
(`src/engine/ext/keys/duplicate.rs`; see [gaps-keys.md](gaps-keys.md)). A
later successful statement resets the RPC status to 0, as SQL Server does.

## Remaining differences

The replay test asserts each of these:

- ROLLBACK and `SET XACT_ABORT` complete with CurCmd 0. BEGIN TRANSACTION
  now sends SQL Server's 212.
  SQL Server sends 212, 210, 185 and 186. These completions come from the
  engine.
- An RPC batch that the engine's own RPC path aborts ends with DONEPROC
  CurCmd 0, not 224. An example is sp_prepexec without OUTPUT values whose
  statement throws. Requests that run through `Session::rpc_call` end with
  224.
- sp_prepare sends no DONEINPROC (`0x11`, CurCmd 193) before its
  RETURNSTATUS. Preparation metadata belongs to the prepared-statement
  work.
- `SERVERPROPERTY('ProductVersion')` reports msduck's `16.0.0.0`, not the
  captured server's build, so the version observation is skipped.

Message texts and other details differ, without affecting numbers or
tokens:

- Engine diagnostics keep their own text: 208 "Catalog Error: Table with
  name ...", 245 "Conversion Error: ...", and parser messages for 102.
- ERROR and INFO tokens carry no procedure name and report line 1. SQL
  Server reports line 0 for call errors.
- A conversion error binding an sp_executesql value has state 1; SQL Server
  uses state 5.
- After 266, RETURNSTATUS is always 0, even when the procedure returned
  another value.
- 8179 for an unknown handle does not set `@@ERROR` for the next batch.
- `SESSION_CONTEXT` of an nvarchar value still needs an explicit CAST
  (40515 otherwise). This is an existing limit, and a SQL batch has the
  same one.
- The keys feature still records whether a batch is an RPC request
  (`src/engine/ext/keys.rs`, another task's scope), although the flag no
  longer changes the outcome.

These forms are not supported:

- table-valued (`READONLY`) parameters by procedure name;
- encrypted (fEncrypted) parameters;
- RPC option flags (WITH RECOMPILE, NO_METADATA);
- numbered procedures and calls into another database, as in a batch.
