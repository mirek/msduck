# SQL Server LIKE reference observations

`reference/like-patterns.json` retains 56 batch/RPC programs and two reusable
prepared programs. `scripts/capture-like-patterns.mjs` ran each program in four
fresh databases across two independent containers, then repeated the complete
four-database capture against the retained fixture. All eight observation sets
matched exactly. The containers used the pinned SQL Server 2025 image digest
`86cc6144ef39bb0fbed2329e1ad79b13ee82e7b2e4739213a0db0800e668a74a`.
The fixture SHA-256 is
`c1747bbce6b55882288c7236e0b1a830464cd24775d8907f83855c00ea6f2793`.
The image's fresh databases reported `SQL_Latin1_General_CP1_CI_AS` as their
default collation. The capture retains result descriptors, ordered rows, exact
diagnostics, DONE events and prepared RPC statuses; it does not rewrite an
existing fixture.

| Probe | Observed SQL Server behavior |
| --- | --- |
| Wildcards and classes | `%` accepts a suffix, `_` consumes one character, `[a-c]` matches `b`, `[^a-c]` matches `z`, and `[[]` matches `[`. Hyphens at either end of the captured class match a literal `-`. Unclosed `[`, empty `[]` and reversed `[z-a]` did not match the tested inputs. The tested `[]]` pattern did not match `]`; this is an observation of that exact pair, not a general closing-bracket rule. |
| ESCAPE | `!%`, `!_`, `![` and `!!` match their literal characters with `ESCAPE '!'`. A trailing escape in the pattern did not match. Empty or two-character escape values raised error 506, class 16, state 1 in direct batches, after an `IntN` descriptor and before a DONE without row count. `ESCAPE NULL` with an otherwise exact pattern matched; a prepared `!%` pattern with NULL escape returned false. |
| NULL and spaces | NULL source or pattern produced a NULL `CASE` result. ANSI `'a ' LIKE 'a'` and fixed `CHAR(3)` source matched, while Unicode `N'a ' LIKE N'a'` and fixed `NCHAR(3)` source did not. A trailing blank in the tested pattern did not match the shorter source, including mixed ANSI/Unicode cases. |
| Collation and units | `Latin1_General_100_BIN2` kept `B` distinct from `[a-c]`; `Latin1_General_100_CI_AS` matched it. The captured `É` versus `[e]` pair failed under `CI_AS` and matched under `CI_AI`. A single `_` did not match the supplementary `🦆` character under the tested BIN2 or non-SC CI collation. |
| Metadata and reuse | Even zero-row queries emitted typed descriptors. The constant true `CASE` probe used nonnullable `Int`, while false/unknown cases and the captured invalid-escape query used nullable `IntN`; this exact distinction is retained. A prepared invalid escape raised 506 state 2, emitted `doneInProc` then `doneProc`, returned RPC status -6, and the next execution of the same handle succeeded. |

This fixture is SQL Server ground truth for the listed inputs, not a pass result
for msduck. A runtime successor should compare the server's raw rows,
descriptors, diagnostics and completion sequence to this fixture at a pinned
revision before replacing backend `LIKE` handling. The capture does not settle
all collations, supplementary-character `_SC` behavior, every bracket syntax,
pattern length limit, binary/noncharacter operands or statement precedence.
