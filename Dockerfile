FROM rust:1.71-slim AS builder

RUN apt-get update && apt-get install -y libssl-dev pkg-config

COPY . /sources
WORKDIR /sources
RUN cargo build --release

FROM debian:bullseye-slim
COPY --from=builder /sources/target/release/rgit /rgit

EXPOSE 8000
ENTRYPOINT ["/rgit", "[::]:8000", "/git", "-d", "/tmp/rgit-cache.db"]
