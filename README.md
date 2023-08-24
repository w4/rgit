# rgit

[See it in action!](https://git.inept.dev/)

A gitweb/cgit-like interface for the modern age. Written in Rust using Axum, git2, Askama and Sled.

Sled is used to store all metadata about a repository including commits, branches, tags. Metadata
will be reindexed every 5 minutes outside of the request path. This leads to up to 97% faster load
times for large repositories.

Files, trees & diffs will be loaded using git2 directly upon request, a small in-memory cache is
included for rendered READMEs and diffs.

Includes a dark mode for late night committing.

Your `SCAN_PATH` should contain (optionally nested) [bare repositories][], and a `config` file
can be written with a `[gitweb.owner]` key to signify ownership.

[bare repositories]: https://git-scm.com/book/en/v2/Git-on-the-Server-Getting-Git-on-a-Server

Usage:

```
rgit 0.1.0

USAGE:
    rgit --db-store <DB_STORE> <BIND_ADDRESS> <SCAN_PATH>

ARGS:
    <BIND_ADDRESS>
            The socket address to bind to (eg. 0.0.0.0:3333)

    <SCAN_PATH>
            The path in which your bare Git repositories reside (will be scanned recursively)

OPTIONS:
    -d, --db-store <DB_STORE>
            Path to a directory in which the Sled database should be stored, will be created if it
            doesn't already exist

            The Sled database is very quick to generate, so this can be pointed to temporary storage

    -h, --help
            Print help information

    -V, --version
            Print version information
```

### Installation

#### From Source

rgit can be installed from source by cloning, building using [`cargo`][] and running the binary:

```bash
git clone https://github.com/w4/rgit
cd rgit
cargo build --release
./target/release/rgit [::]:3333 /path/to/my-repos -d /tmp/rgit-cache.db
```

[`cargo`]: https://www.rust-lang.org/

#### NixOS

Running rgit on NixOS is extremely simple, simply import the module into your `flake.nix` and use the
provided service:

```nix
{
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-23.05";

    rgit = {
      url = "github:w4/rgit";
      inputs.nixpkgs = "nixpkgs";
    };
  };

  outputs = { nixpkgs, ... }: {
    nixosConfigurations.mySystem = nixpkgs.lib.nixosSystem {
      modules = [
        rgit.nixosModules.default
        {
          services.rgit = {
            enable = true;
            bindAddress = "[::]:3333";
            dbStorePath = "/tmp/rgit.db";
            repositoryStorePath = "/path/to/my-repos";
          };
        }
        ...
      ];
    };
  };
}
```

#### Docker

Running rgit in Docker is also simple, just mount the directory containing your repositories to `/git`:

```bash
docker run --mount type=bind,source=/path/to/my-repos,target=/git \
  -it ghcr.io/w4/rgit:main
```
