//! The code-intelligence provider seam.
//!
//! A provider turns a host-supplied file into the file's graph nodes (and,
//! where supported, its edges). The native default implementation is pure Rust:
//! Rust sources go through the hand-written [`RustParser`]; every other
//! supported language goes through the shared tag-query extractor; anything else
//! contributes a file node only. The trait is the seam that lets a future
//! out-of-process provider be swapped in without the ingest pipeline changing —
//! it is not built here, and the only shipped implementation is native.

use crate::language::Language;
use crate::parse::RustParser;
use crate::tags::parse_with_tags;
use crate::{AdmittedFile, CodeGraphError, ParsedFile};
use std::collections::hash_map::Entry;
use std::collections::HashMap;
use tree_sitter::{Parser, Query};

/// Parses one admitted file into its graph contribution and reports which
/// languages it can extract symbols from.
pub trait CodeIntelligenceProvider {
    /// The languages this provider extracts definition/symbol nodes for.
    fn supported_languages(&self) -> Vec<Language>;

    /// Parses one admitted file into its file node, item nodes, and (where the
    /// implementation supports it) raw calls/imports for the resolver.
    fn parse_file(&mut self, file: &AdmittedFile, text: &str)
        -> Result<ParsedFile, CodeGraphError>;
}

/// The native, in-process provider: tree-sitter grammars only, deterministic and
/// offline. Holds the Rust walker, one reusable parser for the tag-query
/// languages, and a lazily compiled query per language.
pub struct NativeProvider {
    rust: RustParser,
    parser: Parser,
    queries: HashMap<Language, Query>,
}

impl NativeProvider {
    pub fn new() -> Result<Self, CodeGraphError> {
        Ok(Self {
            rust: RustParser::new()?,
            parser: Parser::new(),
            queries: HashMap::new(),
        })
    }
}

impl CodeIntelligenceProvider for NativeProvider {
    fn supported_languages(&self) -> Vec<Language> {
        Language::ALL.to_vec()
    }

    fn parse_file(
        &mut self,
        file: &AdmittedFile,
        text: &str,
    ) -> Result<ParsedFile, CodeGraphError> {
        match Language::from_path(&file.relative) {
            // Rust keeps its richer hand-written extraction unchanged.
            Some(Language::Rust) | None => self.rust.parse_file(file, text),
            Some(language) => {
                // Compile the tag query once per language, then extract.
                // `queries` and `parser` are disjoint fields, so the immutable
                // query borrow and the mutable parser borrow do not conflict.
                if let Entry::Vacant(slot) = self.queries.entry(language) {
                    let sources = language.tags_sources();
                    if sources.is_empty() {
                        return Err(CodeGraphError::Grammar(format!(
                            "{} has no tag query",
                            language.as_str()
                        )));
                    }
                    let source = sources.join("\n");
                    let query = Query::new(&language.grammar(), &source).map_err(|error| {
                        CodeGraphError::Grammar(format!(
                            "{} tag query failed to compile: {error}",
                            language.as_str()
                        ))
                    })?;
                    slot.insert(query);
                }
                let query = self.queries.get(&language).ok_or_else(|| {
                    CodeGraphError::Grammar(format!(
                        "{} tag query missing after compile",
                        language.as_str()
                    ))
                })?;
                parse_with_tags(&mut self.parser, language, query, file, text)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{CodeIntelligenceProvider, NativeProvider};
    use crate::language::Language;
    use crate::AdmittedFile;
    use std::path::PathBuf;

    fn admitted(relative: &str) -> AdmittedFile {
        AdmittedFile {
            absolute: PathBuf::from("unused"),
            relative: relative.to_string(),
        }
    }

    #[test]
    fn native_default_reports_its_languages() -> Result<(), Box<dyn std::error::Error>> {
        let provider = NativeProvider::new()?;
        let languages = provider.supported_languages();
        assert!(languages.contains(&Language::Rust));
        assert!(languages.contains(&Language::Python));
        assert!(languages.contains(&Language::TypeScript));
        assert!(languages.contains(&Language::PowerShell));
        assert_eq!(languages.len(), Language::ALL.len());
        Ok(())
    }

    #[test]
    fn rust_files_route_to_the_rust_walker() -> Result<(), Box<dyn std::error::Error>> {
        let mut provider = NativeProvider::new()?;
        let parsed =
            provider.parse_file(&admitted("src/lib.rs"), "pub fn answer() -> u8 { 42 }\n")?;
        assert!(parsed
            .items
            .iter()
            .any(|item| item.qualified_name == "src/lib.rs::answer"));
        Ok(())
    }

    #[test]
    fn unknown_files_contribute_a_file_node_only() -> Result<(), Box<dyn std::error::Error>> {
        let mut provider = NativeProvider::new()?;
        let parsed = provider.parse_file(&admitted("docs/guide.md"), "# Guide\n")?;
        assert!(parsed.items.is_empty());
        Ok(())
    }
}
