# TDS 7.x TLS transport

The listener accepts an optional PEM certificate chain and matching private key:

```sh
cargo run -- --listen 127.0.0.1:1433 --tls-cert chain.pem --tls-key key.pem
```

Supplying these options requires encryption for every session. The current
implementation uses TLS 1.2 through rustls. Without these options, the existing
plaintext development listener remains available. Both configurations remain
loopback-only. Add `--admin-credentials` for
[bootstrap password authentication](authentication.md); encryption alone does
not verify SQL credentials. The certificate options must be supplied together, and
invalid certificates or mismatched keys fail before the listener starts.

The deterministic TDS crate validates PRELOGIN and returns a response plus one
of three transport outcomes: plaintext, TLS, or rejection. The root owns PEM
loading, cryptography and sockets. It sends the negotiation response before
closing incompatible connections. TLS handshake records use PRELOGIN packets
(type 0x12), including the final server flight; subsequent TDS messages travel
inside raw TLS records. The handshake has socket timeouts and a size bound.

For required encryption, OFF receives REQ, ON/REQ receives ON, and NOT_SUP
receives REQ followed by termination. This follows the current
[Microsoft PRELOGIN specification](https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-tds/60f56408-0188-4cd5-8b90-25c6f2423868),
which differs from the copied mssqlite policy that rejected OFF clients.
The copied skill's matrix and reserved encryption bit have been corrected with
an explicit review note. Unsupported client-certificate flags are rejected.

The client tests generate a temporary certificate using OpenSSL, then exercise
CA validation, hostname mismatch, untrusted certificates, negotiation, large
fragmented RPC values, results, transactions and recovery after a SQL error.
Tedious `encrypt: false` sends NOT_SUP, not protocol OFF; a raw PRELOGIN test
covers OFF separately. Run the tests with:

```sh
cargo build --workspace --all-targets
node --test tests/tls.test.mjs
```

Formatting, strict Clippy and all 336 workspace Rust tests pass locally. All three
focused TLS client tests pass, including a certificate large enough to require
multiple PRELOGIN handshake packets. The TLS-only snapshot also passed remote formatting, strict Clippy, 336 Rust
tests and all 348 client/harness tests. All 282 remote audit captures completed;
the comparison against macOS differs only in two reversed rows in an unordered
APPLY query. Raw differences are retained in
`artifacts/remote/linux.local/tls-baseline-comparison.json`. This verification
precedes bootstrap authentication and ORIGINAL_LOGIN. SQL-managed
logins and permissions, optional
login-only encryption, TLS 1.3 configuration, client-certificate authentication
and TDS 8.0 remain unfinished. This is not a production-ready listener.
