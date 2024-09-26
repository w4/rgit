% RGIT(1) version 0.1.2 | User Commands
%
% 11 January 2024

NAME
====

rgit - a gitweb interface written in rust

SYNOPSIS
========

| **rgit** \[*OPTIONS*] **\--db-store** *path* *bind_address* *scan_path*

DESCRIPTION
===========

A gitweb/cgit-like interface for the modern age. Written in Rust using Axum, gitoxide, Askama, and RocksDB.  
  
_bind_address_ 

:   Specifies the network address and port to serve the application on.
(Required)  

    Example:

    :   _0.0.0.0:3333_ (localhost, port 3333 on IPv4)

        _[::]:3333_ (localhost, port 3333 on IPv6)  

_scan_path_ 

:   Specifies the root directory where git repositories reside. Scans recursively.
(Required)  
  
    For information about bare git repositories, see the manual for **git-init**(1).  

    Example:

    :   _/srv/git_

        _$HOME/git_


OPTIONS
=======

**-d** _path_, **\--db-store** _path_

:   Path to a directory in which the RocksDB database should be stored, will be created if it doesn't already exist.  

    The RocksDB database is very quick to generate, so this can be pointed to temporary storage. (Required)

    Example:

    :   **\--db-store** _/tmp/rgit-cache.db_

**\--refresh-interval** _interval_

:   Configures the metadata refresh interval. This parameter accepts human-readable time formats.

    Default: _5m_

    Example:

    :   **\--refresh-interval** _60s_ (refresh every 60 seconds)

        **\--refresh-interval** _never_ (refresh only on server start)

    Documentation:

    :    https://docs.rs/humantime/latest/humantime/

EXAMPLES
========

```
$ rgit -d /tmp/rgit-cache.db [::]:3333 /srv/git
$ rgit --db-store /tmp/rgit-cache.db 0.0.0.0:3333 /srv/git
$ rgit -d /tmp/rgit-cache.db [::]:3333 /srv/git --refresh-interval 12h

```

BUGS
====

https://github.com/w4/rgit/issues

AUTHORS
=======

Jordan Doyle \<jordan@doyle.la>

REPOSITORY
==========

https://git.inept.dev/~doyle/rgit.git

https://github.com/w4/rgit

SEE ALSO
========

**git**(1),
**git-init**(1)
