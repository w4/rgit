# rgit

## Introduction

[See it in action!](https://git.inept.dev/)

A gitweb/cgit-like interface for the modern age. Written in Rust using Axum, gitoxide, Askama and RocksDB.

Includes a dark mode for late night committing.

## Table of Contents

- [Features](#features)
- [Getting Started](#getting-started)
  - [Installation](#installation)
    - [Cargo (automatic)](#cargo-automatic)
    - [From Source (manually)](#from-source-manually)
  - [Usage](#usage)
  - [Configuration](#configuration)
    - [Repository Description](#repository-description)
    - [Repository Owner](#repository-owner)
  - [NixOS](#nixos)
  - [Docker](#docker)
    - [Docker Compose](#docker-compose)
- [Contributing](#contributing)
- [License](#license)
- [Troubleshooting](#troubleshooting)
  - [Cloning Repositories](#cloning-repositories)
    - [Repository not exported](#repository-not-exported)
  - [Launching the Application](#launching-the-application)
    - [...is not owned by the current user](#is-not-owned-by-the-current-user)
  - [Application Usage](#application-usage)
    - [Newly initialized repositories do not appear](#newly-initialized-repositories-do-not-appear)

## Features

- **Efficient Metadata Storage**  
  [RocksDB][] is used to store all metadata about a repository, including commits, branches, and tags. Metadata is reindexed, and the reindex interval is configurable (default: every 5 minutes), resulting in up to 97% faster load times for large repositories.

- **On-Demand Loading**  
  Files, trees, and diffs are loaded using [gitoxide][] directly upon request. A small in-memory cache is included for rendered READMEs and diffs, enhancing performance.

- **Dark Mode Support**  
  Enjoy a dark mode for late-night committing, providing a visually comfortable experience during extended coding sessions.

[RocksDB]: https://github.com/facebook/rocksdb
[gitoxide]: https://github.com/Byron/gitoxide

## Getting Started

Before you begin, ensure that you have the Rust toolchain and Cargo installed. If you haven't installed them yet, you can do so by following the instructions provided on the official Rust website:

- [Install Rust](https://www.rust-lang.org/learn/get-started)

Once you have Rust and Cargo installed, you can proceed with setting up and running the project.

**Note:** This software is designed to work exclusively with bare Git repositories. Make sure to set up bare repositories beforehand by following the [Git on the Server documentation][].

[Git on the Server documentation]: https://git-scm.com/book/en/v2/Git-on-the-Server-Getting-Git-on-a-Server

### Installation

#### Cargo (automatic)

```shell
cargo install --git https://github.com/w4/rgit
```

#### From Source (manually)

Clone the repository and build:

```shell
git clone https://github.com/w4/rgit.git
cd rgit
cargo build --release
```

The rgit binary will be found in the `target/release` directory.

### Usage

To get up and running quickly, run rgit with the following:

```shell
rgit [::]:3333 /path/to/my-bare-repos -d /tmp/rgit-cache.db
```

**Notes:**
- Repository indexing is recursive.
- The database is quick to generate, so this can be pointed to temporary storage.

### Configuration

#### Repository Description

To set a repository description, edit the file named `description` inside the bare git repository. Add your desired description text to this file.

#### Repository Owner

To assign an owner to a repository, edit the file named `config` inside the bare git repository and include the following content:

```ini
[gitweb]
    owner = "Al Gorithm"
```

Replace `Al Gorithm` with the desired owner's name.

### NixOS

Running rgit on NixOS is straightforward, simply import the module into your `flake.nix`
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
            repositoryStorePath = "/path/to/my-bare-repos";
          };
        }
        ...
      ];
    };
  };
}
```

### Docker

Running rgit in Docker is straightforward. Follow these steps, ensuring that your repository directory is correctly mounted:

```shell
docker run --mount type=bind,source=/path/to/my-bare-repos,target=/git \
  --user $UID:$GID \
  -it ghcr.io/w4/rgit:main
```

**Note**: Replace `$UID` and `$GID` with the UID and GID of the user that owns the directory containing your repositories. If these values are incorrect, errors will occur. Learn how to find the UID of a user [here](https://linuxhandbook.com/uid-linux/).

#### Docker Compose

An example `docker-compose.yml` is provided for those who prefer using Compose. To configure
the UID and GID, the user can be specified in `docker-compose.override.yml`.

An example override file has been has been provided with the repository. To use it, remove the
`.example` extension from `docker-compose.override.yml.example`, and adjust the UID and GID to
match the user that owns the directory containing your repositories.

To configure automatic refresh in Docker, an environment variable is also provided.

```yml
services:
  rgit:
    environment:
      - REFRESH_INTERVAL=5m
```

Afterwards, bring up the container with `docker-compose up` to make sure everything works.

## Contributing

Pull requests are welcome via GitHub or [`git-send-email`](https://git-scm.com/docs/git-send-email).

## License

rgit is licensed under the [WTFPL](LICENSE).

## Troubleshooting

### Cloning Repositories

#### Repository not exported

**Symptom:**
When attempting to clone repositories via HTTPS, you encounter the error message:

```
Git returned an error: Repository not exported
```

**Solution:**
Create a file named `git-daemon-export-ok` in the bare git repository. This file signals to the git daemon that the repository is [exportable][].

[exportable]: https://git-scm.com/docs/git-daemon

### Launching the Application

#### ...is not owned by the current user

**Symptom:**
When launching the application, you receive the error message:

```
repository path '/git/path/to/my/repository.git/' is not owned by the current user
```

**Solution:**
Ensure that the user launching `rgit` or the Docker container has the same permissions as the user that owns the repositories directory.

### Application Usage

#### Newly initialized repositories do not appear

**Symptom:**
When using the application, a newly initialized bare repository without commits does not appear in the list.

**Solution:**
Run the following command inside the repository to initialize it:

```shell
git pack-refs --all
```

Alternatively, push a commit with at least one file to the repository. This will also make the repository appear in the list.
