# SQL Server `FOR XML PATH` reference

`reference/for-xml-path.json` retains 21 owner-run observations from each of two fresh databases in the pinned SQL Server 2025 image `mcr.microsoft.com/mssql/server:2025-latest@sha256:86cc6144ef39bb0fbed2329e1ad79b13ee82e7b2e4739213a0db0800e668a74a` (ProductVersion `17.0.4065.4`). The two database runs matched exactly. A second fresh container with two more databases reproduced the retained fixture byte for byte. Its SHA-256 is `5432cbb1144b53c5a28dd57e57126d9fe6934d7771d36e8cd0e619548fc7b9a4`.

The fixture contains every query, ordered result-column descriptor, row, error, information event and DONE event returned to tedious. The captures include ordinary and empty `PATH` row names, the default row name, attributes, element and text aliases, nested paths, XML escaping, ordered rows, NULL omission and `ELEMENTS XSINIL`, zero source rows, `ROOT`, `TYPE`, a typed nested query, binary Base64, and three invalid forms. The generator validates the expected success/error boundary and refuses to overwrite an existing fixture.

| Probe | SQL Server observation |
| --- | --- |
| Direct `FOR XML PATH('row')` | One `NText` column named `XML_F52E2B61-18A1-11d1-B105-00805F49916B`, advertised length `2147483646`, flags `1`, database collation. `<row><value>1</value></row>` is one row. |
| `PATH('')` | Omits the row wrapper: `<value>1</value>`. |
| Attribute and escaped element | `<item id="7"><name>A&amp;B &lt;C&gt; "quoted"</name></item>`. |
| Ordered two-row source | One XML text row containing both ordered `<item>` elements; DONE rowCount is `2`. |
| NULL element/attribute | Omitted, yielding `<row/>`; `ELEMENTS XSINIL` instead emits a namespaced `xsi:nil="true"` element. |
| Zero source rows | The same `NText` descriptor is sent, with zero result rows and DONE rowCount `0`. |
| Direct `TYPE` | One unnamed `Xml` column, null length/collation, flags `3`; the XML value is a single row. |
| Nested `TYPE` projection | Named `Xml` column `payload`, null length/collation, flags `35`. |
| Attribute after element | Error `6852` and no result set; the message identifies `@late` as following a non-attribute sibling. |
| Repeated `ROOT` or `TYPE` directive | Error `102`, with no result set. |

The copied `mssqlite` corpus records a deliberate `NVarChar` versus SQL Server `NText` difference for its `FOR XML PATH` case. This capture preserves SQL Server's `NText` wire descriptor; it does not reinterpret that difference as a pass. `msduck` still lacks `FOR XML PATH` execution and serialization, so the fixture is implementation evidence, not a runtime compatibility claim. Lowering, XML value rules and TDS output require separately claimed implementation work.

To reproduce on a machine with Docker and Node.js 24+, run `node scripts/capture-for-xml-path.mjs artifacts/compatibility/for-xml-path/recheck.json`. The script creates a private pinned SQL Server container and two fresh databases, compares with the committed fixture, writes the raw diagnostic output under ignored `artifacts/`, and removes its own container and databases. It needs no external SQL Server credentials.
