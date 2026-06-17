//! Language detection and the grammar/tags registry.
//!
//! One table maps a source language to the file extensions that select it, the
//! tree-sitter grammar that parses it, and the tag query that names its
//! definitions and calls. Adding a language is one row here plus a fixture; the
//! extractor in [`crate::tags`] is written once and shared. Rust is detected
//! here for dispatch but extracted by the hand-written [`crate::RustParser`],
//! which predates the tag-query path and produces richer scope/skeletons.

/// A source language the native provider can recognize.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Language {
    Rust,
    Python,
    Go,
    JavaScript,
    TypeScript,
    Tsx,
    CSharp,
    Java,
    C,
    Cpp,
    Ruby,
    Php,
    Lua,
    OCaml,
    Elixir,
    PowerShell,
}

impl Language {
    /// Every language the registry knows, in a stable order.
    pub const ALL: &'static [Language] = &[
        Language::Rust,
        Language::Python,
        Language::Go,
        Language::JavaScript,
        Language::TypeScript,
        Language::Tsx,
        Language::CSharp,
        Language::Java,
        Language::C,
        Language::Cpp,
        Language::Ruby,
        Language::Php,
        Language::Lua,
        Language::OCaml,
        Language::Elixir,
        Language::PowerShell,
    ];

    /// A stable lowercase identifier, used in reports and tests.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Language::Rust => "rust",
            Language::Python => "python",
            Language::Go => "go",
            Language::JavaScript => "javascript",
            Language::TypeScript => "typescript",
            Language::Tsx => "tsx",
            Language::CSharp => "csharp",
            Language::Java => "java",
            Language::C => "c",
            Language::Cpp => "cpp",
            Language::Ruby => "ruby",
            Language::Php => "php",
            Language::Lua => "lua",
            Language::OCaml => "ocaml",
            Language::Elixir => "elixir",
            Language::PowerShell => "powershell",
        }
    }

    /// The file extensions (without the dot) that select this language.
    #[must_use]
    pub fn extensions(self) -> &'static [&'static str] {
        match self {
            Language::Rust => &["rs"],
            Language::Python => &["py", "pyi"],
            Language::Go => &["go"],
            Language::JavaScript => &["js", "jsx", "mjs", "cjs"],
            Language::TypeScript => &["ts", "mts", "cts"],
            Language::Tsx => &["tsx"],
            Language::CSharp => &["cs"],
            Language::Java => &["java"],
            Language::C => &["c", "h"],
            Language::Cpp => &["cpp", "cc", "cxx", "hpp", "hh", "hxx"],
            Language::Ruby => &["rb"],
            Language::Php => &["php"],
            Language::Lua => &["lua"],
            Language::OCaml => &["ml"],
            Language::Elixir => &["ex", "exs"],
            Language::PowerShell => &["ps1", "psm1", "psd1"],
        }
    }

    /// Detects the language of a repo-relative path by its extension.
    #[must_use]
    pub fn from_path(relative: &str) -> Option<Language> {
        let extension = relative.rsplit('.').next().filter(|ext| *ext != relative)?;
        let lowered = extension.to_ascii_lowercase();
        Language::ALL
            .iter()
            .copied()
            .find(|language| language.extensions().contains(&lowered.as_str()))
    }

    /// The tree-sitter grammar for this language.
    #[must_use]
    pub fn grammar(self) -> tree_sitter::Language {
        match self {
            Language::Rust => tree_sitter_rust::LANGUAGE.into(),
            Language::Python => tree_sitter_python::LANGUAGE.into(),
            Language::Go => tree_sitter_go::LANGUAGE.into(),
            Language::JavaScript => tree_sitter_javascript::LANGUAGE.into(),
            Language::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            Language::Tsx => tree_sitter_typescript::LANGUAGE_TSX.into(),
            Language::CSharp => tree_sitter_c_sharp::LANGUAGE.into(),
            Language::Java => tree_sitter_java::LANGUAGE.into(),
            Language::C => tree_sitter_c::LANGUAGE.into(),
            Language::Cpp => tree_sitter_cpp::LANGUAGE.into(),
            Language::Ruby => tree_sitter_ruby::LANGUAGE.into(),
            Language::Php => tree_sitter_php::LANGUAGE_PHP.into(),
            Language::Lua => tree_sitter_lua::LANGUAGE.into(),
            Language::OCaml => tree_sitter_ocaml::LANGUAGE_OCAML.into(),
            Language::Elixir => tree_sitter_elixir::LANGUAGE.into(),
            Language::PowerShell => tree_sitter_powershell::LANGUAGE.into(),
        }
    }

    /// The tag-query sources that name this language's definitions and calls.
    /// Empty for Rust (extracted by the hand-written walker). Usually one source;
    /// TypeScript/TSX combine the JavaScript base query (which carries the
    /// `function`/`class` patterns the TS grammar inherits via
    /// `; inherits: javascript`) with the TypeScript-specific query.
    #[must_use]
    pub fn tags_sources(self) -> &'static [&'static str] {
        match self {
            Language::Rust => &[],
            Language::Python => &[tree_sitter_python::TAGS_QUERY],
            Language::Go => &[tree_sitter_go::TAGS_QUERY],
            Language::JavaScript => &[tree_sitter_javascript::TAGS_QUERY],
            Language::TypeScript | Language::Tsx => TYPESCRIPT_TAGS,
            Language::CSharp => &[tree_sitter_c_sharp::TAGS_QUERY],
            Language::Java => &[tree_sitter_java::TAGS_QUERY],
            Language::C => &[tree_sitter_c::TAGS_QUERY],
            Language::Cpp => &[tree_sitter_cpp::TAGS_QUERY],
            Language::Ruby => &[tree_sitter_ruby::TAGS_QUERY],
            Language::Php => &[tree_sitter_php::TAGS_QUERY],
            Language::Lua => &[tree_sitter_lua::TAGS_QUERY],
            Language::OCaml => &[tree_sitter_ocaml::TAGS_QUERY],
            Language::Elixir => &[tree_sitter_elixir::TAGS_QUERY],
            // PowerShell ships no tag query; this one is original to this repo.
            Language::PowerShell => &[POWERSHELL_TAGS_QUERY],
        }
    }
}

