FROM rust:1.71-slim AS builder

RUN apt-get update && apt-get install -y libssl-dev pkg-config

COPY . /sources
WORKDIR /sources
RUN cargo build --release

FROM debian:bullseye-slim
COPY --from=builder /sources/target/release/rgit /rgit

COPY ./scripts/docker/entrypoint.sh .
RUN chmod +x entrypoint.sh

EXPOSE 8000
ENTRYPOINT ["/entrypoint.sh"]
