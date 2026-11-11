# syntax=docker/dockerfile:1

# Multi-arch: build with
#   docker buildx build --platform linux/amd64,linux/arm64 -t quota .
#
# This repo ships both a Python library and a Rust port. By default the
# image contains the Python library. To build the Rust binary instead,
# pass --build-arg BUILD_RUST=1 (requires docker buildx; the rust stage
# builds natively per-target so it works for both amd64 and arm64).

FROM python:3.12-slim AS build
WORKDIR /app
COPY . .
RUN python -m compileall -q quota.py tests

# Optional Rust build stage (enabled with --build-arg BUILD_RUST=1).
# rust:1.83-slim builds natively per target platform under buildx, so
# it works for both amd64 and arm64. The crate is a library (no bin
# target), so the stage emits the rlib artifact rather than an
# executable.
FROM rust:1.83-slim AS rust-build
ARG BUILD_RUST=0
WORKDIR /src
COPY Cargo.toml ./
COPY src ./src
# Build the Rust port when requested; otherwise emit a placeholder so
# the COPY below stays valid in default builds.
RUN mkdir -p /out \
    && if [ "$BUILD_RUST" = "1" ]; then \
         cargo build --release --lib; \
         cp target/release/*.rlib /out/ 2>/dev/null || cp target/release/*.so /out/ 2>/dev/null || true; \
       else \
         echo "rust build disabled (BUILD_RUST=0)" > /out/README-rust.txt; \
       fi

FROM python:3.12-slim AS runtime
LABEL org.opencontainers.image.title="Quota" \
      org.opencontainers.image.description="Per-tenant token budgets with scheduled reset"
WORKDIR /app
COPY --from=build /app /app
COPY --from=rust-build /out/ /opt/rust/
# Non-root user
RUN useradd --create-home --uid 10001 appuser
USER appuser
ENV PYTHONUNBUFFERED=1
# Smoke: import the library
CMD ["python", "-c", "import sys; sys.path.insert(0, '/app'); import quota; print('quota ok')"]
