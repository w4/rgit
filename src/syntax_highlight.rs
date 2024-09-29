use std::{
    cell::RefCell,
    collections::HashMap,
    fmt::Write as FmtWrite,
    io::{ErrorKind, Write as IoWrite},
    sync::LazyLock,
};

use comrak::adapters::SyntaxHighlighterAdapter;
use tracing::debug;
use tree_sitter_highlight::{HighlightConfiguration, HighlightEvent, Highlighter};

thread_local! {
    static HIGHLIGHTER: RefCell<Highlighter> = RefCell::new(Highlighter::new());
}

macro_rules! count {
    () => (0);
    ($e:expr) => (1);
    ($e:expr, $($rest:expr),*) => (1 + count!($($rest),*));
}

macro_rules! define_classes {
    ($($name:literal => $class:literal),*,) => {
        static HIGHLIGHT_NAMES: [&str; count!($($name),*)] = [
            $($name),*
        ];

        static HIGHLIGHT_CLASSES: [&str; count!($($name),*)] = [
            $($class),*
        ];
    };
}

define_classes! {
    "keyword.directive" => "keyword directive",
    "markup.strikethrough" => "markup strikethrough",
    "markup.link" => "markup link",
    "keyword.control.conditional" => "keyword control conditional",
    "markup.bold" => "markup bold",
    "diff.plus" => "diff plus",
    "markup.heading.2" => "markup heading 2",
    "markup" => "markup",
    "diff.delta" => "diff delta",
    "variable.other.member" => "variable other member",
    "namespace" => "namespace",
    "comment.line" => "comment line",
    "function" => "function",
    "keyword.operator" => "keyword operator",
    "punctuation.bracket" => "punctuation bracket",
    "markup.list" => "markup list",
    "type.builtin" => "type builtin",
    "keyword.storage.modifier" => "keyword storage modifier",
    "constant" => "constant",
    "markup.italic" => "markup italic",
    "variable" => "variable",
    "keyword" => "keyword",
    "punctuation.special" => "punctuation special",
    "string.special.path" => "string special path",
    "keyword.storage.type" => "keyword storage type",
    "markup.heading.5" => "markup heading 5",
    "markup.heading.6" => "markup heading 6",
    "markup.link.label" => "markup link label",
    "markup.list.numbered" => "markup list numbered",
    "diff.delta.moved" => "diff delta moved",
    "constant.numeric" => "constant numeric",
    "markup.heading" => "markup heading",
    "markup.link.text" => "markup link text",
    "keyword.function" => "keyword function",
    "string.special.url" => "string special url",
    "keyword.control.return" => "keyword control return",
    "keyword.control.repeat" => "keyword control repeat",
    "constant.builtin" => "constant builtin",
    "type.enum.variant" => "type enum variant",
    "markup.raw.block" => "markup raw block",
    "markup.heading.3" => "markup heading 3",
    "escape" => "escape",
    "comment.block" => "comment block",
    "constant.numeric.integer" => "constant numeric integer",
    "punctuation.delimiter" => "punctuation delimiter",
    "constructor" => "constructor",
    "type" => "type",
    "string.regexp" => "string regexp",
    "variable.parameter" => "variable parameter",
    "markup.quote" => "markup quote",
    "string.special" => "string special",
    "constant.numeric.float" => "constant numeric float",
    "constant.character.escape" => "constant character escape",
    "tag" => "tag",
    "keyword.storage" => "keyword storage",
    "string" => "string",
    "function.macro" => "function macro",
    "markup.list.unnumbered" => "markup list unnumbered",
    "diff.minus" => "diff minus",
    "punctuation" => "punctuation",
    "markup.link.url" => "markup link url",
    "function.method" => "function method",
    "markup.raw" => "markup raw",
    "function.special" => "function special",
    "attribute" => "attribute",
    "operator" => "operator",
    "special" => "special",
    "function.builtin" => "function builtin",
    "diff" => "diff",
    "markup.heading.4" => "markup heading 4",
    "keyword.control" => "keyword control",
    "markup.list.unchecked" => "markup list unchecked",
    "keyword.control.exception" => "keyword control exception",
    "constant.builtin.boolean" => "constant builtin boolean",
    "markup.heading.1" => "markup heading 1",
    "markup.heading.marker" => "markup heading marker",
    "constant.character" => "constant character",
    "markup.raw.inline" => "markup raw inline",
    "variable.builtin" => "variable builtin",
    "variable.other" => "variable other",
    "tag.builtin" => "tag builtin",
    "type.enum" => "type enum",
    "comment.block.documentation" => "comment block documentation",
    "comment" => "comment",
    "string.special.symbol" => "string special symbol",
    "label" => "label",
    "keyword.control.import" => "keyword control import",
    "markup.list.checked" => "markup list checked",
}

