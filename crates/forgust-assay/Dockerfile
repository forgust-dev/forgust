# assay's image, to map a repository in a CI job (ghcr.io/forgust-dev/forgust-assay),
# released with Forgust under the same version. Built from the repository's root:
#   docker build -f crates/forgust-assay/Dockerfile -t forgust-assay:local .
# Alpine, git (the clone's history tells who knows each block), the system's certificate
# authorities and a shell: GitLab Runner's Docker executor runs a job's script through one,
# so the image has no entrypoint of its own.
FROM rust:1.99-alpine@sha256:44819b27be4c97b0e4d7203bd2e02c4820f97700211b6a37fb4c246a2de59383 AS build
ARG BUILDKIT_SBOM_SCAN_STAGE=true
RUN apk add --no-cache musl-dev
WORKDIR /src
ENV CARGO_TERM_COLOR=never CARGO_PROFILE_RELEASE_STRIP=true CARGO_PROFILE_RELEASE_LTO=thin
# the workspace's manifests (cargo reads them all); only assay is built
COPY Cargo.toml Cargo.lock ./
COPY crates crates
COPY bench bench
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked -p forgust-assay \
 && cp target/release/assay /usr/local/bin/assay

FROM alpine:3.24@sha256:f59abe656b692d901e184be3445e084786602108599d2a51d229c0f5a48c71bf
# a runner's clone, or a folder mounted in, belongs to another user than the image's: git
# reads it all the same (else it refuses it as of "dubious ownership")
RUN apk upgrade --no-cache \
 && apk add --no-cache git ca-certificates \
 && git config --system --add safe.directory '*' \
 && adduser -D -u 10001 assay
COPY --from=build /usr/local/bin/assay /usr/local/bin/assay
USER assay
# `docker run -v "$PWD:/repo" …` maps the folder mounted there
WORKDIR /repo
CMD ["assay", "map", "--text"]
