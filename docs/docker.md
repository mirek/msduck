# Docker image

`mirek/msduck` on Docker Hub is meant to replace
`mcr.microsoft.com/mssql/server` without configuration changes. SQL
compatibility is still incomplete; see README.md and ROADMAP.md for what works.

```sh
docker run -e ACCEPT_EULA=Y -e MSSQL_SA_PASSWORD='yourStrong(!)Password' \
  -p 1433:1433 -v msduck:/var/opt/mssql mirek/msduck
```

Connect as `sa` with the same client settings you would use for the SQL Server
image. For example, `Encrypt=true;TrustServerCertificate=true`, or `-C` for
sqlcmd.

## Tags

| Tag | Source |
| --- | --- |
| `latest`, `X.Y.Z`, `X.Y` | a pushed stable `vX.Y.Z` Git tag |
| `X.Y.Z-pre` | a prerelease tag such as `vX.Y.Z-rc.1` (never `latest`) |

Only release tags, `v` followed by a digit, publish; `verify-*` CI tags do not
start this workflow. Each tag is one manifest for `linux/amd64` and
`linux/arm64`, built on native GitHub runners rather than under emulation.
An `edge` tag was published from `main` on 2026-09-27 before publishing was
limited to releases. It is no longer updated.

## Environment

| Variable | Behavior |
| --- | --- |
| `MSSQL_SA_PASSWORD` | Required `sa` password. SQL Server's policy applies: at least 8 characters from three of uppercase, lowercase, digits and symbols. Weak or missing passwords stop the container. |
| `SA_PASSWORD` | Deprecated alias, used when `MSSQL_SA_PASSWORD` is unset. |
| `MSSQL_TCP_PORT` | Listening port (default 1433). |
| `MSSQL_IP_ADDRESS` | Listening address (default `0.0.0.0`). |
| `MSSQL_DATA_DIR` | Directory holding `msduck.duckdb` (default `/var/opt/mssql/data`). |
| `ACCEPT_EULA`, `MSSQL_PID`, `MSSQL_COLLATION`, `MSSQL_AGENT_ENABLED`, other `MSSQL_*` | Accepted and logged by name as having no effect. |
| `MSDUCK_TLS_CERT`, `MSDUCK_TLS_KEY` | PEM certificate chain and key to use instead of the generated certificate. |

The container runs as uid 10001 with data under `/var/opt/mssql`, the same
layout as the SQL Server image, so existing volume mounts and ownership keep
working. The start command is `/opt/mssql/bin/sqlservr`, so compose files that
set `command:` to that path still work. After listening, the container logs
SQL Server's `Recovery is complete` and `SQL Server is now ready for client
connections` lines, for readiness checks that wait on them.

Ctrl+C on an attached `docker run` (SIGINT) and `docker stop` (SIGTERM) stop
the server immediately with exit status 0, logging `msduck: received SIGINT;
shutting down` (or `SIGTERM`). The server runs as the container's PID 1 and
handles both signals itself, so `--init` is not needed. Open connections are
closed without a final checkpoint: committed transactions are in the DuckDB
write-ahead log, which the next start replays, and uncommitted ones are rolled
back. The smoke test stops the image with both signals and checks that a row
committed just before the signal survives the restart.

## Security model

The development CLI (`msduck`) stays loopback-only. The image instead runs
`msduck-container`, which listens on all interfaces. For that reason it always
requires TLS and `sa` password authentication:

- On first start, the image generates a self-signed certificate (like SQL
  Server's default) in `/var/opt/mssql/secrets`. It persists with the volume.
  If an interrupted start left only one file of the pair, the next start
  regenerates both. A pair set through `MSDUCK_TLS_CERT`/`MSDUCK_TLS_KEY` is
  never generated or replaced.
- On each start, the password is hashed into
  `/var/opt/mssql/secrets/msduck-admin.json` (mode 0600; see
  [authentication](authentication.md)). The plaintext is never written or passed
  as an argument, and a changed `MSSQL_SA_PASSWORD` takes effect on restart.
  SQL Server applies the variable only when it first initializes.
  The credential is written only after the database lock and port are held,
  so a second container that cannot start against a shared volume never
  changes the running server's password.
- Database, key and credential files are created with mode 0600 (umask 077).

## Differences from the SQL Server image

- Clients that advertise no encryption support are rejected, because
  password authentication requires TLS. SQL Server accepts them. Examples are
  tedious with `encrypt: false`, and clients that disable TLS entirely.
  Clients sending `Encrypt=false` (ENCRYPT_OFF) are upgraded to full TLS
  instead of login-only encryption, and work unchanged.
- `sa` is the only login. `CREATE LOGIN`, other principals and permissions are
  not implemented.
- All databases share one DuckDB file. SQL Server's `.mdf`/`.ldf` files, backups
  and `mssql.conf` are not read.
- `sqlcmd` is Microsoft's [go-sqlcmd](https://github.com/microsoft/go-sqlcmd)
  (v1.10.0, checksum-verified; notice in `/usr/share/doc/go-sqlcmd`), not
  the ODBC `mssql-tools` build. It is installed at both
  `/opt/mssql-tools18/bin/sqlcmd` and `/opt/mssql-tools/bin/sqlcmd`, so
  health checks such as
  `sqlcmd -S localhost -U sa -P "$MSSQL_SA_PASSWORD" -C -Q "SELECT 1" -b`
  work unchanged. The smoke test covers the health-check forms (`-S -U -P -Q`,
  with and without `-C` and `-b`, at both paths). Other options are go-sqlcmd's
  and are not separately verified. ODBC-only options and scripts that query
  catalog views msduck lacks, such as `sys.databases`, can still fail. `bcp` is
  not bundled.

## Publishing

`.github/workflows/docker.yml` builds each platform, runs
`scripts/docker-smoke.mjs` against the built image, pushes by digest, and then
merges the digests into one tagged manifest. It publishes only for `v*` tags.
Pushes to `main` do not run it, because each run builds DuckDB natively for
two platforms. The tag run smoke-tests every platform before anything is
pushed, so a broken image never reaches Docker Hub. Pull requests build and
smoke-test without pushing, and only when packaging, Rust
manifests/lockfile/vendor build inputs, the smoke test, the container
entrypoint, authentication/TLS/server session handling, Node package
dependencies, or this workflow change. Manual runs build and test without
pushing. Runs require the owner as actor, like CI.

The workflow needs two repository secrets: the Docker Hub account name and a
Docker Hub access token with read/write scope for `mirek/msduck`:

```sh
gh secret set DOCKERHUB_USERNAME --body mirek
gh secret set DOCKERHUB_TOKEN   # paste the token when prompted
```

To test an image locally: `docker build -t msduck:local .` and then
`node scripts/docker-smoke.mjs msduck:local`. The smoke test covers password
policy, ignored settings, TLS login, wrong-password rejection, and persistence
of data and certificate across restarts, and recovery from a partial
certificate pair. It also covers `SA_PASSWORD`, `MSSQL_TCP_PORT` and the
bundled `sqlcmd` health checks.
