# RETURN and RPC completion

RETURN stops the active batch, including all pending nested IF/WHILE/block work.
A bare RETURN uses status zero. An expression is evaluated with the current
bindings and converted to INT. RPC responses carry the signed 32-bit status in
RETURNSTATUS before DONEPROC. Return status is local to the request; the next
request starts at zero. Bare SQL-batch RETURN produces normal final completion.

NULL return expressions produce informational message 282 (severity 10) and
status zero. INFO and ERROR share the same bounded diagnostic payload encoder,
with separate token IDs and severity. RETURN emits completion after any INFO
token, including when no result statements ran. Conversion errors stop the
batch with an error completion. RETURN updates the session row count to one.

RETURN neither commits nor rolls back. Prior autocommit writes remain committed,
and an explicit transaction remains open for subsequent caller operations.
The existing variable preflight still checks references after RETURN because
unreachable statements remain part of the parsed batch.

Tedious tests verify nested early exit, signed INT boundaries, NULL warnings,
status reset, conversion-error recovery, skipped writes and transaction rollback
after early return. Tiberius tests verify bare SQL-batch RETURN, completion
without results and connection reuse. SQL Server differential validation remains
required, especially exact context restrictions for valued RETURN, conversion
edge cases, diagnostic wording and token/row-count behavior. Application stored
procedures and procedural prepared RPCs are not implemented yet.

References: [RETURN](https://learn.microsoft.com/en-us/sql/t-sql/language-elements/return-transact-sql)
and [message 282](https://learn.microsoft.com/en-us/sql/relational-databases/errors-events/database-engine-events-and-errors-0-to-999).
