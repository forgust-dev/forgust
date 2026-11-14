# syntax=docker/dockerfile:1.7
# Forgust: one binary + the web UI. Runtime needs git >= 2.45 (trixie ships 2.47).
#
# Base images are pinned by digest (Dependabot moves the tag and the digest
# together). The image's SBOM (`docker buildx build --sbom=true`) lists the
# Debian packages of the final stage and the crates in the binary, which
# cargo-auditable records there; BUILDKIT_SBOM_SCAN_STAGE adds one more SBOM
# per build stage, for the npm packages and the toolchains.

FROM node:26.12-bookworm-slim@sha256:88e480776c6149ff1d098cd304f44bf18057fb5d284a3329b78162d29821e28c AS web
ARG BUILDKIT_SBOM_SCAN_STAGE=true
WORKDIR /src/web
COPY web/package.json web/package-lock.json ./
RUN npm ci --no-audit --no-fund
COPY web/ ./
RUN npm run build

FROM rust:1.99-trixie@sha256:8a4e90c7946b74626488b1446f712c0fe0ed1a596cbd3d871684bc9c5433ae07 AS build
ARG BUILDKIT_SBOM_SCAN_STAGE=true
# writes the dependency list into the binary: SBOM and vulnerability scanners read it
ARG CARGO_AUDITABLE=0.7.7
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    cargo install --locked cargo-auditable@${CARGO_AUDITABLE}
WORKDIR /src
ENV SQLX_OFFLINE=true CARGO_TERM_COLOR=never
COPY Cargo.toml Cargo.lock ./
COPY .sqlx .sqlx
COPY crates crates
COPY bench bench
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo auditable build --release --locked -p forgust-server \
 && cp target/release/forgust /usr/local/bin/forgust

FROM debian:trixie-slim@sha256:a72b4433de1cf7d6bae0c4cee74d0547f58601a643b3375f070c51600d63b6b2
# upgrade: security fixes Debian published after the pinned base image was built
RUN apt-get update \
 && apt-get upgrade -y \
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
