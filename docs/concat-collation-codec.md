# Captured SC and UTF8 wire collations

The deterministic TDS collation lookup recognizes two additional exact names,
case-insensitively. Both use LCID 1033, version 2 and sort ID 0:

| Name | Flags | Five wire bytes |
| --- | --- | --- |
| Latin1_General_100_CI_AS_SC | 13 | `09 04 d0 20 00` |
| Latin1_General_100_CI_AS_SC_UTF8 | 77 | `09 04 d0 24 00` |

Authority is the merged owner-run reference #807,
`reference/concat-ws-translate.json`, SHA-256
`74cf73753ea383e260de98488cf7dde8cc2bb521128a9585366cb5ed9d54351f`.
Each of four observations retains three SC descriptors (successful translation,
mapping-length failure and supplementary replacement) and one UTF8 descriptor.
The regression replays all 16 descriptors, including the complete encoded
single-column NVARCHAR COLMETADATA header, flags, length, collation and label.
The retained outputs are NVARCHAR(4000), advertised as 8000 bytes, with flags 33.
No ANSI UTF8 row encoding is demonstrated by those NVARCHAR descriptors.

The raw fixture stays unchanged. Its four isolated-surrogate strings cannot be
parsed directly by serde_json; the test represents those exact strings as
explicit UTF-16 unit carriers, checks the retained units and processes all 116
records in every observation. It never substitutes replacement characters or
discards a record. Unknown names, suffix lookalikes and invalid field bounds
remain rejected.

This change establishes descriptor lookup and byte encoding only. Function
binding, runtime result metadata, supplementary-character operations, ANSI UTF8
row codecs and linguistic comparison keys need their own implementation and
reference verification. The collation name does not supply comparison weights.
