# Native ANSI BulkLoad capacity

`msduck_core::ansi_conversion::capacity::Plan` separates SQL capacity from the
complete projections in its parent module. A plan validates CP1251/CP1252/UTF8 source
identity and bounded/MAX source form, native CP1251/CP1252/UTF8 or SQL UTF16 target, variable/fixed family and
bounded/MAX capacity before a nullable row is supplied. Source form comes from the admitted declaration,
not the row length or value. Native capacities count
bytes; Unicode capacities count two-byte units. Bounded declarations accept
1..=8000 native bytes or 1..=4000 Unicode units; fixed MAX is invalid.
Opaque tags are unsupported. The parent's complete
projection remains separate from capacity admission.

The root must first admit the actual source declaration, wire shape and target
catalog profile. The measured profiles are Cyrillic_General_100_BIN2,
SQL_Latin1_General_CP1_CI_AS and Latin1_General_100_BIN2_UTF8. An encoding tag
does not enable arbitrary collations. This module cannot validate TYPE_INFO,
source row/declaration widths or source CHAR-to-MAX conversion. In particular,
the 4815/4816 controls remain root preflight evidence; no universal source
padding or source CHAR-to-MAX rule is invented here.

## Admission and conversion are separate

For bounded single-byte source declarations, overflow checks all bytes after
the target capacity. Same-codepage CP1251 and CP1252 MAX conversion do the same. Both reject any
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
arbitrary collations and complete storage/BulkLoad/output
adoption remain separate work under the adapter plan.

## Native CP1251 target extension

Three additional private matrices retain four fresh databases in two pinned
SQL Server17.0.4065.4 containers each. They cover native CP1251 and CP1252
sources into CP1251 variable/fixed targets, with bounded VARCHAR64/8000,
bounded CHAR64 and MAX source declarations. The original collector, all
requests/frames/descriptors, errors/info, callbacks, native/SQL projections,
original-session counters and complete unnormalized comparison sidecars remain
in the worker's `.tmp/ansi921-*-four.json` artifacts. The frozen wrapper SHA256 is
`36ceba520c75c7935d62adb56ba6296ea85e060eea06cff3a14ec068c0b2a429`;
the reviewed ancestor capacity collector SHA above is unchanged.

| Private CP1251 target matrix | Bytes | SHA256 |
| --- | ---: | --- |
| Space/NUL/NBSP/embedded cutoffs, widths1/2 | 5935415 | `74e32f6446dfd5d1a45a7587ceaf8057ed815ff53891adae539343b013d37142` |
| Bounded VARCHAR/CHAR64 and MAX windows, widths3/4 | 6057662 | `ff3887d4296916174b3fbef42ad7df831a6cf496c76206372592d0cd359e02b5` |
| Width8/64/4000/8000 windows and complete MAX | 23431792 | `3ff838436fb1f4d9ab15d35445f7ba863ccd8bfd2842942dc5550a5165ebcd27` |

The CP1251-target observations establish the same-codepage/full-remainder
distinction directly: CP1251 MAX `A space B` to CP1251 VARCHAR(1) fails2628,
whereas CP1252 MAX stores `A`. Bounded CP1252 rejects that overflow. Within
the cross-encoding MAX window, NUL/NBSP/non-space rejects; a later non-space
beyond the window is discarded. Width8 through8000 retain these distinctions.
CP1251 identity preserves every original byte, including undefined0x98;
CP1252 conversion uses the unchanged observed parent CP1252-to-CP1251 table.
Complete MAX, NULL, empty and fixed padding are independently retained outputs.

The public CP1251 tests preserve each of286 cases' four native/error oracles:
1144 observations,2336 input-row applications and920 failed loads. Byte arrays
are losslessly run-length packed as `[byte,count]`; no display-derived expected
bytes or generated conversion table replace the oracle. All successful non-NULL
rows verify exact input/output budgets and one-under boundaries, with immutable
source bytes. Failed-load tests require a rejected row, without claiming that
the row API implements SQL Server's whole-load rollback or counters.
Each matrix's original counters agree within its four runs, while the complete
sidecars retain respectively1942,2187 and3161 raw differences elsewhere.
These finite collations and BulkLoad declarations do not establish arbitrary
collation, CAST/assignment, source-wire admission or runtime adoption.


## UTF8 BulkLoad source extension

Task #942 extends capacity to UTF8 source declarations, independently of the
strict complete-projection API. The original source EOF is checked before
capacity conversion: a mismatched nominal final span is a typed stream error
(7339/state1/class16), or the native UTF8 MAX-target BCP error
(4896/state7/class17). A final span with no recognized lead is the distinct
9833/state2/class16 boundary failure. These are core observations, not endpoint
ERROR tokens or a whole-load transaction implementation.

Native UTF8 keeps original bytes. Bounded native UTF8 admission checks the full
overflow for ASCII spaces, with nominal fitting and fixed padding. Cross-target
MAX conversion examines a fitted source window of three times the target width;
bounded sources use the complete source. The captured width1–4 controls retain
the last checked nonspace/NUL versus a later discarded nonspace. Cropping an
incomplete variation selector can also discard its preceding base. Original
source EOF admission remains stricter than this internal window-fitting step.

For Unicode targets, capacity counts decoded UTF16 units and preserves complete
supplementary pairs. Native CP1251/CP1252 admission checks source bytes in the
conversion window before applying codepage mapping; converted length alone is
insufficient. UTF8 ideographic space into VARCHAR(1) fails2628 although its
codepage projection occupies one byte. SQL-specific malformed repair consumes
forbidden lead/second-byte pairs together. No platform lossy string supplies an
oracle. The final payload is preflighted and allocated fallibly; no expanded
intermediate is required.

The new pure replay preserves all512 original task940 outcomes (248 successful
observations/496 row applications and264 failed loads), plus4032 earlier
admitted UTF8 observations/8064 applications/224 failed loads from boundary913,
bounded-target919, octet and multibyte930. Raw source/native bytes and SQL
UTF16 units remain distinct from display. Original failed messages can contain
lone surrogate escapes; their numeric diagnostic fields and empty readbacks are
read without repairing those messages. Native and pure fixtures are unchanged.

Private four-database/two-container matrices currently retain2816 additional
capacity/window/selector/EOF observations with2312 failed loads. Tests preserve
each run's original native/error oracle in lossless JSON literals. A separate
MAX-source capture contains every63488 valid BMP scalar in original UTF8 order
and preserves eight original CP1251/CP1252 native outputs. These supply507904
codepage cells; CP1252 agrees exactly with its existing captured map. CP1251's
663 non-question-mark cells are retained as a sorted constant table; supplementary
units retain the original two-question-mark controls. NULL and empty, fixed
padding, original-input and final-output limits and unchanged source are tested.

Full frozen-revision workspace/native/client/audit verification and review remain
pending while this task is in progress. Exact raw/source/reproduction hashes and
full differences will be recorded before merge. These measured profiles do not
admit arbitrary collations, general CAST/assignment, source CHAR, or public
engine/catalog/Value/BulkLoad/wire integration.
