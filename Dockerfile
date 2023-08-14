FROM rust:1.71-slim AS builder

RUN apt-get update && apt-get install -y libssl-dev pkg-config

COPY . /sources
WORKDIR /sources
RUN cargo build --release
RUN chown nobody:nogroup /sources/target/release/rgit

FROM debian:bullseye-slim
COPY --from=builder /sources/target/release/rgit /rgit

USER nobody
EXPOSE 8000
ENTRYPOINT ["/rgit", "[::]:8000", "/git", "-d", "/tmp/rgit-cache.db"]