macro_rules! build_highlighter_configs {
    ($($i:literal => $($extension:literal)|* => $($token:literal)|* => $config:expr),*,) => {
        static BUILD_HIGHLIGHTER_CONFIGS: LazyLock<[HighlightConfiguration; count!($($config),*)]> = LazyLock::new(|| [
            $({
                let mut config = $config.unwrap();
                config.configure(&HIGHLIGHT_NAMES);
                config
            }),*
        ]);

        pub fn fetch_highlighter_config(extension: &str) -> Option<&'static HighlightConfiguration> {
            match extension {
                $($($extension)|* => Some(&BUILD_HIGHLIGHTER_CONFIGS[$i])),*,
                _ => None,
            }
        }

        pub fn fetch_highlighter_config_by_token(extension: &str) -> Option<&'static HighlightConfiguration> {
            match extension {
                $($($token)|* => Some(&BUILD_HIGHLIGHTER_CONFIGS[$i])),*,
                _ => None,
            }
        }
    };
}

build_highlighter_configs! {
    // #   extensions             name/aliases
    0  => "java"              => "java"                    => HighlightConfiguration::new(tree_sitter_java::LANGUAGE.into(), "java", tree_sitter_java::HIGHLIGHTS_QUERY, "", ""),
    1  => "html"              => "html"                    => HighlightConfiguration::new(tree_sitter_html::LANGUAGE.into(), "html", include_str!("../grammar/html/highlights.scm"), include_str!("../grammar/html/injections.scm"), ""),
    2  => "md"                => "markdown"                => HighlightConfiguration::new(tree_sitter_md::LANGUAGE.into(), "markdown", tree_sitter_md::HIGHLIGHT_QUERY_BLOCK, tree_sitter_md::INJECTION_QUERY_BLOCK, ""),
    3  => "rs"                => "rust"                    => HighlightConfiguration::new(tree_sitter_rust::LANGUAGE.into(), "rust", tree_sitter_rust::HIGHLIGHTS_QUERY, tree_sitter_rust::INJECTIONS_QUERY, ""),
    4  => "toml"              => "toml"                    => HighlightConfiguration::new(tree_sitter_toml_ng::language(), "toml", tree_sitter_toml_ng::HIGHLIGHTS_QUERY, "", ""),
    5  => "yaml" | "yml"      => "yaml" | "yml"            => HighlightConfiguration::new(tree_sitter_yaml::language(), "yaml", tree_sitter_yaml::HIGHLIGHTS_QUERY, "", ""),
    6  => "hs"                => "haskell"                 => HighlightConfiguration::new(tree_sitter_haskell::LANGUAGE.into(), "haskell", tree_sitter_haskell::HIGHLIGHTS_QUERY, tree_sitter_haskell::INJECTIONS_QUERY, tree_sitter_haskell::LOCALS_QUERY),
    7  => "f" | "f90" | "for" => "fortran"                 => HighlightConfiguration::new(tree_sitter_fortran::LANGUAGE.into(), "fortran", include_str!("../grammar/fortran/highlights.scm"), "", ""),
    8  => "svelte"            => "svelte"                  => HighlightConfiguration::new(tree_sitter_svelte_ng::LANGUAGE.into(), "svelte", tree_sitter_svelte_ng::HIGHLIGHTS_QUERY, tree_sitter_svelte_ng::INJECTIONS_QUERY, tree_sitter_svelte_ng::LOCALS_QUERY),
    9  => "js"                => "js" | "javascript"       => HighlightConfiguration::new(tree_sitter_javascript::LANGUAGE.into(), "javascript", tree_sitter_javascript::HIGHLIGHT_QUERY, tree_sitter_javascript::INJECTIONS_QUERY, tree_sitter_javascript::LOCALS_QUERY),
    10 => "jsx"               => "jsx"                     => HighlightConfiguration::new(tree_sitter_javascript::LANGUAGE.into(), "jsx", tree_sitter_javascript::JSX_HIGHLIGHT_QUERY, tree_sitter_javascript::INJECTIONS_QUERY, tree_sitter_javascript::LOCALS_QUERY),
    11 => "ts"                => "ts" | "typescript"       => HighlightConfiguration::new(tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(), "typescript", tree_sitter_typescript::HIGHLIGHTS_QUERY, "", ""),
    12 => "tsx"               => "tsx"                     => HighlightConfiguration::new(tree_sitter_typescript::LANGUAGE_TSX.into(), "tsx", tree_sitter_typescript::HIGHLIGHTS_QUERY, "", ""),
    13 => "scss"              => "scss"                    => HighlightConfiguration::new(tree_sitter_scss::language(), "scss", tree_sitter_scss::HIGHLIGHTS_QUERY, "", ""),
    14 => "css"               => "css"                     => HighlightConfiguration::new(tree_sitter_css::LANGUAGE.into(), "css", tree_sitter_css::HIGHLIGHTS_QUERY, "", ""),
    15 => "bash" | "sh"       => "bash" | "shell" | "sh"   => HighlightConfiguration::new(tree_sitter_bash::LANGUAGE.into(), "css", tree_sitter_bash::HIGHLIGHT_QUERY, "", ""),
    16 => "c"                 => "c"                       => HighlightConfiguration::new(tree_sitter_c::LANGUAGE.into(), "c", tree_sitter_c::HIGHLIGHT_QUERY, "", ""),
    17 => "cpp" | "c++"       => "cpp" | "c++"             => HighlightConfiguration::new(tree_sitter_cpp::LANGUAGE.into(), "c++", tree_sitter_cpp::HIGHLIGHT_QUERY, "", ""),
    18 => "cs"                => "c#" | "cs" | "csharp"    => HighlightConfiguration::new(tree_sitter_c_sharp::LANGUAGE.into(), "c#", tree_sitter_c_sharp::HIGHLIGHTS_QUERY, "", ""),
    19 => "ex" | "exs"        => "elixir"                  => HighlightConfiguration::new(tree_sitter_elixir::LANGUAGE.into(), "elixir", tree_sitter_elixir::HIGHLIGHTS_QUERY, tree_sitter_elixir::INJECTIONS_QUERY, ""),
    21 => "go"                => "go" | "golang"           => HighlightConfiguration::new(tree_sitter_go::LANGUAGE.into(), "go", tree_sitter_go::HIGHLIGHTS_QUERY, "", ""),
    22 => "php"               => "php"                     => HighlightConfiguration::new(tree_sitter_php::LANGUAGE_PHP.into(), "php", tree_sitter_php::HIGHLIGHTS_QUERY, tree_sitter_php::INJECTIONS_QUERY, ""),
    23 => "json"              => "json"                    => HighlightConfiguration::new(tree_sitter_json::LANGUAGE.into(), "json", tree_sitter_json::HIGHLIGHTS_QUERY, "", ""),
    24 => "ml"                => "ml" | "ocaml"            => HighlightConfiguration::new(tree_sitter_ocaml::LANGUAGE_OCAML.into(), "ocaml", tree_sitter_ocaml::HIGHLIGHTS_QUERY, "", tree_sitter_ocaml::LOCALS_QUERY),
    25 => "mli"               => "mli" | "ocaml-interface" => HighlightConfiguration::new(tree_sitter_ocaml::LANGUAGE_OCAML_INTERFACE.into(), "ocaml", tree_sitter_ocaml::HIGHLIGHTS_QUERY, "", tree_sitter_ocaml::LOCALS_QUERY),
    26 => "py"                => "py" | "python"           => HighlightConfiguration::new(tree_sitter_python::LANGUAGE.into(), "python", tree_sitter_python::HIGHLIGHTS_QUERY, "", ""),
    27 => "regex"             => "regex"                   => HighlightConfiguration::new(tree_sitter_regex::LANGUAGE.into(), "regex", tree_sitter_regex::HIGHLIGHTS_QUERY, "", ""),
    28 => "rb"                 => "rb" | "ruby"            => HighlightConfiguration::new(tree_sitter_ruby::LANGUAGE.into(), "ruby", tree_sitter_ruby::HIGHLIGHTS_QUERY, "", tree_sitter_ruby::LOCALS_QUERY),
}

