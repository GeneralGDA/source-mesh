use std::collections::BTreeMap;
use std::collections::BTreeSet;

use anyhow::Context as _;
use anyhow::Result;
use anyhow::anyhow;
use anyhow::ensure;
use getset::Getters;
use getset::MutGetters;
use protobuf::CodedInputStream;
use protobuf::Message as _;
use protobuf::MessageField;
use protobuf::SpecialFields;
use scip::symbol::format_symbol;
use scip::symbol::parse_symbol;
use scip::types::Document;
use scip::types::Index;
use scip::types::Metadata;
use scip::types::Occurrence;
use scip::types::Symbol;
use scip::types::SymbolInformation;

use crate::model::FileNode;
use crate::scip::path::source_path;

#[derive(Getters, MutGetters)]
pub(crate) struct ParsedIndex {
    #[getset(get = "pub(crate)", get_mut = "pub(crate)")]
    metadata: MessageField<Metadata>,
    #[getset(get = "pub(crate)")]
    documents: Vec<ParsedDocument>,
    external_symbols: Vec<SymbolInformation>,
    special_fields: SpecialFields,
    #[getset(get = "pub(crate)")]
    symbol_cache: SymbolCache,
}

impl ParsedIndex {
    pub(crate) fn parse(bytes: &[u8]) -> Result<Self> {
        Self::parse_baseline(bytes)?.context("SCIP index contains no source documents")
    }

    pub(crate) fn parse_baseline(bytes: &[u8]) -> Result<Option<Self>> {
        ensure!(!bytes.is_empty(), "SCIP index is empty");
        let mut input = CodedInputStream::from_bytes(bytes);
        let input_length = u64::try_from(bytes.len()).context("SCIP index exceeds the protobuf input size limit")?;
        // The protobuf decoder needs an explicit limit to reject nested messages extending beyond the input.
        input.push_limit(input_length).context("Cannot parse SCIP protobuf index")?;
        let index = Index::parse_from(&mut input).context("Cannot parse SCIP protobuf index")?;
        input.check_eof().context("Cannot parse SCIP protobuf index")?;
        let Index { metadata, documents, external_symbols, special_fields } = index;
        if documents.is_empty() {
            return Ok(None);
        }
        let documents = documents.into_iter().map(|document| {
            Ok(ParsedDocument { file: source_path(&document.relative_path)?, document })
        });
        let documents = documents.collect::<Result<Vec<_>>>()?;
        let document_symbols = documents.iter().flat_map(|document| {
            let symbols = document.document.symbols.iter().map(|information| information.symbol.as_str());
            let occurrences = document.document.occurrences.iter().map(|occurrence| occurrence.symbol.as_str());
            symbols.chain(occurrences)
        });
        let external_symbol_names = external_symbols.iter().map(|information| information.symbol.as_str());
        let symbol_cache = SymbolCache::parse(document_symbols.chain(external_symbol_names));
        Ok(Some(Self { metadata, documents, external_symbols, special_fields, symbol_cache }))
    }

    pub(crate) const fn documents_mut(&mut self) -> &mut [ParsedDocument] {
        self.documents.as_mut_slice()
    }

    pub(crate) const fn documents_with_symbols_mut(&mut self) -> (&mut [ParsedDocument], &mut SymbolCache) {
        (self.documents.as_mut_slice(), &mut self.symbol_cache)
    }

    pub(crate) fn symbols(&self) -> impl Iterator<Item = &SymbolInformation> {
        let document_symbols = self.documents.iter().flat_map(|document| &document.document.symbols);
        document_symbols.chain(&self.external_symbols)
    }

    #[must_use]
    pub(crate) fn into_index(self) -> Index {
        let documents = self.documents.into_iter().map(|document| document.document).collect();
        Index { metadata: self.metadata, documents, external_symbols: self.external_symbols, special_fields: self.special_fields }
    }
}

#[derive(Getters)]
#[getset(get = "pub(crate)")]
pub(crate) struct ParsedDocument {
    file: FileNode,
    document: Document,
}

impl ParsedDocument {
    pub(crate) const fn occurrences_mut(&mut self) -> &mut Vec<Occurrence> {
        &mut self.document.occurrences
    }

    pub(crate) const fn symbols_mut(&mut self) -> &mut Vec<SymbolInformation> {
        &mut self.document.symbols
    }
}

pub(crate) struct SymbolCache {
    symbols: BTreeMap<String, ParsedSymbol>,
}

