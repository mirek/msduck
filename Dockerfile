# Drop-in replacement image for mcr.microsoft.com/mssql/server; see docs/docker.md.
FROM rust:1.95.0-trixie AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY crates crates
COPY vendor vendor
COPY src src
RUN cargo build --release --locked --bin msduck-container \
  && strip target/release/msduck-container

# go-sqlcmd provides sqlcmd at the SQL Server image's mssql-tools paths.
FROM rust:1.95.0-trixie AS sqlcmd
ARG TARGETARCH
ARG SQLCMD_VERSION=v1.10.0
ARG SQLCMD_SHA256_AMD64=92516d98c63d99b0994de5b61350c91f6915f9b76f139a59039fbcb225c2e987
ARG SQLCMD_SHA256_ARM64=9faaa981f9c374f319ac796dedb4678499b8596c87d5b6c512e9b0e7a3b74f8e
WORKDIR /sqlcmd
RUN arch="${TARGETARCH:-$(dpkg --print-architecture)}" \
  && case "$arch" in \
       amd64) sha="$SQLCMD_SHA256_AMD64" ;; \
       arm64) sha="$SQLCMD_SHA256_ARM64" ;; \
       *) echo "unsupported architecture $arch" >&2; exit 1 ;; \
     esac \
  && curl -fsSLo sqlcmd.tar.bz2 "https://github.com/microsoft/go-sqlcmd/releases/download/$SQLCMD_VERSION/sqlcmd-linux-$arch.tar.bz2" \
  && echo "$sha  sqlcmd.tar.bz2" | sha256sum -c - \
  && tar xjf sqlcmd.tar.bz2 sqlcmd NOTICE.md \
  && chmod 0755 sqlcmd && chmod 0644 NOTICE.md

FROM debian:trixie-slim
RUN apt-get update \
  && apt-get install -y --no-install-recommends openssl \
  && rm -rf /var/lib/apt/lists/* \
  && useradd --uid 10001 --gid 0 --home-dir /var/opt/mssql --no-create-home --shell /usr/sbin/nologin mssql \
  && mkdir -p /var/opt/mssql/data /var/opt/mssql/secrets /opt/mssql-tools/bin \
  && chown -R 10001:0 /var/opt/mssql \
  && chmod -R g=u /var/opt/mssql
COPY --from=build /src/target/release/msduck-container /usr/local/bin/
COPY --from=sqlcmd /sqlcmd/sqlcmd /opt/mssql-tools18/bin/sqlcmd
COPY --from=sqlcmd /sqlcmd/NOTICE.md /usr/share/doc/go-sqlcmd/NOTICE.md
RUN ln -s /opt/mssql-tools18/bin/sqlcmd /opt/mssql-tools/bin/sqlcmd
COPY docker/sqlservr /opt/mssql/bin/sqlservr
USER 10001
EXPOSE 1433
CMD ["/opt/mssql/bin/sqlservr"]