/// TypeScript/TSX combine the JavaScript base tag query (`function`/`class`/…)
/// with the TypeScript-specific one (`interface`/method signatures). The
/// TypeScript grammar is a superset of JavaScript, so the base patterns compile
/// and match against it.
const TYPESCRIPT_TAGS: &[&str] = &[
    tree_sitter_javascript::TAGS_QUERY,
    tree_sitter_typescript::TAGS_QUERY,
];

/// Original tag query for PowerShell (the grammar ships none). Names function
/// definitions and command/method call sites with the standard tags captures so
/// the shared extractor treats PowerShell like any other language.
pub const POWERSHELL_TAGS_QUERY: &str = r#"
(function_statement (function_name) @name) @definition.function
(command command_name: (command_name) @name) @reference.call
(invokation_expression (member_name) @name) @reference.call
"#;

#[cfg(test)]
mod tests {
    use super::Language;

    #[test]
    fn detects_languages_by_extension() {
        assert_eq!(Language::from_path("src/lib.rs"), Some(Language::Rust));
        assert_eq!(Language::from_path("app/main.py"), Some(Language::Python));
        assert_eq!(Language::from_path("pkg/server.go"), Some(Language::Go));
        assert_eq!(Language::from_path("ui/App.tsx"), Some(Language::Tsx));
        assert_eq!(Language::from_path("ui/api.ts"), Some(Language::TypeScript));
        assert_eq!(Language::from_path("Program.cs"), Some(Language::CSharp));
        assert_eq!(Language::from_path("build.ps1"), Some(Language::PowerShell));
        assert_eq!(Language::from_path("README.md"), None);
        assert_eq!(Language::from_path("Makefile"), None);
    }

    #[test]
    fn every_non_rust_language_has_a_tag_query() {
        for language in Language::ALL.iter().copied() {
            if language == Language::Rust {
                assert!(language.tags_sources().is_empty());
            } else {
                assert!(
                    !language.tags_sources().is_empty(),
                    "{} must have a tag query",
                    language.as_str()
                );
            }
        }
    }
}
