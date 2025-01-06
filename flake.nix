{
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/staging-next";

    crane.url = "github:ipetkov/crane";
    utils.url = "github:numtide/flake-utils";
    treefmt-nix.url = "github:numtide/treefmt-nix";

    advisory-db = {
      url = "github:rustsec/advisory-db";
      flake = false;
    };

    helix = {
      url = "github:JordanForks/helix";
      flake = false;
    };

    nix-github-actions = {
      url = "github:nix-community/nix-github-actions";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = { self, nixpkgs, utils, crane, advisory-db, treefmt-nix, helix, nix-github-actions }:
    {
      githubActions = nix-github-actions.lib.mkGithubMatrix {
        checks = { inherit (self.checks) x86_64-linux x86_64-darwin aarch64-darwin; };
      };
    }
    //
    utils.lib.eachDefaultSystem (system:
      let
        pkgs = import nixpkgs { inherit system; };
        craneLib = crane.mkLib pkgs;
        cargoOnlySrc = craneLib.cleanCargoSource ./.;
        src = pkgs.lib.fileset.toSource {
          root = ./.;
          fileset = pkgs.lib.fileset.unions [
            ./.cargo
            ./Cargo.toml
            ./Cargo.lock
            ./tree-sitter-grammar-repository
            ./src
            ./statics
            ./templates
            ./themes
            ./build.rs
          ];
        };
        rgit-grammar = pkgs.callPackage ./grammars.nix { inherit helix; };
        commonArgs = {
          inherit src;
          strictDeps = true;
          buildInputs = pkgs.lib.optionals pkgs.stdenv.isDarwin [ pkgs.libiconv ];
          nativeBuildInputs = with pkgs; [ cmake clang ];
          LIBCLANG_PATH = "${pkgs.clang.cc.lib}/lib";
          ROCKSDB_LIB_DIR = "${pkgs.rocksdb}/lib";
        };
        cargoArtifacts = craneLib.buildDepsOnly (commonArgs // { src = cargoOnlySrc; });
        buildArgs = commonArgs // {
          inherit cargoArtifacts;
          buildInputs = [ rgit-grammar ] ++ commonArgs.buildInputs;
          TREE_SITTER_GRAMMAR_LIB_DIR = rgit-grammar;
        };
        rgit = craneLib.buildPackage (buildArgs // { doCheck = false; });
        treefmt = treefmt-nix.lib.evalModule pkgs ./treefmt.nix;
      in
      {
        checks = {
          build = rgit;
          clippy = craneLib.cargoClippy buildArgs;
          doc = craneLib.cargoDoc buildArgs;
          audit = craneLib.cargoAudit { inherit advisory-db; src = cargoOnlySrc; };
          test = craneLib.cargoNextest (buildArgs // {
            partitions = 1;
            partitionType = "count";
          });
          formatting = treefmt.config.build.check self;
        };

        formatter = treefmt.config.build.wrapper;

        packages.default = rgit;
        apps.default = utils.lib.mkApp { drv = rgit; };

        devShells.default = craneLib.devShell {
          checks = self.checks.${system};
        };

        nixosModules.default = { config, lib, pkgs, ... }:
          with lib;
          let
            cfg = config.services.rgit;
          in
          {
            options.services.rgit = {
              enable = mkEnableOption "rgit";
              bindAddress = mkOption {
                default = "[::]:8333";
                description = "Address and port to listen on";
                type = types.str;
              };
              dbStorePath = mkOption {
                default = "/tmp/rgit.db";
                description = "Path to store the temporary cache";
                type = types.path;
              };
              repositoryStorePath = mkOption {
                default = "/git";
                description = "Path to repositories";
                type = types.path;
              };
              requestTimeout = mkOption {
                default = "10s";
                description = "Timeout for incoming HTTP requests";
                type = types.str;
              };
              package = mkOption {
                default = rgit;
                description = "rgit package to use";
                type = types.package;
              };
            };

            config = mkIf cfg.enable {
              users.groups.rgit = { };
              users.users.rgit = {
                description = "RGit service user";
                group = "rgit";
                isSystemUser = true;
                home = "/git";
              };

              systemd.services.rgit = {
                enable = true;
                wantedBy = [ "multi-user.target" ];
                after = [ "network-online.target" ];
                path = [ pkgs.git ];
                serviceConfig = {
                  Type = "exec";
                  ExecStart = "${cfg.package}/bin/rgit --request-timeout ${cfg.requestTimeout} --db-store ${cfg.dbStorePath} ${cfg.bindAddress} ${cfg.repositoryStorePath}";
                  Restart = "on-failure";

                  User = "rgit";
                  Group = "rgit";

                  CapabilityBoundingSet = "";
                  NoNewPrivileges = true;
                  PrivateDevices = true;
                  PrivateTmp = true;
                  PrivateUsers = true;
                  PrivateMounts = true;
                  ProtectHome = true;
                  ProtectClock = true;
                  ProtectProc = "noaccess";
                  ProcSubset = "pid";
                  ProtectKernelLogs = true;
                  ProtectKernelModules = true;
                  ProtectKernelTunables = true;
                  ProtectControlGroups = true;
                  ProtectHostname = true;
                  RestrictSUIDSGID = true;
                  RestrictRealtime = true;
                  RestrictNamespaces = true;
                  LockPersonality = true;
                  RemoveIPC = true;
                  RestrictAddressFamilies = [ "AF_INET" "AF_INET6" ];
                  SystemCallFilter = [ "@system-service" "~@privileged" ];
                };
              };
            };
          };
      });

  nixConfig = {
    extra-substituters = [ "https://rgit.cachix.org" ];
    extra-trusted-public-keys = [ "rgit.cachix.org-1:3Wva/GHhrlhbYx+ObbEYQSYq1Yzk8x9OAvEvcYazgL0=" ];
  };
}
