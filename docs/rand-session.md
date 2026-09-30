# RAND session reference

Two independent pinned SQL Server 2025 containers (`17.0.4065.4`) produced
identical 53-record captures, covering 73 request phases in each run. The retained
fixture `reference/rand-session.json` has SHA-256
`a1317e5b7faf9b8f37bcbd11a374907b7196d30d524c5ec5b32c973e9d29d455`.
It includes nine seeded streams: 64 subsequent values for each seed except 42,
which has 256. Every decoded FLOAT row retains both its value and exact big-endian
binary32/binary64 `floatBits`; signed zero would be encoded explicitly.

The capture retains full decoded descriptors (name, userType, type, width,
precision, scale, flags and collation), rows, error/warning fields, ordered token
names, typed RETURNVALUE payloads and each prepare/execute/unprepare phase.
Raw DONE status/command words are retained alongside decoded row counts. The
raw eight-byte count payload and ORDER payload are not captured independently;
counts become NULL when DONE_COUNT is absent. No event differences are discarded.

## Observations

- Reseeding with 42 restarts the same stream. Interleaved connections retain
  separate states; reseeding the second leaves the first's next value unchanged.
- Three separate RAND calls in one tested projection consume three successive
  values. A tested RAND projection over four rows consumes one value and repeats
  it across rows. These are distinct evaluation contexts.
- An ordinary RAND projection over an empty source does not consume a value.
  The earlier percentile reference's empty `RAND()*0+.5` fraction consumes one.
  Do not infer one universal evaluation frequency from either plan.
- An unselected CASE branch neither consumes an unseeded RAND value nor reseeds
  through RAND(1). The selected branch consumes a value. A selected division
  error leaves the unused RAND branch untouched. Transaction rollback does not
  restore the observed RNG state.
- RAND(NULL), including prepared typed NULL, returns NULL and leaves the current
  stream unchanged. The tested numeric seeds convert to INT: 1.9 truncates to 1,
  -1.9 produces the same stream as -1, and -1 matches 1. Numeric character 42 is
  accepted; malformed/decimal character text reports source-specific 245 errors.
  Out-of-range BIGINT seeds report 8115/state 2; the prepared FLOAT overflow
  reports 232/state 3 with different message/completion details.
- INT, FLOAT and NVARCHAR prepared seed bindings preserve their complete phases,
  NULL/error/valid recovery and final connection reuse. Successful RAND columns
  use the captured nullable FLOAT descriptor independently of the bound value.

## Empirical algorithm candidate

The following recurrence reproduces every captured stream bit for seeds
1, 2, 42, 1000, 1974 and -1 (65 values each, 257 for 42). Start with
`x = abs(seed)`, `y = 67890`, then for each result:

```text
x = (40014 * x) mod 2147483563
y = (40692 * y) mod 2147483399
z = x - y
if z < 1: z += 2147483562
result = binary64(z) * binary64(4.656613e-10)
```

Seeds 0, 2147483647 and -2147483648 instead match the same recurrence starting
with `x = 12345`, `y = 67890`. The decimal scale constant above matters: replacing
it with an exact reciprocal produces different bits. Integer products fit signed
64-bit arithmetic. This is an inference from independently repeated finite
streams, not a published SQL Server algorithm contract. The seed-validity
threshold, complete conversion surface, initial unseeded entropy, connection
reset/pooling and other optimizer/effect contexts remain unverified. A compatible
implementation must verify those boundaries rather than hardcode observed rows
or substitute Math.random/DuckDB random.

The locally retained first-party mssqlite source at owner commit
`7f71f2081602f8e3051998f5c11f058e65fe24ec` implements RAND using Math.random and
discards seed arguments. It does not provide this compatible session algorithm.
This reference task changes no runtime code and establishes no msduck RAND pass.

## Capture ownership and verification

`node scripts/capture-rand-session.mjs --check` verifies the immutable hash,
complete plan/stream lengths/phases, FLOAT bits, descriptors and both equal runs.
A fresh capture creates two new pinned containers and two connections in each
fresh database, compares every retained property, and writes only a new output.
`--write-fixture` refuses an existing fixture. Exclusive output creation refuses
existing files, symbolic/hard-link aliases and fixture reuse. Owned connection,
database and container cleanup uses the trusted shared helpers; loopback ports,
readiness and request times are bounded. The shared Linux runner lock protects
capture execution, and existing dependencies are reused without native builds.
