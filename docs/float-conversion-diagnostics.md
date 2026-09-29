# FLOAT character conversion diagnostics

Two independent pinned SQL Server 2025 containers captured malformed percentile
character fractions as 8114/state 5/severity 16, with exactly
`Error converting data type varchar to float.` or the corresponding `nvarchar`
message. The deterministic core recognizes only those complete messages.
Existing adapters strip their own backend envelope and preserve explicit
application error identity. Other source/target families are not inferred.
FLOAT numeric overflow retains 8115/state 2. Integrated verification and merge
are recorded with the separately claimed percentile character-fraction PR.
