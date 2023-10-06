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

Example given:
```text
$ cat config
[core]
	repositoryformatversion = 0
	filemode = true
	bare = true
[gitweb]
	owner = "Jordan Doyle"
$
```

[bare repositories]: https://git-scm.com/book/en/v2/Git-on-the-Server-Getting-Git-on-a-Server

Usage:

```
rgit 0.1.1

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
    --refresh-interval <REFRESH_INTERVAL>
            Configures the metadata refresh interval (eg. "never" or "60s")

            [default: 5m]

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

Running rgit on NixOS is extremely simple, simply import the module into your `flake.nix`
and use the provided service:

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

Running rgit in Docker is also simple, just mount the directory containing your repositories to
`/git`:

```bash
docker run --mount type=bind,source=/path/to/my-repos,target=/git \
  --user $UID:$GID \
  -it ghcr.io/w4/rgit:main
```

**Note**: Take care to replace `$UID` and `$GID` with the UID and GID of the user
that owns the directory containing your repositories or there will be errors! [See
here](https://linuxhandbook.com/uid-linux/) to learn how to find the UID of a user.

#### Docker Compose

An example `docker-compose.yml` is provided for those who prefer using Compose. To configure
the UID and GID, the user is specified in `docker-compose.override.yml`.

An example override file has been has been provided with the repository. To use it, remove the
`.example` extension from `docker-compose.override.yml.example`, and adjust the UID and GID to
match the user that owns the directory containing your repositories.

To configure automatic refresh in Docker, an environment variable is also provided.

```
services:
  rgit:
    environment:
	  - REFRESH_INTERVAL=5m
```

Afterwards, bring up the container with `docker-compose up` to make sure everything works.

### Notes

#### not owned by current user

When you get `message: "repository path '/git/orzklv-dots/' is not owned by current user"` in the
logging, it means exactly that. It is a _git design choice_, only owner writes to the git
repository. Match the `uid` what `rgit` started with the `uid` of the git repo on the filesystem.

##### Repository not exported

Message `Git returned an error: Repository not exported` is like _"repo not yet exposed"_.

Go to the `.git` directory and create file `git-daemon-export-ok`.

```text
$ cd /srv/rgit/rgit.git
$ ls
HEAD      config       hooks  objects      refs
branches  description  info   packed-refs
$ touch git-daemon-export-ok
$ ls
HEAD      config       git-daemon-export-ok  info     packed-refs
branches  description  hooks                 objects  refs
$
```
