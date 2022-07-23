# rgit

[See it in action!](https://git.inept.dev/)

A gitweb/cgit-like interface for the modern age. Written in Rust using Axum, git2, Askama and Sled.

Sled is used to store all metadata about a repository including commits, branches, tags. Metadata
will be reindexed every 5 minutes outside of the request path. This leads to up to 97% faster load
times for large repositories.

Files, trees & diffs will be loaded using git2 directly upon request, a small in-memory cache is
included for rendered READMEs and diffs.

Includes a dark mode for late night committing.

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
