FROM nixos/nix:latest AS builder

WORKDIR /app
COPY . .

RUN nix --extra-experimental-features "nix-command flakes" --accept-flake-config build .#
RUN mkdir /tmp/nix-store-closure
RUN cp -R $(nix-store -qR result/) /tmp/nix-store-closure

FROM scratch

WORKDIR /app
COPY --from=builder /tmp/nix-store-closure /nix/store
COPY --from=builder /app/result /app

ENTRYPOINT ["/app/bin/rgit"]
