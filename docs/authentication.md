# Bootstrap SQL administrator

`--admin-credentials path.json` enables a configured bootstrap administrator.
It requires `--tls-cert` and `--tls-key`, so the username and password only arrive
through the encrypted LOGIN7 path. The CLI remains loopback-only while SQL-managed
principals, roles and permissions are unfinished. Without a credential file, the
explicitly documented development mode continues to accept SQL credentials.

Create the hash-only JSON from a password supplied on stdin:

```sh
umask 077
mkdir -p .msduck
node scripts/create-admin.mjs sa < /path/to/password-input > .msduck/admin.json
cargo run -- --tls-cert chain.pem --tls-key key.pem --admin-credentials .msduck/admin.json
```

The generator accepts no password argument and refuses interactive echoed input.
It consumes one optional trailing line ending as a delimiter. Passwords are UTF-8
input bounded to LOGIN7's 128 UTF-16 code units; Unicode and spaces are preserved.
The JSON contains `userName` and `passwordHash`, never a plaintext password.
`.msduck/` is excluded from Git and remote-build synchronization. Other credential
file locations are the operator's responsibility.

The versioned format is
`msduck$pbkdf2-sha256$v1$<base64url-salt>$<base64url-digest>`, using 32-byte random
salts, 32-byte digests and 600,000 PBKDF2-HMAC-SHA256 iterations. The server uses
ring's password verification operation; the independent Node generator supplies
the test hashes. This matches the PBKDF2 work factor in the
[OWASP password-storage guidance](https://cheatsheetseries.owasp.org/cheatsheets/Password_Storage_Cheat_Sheet.html).
It does not claim FIPS certification or compatibility with SQL Server's stored
password-hash format.

The server validates the configuration at startup and reloads it for each new
login. Replace the file atomically to rotate credentials. Missing, malformed or
oversized configurations deny new logins; the old hash is retained only to
perform comparable password work on reload failures, never to authorize access.
Unknown usernames also perform password verification. Existing authenticated
connections remain valid after rotation, matching the intended session lifecycle.

LOGIN7 decoding and nibble/XOR descrambling remain in the deterministic TDS crate.
The root owns file access, password hashing and the authentication decision before
creating a SQL session. The reversible scrambled LOGIN7 payload is cleared after decoding, and decoded
password storage is zeroized immediately after verification. Unknown names, incorrect passwords and reload failures all produce
ERROR 18456, state 1, severity 14, then DONE_ERROR and connection closure.
Configured administrator names use Unicode lowercase comparison; full SQL Server
collation-dependent principal resolution remains unfinished.

This reuses mssqlite's hash-only configuration, pre-session authentication,
rotation and uniform-failure design. Its scrypt hash format is not imported.
SQL CREATE/ALTER/DROP LOGIN, catalog-backed principals, authorization, other login
identity functions, password-change requests, integrated authentication and
federated authentication remain open compatibility work.

The focused client test verifies Unicode passwords, name matching, rejected
credentials, atomic rotation, failed reloads and continued use of existing
sessions. Final local formatting, strict Clippy, all 337 workspace Rust tests and
all four TLS/authentication client tests pass. Full remote verification of this
authentication snapshot is pending the preceding TLS snapshot’s run.


### Original login identity

The canonical authenticated administrator name now enters per-connection session
state. `ORIGINAL_LOGIN()` reads that captured identity, so changing the credential
file affects future connections without rewriting existing identities. The
insecure development mode captures the supplied username (or `sa` if empty).
Prepared execution and transaction boundaries preserve it. No impersonation
support is implied by this function.

The SQL crate validates its zero-argument signature before execution, including
unreachable branches, and carries the live-reference NVARCHAR(4000) declaration.
Eleven saved SQL Server probes match exactly in
`artifacts/compatibility/original-login-after.json`, covering direct, empty,
conditional, derived and invalid calls. The comparison was repeated after shared conditional inference changes. Additional literal-only conditional probes
are retained in `sql-server-conditional-literals.json`; constant-folded metadata
is a separate incomplete compiler contract and must not be inferred by blindly
maximizing literal widths. Invalid original-login calls are tested before writes,
and an SQL-looking username is carried as an AST string value, never raw SQL.


Final local verification of authentication plus original-login identity passed
formatting, strict Clippy, all 338 Rust tests, 20 focused SQL client tests and all four TLS/authentication
client tests.
The local audit and full remote Rust/client/audit run are in progress on the
current snapshot. TLS/authentication tests additionally check canonical names
across credential-name rotation, while existing connections retain their original
identity. Full SQL Server compatibility remains incomplete.


The authentication/ORIGINAL_LOGIN snapshot completed Linux verification with
formatting, strict Clippy, all 338 Rust tests, all 351 client tests and all 285
audit cases. Its raw audit capture exactly matches the corresponding macOS
capture; comparison is retained in
`artifacts/remote/linux.local/auth-original-baseline-comparison.json`.
This evidence predates the subsequent CHOOSE/MAX/parameter metadata changes and
does not establish full authentication or SQL Server compatibility.
