# SQL Server system column reference

Task #175 captures `sys.all_columns`, `sys.columns`, and
`sys.system_columns` from the pinned SQL Server 2025 image. Run
`node scripts/capture-system-all-columns.mjs OUTPUT_DIRECTORY` where that image
is available to Docker. The generator creates its own container and two isolated
fresh databases, and removes only those resources. Credentials stay in child
environments, never in SQL, arguments, or the fixture.

`reference/system-all-columns.json` retains both complete raw runs, each with
17 observations: server identity, three empty-result TDS descriptors, six
`sys.all_columns` declaration inventories, key and value membership checks,
the full shipped-column inventory, and a created table and view. Every
observation includes SQL, rows, TDS column descriptors, errors, information,
row counts, and tedious completion events. It is decoded client evidence, not
a capture of complete TDS packets. A second invocation used a new container and
two more databases, then compared each run against both retained runs.

## Membership and identities

The pinned image returns 12,803 fresh `all_columns` rows, 1,269 `columns` rows,
and 11,534 `system_columns` rows. On `(object_id, column_id)` the two source
sets are disjoint and their union is `all_columns`. SQL `EXCEPT` over all 43
values also reports zero rows in both directions. This establishes value
membership for this image; it does not make the three views' TDS descriptors
interchangeable.

The full inventory covers 1,003 owner IDs. Of its rows, 10,497 belong to 837
owners present in the retained `reference/all-objects.json` snapshot. The
generator checks each visible owner's raw ID against that snapshot's
schema/name/type identity. Another 2,306 rows belong to 166 owner IDs absent
from `sys.all_objects`. All 166 resolve through `OBJECT_SCHEMA_NAME` and
`OBJECT_NAME` to distinct names in `sys`; for example,
`sys.dm_pdw_nodes_os_tasks`. All 2,306 are from `system_columns`. They must not
be represented as fabricated `all_objects` rows. The presence of an
`all_columns` row does not prove the owner is visible through `all_objects`.

The visible rows span views (`V `: 7,299 columns), inline table functions
(`IF`: 1,783), table functions (`TF`: 140), user tables (`U `: 6), system
tables (`S `: 705), and internal tables (`IT`: 564). The counts describe
columns, not object counts. The raw inventory includes both `sys` and
`INFORMATION_SCHEMA` owner names. Counts and IDs are pinned-build observations,
not universal constants for an emulator.

## Columns and metadata

All three views expose 43 columns in the same name order. The fixture stores
the complete TDS type, length, precision, scale, flag, and collation metadata
for each view, plus the 43 declaration rows for each view. `system_columns`
reports several flags, including `is_computed`, `is_sparse`, and `is_hidden`,
as `Bit`; `all_columns` and `columns` report those as `BitN(1)`. Copying
`all_columns` result descriptors from the source view would therefore be
incorrect even though their row values form a union. The fixture also retains
the 12 declaration rows each for `sys.objects`, `sys.system_objects`, and
`sys.all_objects`, including source type IDs, lengths, collation and flags.

The six columns created for `column_probe.sample` and
`column_probe.sample_view` demonstrate ordinary `columns` membership, identity,
explicit collation, default-object linkage, a computed column, and view
projection. In the shipped inventory, `default_object_id` and `rule_object_id`
are zero; the created `label` column has a nonzero default ID resolving to
`column_probe.df_sample_label`. Forty-eight shipped rows have a nonzero
`owner_parent_object_id`. No owner row is fabricated for unresolved owners.

Rows are queried in `(object_id, column_id)` order. That order is an explicit
capture order, not a promised natural order for an unqualified SELECT. The
fixture retains raw IDs. For comparison across fresh databases, `object_id`
binds to the captured owner schema/name/type; nonzero owner-parent IDs bind
to captured parent schema/name/type, while default and rule IDs bind to their
resolved qualified names. All other row values,
descriptors, errors, and completion records compare exactly. The four fresh
databases happened to return identical raw observations, including IDs. No
clock-dependent field occurred in these captures; this does not establish
stability for other DDL, databases, images, or server versions.

## Runtime boundary

The current msduck catalog exposes `sys.objects`, `sys.system_objects`,
`sys.all_objects`, and a user-column `sys.columns` view backed by transactional
table/view metadata. Its `sys.columns` projection stops at `is_masked` and
lacks the nine later fields in this pinned build. There is no
`sys.system_columns` or `sys.all_columns` view. Runtime work needs separate
source membership, the full shipped inventory, the missing `sys.columns`
fields, and per-view result descriptors and declaration metadata. It must
preserve transactional user-column changes and account for the
166 resolvable column owners outside `all_objects` without altering that
view's captured membership. The fixture does not establish permissions-based
visibility, arbitrary DDL transitions, collation semantics, or execution of
the catalog objects whose columns it records.