pub struct ComrakHighlightAdapter;

impl SyntaxHighlighterAdapter for ComrakHighlightAdapter {
    fn write_highlighted(
        &self,
        output: &mut dyn IoWrite,
        lang: Option<&str>,
        code: &str,
    ) -> std::io::Result<()> {
        let out = format_file(code, FileIdentifier::Token(lang.unwrap_or_default()))
            .map_err(|e| std::io::Error::new(ErrorKind::Other, e))?;
        output.write_all(out.as_bytes())
    }

    fn write_pre_tag(
        &self,
        output: &mut dyn IoWrite,
        _attributes: HashMap<String, String>,
    ) -> std::io::Result<()> {
        write!(output, r#"<pre>"#)
    }

    fn write_code_tag(
        &self,
        _output: &mut dyn IoWrite,
        _attributes: HashMap<String, String>,
    ) -> std::io::Result<()> {
        Ok(())
    }
}

#[derive(Copy, Clone)]
pub enum FileIdentifier<'a> {
    Extension(&'a str),
    Token(&'a str),
}

pub fn format_file(content: &str, identifier: FileIdentifier<'_>) -> anyhow::Result<String> {
    let mut out = String::new();
    format_file_inner(&mut out, content, identifier, true)?;
    Ok(out)
}

pub fn format_file_inner(
    out: &mut String,
    content: &str,
    identifier: FileIdentifier<'_>,
    code_tag: bool,
) -> anyhow::Result<()> {
    let config = match identifier {
        FileIdentifier::Extension(v) => fetch_highlighter_config(v),
        FileIdentifier::Token(v) => fetch_highlighter_config_by_token(v),
    };

    let line_prefix = if code_tag { "<code>" } else { "" };

    let line_suffix = if code_tag { "</code>\n" } else { "\n" };

    let Some(config) = config else {
        for line in content.lines() {
            out.push_str(line_prefix);
            v_htmlescape::b_escape(line.as_bytes(), out);
            out.push_str(line_suffix);
        }

        return Ok(());
    };

    HIGHLIGHTER.with_borrow_mut(|highlighter| {
        let mut spans = highlighter.highlight(config, content.as_bytes(), None, |extension| {
            debug!(extension, "Highlighter switch requested");
            fetch_highlighter_config(extension).or(fetch_highlighter_config_by_token(extension))
        })?;

        let mut tag_open = true;
        out.push_str(line_prefix);

        while let Some(span) = spans.next().transpose()? {
            if !tag_open {
                out.push_str(line_prefix);
                tag_open = true;
            }

            match span {
                HighlightEvent::Source { start, end } => {
                    let content = &content[start..end];

                    for (i, line) in content.lines().enumerate() {
                        if i != 0 {
                            out.push_str(line_suffix);
                            out.push_str(line_prefix);
                        }

                        v_htmlescape::b_escape(line.as_bytes(), out);
                    }

                    if content.ends_with('\n') {
                        out.push_str(line_suffix);
                        tag_open = false;
                    }
                }
                HighlightEvent::HighlightStart(highlight) => {
                    write!(
                        out,
                        r#"<span class="highlight {}">"#,
                        HIGHLIGHT_CLASSES[highlight.0]
                    )?;
                }
                HighlightEvent::HighlightEnd => {
                    out.push_str("</span>");
                }
            }
        }

        if tag_open {
            out.push_str(line_suffix);
        }

        Ok::<_, anyhow::Error>(())
    })?;

    Ok(())
}
