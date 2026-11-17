# syntax=docker/dockerfile:1
# rackend container image (SPEC §18.2). Multi-arch: linux/amd64, linux/arm64.

# --- UI ---
FROM oven/bun:1.4.3 AS ui
WORKDIR /src/admin-ui
COPY admin-ui/package.json admin-ui/bun.lock ./
RUN bun install --frozen-lockfile
COPY admin-ui/ ./
RUN bun --bun run build

# --- Rust ---
FROM rust:1.99.1-alpine AS build
RUN apk add --no-cache musl-dev cmake make perl clang git
WORKDIR /src
COPY . .
COPY --from=ui /src/admin-ui/build ./admin-ui/build
ARG RACKEND_GIT_COMMIT=unknown
ARG SOURCE_DATE_EPOCH
ENV RACKEND_GIT_COMMIT=${RACKEND_GIT_COMMIT}
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release -p rackend --locked \
 && cp target/release/rackend /rackend
# `scratch` has no /data; create it owned by the fixed runtime UID/GID so
# named volumes inherit the right ownership (SPEC §7.6, §18.2).
RUN mkdir -p /data-skel && chown 10001:10001 /data-skel

# --- Runtime ---
FROM scratch
LABEL org.opencontainers.image.title="rackend" \
      org.opencontainers.image.description="Self-hosted, single-binary backend: database, REST API, auth, files, realtime." \
      org.opencontainers.image.source="https://github.com/axanet/rackend" \
      org.opencontainers.image.vendor="axanet GmbH" \
      org.opencontainers.image.licenses="Apache-2.0"
COPY --from=build /etc/ssl/certs/ca-certificates.crt /etc/ssl/certs/
COPY LICENSE NOTICE /licenses/
COPY --from=build /rackend /rackend
COPY --from=build --chown=10001:10001 /data-skel /data
USER 10001:10001
ENV RACKEND_DATA_DIR=/data
VOLUME ["/data"]
EXPOSE 8080
HEALTHCHECK --interval=15s --timeout=3s --start-period=30s CMD ["/rackend", "healthcheck"]
ENTRYPOINT ["/rackend"]
CMD ["serve"]
