# Built-in type catalog

sys.types exposes the 34 built-in definitions reviewed in the upstream
mssqlite packages/catalog/src/type-row.ts and the sys skill. It includes distinct
system_type_id and user_type_id values, schema ownership, maximum length,
precision, scale, collation and type flags. sysname has user_type_id 256 and
system_type_id 231, with length 256 bytes. Built-in CLR entries use system ID 240
and the upstream user IDs 128–130. The catalog is a read-only view recreated at
startup and links to the sys schema.

TYPE_ID accepts unqualified built-in names or sys-qualified names, including
quoted identifiers. Lookup is case insensitive; missing, malformed and NULL
names return NULL. TYPE_NAME converts its argument using the shared INT assignment
rules and returns an unqualified name or NULL for an unknown ID. Known TYPE_NAME
expressions expose NVARCHAR(128), including empty/NULL results. TYPE_ID returns
INT. Numeric and boolean sys.types columns expose their documented wire widths.

Native tests verify all seeded type ID/name round trips and once-per-row volatile
arguments across 6,000 rows. Client tests cover schema joins, sysname, numeric and
temporal definitions, prepared inputs, empty descriptors and invalid arguments.
The local audit records selected definitions without claiming SQL Server parity.

This catalog does not add implementations for the represented types: CLR types,
XML, sql_variant and several other type behaviors remain unfinished. User-defined
alias/table/assembly types, type DDL, permissions, exact collation behavior and
SQL Server version-specific additions (including JSON/vector types) remain open.
DDL type synonyms are not separate sys.types rows and are not resolved by the
current TYPE_ID lookup. Catalog name/collation text descriptors remain unbounded;
[sys.columns](columns.md) preserves newly declared table types, while full
column provenance and metadata remain pending. Character collation names use the
existing SQL_Latin1_General_CP1_CI_AS catalog default.

References: [sys.types](https://learn.microsoft.com/en-us/sql/relational-databases/system-catalog-views/sys-types-transact-sql),
[TYPE_ID](https://learn.microsoft.com/en-us/sql/t-sql/functions/type-id-transact-sql),
[TYPE_NAME](https://learn.microsoft.com/en-us/sql/t-sql/functions/type-name-transact-sql).
