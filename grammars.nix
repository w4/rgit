# adapted from https://github.com/helix-editor/helix/blob/217818681ea9bbc7f995c87f8794c46eeb012b1c/grammars.nix
{ stdenv
, lib
, runCommand
, includeGrammarIf ? _: true
, grammarOverlays ? [ ]
, helix
, ...
}:
let
  languagesConfig = builtins.fromTOML (builtins.readFile "${helix}/languages.toml");
  isGitGrammar = grammar:
    builtins.hasAttr "source" grammar
    && builtins.hasAttr "git" grammar.source
    && builtins.hasAttr "rev" grammar.source;
  isGitHubGrammar = grammar: lib.hasPrefix "https://github.com" grammar.source.git;
  toGitHubFetcher = url:
    let
      match = builtins.match "https://github\.com/([^/]*)/([^/]*)/?" url;
    in
    {
      owner = builtins.elemAt match 0;
      repo = builtins.elemAt match 1;
    };
  # If `use-grammars.only` is set, use only those grammars.
  # If `use-grammars.except` is set, use all other grammars.
  # Otherwise use all grammars.
  useGrammar = grammar:
    if languagesConfig?use-grammars.only then
      builtins.elem grammar.name languagesConfig.use-grammars.only
    else if languagesConfig?use-grammars.except then
      !(builtins.elem grammar.name languagesConfig.use-grammars.except)
    else true;
  grammarsToUse = builtins.filter useGrammar languagesConfig.grammar;
  gitGrammars = builtins.filter isGitGrammar grammarsToUse;
  buildGrammar = grammar:
    let
      gh = toGitHubFetcher grammar.source.git;
      sourceGit = builtins.fetchTree {
        type = "git";
        url = grammar.source.git;
        inherit (grammar.source) rev;
        ref = grammar.source.ref or "HEAD";
        shallow = true;
      };
      sourceGitHub = builtins.fetchTree {
        type = "github";
        inherit (gh) owner;
        inherit (gh) repo;
        inherit (grammar.source) rev;
      };
      source =
        if isGitHubGrammar grammar
        then sourceGitHub
        else sourceGit;
    in
    stdenv.mkDerivation {
      # see https://github.com/NixOS/nixpkgs/blob/fbdd1a7c0bc29af5325e0d7dd70e804a972eb465/pkgs/development/tools/parsing/tree-sitter/grammar.nix

      pname = "tree-sitter-${grammar.name}";
      version = grammar.source.rev;

      src = source;
      sourceRoot =
        if builtins.hasAttr "subpath" grammar.source then
          "source/${grammar.source.subpath}"
        else
          "source";

      dontConfigure = true;

      FLAGS = [
        "-Isrc"
        "-g"
        "-O3"
        "-fPIC"
        "-fno-exceptions"
        "-Wl,-z,relro,-z,now"
      ];

      NAME = "libtree-sitter-${grammar.name}";

      buildPhase = ''
        runHook preBuild

        if [[ -e src/scanner.cc ]]; then
          $CXX -c src/scanner.cc -o scanner.o $FLAGS
        elif [[ -e src/scanner.c ]]; then
          $CC -c src/scanner.c -o scanner.o $FLAGS
        fi

        $CC -c src/parser.c -o parser.o $FLAGS
        $CXX -shared${lib.optionalString stdenv.isDarwin " -install_name $out/$NAME.so"} -o $NAME.so *.o

        runHook postBuild
      '';

      installPhase = ''
        runHook preInstall
        mkdir $out
        mv $NAME.so $out/
        runHook postInstall
      '';

      # Strip failed on darwin: strip: error: symbols referenced by indirect symbol table entries that can't be stripped
      fixupPhase = lib.optionalString stdenv.isLinux ''
        runHook preFixup
        $STRIP $out/$NAME.so
        runHook postFixup
      '';
    };
  grammarsToBuild = builtins.filter includeGrammarIf gitGrammars;
  builtGrammars = builtins.map
    (grammar: {
      inherit (grammar) name;
      value = buildGrammar grammar;
    })
    grammarsToBuild;
  extensibleGrammars =
    lib.makeExtensible (self: builtins.listToAttrs builtGrammars);
  overlayedGrammars = lib.pipe extensibleGrammars
    (builtins.map (overlay: grammar: grammar.extend overlay) grammarOverlays);
  grammarLinks = lib.mapAttrsToList
    (name: artifact: "ln -s ${artifact}/libtree-sitter-${name}.so $out/libtree-sitter-${name}.so")
    (lib.filterAttrs (n: v: lib.isDerivation v) overlayedGrammars);
in
runCommand "consolidated-rit-grammars" { } ''
  mkdir -p $out
  ${builtins.concatStringsSep "\n" grammarLinks}
  ln -s "${helix}/languages.toml" "$out/languages.toml"
  ln -s "${helix}/runtime/queries" "$out/queries"
''
