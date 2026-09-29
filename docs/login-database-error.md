# LOGIN7 with a database that does not exist

`scripts/capture-login-database-error.mjs` logs in with Tedious 20.0.0 to the
pinned SQL Server 2025 image
`sha256:86cc6144ef39bb0fbed2329e1ad79b13ee82e7b2e4739213a0db0800e668a74a`
with `encrypt: true`. It records each LOGIN7 response message: the packet
header, every token fully decoded, the payload bytes, Tedious' events and
connect callback error, and whether the server closed the connection. It
records only incoming bytes after the TLS handshake. The outgoing LOGIN7, which
carries the password, is never recorded. The retained artifact is
`reference/login-database-error.json` (SHA-256
`6e4f9ad52b5f41b4bacf87bcf5a61060ef6b6a1328a957306edc16c5ce2d31bc`). Each
capture runs every case twice in one container, and a rerun in a fresh
container matched the retained file.

## SQL Server's response

A login naming an unknown database (`foo`) gets one type 4 packet (status EOM,
packet ID 1, window 0) containing exactly:

| Token | Fields |
|---|---|
| ERROR | 4060, state 1, class 11, `Cannot open database "foo" requested by the login. The login failed.`, procedure `''`, line 1 |
| ERROR | 18456, state 1, class 14, `Login failed for user 'sa'.`, procedure `''`, line 1 |
| DONE | status 0x0002 (DONE_ERROR), command 0, row count 0 |

No ENVCHANGE, INFO or LOGINACK precedes or follows these tokens. The server then
closes the connection. Tedious reports both `errorMessage` events, calls the
connect callback with `ELOGIN` and `Login failed for user 'sa'.` (the last
error), then emits `end`.

The capture also shows:

- The database name is quoted verbatim in 4060, without escaping (`fo"o]`) and
  in UTF-16 (`dbé🦆`).
- 18456 names the canonical login, not the LOGIN7 spelling: `SA` is reported as
  `sa`. A single quote in the login name is written as `\'`
  (`Login failed for user 'o\'brien'.`).
- The password is checked before the database. A wrong password together with
  an unknown database gets only ERROR 18456 (state 1, class 14) and DONE_ERROR.
- A known database matches case-insensitively, and so does a name with a
  trailing space. The ENVCHANGE and INFO 5701 echo the requested spelling
  (`MASTER`, `casedb`, `CaseDb `) with old value `master`, while `DB_NAME()`
  returns the stored name. An empty LOGIN7 database selects `master`.

## msduck

msduck opens the LOGIN7 database after authenticating. An unknown database now
gets the same three tokens. 18456 names the authenticated login with the same
quote escaping: the configured administrator name when `--admin-credentials`
is set, otherwise the supplied user name (or `sa` if it is empty). Before this change msduck sent
`Login failed.` as the 18456 message. Then it closes the connection, as SQL Server does.

Two values depend on the environment and are mapped as follows:

- **Server name.** SQL Server writes `@@SERVERNAME`, which is the container host
  name in the capture. msduck writes `msduck` in every diagnostic. The capture
  replaces the name in each ERROR/INFO token with `<serverName>` and subtracts
  its bytes from the token and packet lengths. It checks that each diagnostic
  carries the name exactly once and that no other token contains it.
- **SPID.** SQL Server writes the session ID in the packet header; msduck
  writes 0 in every packet header (`src/tds.rs`). The artifact records only
  whether it is non-zero.

`tests/login_database_error.test.mjs` starts msduck with TLS and compares
unknown `foo`, `fo"o]` and `dbé🦆`, and the `o'brien` login, with the retained
capture: header fields, decoded tokens, normalized payload bytes, Tedious events
and closure. The comparison is exact apart from the two mappings above. With an administrator file it also compares the `SA` login, which
is reported as `sa`. A `master` login still returns `SELECT 42` as `[[42]]`.

## Remaining differences

These differences were captured but are outside this change:

- A failed authentication sends 18456 with `Login failed.` rather than
  `Login failed for user '<name>'.` (`tests/tls.test.mjs` asserts the current text).
- A successful login response differs from SQL Server's. SQL Server sends
  ENVCHANGE 1 (requested spelling, old `master`), INFO 5701 state 2, ENVCHANGE 7,
  ENVCHANGE 2, INFO 5703, LOGINACK (`Microsoft SQL Server`, 17.0.15.225),
  ENVCHANGE 4 (`4096`, old `4096`), FEATUREEXTACK and DONE. msduck sends no 5701
  or 5703 INFO and no FEATUREEXTACK, and it orders the tokens ENVCHANGE 1, 2,
  4, 7, LOGINACK (`msduck`), DONE. Its ENVCHANGE 1 reports the stored name
  with an empty old value; docs/databases.md describes that behavior.
- The PRELOGIN version differs (`16.0.0.0` against `17.0.4065.0`).
