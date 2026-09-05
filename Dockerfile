# syntax=docker/dockerfile:1

FROM rust:1.88-bookworm AS builder

WORKDIR /app

COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY migrations ./migrations
COPY src ./src

RUN cargo build --locked --release --bin agent-execution-platform

FROM debian:bookworm-slim AS runtime

RUN apt-get update \
    && apt-get install --no-install-recommends -y ca-certificates curl \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd --gid 10001 ardent \
    && useradd --uid 10001 --gid ardent --create-home --shell /usr/sbin/nologin ardent

COPY --from=builder /app/target/release/agent-execution-platform /usr/local/bin/agent-execution-platform

USER ardent

ENV RUST_LOG=info \
    RUST_BACKTRACE=0

EXPOSE 8080

CMD ["agent-execution-platform"]
