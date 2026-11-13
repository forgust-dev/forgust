# syntax=docker/dockerfile:1.7
# Forgust: one binary + the web UI. Runtime needs git >= 2.45 (trixie ships 2.47).

FROM node:26.12-bookworm-slim AS web
WORKDIR /src/web
COPY web/package.json web/package-lock.json ./
RUN npm ci --no-audit --no-fund
COPY web/ ./
RUN npm run build

FROM rust:1.99-trixie AS build
WORKDIR /src
ENV SQLX_OFFLINE=true CARGO_TERM_COLOR=never
COPY Cargo.toml Cargo.lock ./
COPY .sqlx .sqlx
COPY crates crates
COPY bench bench
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked -p forgust-server \
 && cp target/release/forgust /usr/local/bin/forgust

FROM debian:trixie-slim
RUN apt-get update \
 && apt-get install -y --no-install-recommends git ca-certificates postgresql-client tini \
 && rm -rf /var/lib/apt/lists/* \
 && useradd --system --home /var/lib/forgust --create-home --uid 10001 forgust
COPY --from=build /usr/local/bin/forgust /usr/local/bin/forgust
COPY --from=web /src/web/dist /usr/share/forgust/web
ENV FORGUST_SERVER__DATA_DIR=/var/lib/forgust \
    FORGUST_SERVER__STATIC_DIR=/usr/share/forgust/web \
    FORGUST_SERVER__LISTEN=0.0.0.0:8080 \
    FORGUST_SERVER__SSH_LISTEN=0.0.0.0:2222 \
    FORGUST_TELEMETRY__LOG_FORMAT=json
USER forgust
VOLUME /var/lib/forgust
EXPOSE 8080 2222
HEALTHCHECK --interval=30s --timeout=3s CMD ["/usr/local/bin/forgust", "health"]
ENTRYPOINT ["/usr/bin/tini", "--", "/usr/local/bin/forgust"]
CMD ["serve"]
