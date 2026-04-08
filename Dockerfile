FROM rust:1-slim AS builder

WORKDIR /build

COPY Cargo.toml Cargo.lock ./
COPY src/ src/

RUN cargo build --release --locked


FROM debian:13-slim

RUN apt-get update &&\
    apt-get install -y --no-install-recommends ca-certificates tini

COPY --from=builder /build/target/release/safenet-monitor /usr/local/bin/safenet-monitor

ENTRYPOINT ["/usr/bin/tini", "--"]
CMD ["safenet-monitor"]
