# Third-party source material

- `vendor/duckdb` and `vendor/libduckdb-sys` are local adaptations of DuckDB Rust
  packages 1.10505.0. Their MIT notices are retained in each directory's `LICENSE`.
  See their manifests and `docs/reference-review.md` for the adapter context.
- `reference/mssqlite` and `.agents/skills/{sys,t-sql,tds-protocol,tedious}` contain
  material from `mirek/mssqlite` at
  `7f71f2081602f8e3051998f5c11f058e65fe24ec`. The upstream MIT notice is retained
  in `reference/mssqlite/License.md` and `.agents/skills/License.md`.
- Cargo and npm dependencies retain their respective upstream licenses; the lock
  files record versions. The SQL Server reference image is not distributed here.

Copied implementation-status notes describe their upstream project. They are
not compatibility claims for msduck.

SMP byte fixtures and test scenarios in `crates/msduck-tds/src/smp.rs` are adapted
from `packages/tds/src/smp.test.ts` in mirek/mssqlite at the revision above. The
upstream MIT notice is retained in `reference/mssqlite/License.md`. The Rust
decoder uses a bounded, one-packet-at-a-time API and follows MC-SMP's generic
DATA framing rather than imposing the upstream codec's TDS-sized minimum.
