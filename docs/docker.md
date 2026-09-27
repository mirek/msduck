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
| `edge`, `sha-<short>` | each push to `main` |

Each tag is one manifest for `linux/amd64` and `linux/arm64`. Each platform is
built on a native GitHub runner, not under emulation.

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
- `sqlcmd` and `mssql-tools` are not bundled. Health checks that run
  `/opt/mssql-tools*/bin/sqlcmd` inside the container fail; probe from a client
  or use the readiness log line instead.

## Publishing

`.github/workflows/docker.yml` builds each platform, runs
`scripts/docker-smoke.mjs` against the built image, pushes by digest, and then
merges the digests into one tagged manifest. Pull requests that touch any file
the image is built from build and smoke-test it without pushing. Runs require the owner as actor, like CI.

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
certificate pair. It also covers `SA_PASSWORD` and `MSSQL_TCP_PORT`.
