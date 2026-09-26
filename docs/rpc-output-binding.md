# Application RPC OUTPUT binding

`crates/msduck-sql/src/rpc_output.rs` is a pure binding plan for application RPC
parameters. It accepts ordered, already-parsed declarations with an explicit
OUTPUT direction and ordered, already-decoded RPC parameters. It resolves named
parameters case-insensitively, treats an empty name as the declaration at that
same RPC ordinal, rejects duplicate or undeclared names and mismatched
directions, and retains output slots in received RPC order. Final values are
planned in TDS emission order: bounded/scalar outputs first, then MAX character
and binary outputs, retaining received order within each group. The slot's
ordinal remains its original **application-parameter** ordinal; the root
adapter must add the built-in RPC's leading control-parameter count when
encoding the full TDS `ParamOrdinal`. This grouping follows the
[MS-TDS RETURNVALUE rule](https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-tds/7091f6f6-b83d-4ed2-afeb-ba5013dfb18f);
the initial first-party capture exercises only two small outputs. A later
first-party raw wire probe found a small INT at RPC ordinal 3 emitted before
NVARCHAR(MAX) at ordinal 2 and VARBINARY(MAX) at ordinal 4, even though both
MAX values were small. Every binding and
output slot retains the declared SQL type independently of its input or final
value. The plan supports output types that the first typed RETURNVALUE codec
can encode: integer widths, BIT, character, binary and DECIMAL. Other output
types fail explicitly until an encoder and reference evidence support them.

The caller supplies the final variable map after execution. Only a completed
RPC yields output values. An uncaught error yields none, even if an earlier
statement assigned a value; a caught error that completes returns its final
values. Missing or retyped output variables fail rather than fabricating a
NULL. This follows the first-party SQL Server observations in
`reference/rpc-output.json` and `docs/rpc-output-reference.md` from the
owner-authored RPC reference work: two outputs followed RPC order, declared
metadata survived NULL, raw UTF-16 and empty binary remained distinct from
NULL, and uncaught THROW suppressed outputs.

This file is not yet exported from `msduck-sql`: `lib.rs` belongs to another
exclusive task, so its tests import the production source by path. A successor
integration task must add that export, parse declaration OUTPUT direction
without losing it (the current `parameter_declarations` returns only names and
types), use this binder in `src/rpc.rs`, retain final variables from
`Session::batch_response_inner`, adapt each typed value to `msduck-tds`
RETURNVALUE encoding, and insert those tokens after statement results and
before RETURNSTATUS/DONEPROC. It must separately cover `sp_execute` and error
paths. The reference capture records tedious interpreting prepared OUTPUT
values as an unexpected parameter even though SQL Server sent the values;
the server must preserve the wire behavior rather than hide that client error.
