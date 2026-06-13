//! [`CompilationBuilderConfig`] implementation that reads Solidity sources from
//! disk and resolves imports.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use edr_common::fs::normalize_path;
use slang_solidity_v2::compilation::CompilationBuilderConfig;

/// Reads files from disk and resolves imports. Relative imports (`./`, `../`)
/// are normalized against the importer's directory; every other import path is
/// looked up in a caller-provided map of import source name to absolute path.
pub(crate) struct DiskResolver {
    import_map: HashMap<String, PathBuf>,
}

impl DiskResolver {
    pub(crate) fn new(import_map: HashMap<String, PathBuf>) -> Self {
        Self { import_map }
    }
}

impl CompilationBuilderConfig for DiskResolver {
    fn read_file(&mut self, file_id: &str) -> Result<String, String> {
        std::fs::read_to_string(Path::new(file_id)).map_err(|error| error.to_string())
    }

    fn resolve_import(
        &mut self,
        source_file_id: &str,
        import_path: &str,
    ) -> Result<String, String> {
        if is_relative_import(import_path) {
            let parent = Path::new(source_file_id)
                .parent()
                .unwrap_or_else(|| Path::new(""));
            let normalized = normalize_path(&parent.join(import_path));
            Ok(normalized.to_string_lossy().into_owned())
        } else {
            self.import_map
                .get(import_path)
                .map(|path| normalize_path(path).to_string_lossy().into_owned())
                .ok_or_else(|| format!("import '{import_path}' not found in import mappings"))
        }
    }
}

/// Whether an import path is relative (resolved against the importer) rather
/// than mapped (npm-style, resolved via the import map).
fn is_relative_import(import_path: &str) -> bool {
    import_path.starts_with("./") || import_path.starts_with("../")
}
