//! SDL discovery and parsing.
//!
//! Loads every `**/*.graphql` under a directory into one document set. There is
//! no `import`: file layout is a build concern, not a language feature.

use std::path::{Path, PathBuf};

use graphql_parser::schema::{Definition, Document, TypeDefinition};

use crate::error::CompileError;

/// One parsed SDL file, kept alongside its path for diagnostics.
pub struct SdlFile {
    pub path: PathBuf,
    pub document: Document<'static, String>,
}

/// Recursively collect `*.graphql` files, sorted for deterministic output.
pub fn discover(dir: &Path) -> Result<Vec<PathBuf>, CompileError> {
    if !dir.is_dir() {
        return Err(CompileError::new(format!(
            "SDL directory not found: {}",
            dir.display()
        )));
    }

    let mut files = Vec::new();
    collect(dir, &mut files)?;
    files.sort();

    if files.is_empty() {
        return Err(CompileError::new(format!(
            "no .graphql files found under {}",
            dir.display()
        )));
    }
    Ok(files)
}

fn collect(dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), CompileError> {
    let entries = std::fs::read_dir(dir)
        .map_err(|e| CompileError::new(format!("cannot read {}: {e}", dir.display())))?;

    for entry in entries {
        let entry =
            entry.map_err(|e| CompileError::new(format!("cannot read {}: {e}", dir.display())))?;
        let path = entry.path();
        if path.is_dir() {
            collect(&path, out)?;
        } else if path.extension().is_some_and(|e| e == "graphql") {
            out.push(path);
        }
    }
    Ok(())
}

pub fn parse_dir(dir: &Path) -> Result<Vec<SdlFile>, CompileError> {
    discover(dir)?.into_iter().map(parse_file).collect()
}

pub fn parse_file(path: PathBuf) -> Result<SdlFile, CompileError> {
    let text = std::fs::read_to_string(&path)
        .map_err(|e| CompileError::new(format!("cannot read {}: {e}", path.display())))?;

    // parse_schema borrows from the input, so leak the text to obtain a 'static
    // document. The compiler is a short-lived CLI process; this is bounded by
    // the number of SDL files.
    let leaked: &'static str = Box::leak(text.into_boxed_str());

    let document = graphql_parser::parse_schema::<String>(leaked)
        .map_err(|e| CompileError::new(format!("failed to parse {}: {e}", path.display())))?;

    Ok(SdlFile { path, document })
}

/// Type definitions across all files, paired with the file that declared them.
pub fn type_definitions(files: &[SdlFile]) -> Vec<(&Path, &TypeDefinition<'static, String>)> {
    let mut out = Vec::new();
    for file in files {
        for def in &file.document.definitions {
            if let Definition::TypeDefinition(td) = def {
                out.push((file.path.as_path(), td));
            }
        }
    }
    out
}
