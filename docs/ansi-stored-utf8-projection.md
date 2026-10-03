# Stored UTF8 projection investigation

Task #933 is in progress. The stored-value decoder is not implemented or exported. The preliminary scalar/repair walker is retained privately because new original SQL observations disprove its end-of-buffer behavior. The existing strict valid-UTF8 API and all CP1251/CP1252 projections remain unchanged.

## Source and operation

The collector creates each fresh database with `Latin1_General_100_BIN2_UTF8` as its default collation and records that property with SQL Server version/database/server identity. Converting varbinary to varchar in a CP1252-default database and then applying UTF8 collation transcodes CP1252 characters: that discarded setup is preserved separately and is not an oracle for stored UTF8 byte identity.

Each probe declares a VARCHAR(MAX) variable from the original binary input. It then issues **separate** native binary and Unicode-conversion SELECTs. This preserves native identity before a Unicode error instead of hiding both fields behind one failed result row. Original errors, descriptors, done tokens and complete packets are retained.

The frozen 1522 inputs include NULL, empty, all256 octets, all400 previous multibyte anchors without suffix, explicit incomplete/range-forbidden prefixes and every unique admitted variable-storage UTF8 payload in seven previous first-party fixtures. The largest input is12401bytes. Inputs and all reviewed helpers/fixtures are pinned before Docker; guards remain48MiB/500000nodes/depth64/2MiB-phase/1024frames/connect2seconds.

## Original findings

Four fresh databases in two pinned SQL Server17.0.4065.4 containers each produce153 projection failures (error9833,state2,class16) and1369 successful projections, including NULL. Native readback preserves every supplied input, including all failing projections. A failed Unicode result has its typed metadata and no fabricated row.

For these MAX variables, incomplete inputs C2, E0A0, E080, F09080 and F49080 project to an empty non-NULL Unicode byte sequence. Standalone80/BF, C0AF and F5808080 raise9833. Adding suffix42 to80 instead yields `FDFF4200`. NULL and empty retain their distinct meanings and VarBinary(MAX) descriptors.

The earlier repair hypothesis matched4912 admitted non-NULL BulkLoad-derived rows, but the direct variable probes add70 successful disagreements and153 failures per database. Those raw differences invalidate using that hypothesis as a general SQL stored-value decoder. Source-form, tail handling and projection errors must be modeled from evidence; the client decoder is not the oracle.

The retained separated-projection artifact is16032371bytes, SHA256 `04b4b49603116046ac475d31a244207aa61054a672aacaffa976a106800961e9`. Initial source SHA256 `b69e7d39a9f3dd5f56458c5c76da848b66422b4052dfe9b3e07489fb8941fd1a`; frozen source SHA256 `98643cbf943d857ef5e5eb98292b19908da324e68564331128b43833c711dca7`. All four original runs agree on complete result/request/response/framing pins. The final-source reproduction completed all6088queries and awaited cleanup, exited0, and matched the fixed pins. Its raw artifact is17149937bytes, SHA256 `483098e49ec0f2ff063a6a0125c4d65ad9b22b59c359b4b7c17395e4977348e9`; the complete10249-difference sidecar was independently reconstructed without altering raw data. Three Rust reference-spec tests pass in the existing deterministic cache. Those tests preserve the observations; they do not constitute implementation or workspace/client/audit completion.

## Remaining work

Determine the SQL tail/projection rules and implement the deterministic projector with explicit source identity/NULL/resource plans, exact output preflight and fallible allocation. Ground it against both these outcomes and unchanged admitted BulkLoad rows. Complete full exact-head Rust/client/audit verification, independent/Codex review, required CI, merge and registry/project completion. This checkpoint is not runtime support, admission or capacity completion. Engine/Value/TDS integration remains under the separately coordinated adapter plan.