impl SymbolCache {
    #[must_use]
    fn parse<'a>(symbol_names: impl Iterator<Item = &'a str>) -> Self {
        let distinct_symbols = symbol_names.collect::<BTreeSet<_>>();
        let symbols = distinct_symbols.into_iter().map(|symbol| (symbol.to_owned(), ParsedSymbol::parse(symbol))).collect();
        Self { symbols }
    }

    #[must_use]
    pub(crate) fn get(&self, symbol: &str) -> &ParsedSymbol {
        self.symbols.get(symbol).unwrap_or_else(|| unreachable!("SCIP symbol must be registered before use: {symbol:?}"))
    }

    pub(crate) fn intern(&mut self, symbol: Symbol) -> String {
        let formatted = format_symbol(symbol);
        /* Formatting can canonicalize fields, so the original structure may not match the emitted symbol. */
        self.symbols.entry(formatted.clone()).or_insert_with(|| ParsedSymbol::parse(&formatted));
        formatted
    }
}

pub(crate) enum ParsedSymbol {
    Structured(Symbol),
    Opaque { decoding_error: String },
}

impl ParsedSymbol {
    #[must_use]
    fn parse(symbol: &str) -> Self {
        match parse_symbol(symbol) {
            Ok(parsed) => Self::Structured(parsed),
            Err(error) => Self::Opaque { decoding_error: format!("Cannot decode symbol {symbol:?}: {error:?}") },
        }
    }

    #[must_use]
    pub(crate) const fn structure(&self) -> Option<&Symbol> {
        match *self {
            Self::Structured(ref symbol) => Some(symbol),
            Self::Opaque { .. } => None,
        }
    }

