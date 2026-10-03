# Native ANSI BulkLoad capacity

`msduck_core::ansi_conversion::capacity::Plan` separates SQL capacity from the
complete projections in its parent module. A plan validates CP1251/CP1252 source
identity and bounded/MAX source form, native CP1252/UTF8 or SQL UTF16 target, variable/fixed family and
bounded/MAX capacity before a nullable row is supplied. Source form comes from the admitted declaration,
not the row length or value. Native capacities count
bytes; Unicode capacities count two-byte units. Bounded declarations accept
1..=8000 native bytes or 1..=4000 Unicode units; fixed MAX is invalid.
Opaque tags and UTF8 source plans are unsupported. Native CP1251 capacity is
not admitted by this version; the parent's complete projection is separate.

The root must first admit the actual source declaration, wire shape and target
catalog profile. The measured profiles are Cyrillic_General_100_BIN2,
SQL_Latin1_General_CP1_CI_AS and Latin1_General_100_BIN2_UTF8. An encoding tag
does not enable arbitrary collations. This module cannot validate TYPE_INFO,
source row/declaration widths or source CHAR-to-MAX conversion. In particular,
the 4815/4816 controls remain root preflight evidence; no universal source
padding or source CHAR-to-MAX rule is invented here.

## Admission and conversion are separate

For bounded single-byte source declarations, overflow checks all bytes after
the target capacity. Same-CP1252 MAX conversion does the same. Both reject any
byte other than ASCII space in that remainder. Cross-encoding MAX source paths
have a different validation window: they inspect
`input[width..min(input.len(), 2*width)]`, rather than the whole remainder.
Non-space bytes beyond that window can be discarded without error. NUL and
nonbreaking space within the checked region are not ASCII spaces.

For example, CP1251 VARCHAR(MAX) `A space B` into CP1252 VARCHAR(1) succeeds
with native `A`, while the same CP1252 source fails with 2628. Bounded CP1251
VARCHAR/CHAR declarations also fail on those bytes; 8/64-byte source-form
controls retain that distinction across all four runs. At width2, `AB space C` fails, but
`AB two-spaces C` succeeds on the cross-encoding path. Width3/4 probes retain
three/four checked spaces followed by a non-space; replacing the last checked
space with NUL, NBSP or a non-space causes rejection. These are observed pinned
SQL Server BulkLoad behaviors, not a general SQL CAST/assignment trim rule.

After admission, conversion takes the longest prefix fitting the target. UTF8
cropping stops before a complete converted character that would exceed capacity;
it never writes a partial encoding or skips that character to take later input.
Fixed targets then pad with native ASCII spaces or UTF16 U+0020. Thus CP1251
`cff0` into UTF8 VARCHAR(2) stores `d09f`, and CP1252 copyright into UTF8
VARCHAR(1) stores empty bytes while CHAR(1) stores one space. MAX variable
targets retain the complete projection. NULL stays NULL; empty fixed input pads.

Source identity and input payload bounds precede reading/copying. Checked final
size preflight precedes fallible reservation. Unicode resource bounds count
bytes, so NCHAR(4) needs eight payload bytes. Conversion expansion is streamed
into the final allocation; no full expanded temporary is allocated to crop it.
The input remains immutable. Truncation is a typed row-level result, distinct
from resource exhaustion. Root adapters must create the captured diagnostics,
completion tokens and whole-load transaction behavior; this API does not do so.

## Retained evidence

Public tests replay 476 applicable original observations, 3,248 rows and 64
failed loads from all four runs of these unchanged references. Successful
expectations use native-byte fields, not client display; failure tests establish
that an input row is rejected and preserve the reference's empty whole-load
readback. Counter/token correctness remains a root integration requirement.

| Reference | SHA256 |
| --- | --- |
| Conversion895 | `f55527a2b0969d7104510d9b76a3303c82b28eac05d4ad11bc2acaf472caab27` |
| Capacity899 | `cd82a8853bcac9f3fee98c1fb6c6f17564443918868ece1f1d9f66b3abb0ec92` |
| Trailing spaces909 | `d9743a80462871dbed0f2830e984c71e621afb2d704d6a541271ae0f3c305740` |

Six additional private matrices each retain four fresh databases in two pinned
SQL Server17.0.4065.4 containers. Five have96 isolated NULL/challenge probes
per run; the declaration-limit matrix has88. All2272 observations keep actual source bytes, native/SQL projections, typed
descriptors, original post-login frames, errors/info/callbacks and original-session
counters. Collector source, raw artifacts and complete comparison sidecars remain
in the worker's private `.tmp/` and are independently reviewable. Their ancestor
capacity collector is unchanged at SHA256
`2cbb4172e902e2a4817812de570d09e375533da8a3efaf539cd08f9408975e87`;
each private wrapper's actual source SHA is recorded in its own artifact. Tests
preserve all568 baseline native-output/error vectors after checking agreement
on those fields across all four databases. They do not claim uniform counters.

| Private raw matrix | Bytes | SHA256 |
| --- | ---: | --- |
| NUL/NBSP/embedded spaces | 5745771 | `8ac615316de2828f796828ceeb9d117c102d51cf2c31043f8b7b7405cdfdb016` |
| Width2/3 cutoff | 6073779 | `195f2839c5324033a0b46807a793855868fdad009c356b7f7d2c881daa1f8ee8` |
| Sixteen space patterns | 5947526 | `4d7759d4c608b61b4a45fc9679fed4f2559bba23b43849163d01c136353b4337` |
| Width3/4 validation windows | 5990227 | `58ccf3185b78fd330bac375091630104d7d0c624fa384254226d0cb0ab795822` |
| Width8/64/4000/8000 windows | 24744835 | `47508f5112bb5f25a7398a9740829437f09f0eccabb389526b385196ccff847c` |
| Bounded VARCHAR/CHAR source8/64 | 6056587 | `30f1dd9c4ad4df2c9616e186169c4937ae4aa249f46d5d7b132b25ec98da1874` |

Raw differences stay unnormalized. In the sixteen-pattern matrix, the failed
CP1251-to-CP1252 VARCHAR(2) leading-spaces case has @@ERROR2628 in run0 and0
in runs1–3. Two failed UTF8 CHAR(3) cutoff cases consistently have @@ERROR0.
Their error tokens and empty readbacks remain failures. This API does not turn
those counters into a universal diagnostic claim. General UTF8/Unicode sources,
native CP1251 capacity, arbitrary collations and complete storage/BulkLoad/output
adoption remain separate work under the adapter plan.
