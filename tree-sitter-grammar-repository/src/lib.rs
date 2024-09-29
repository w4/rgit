//! # tree-sitter-grammar-repository
//!
//! This crate loads in all known languages and grammars from `helix`'s
//! `languages.toml` at compile time and provides an easy way for you
//! to easily map the language to a highlighter configuration.
//!
//! `tree-sitter` grammars can be dynamically linked by setting the
//! `TREE_SITTER_GRAMMAR_LIB_DIR` environment variable. If set, this library
//! expects a directory of the format:
//!
//! ```text
//! - TREE_SITTER_GRAMMAR_LIB_DIR
//!   - sources/
//!     - html/
//!       - queries/
//!         - highlights.scm
//!         - injections.scm
//!       - package.json
//!     - javascript/
//!       - queries/
//!         - highlights.scm
//!         - injections.scm
//!       - package.json
//!   - libhtml-parser.so
//!   - libhtml-scanner.so
//!   - libjavascsript-scanner.so
//!   - ...
//! ```
//!
//! Usage:
//!
//! ```ignore
//! use std::collections::HashMap;
//! use tree_sitter_grammar_repository::Grammar;
//! use tree_sitter_highlight::HighlightConfiguration;
//!
//! let highlighter_configurations = Grammar::VARIANTS
//!     .iter()
//!     .copied()
//!     .map(Grammar::highlight_configuration_params)
//!     .map(|v| (v, HighlightConfiguration::new(
//!         v.language.into(),
//!         v.name,
//!         v.highlights_query,
//!         v.injection_query,
//!         v.locals_query
//!     )))
//!     .collect::<HashMap<Grammar, HighlightConfiguration>>();
//!
//! let highlighter_configuration = highlighter_configurations
//!     .get(&Language::from_file_name("hello_world.toml").grammar());
//! ```

include!(concat!(env!("OUT_DIR"), "/grammar.registry.rs"));
include!(concat!(env!("OUT_DIR"), "/language.registry.rs"));
pub mod grammar {
    include!(concat!(env!("OUT_DIR"), "/grammar.defs.rs"));
}

pub struct HighlightConfigurationParams {
    pub language: tree_sitter_language::LanguageFn,
    pub name: &'static str,
    pub highlights_query: &'static str,
    pub injection_query: &'static str,
    pub locals_query: &'static str,
}