    pub(crate) fn require_structure(&self) -> Result<&Symbol> {
        match *self {
            Self::Structured(ref symbol) => Ok(symbol),
            Self::Opaque { ref decoding_error } => Err(anyhow!(decoding_error.clone())),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use claims::assert_ok;
    use claims::assert_some;
    use pretty_assertions::assert_eq;
    use protobuf::Message as _;
    use rstest::rstest;
    use scip::types::Document;
    use scip::types::Index;
    use scip::types::Metadata;
    use scip::types::SymbolInformation;

    use super::ParsedIndex;
    use super::SymbolCache;

    #[rstest]
    #[case::empty(Index::default())]
    #[case::metadata_only(Index { metadata: Some(Metadata::default()).into(), ..Index::default() })]
    #[case::external_symbols_only(Index { external_symbols: vec![SymbolInformation::default()], ..Index::default() })]
    fn test_source_document_requirement(#[case] mut fixture: Index, #[values(false, true)] has_document: bool) {
        if has_document {
            fixture.documents.push(Document { relative_path: "isolated.rs".into(), ..Document::default() });
        }
        let bytes = fixture.write_to_bytes();
        let bytes = assert_ok!(bytes, "The source-document boundary fixture must serialize");

        let system_under_test = ParsedIndex::parse(&bytes);

        if has_document {
            let system_under_test = assert_ok!(system_under_test, "Adding a source document must make the index acceptable");
            assert_eq!(system_under_test.into_index(), fixture, "Accepted index data must be preserved");
        } else {
            assert!(system_under_test.is_err(), "An index without source documents must be rejected");
        }
    }

    #[rstest]
    #[case::metadata_only(Index { metadata: Some(Metadata::default()).into(), ..Index::default() })]
    #[case::external_symbols_only(Index { external_symbols: vec![SymbolInformation::default()], ..Index::default() })]
    fn test_document_free_baselines(#[case] fixture: Index) {
        let bytes = fixture.write_to_bytes();
        let bytes = assert_ok!(bytes, "The document-free baseline fixture must serialize");
        assert!(!bytes.is_empty(), "The baseline must have a nonzero protobuf encoding");

        let system_under_test = ParsedIndex::parse_baseline(&bytes);
        let system_under_test = assert_ok!(system_under_test, "A valid document-free baseline must decode successfully");

        assert!(system_under_test.is_none(), "A document-free baseline must not construct an empty ParsedIndex");
    }

    #[rstest]
    #[case::empty(&[])]
    #[case::malformed(&[0xff])]
    #[case::truncated_metadata(&[0x0a, 0x01])]
    #[case::metadata_with_malformed_trailing_field(&[0x0a, 0x00, 0xff])]
    fn test_invalid_baseline_data(#[case] bytes: &[u8]) {
        let system_under_test = ParsedIndex::parse_baseline(bytes);

        assert!(system_under_test.is_err(), "Empty or malformed baseline bytes must not become an absent baseline");
    }

    #[rstest]
    #[case("")]
    #[case("src/..")]
    #[case("../outside.rs")]
    fn test_invalid_baseline_document_paths(#[case] path: &str) {
        let fixture = Index {
            documents: vec![Document { relative_path: path.into(), ..Document::default() }],
            ..Index::default()
        };
        let bytes = fixture.write_to_bytes();
        let bytes = assert_ok!(bytes, "The invalid baseline document path fixture must serialize");

        let system_under_test = ParsedIndex::parse_baseline(&bytes);

        assert!(system_under_test.is_err(), "An invalid document path must not become an absent baseline: {path:?}");
    }

    #[rstest]
    #[case::original(false)]
    #[case::baseline(true)]
    fn test_isolated_document(#[case] baseline: bool) {
        let path = "isolated.rs";
        let fixture = Index {
            documents: vec![Document { relative_path: path.into(), ..Document::default() }],
            ..Index::default()
        };
        let bytes = fixture.write_to_bytes();
        let bytes = assert_ok!(bytes, "The isolated document fixture must serialize");

        let system_under_test = if baseline { ParsedIndex::parse_baseline(&bytes) } else { ParsedIndex::parse(&bytes).map(Some) };
        let system_under_test = assert_ok!(system_under_test, "A source document needs neither occurrences nor symbols");
        let system_under_test = assert_some!(system_under_test, "An occurrence-free document is still a nonempty index");

        let document = system_under_test.documents().first();
        let document = assert_some!(document, "Parsing must retain the isolated document");
        assert_eq!(document.file().path(), Path::new(path), "The isolated document must have its parsed file identity");
        assert_eq!(system_under_test.into_index(), fixture, "Parsing must preserve an occurrence-free document");
    }

    #[rstest]
    #[case::structured("rust-analyzer cargo fixture 0.0.0 run().", true)]
    #[case::opaque("synthetic-method", false)]
    #[case::empty("", false)]
    fn test_distinct_symbol_outcomes(#[case] symbol: &str, #[case] structured: bool) {
        let system_under_test = SymbolCache::parse([symbol, symbol, symbol].into_iter());

        let optional_structure = system_under_test.get(symbol).structure();
        let required_structure = system_under_test.get(symbol).require_structure();
        let repeated_structure = system_under_test.get(symbol).require_structure();

        assert_eq!(system_under_test.symbols.len(), 1, "Repeated raw symbols must share one cached outcome");
        assert_eq!(optional_structure.is_some(), structured, "Optional symbol structure must match the SCIP encoding");
        if structured {
            let required_structure = required_structure.as_ref();
            assert_ok!(required_structure, "A structured SCIP symbol must provide its required structure");
        } else {
            assert!(required_structure.is_err(), "An opaque SCIP symbol must fail when its structure is required");
        }
        let required_error = required_structure.err().map(|error| error.to_string());
        let repeated_error = repeated_structure.err().map(|error| error.to_string());
        assert_eq!(required_error, repeated_error, "Decoding failures must retain their diagnostic");
    }

    #[rstest]
    #[case::canonical("rust-analyzer cargo fixture 0.0.0 run().")]
    #[case::escaped_package("rust-analyzer cargo fixture  name 0.0.0 run().")]
    fn test_emitted_symbol_outcome(#[case] symbol: &str) {
        let mut system_under_test = SymbolCache::parse([symbol].into_iter());
        let structure = system_under_test.get(symbol).structure();
        let structure = assert_some!(structure, "The emitted-symbol fixture must have SCIP structure");
        let structure = structure.clone();

        let emitted = system_under_test.intern(structure);
        let replayed = SymbolCache::parse([emitted.as_str()].into_iter());
        let cached_structure = system_under_test.get(&emitted).require_structure();
        let replayed_structure = replayed.get(&emitted).require_structure();

        assert_eq!(system_under_test.symbols.len(), if emitted == symbol { 1 } else { 2 }, "Only new symbols add cache entries");
        assert_eq!(cached_structure.as_ref().ok(), replayed_structure.as_ref().ok(), "Cached and replayed structures differ");
        let cached_error = cached_structure.err().map(|error| error.to_string());
        let replayed_error = replayed_structure.err().map(|error| error.to_string());
        assert_eq!(cached_error, replayed_error, "Cached and replayed symbols must retain identical decoding errors");
    }
}
