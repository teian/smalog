# Multi-arch build for smalog (386, amd64, armv7 and arm64).
#
# Built per-target-arch under `docker buildx` + QEMU, so ring and the
# bundled SQLite compile natively for each platform — no cross toolchain
# to configure. Build all four at once with:
#
#   docker buildx build \
#     --platform linux/386,linux/amd64,linux/arm/v7,linux/arm64 \
#     -t ghcr.io/teian/smalog:latest \
#     -t fgehann/smalog:latest --push .

# --- Web UI: build the static dashboard to embed into the binary --------
# The UI output is architecture-independent. Building this stage on the
# BuildKit host also enables linux/386 images, for which Node has no image.
FROM --platform=$BUILDPLATFORM node:22-bookworm-slim AS ui
WORKDIR /ui
RUN corepack enable && corepack prepare pnpm@11.1.1 --activate
COPY src/ui/package.json src/ui/pnpm-lock.yaml src/ui/pnpm-workspace.yaml ./
RUN pnpm install --frozen-lockfile
COPY src/ui/ ./
RUN pnpm run build

# --- Rust service --------------------------------------------------------
FROM rust:1.93-bookworm AS builder

WORKDIR /build

# Cache dependency compilation: build stub crates against the real
# workspace manifests first, then the actual sources.
COPY Cargo.toml Cargo.lock ./
COPY src/crates/smalog/Cargo.toml src/crates/smalog/
COPY src/crates/smalog-connection/Cargo.toml src/crates/smalog-connection/
COPY src/crates/smalog-export/Cargo.toml src/crates/smalog-export/
COPY src/crates/smalog-observation/Cargo.toml src/crates/smalog-observation/
COPY src/crates/smalog-storage/Cargo.toml src/crates/smalog-storage/
COPY src/crates/smalog-tags/Cargo.toml src/crates/smalog-tags/
COPY src/crates/smalog-sbfspot-migrator/Cargo.toml src/crates/smalog-sbfspot-migrator/
COPY src/crates/smalog-schema-benchmark/Cargo.toml src/crates/smalog-schema-benchmark/
RUN mkdir -p src/crates/smalog/src \
    src/crates/smalog-connection/src \
    src/crates/smalog-export/src \
    src/crates/smalog-observation/src \
    src/crates/smalog-storage/src \
    src/crates/smalog-tags/src \
    src/crates/smalog-sbfspot-migrator/src \
    src/crates/smalog-schema-benchmark/src \
    && echo "fn main() {}" > src/crates/smalog/src/main.rs \
    && echo "" > src/crates/smalog/src/lib.rs \
    && echo "" > src/crates/smalog-connection/src/lib.rs \
    && echo "" > src/crates/smalog-export/src/lib.rs \
    && echo "" > src/crates/smalog-observation/src/lib.rs \
    && echo "" > src/crates/smalog-storage/src/lib.rs \
    && echo "" > src/crates/smalog-tags/src/lib.rs \
    && echo "" > src/crates/smalog-sbfspot-migrator/src/lib.rs \
    && echo "fn main() {}" > src/crates/smalog-schema-benchmark/src/main.rs \
    && cargo build --release --quiet -p smalog ; \
    rm -rf src/crates/smalog/src \
        src/crates/smalog-connection/src \
        src/crates/smalog-export/src \
        src/crates/smalog-observation/src \
        src/crates/smalog-storage/src \
        src/crates/smalog-tags/src \
        src/crates/smalog-sbfspot-migrator/src \
        src/crates/smalog-schema-benchmark/src \
    && rm -rf target/release/.fingerprint/smalog-* \
        target/release/deps/*smalog*

# Real sources + the built UI, then compile with it embedded.
#
# The stub artifacts above are deleted rather than left for cargo to
# invalidate: cargo decides freshness by mtime, and COPY gives these files the
# checkout's timestamps, which are older than the stub build inside the image.
# Cargo would call the workspace crates fresh and keep linking the empty stub
# libraries, so every import from them fails to resolve.
COPY src ./src
COPY --from=ui /ui/dist ./src/ui/dist
RUN cargo build --release --locked -p smalog --features ui

# ---------------------------------------------------------------------------

FROM debian:bookworm-slim AS runtime

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --no-create-home --uid 10001 smalog \
    && mkdir -p /var/lib/smalog /etc/smalog \
    && chown smalog:smalog /var/lib/smalog

COPY --from=builder /build/target/release/smalog /usr/local/bin/smalog

USER smalog
VOLUME ["/var/lib/smalog"]
EXPOSE 8080

# Probes the running service's /healthz endpoint (requires service.listen).
HEALTHCHECK --interval=60s --timeout=5s --start-period=20s --retries=3 \
    CMD ["/usr/local/bin/smalog", "--config", "/etc/smalog/config.toml", "healthcheck"]

ENTRYPOINT ["/usr/local/bin/smalog", "--config", "/etc/smalog/config.toml"]
CMD ["run"]
