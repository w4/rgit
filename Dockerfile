FROM nixos/nix:2.24.9 AS builder

RUN nix-channel --update
RUN echo "experimental-features = nix-command flakes" >> /etc/nix/nix.conf

WORKDIR /app
COPY . .

RUN nix build .#

CMD ["/app/result/bin/rgit"]
