# The AI review job's image, released with Forgust under the same version
# (ghcr.io/forgust-dev/forgust-reviewer). Built from the repository's root:
#   docker build -f crates/forgust-reviewer/Dockerfile -t forgust-reviewer:local .
# Alpine, git (to diff the runner's clone), the system's certificate authorities
# and a shell: GitLab Runner's Docker executor runs a job's script through one,
# so the image has no entrypoint of its own.
FROM rust:1.99-alpine@sha256:44819b27be4c97b0e4d7203bd2e02c4820f97700211b6a37fb4c246a2de59383 AS build
ARG BUILDKIT_SBOM_SCAN_STAGE=true
RUN apk add --no-cache musl-dev
WORKDIR /src
ENV CARGO_TERM_COLOR=never CARGO_PROFILE_RELEASE_STRIP=true CARGO_PROFILE_RELEASE_LTO=thin
# the workspace's manifests (cargo reads them all); only the reviewer is built
COPY Cargo.toml Cargo.lock ./
COPY crates crates
COPY bench bench
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked -p forgust-reviewer \
 && cp target/release/forgust-review /usr/local/bin/forgust-review

FROM alpine:3.22@sha256:49340b346798875b5592e5d420241f2f9cfba5f47e62ebb1446a1b34734e898f
RUN apk upgrade --no-cache \
 && apk add --no-cache git ca-certificates \
 && adduser -D -u 10001 reviewer
COPY --from=build /usr/local/bin/forgust-review /usr/local/bin/forgust-review
USER reviewer
CMD ["forgust-review"]
