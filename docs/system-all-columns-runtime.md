# System column catalog runtime

The reference for this implementation is the pinned SQL Server capture in
`reference/system-all-columns.json`. `scripts/build-system-column-seed.mjs`
validates both retained fresh-database runs against one another and the
`all_objects` capture, then writes the 43 raw column values and source
membership to `src/column_catalog/system_columns.json`. The generated seed
also retains the 166 resolved `sys` names for column-owner IDs that are absent
from `sys.all_objects`. Run the generator twice and compare file hashes to
check reproducibility.

On a fresh database, `column_catalog::register` loads the 12,803 captured
built-in rows and 166 hidden-owner names in one transaction. The database
retains the pinned source-image identity and validates seed counts on reopen.
The public `sys.columns` view combines its existing transactional user-column
projection with the 1,269 shipped rows; `sys.system_columns` returns the
11,534 distinct system rows; `sys.all_columns` is their disjoint union.
These views have the pinned 43-column value shape. A hidden owner can resolve
through `OBJECT_NAME` and `OBJECT_SCHEMA_NAME` without adding a fabricated
`sys.all_objects` row. `OBJECT_ID` continues to use its existing object
membership because the reference did not establish reverse lookup for those
hidden names.

The captured built-in values retain their exact IDs, flags, type IDs,
collations, NULLs and source membership. User table/view columns still flow
through the existing live catalog and DDL transaction path. Their new fields
are NULL or false for currently supported ordinary columns. Explicitly named
DEFAULT constraints on newly created columns receive a transactional object
identity, parent link and `default_object_id`, including after reopen. The
current DDL path does not retain SQL Server's generated names for unnamed
DEFAULT constraints, so their `default_object_id` remains unknown. SQL Server
computed-column syntax is rejected by the current parser before this catalog
can record expression metadata; the user-column projection does not invent
computed flags. Permission-filtered visibility and arbitrary
system-object execution are also outside this catalog seed.

The SQL value views do not by themselves establish the exact TDS descriptors
captured for `sys.columns`, `sys.system_columns`, and `sys.all_columns`.
`src/query_catalog.rs` handles result metadata and is owned by a separate
ready task; wire descriptor matching remains follow-up work. In particular,
the captured `system_columns` Boolean descriptor flags differ from the
nullable Boolean descriptors of the two other views even when their values
are identical.
