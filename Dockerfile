# Drop-in replacement image for mcr.microsoft.com/mssql/server; see docs/docker.md.
FROM rust:1.95.0-trixie AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY crates crates
COPY vendor vendor
COPY src src
RUN cargo build --release --locked --bin msduck-container \
  && strip target/release/msduck-container

FROM debian:trixie-slim
RUN apt-get update \
  && apt-get install -y --no-install-recommends openssl \
  && rm -rf /var/lib/apt/lists/* \
  && useradd --uid 10001 --gid 0 --home-dir /var/opt/mssql --no-create-home --shell /usr/sbin/nologin mssql \
  && mkdir -p /var/opt/mssql/data /var/opt/mssql/secrets \
  && chown -R 10001:0 /var/opt/mssql \
  && chmod -R g=u /var/opt/mssql
COPY --from=build /src/target/release/msduck-container /usr/local/bin/
COPY docker/sqlservr /opt/mssql/bin/sqlservr
USER 10001
EXPOSE 1433
CMD ["/opt/mssql/bin/sqlservr"]
