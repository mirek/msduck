# Unicode casing

`msduck-core::case_mapping` maps UTF-16 code units using the SQL Server 2025 CU7
reference capture retained at owner-authored commit `3dfbe04`, in
`reference/unicode-case.json` (PR #34). Its explicit `Family` and `Direction`
inputs have no catalog, backend or environment dependency. `map_in_place`
preserves length and allocates nothing. Unknown collation names return `None`.

The six currently recognized non-SC collations use two tables: the legacy
`SQL_Latin1_General_CP1_CI_AS` table has 1,326 changed units; the five supported
`Latin1_General_100_*` collations share a table with 1,764 changed units. Unlisted
units map to themselves, including every surrogate unit. Case-sensitive and BIN2
collations still have LOWER/UPPER mappings. Supplementary letters remain unchanged
under these non-SC collations. Greek sigma does not gain contextual final-sigma
mapping, and sharp S and Kelvin sign differ from generic Unicode casing behavior.

Regenerate from the owner-approved capture:

```sh
git show 3dfbe04:reference/unicode-case.json > /tmp/msduck-unicode-case.json
node scripts/generate-case-maps.mjs /tmp/msduck-unicode-case.json
cargo fmt --all
node scripts/generate-case-maps.mjs /tmp/msduck-unicode-case.json --check
```

The generator validates full BMP coverage statistics, ordered bounded delta
rows, table-family equality, all 54 retained string probes and unchanged
surrogates before writing. The generated section records the input SHA-256. Core regressions also hash every
mapped BMP unit in both directions against independently calculated reference
checksums, covering 262,144 mapping results across the two table families.
Keep isolated surrogates intact: the capture must be read as UTF-16-preserving
JSON, as Node does. Rust UTF-8-only JSON strings cannot represent those probes.

The root `unicode_case` adapter registers four native functions for lower/upper
and legacy/version-100 maps. Each registration receives an immutable typed
family, so a nullable runtime selector cannot bypass validation. It accepts
UTF-16 carriers, preserves NULLs, bounds cell/chunk output and evaluates the
source once. Native regressions include malformed payloads and filtered vectors.

SQL LOWER/UPPER binding is still required. It must select the collation from
logical binding, choose the corresponding native function and preserve result
metadata. These case maps are not collation comparison weights and must not be
used as a substitute for the sequence-sensitive trim matching documented by
`reference/trim-collation.json` (PR #38).
