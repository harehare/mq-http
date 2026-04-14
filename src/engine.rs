use crate::cli::Args;
use miette::{IntoDiagnostic, Result};
use mq_lang::DefaultEngine;
use std::path::PathBuf;

/// Create a new engine for each request.
/// The engine is fully isolated so concurrent requests never share state.
pub fn create_engine(args: &Args) -> DefaultEngine {
    let mut engine = DefaultEngine::default();
    engine.load_builtin_module();

    if let Some(dirs) = &args.module_directories {
        engine.set_search_paths(dirs.clone());
    }

    if let Some(cli_args) = &args.args {
        for v in cli_args.chunks(2) {
            if v.len() == 2 {
                engine.define_string_value(&v[0], &v[1]);
            }
        }
    }

    engine
}

/// Load raw file contents as named string values into the engine.
pub fn load_raw_files(engine: &DefaultEngine, args: &Args) -> Result<()> {
    if let Some(raw_file) = &args.raw_file {
        for v in raw_file.chunks(2) {
            if v.len() == 2 {
                let path = PathBuf::from(&v[1]);
                if path.exists() {
                    let content = std::fs::read_to_string(&path).into_diagnostic()?;
                    engine.define_string_value(&v[0], &content);
                }
            }
        }
    }
    Ok(())
}

