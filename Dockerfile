FROM rust:1.71-slim AS builder

RUN rustup component add rustfmt
RUN apt-get update && apt-get install -y libssl-dev pkg-config clang

COPY . /sources
WORKDIR /sources
RUN cargo build --release

FROM debian:bullseye-slim

# Install git and cleanup package lists.
# This is required for git-http-backend to work.
RUN apt-get update && \
    apt-get install -y git && \
    rm -rf /var/lib/apt/lists/*

COPY --from=builder /sources/target/release/rgit /rgit

COPY ./scripts/docker/entrypoint.sh .
RUN chmod +x entrypoint.sh

EXPOSE 8000
ENTRYPOINT ["/entrypoint.sh"]
