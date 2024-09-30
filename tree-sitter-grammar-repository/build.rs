use std::{
    borrow::Cow,
    ffi::OsStr,
    fmt::Write,
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::LazyLock,
};

use anyhow::{bail, Context};
use heck::{ToSnakeCase, ToUpperCamelCase};
use quote::{format_ident, quote};
use serde::Deserialize;
use threadpool::ThreadPool;

const GRAMMAR_REPOSITORY_URL: &str = "https://github.com/helix-editor/helix";
const GRAMMAR_REPOSITORY_REF: &str = "82dd96369302f60a9c83a2d54d021458f82bcd36";
const GRAMMAR_REPOSITORY_CONFIG_PATH: &str = "languages.toml";

static BLACKLISTED_MODULES: &[&str] = &[
    // these languages all don't have corresponding grammars
    "cabal",
    "idris",
    "llvm-mir-yaml",
    "prolog",
    "mint",
    "hare",
    "wren",
    // doesn't compile on macos
    "gemini",
];

fn main() -> anyhow::Result<()> {
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").context("OUT_DIR not set by rustc")?);

    let root = std::env::var("TREE_SITTER_GRAMMAR_LIB_DIR").ok();
    println!("cargo::rerun-if-env-changed=TREE_SITTER_GRAMMAR_LIB_DIR");

    let (root, dylib) = if let Some(root) = root.as_deref() {
        (Path::new(root), true)
    } else {
        (out_dir.as_path(), false)
    };

    let (config, query_path) = if dylib {
        let config: HelixLanguages = toml::from_str(
            &fs::read_to_string(root.join("languages.toml"))
                .context("failed to read languages.toml")?,
        )
        .context("failed to parse helix languages.toml")?;

        println!("cargo::rustc-link-search=native={}", root.display());

        for grammar in &config.grammar {
            if BLACKLISTED_MODULES.contains(&grammar.name.as_str()) {
                continue;
            }

            println!("cargo::rustc-link-lib=dylib=tree-sitter-{}", grammar.name);
        }

        (config, root.join("queries"))
    } else {
        let sources = out_dir.join("sources");
        fs::create_dir_all(&sources)?;

        let helix_root = sources.join("helix");

        fetch_git_repository(GRAMMAR_REPOSITORY_URL, GRAMMAR_REPOSITORY_REF, &helix_root)
            .context(GRAMMAR_REPOSITORY_URL)?;

        let config: HelixLanguages = toml::from_str(
            &fs::read_to_string(helix_root.join(GRAMMAR_REPOSITORY_CONFIG_PATH))
                .context("failed to read helix languages.toml")?,
        )
        .context("failed to parse helix languages.toml")?;

        fetch_and_build_grammar(config.grammar.clone(), &sources)?;

        (config, helix_root.join("runtime/queries"))
    };

    let mut grammar_defs = Vec::new();
    for grammar in &config.grammar {
        let name = &grammar.name;
        if let Some(tokens) =
            build_language_module(name, query_path.as_path()).with_context(|| name.to_string())?
        {
            grammar_defs.push(tokens);
        }
    }
    fs::write(
        out_dir.join("grammar.defs.rs"),
        prettyplease::unparse(
            &syn::parse2(quote!(#(#grammar_defs)*)).context("failed to parse grammar defs")?,
        ),
    )
    .context("failed to write grammar defs")?;

    let registry = build_grammar_registry(config.grammar.iter().map(|v| v.name.clone()));
    fs::write(
        out_dir.join("grammar.registry.rs"),
        prettyplease::unparse(&syn::parse2(registry).context("failed to parse grammar registry")?),
    )
    .context("failed to write grammar registry")?;

    let language = build_language_registry(config.language)?;
    fs::write(
        out_dir.join("language.registry.rs"),
        prettyplease::unparse(&syn::parse2(language)?),
    )?;

    Ok(())
}

fn build_language_registry(
    language_definition: Vec<LanguageDefinition>,
) -> anyhow::Result<proc_macro2::TokenStream> {
    let mut camel = Vec::new();
    let mut grammars = Vec::new();

    let mut globs = Vec::new();
    let mut globs_to_camel = Vec::new();

    let mut injection_regex = Vec::new();
    let mut injection_regex_str_len = Vec::new();
    let mut regex_to_camel = Vec::new();

    for language in &language_definition {
        if BLACKLISTED_MODULES.contains(&language.name.as_str()) {
            continue;
        }

        let camel_cased_name = format_ident!("{}", language.name.to_upper_camel_case());
        camel.push(camel_cased_name.clone());

        let grammar = language
            .grammar
            .as_deref()
            .unwrap_or(language.name.as_str());
        grammars.push(format_ident!("{}", grammar.to_upper_camel_case()));

        for ty in &language.file_types {
            match ty {
                FileType::Glob { glob } => globs.push(Cow::Borrowed(glob)),
                FileType::Extension(ext) => globs.push(Cow::Owned(format!("*.{ext}"))),
            }

            globs_to_camel.push(camel_cased_name.clone());
        }

        if let Some(regex) = language.injection_regex.as_deref() {
            injection_regex.push(format!("^{regex}$"));
            injection_regex_str_len.push(regex.len());
            regex_to_camel.push(camel_cased_name.clone());
        }
    }

    let injection_regex_len = injection_regex.len();
    let globs_array_len = globs.len();
    let globs_string_len = globs.iter().map(|v| v.len()).collect::<Vec<_>>();

    Ok(quote! {
        #[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub enum Language {
            #(#camel),*
        }

        impl Language {
            pub const VARIANTS: &[Self] = &[
                #(Self::#camel),*
            ];

            pub const fn grammar(self) -> Grammar {
                match self {
                    #(Self::#camel => Grammar::#grammars),*
                }
            }

            pub fn from_file_name<P: AsRef<::std::path::Path>>(name: P) -> Option<Self> {
                const LENGTHS: [usize; #globs_array_len] = [#(#globs_string_len),*];
                const GLOB_TO_VARIANT: [Language; #globs_array_len] = [#(Language::#globs_to_camel),*];

                thread_local! {
                    static GLOB: ::std::cell::LazyCell<::globset::GlobSet> = ::std::cell::LazyCell::new(|| {
                        ::globset::GlobSetBuilder::new()
                            #(.add(::globset::Glob::new(#globs).unwrap()))*
                            .build()
                            .unwrap()
                    });
                }

                let mut max = usize::MAX;
                let mut curr = None;

                GLOB.with(|glob| {
                    for m in glob.matches(name) {
                        let curr_length = LENGTHS[m];

                        if curr_length < max {
                            max = curr_length;
                            curr = Some(GLOB_TO_VARIANT[m]);
                        }
                    }
                });

                curr
            }

            pub fn from_injection(name: &str) -> Option<Self> {
                const LENGTHS: [usize; #injection_regex_len] = [#(#injection_regex_str_len),*];
                const REGEX_TO_VARIANT: [Language; #injection_regex_len] = [#(Language::#regex_to_camel),*];

                thread_local! {
                    static REGEX: ::std::cell::LazyCell<::regex::RegexSet> = ::std::cell::LazyCell::new(|| {
                        ::regex::RegexSet::new([
                            #(#injection_regex),*
                        ])
                        .unwrap()
                    });
                }

                let mut max = usize::MAX;
                let mut curr = None;

                REGEX.with(|regex| {
                    for m in regex.matches(name) {
                        let curr_length = LENGTHS[m];

                        if curr_length < max {
                            max = curr_length;
                            curr = Some(REGEX_TO_VARIANT[m]);
                        }
                    }
                });

                curr
            }
        }
    })
}

fn build_grammar_registry(names: impl Iterator<Item = String>) -> proc_macro2::TokenStream {
    let (ids, plain, camel, snake) = names
        .filter(|name| !BLACKLISTED_MODULES.contains(&name.as_str()))
        .enumerate()
        .fold(
            (Vec::new(), Vec::new(), Vec::new(), Vec::new()),
            |(mut ids, mut plain_acc, mut camel_acc, mut snake_acc), (i, name)| {
                camel_acc.push(format_ident!("{}", name.to_upper_camel_case()));

                if name == "move" {
                    snake_acc.push(format_ident!("r#{}", name.to_snake_case()));
                } else {
                    snake_acc.push(format_ident!("{}", name.to_snake_case()));
                }

                plain_acc.push(name);

                ids.push(i);
                (ids, plain_acc, camel_acc, snake_acc)
            },
        );

    quote! {
        #[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub enum Grammar {
            #(#camel),*
        }

        impl Grammar {
            pub const VARIANTS: &[Self] = &[
                #(Self::#camel),*
            ];

            pub const fn highlight_configuration_params(self) -> crate::HighlightConfigurationParams {
                match self {
                    #(Self::#camel => crate::HighlightConfigurationParams {
                        language: crate::grammar::#snake::LANGUAGE,
                        name: #plain,
                        highlights_query: crate::grammar::#snake::HIGHLIGHTS_QUERY,
                        injection_query: crate::grammar::#snake::INJECTIONS_QUERY,
                        locals_query: crate::grammar::#snake::LOCALS_QUERY,
                    }),*
                }
            }

            pub const fn idx(self) -> usize {
                match self {
                    #(Self::#camel => #ids),*
                }
            }
        }
    }
}

fn build_language_module(
    name: &str,
    query_path: &Path,
) -> anyhow::Result<Option<proc_macro2::TokenStream>> {
    if BLACKLISTED_MODULES.contains(&name) {
        return Ok(None);
    }

    let highlights_query = read_local_query(query_path, name, "highlights.scm");
    let injections_query = read_local_query(query_path, name, "injections.scm");
    let locals_query = read_local_query(query_path, name, "locals.scm");

    let ffi = format_ident!("tree_sitter_{}", name.to_snake_case());
    let name = if name == "move" {
        format_ident!("r#{}", name.to_snake_case())
    } else {
        format_ident!("{}", name.to_snake_case())
    };

    Ok(Some(quote! {
        pub mod #name {
            extern "C" {
                fn #ffi() -> *const ();
            }

            pub const LANGUAGE: tree_sitter_language::LanguageFn = unsafe { tree_sitter_language::LanguageFn::from_raw(#ffi) };
            pub const HIGHLIGHTS_QUERY: &str = #highlights_query;
            pub const INJECTIONS_QUERY: &str = #injections_query;
            pub const LOCALS_QUERY: &str = #locals_query;
        }
    }))
}

// taken from https://github.com/helix-editor/helix/blob/2ce4c6d5fa3e50464b41a3d0190ad0e5ada2fc3c/helix-core/src/syntax.rs#L721
fn read_local_query(query_path: &Path, language: &str, filename: &str) -> String {
    static INHERITS_REGEX: LazyLock<regex::Regex> =
        LazyLock::new(|| regex::Regex::new(r";+\s*inherits\s*:?\s*([a-z_,()-]+)\s*").unwrap());

    let path = query_path.join(language).join(filename);

    if !path.exists() {
        return String::new();
    }

    let query =
        fs::read_to_string(&path).unwrap_or_else(|e| panic!("failed to fetch {path:?}: {e:?}"));

    INHERITS_REGEX
        .replace_all(&query, |captures: &regex::Captures| {
            captures[1]
                .split(',')
                .fold(String::new(), |mut output, language| {
                    // `write!` to a String cannot fail.
                    write!(
                        output,
                        "\n{}\n",
                        read_local_query(query_path, language, filename)
                    )
                    .unwrap();
                    output
                })
        })
        .to_string()
}

fn fetch_and_build_grammar(
    grammars: Vec<GrammarDefinition>,
    source_dir: &Path,
) -> anyhow::Result<()> {
    let pool = ThreadPool::new(std::thread::available_parallelism()?.get());

    for grammar in grammars {
        if BLACKLISTED_MODULES.contains(&grammar.name.as_str()) {
            continue;
        }

        let mut grammar_root = source_dir.join(&grammar.name);

        pool.execute(move || {
            let grammar_root = match grammar.source {
                GrammarSource::Git {
                    remote,
                    revision,
                    subpath,
                } => {
                    fetch_git_repository(&remote, &revision, &grammar_root)
                        .context(GRAMMAR_REPOSITORY_URL)
                        .expect("failed to fetch git repository");

                    if let Some(subpath) = subpath {
                        grammar_root.push(subpath);
                    }

                    grammar_root
                }
                GrammarSource::Local { path } => path,
            };

            let grammar_src = grammar_root.join("src");

            let parser_file = Some(grammar_src.join("parser.c"))
                .filter(|s| s.exists())
                .or_else(|| Some(grammar_src.join("parser.cc")))
                .filter(|s| s.exists());
            let scanner_file = Some(grammar_src.join("scanner.c"))
                .filter(|s| s.exists())
                .or_else(|| Some(grammar_src.join("scanner.cc")))
                .filter(|s| s.exists());

            if let Some(parser_file) = parser_file {
                cc::Build::new()
                    .cpp(parser_file.extension() == Some(OsStr::new("cc")))
                    .file(parser_file)
                    .flag_if_supported("-w")
                    .flag_if_supported("-s")
                    .include(&grammar_src)
                    .compile(&format!("{}-parser", grammar.name));
            }

            if let Some(scanner_file) = scanner_file {
                cc::Build::new()
                    .cpp(scanner_file.extension() == Some(OsStr::new("cc")))
                    .file(scanner_file)
                    .flag_if_supported("-w")
                    .flag_if_supported("-s")
                    .include(&grammar_src)
                    .compile(&format!("{}-scanner", grammar.name));
            }
        });
    }

    pool.join();

    Ok(())
}

fn fetch_git_repository(url: &str, ref_: &str, destination: &Path) -> anyhow::Result<()> {
    if !destination.exists() {
        let res = Command::new("git").arg("init").arg(destination).status()?;
        if !res.success() {
            bail!("git init failed with exit code {res}");
        }

        let res = Command::new("git")
            .args(["remote", "add", "origin", url])
            .current_dir(destination)
            .status()?;
        if !res.success() {
            bail!("git remote failed with exit code {res}");
        }
    }

    let res = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(destination)
        .output()?
        .stdout;
    if res == ref_.as_bytes() {
        return Ok(());
    }

    let res = Command::new("git")
        .args(["fetch", "--depth", "1", "origin", ref_])
        .current_dir(destination)
        .status()?;
    if !res.success() {
        bail!("git fetch failed with exit code {res}");
    }

    let res = Command::new("git")
        .args(["reset", "--hard", ref_])
        .current_dir(destination)
        .status()?;
    if !res.success() {
        bail!("git fetch failed with exit code {res}");
    }

    Ok(())
}

#[derive(Deserialize)]
#[serde(rename_all = "kebab-case")]
struct LanguageDefinition {
    name: String,
    injection_regex: Option<String>,
    file_types: Vec<FileType>,
    grammar: Option<String>,
}

#[derive(Deserialize)]
#[serde(untagged)]
pub enum FileType {
    Glob { glob: String },
    Extension(String),
}

#[derive(Deserialize, Clone)]
#[serde(rename_all = "kebab-case")]
pub struct GrammarDefinition {
    name: String,
    source: GrammarSource,
}

#[derive(Deserialize, Clone)]
#[serde(rename_all = "lowercase", untagged)]
enum GrammarSource {
    Git {
        #[serde(rename = "git")]
        remote: String,
        #[serde(rename = "rev")]
        revision: String,
        subpath: Option<String>,
    },
    Local {
        path: PathBuf,
    },
}

#[derive(Deserialize)]
#[serde(rename_all = "kebab-case")]
struct HelixLanguages {
    language: Vec<LanguageDefinition>,
    grammar: Vec<GrammarDefinition>,
}
