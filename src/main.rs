//! graphql-rpcgen CLI.
//!
//! `generate` writes the artifacts; `check` verifies the checked-in files match
//! what the current SDL would produce, and exits non-zero on drift.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

const DEFAULT_SDL_DIR: &str = "api";
const USAGE: &str = "\
graphql-rpcgen - GraphQL SDL -> Rust / TypeScript / OpenAPI

USAGE:
    graphql-rpcgen generate [--api <dir>] [--root <dir>] [--config <file>]
    graphql-rpcgen check    [--api <dir>] [--root <dir>] [--config <file>]

COMMANDS:
    generate    Write generated artifacts to disk.
    check       Validate the SDL and fail if generated files are stale.

OPTIONS:
    --api <dir>       SDL directory (default: api)
    --root <dir>      Project root that output paths are relative to (default: .)
    --config <file>   Settings file (default: <root>/rpcgen.toml)
    -h, --help        Show this help.
";

struct Args {
    command: Command,
    api_dir: PathBuf,
    root: PathBuf,
    config: Option<PathBuf>,
}

enum Command {
    Generate,
    Check,
}

fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(Some(args)) => args,
        Ok(None) => {
            print!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Err(message) => {
            eprintln!("error: {message}\n\n{USAGE}");
            return ExitCode::FAILURE;
        }
    };

    match run(&args) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn parse_args() -> Result<Option<Args>, String> {
    let mut argv = std::env::args().skip(1);
    let Some(command) = argv.next() else {
        return Err("missing command".to_string());
    };

    if command == "-h" || command == "--help" || command == "help" {
        return Ok(None);
    }

    let command = match command.as_str() {
        "generate" => Command::Generate,
        "check" => Command::Check,
        other => return Err(format!("unknown command `{other}`")),
    };

    let mut api_dir = PathBuf::from(DEFAULT_SDL_DIR);
    let mut root = PathBuf::from(".");
    let mut config = None;
    while let Some(flag) = argv.next() {
        match flag.as_str() {
            "--api" => {
                api_dir = argv.next().ok_or("`--api` requires a directory")?.into();
            }
            "--root" => {
                root = argv.next().ok_or("`--root` requires a directory")?.into();
            }
            "--config" => {
                config = Some(argv.next().ok_or("`--config` requires a file")?.into());
            }
            "-h" | "--help" => return Ok(None),
            other => return Err(format!("unknown option `{other}`")),
        }
    }

    Ok(Some(Args {
        command,
        api_dir,
        root,
        config,
    }))
}

fn run(args: &Args) -> erris::Result<ExitCode> {
    let config = load_config(args)?;
    let api = graphql_rpcgen::compile(&args.api_dir)?;
    let files = graphql_rpcgen::generate_all(&api, &config)?;

    // A run producing nothing is a misconfiguration, not a quiet success. It is
    // the shape a missing settings file takes — `load_config` falls back to the
    // defaults, which name no target — and left as a warning it made `check`
    // print "up to date" and exit zero without reading a single file.
    if files.is_empty() {
        return Err(erris::report!(
            "no outputs configured, so nothing was {}: name at least one target in an \
             [output] section of {}",
            match args.command {
                Command::Generate => "generated",
                Command::Check => "checked",
            },
            config_path(args).display()
        ));
    }

    match args.command {
        Command::Generate => {
            for file in &files {
                let target = args.root.join(&file.path);
                write_if_changed(&target, &file.contents)?;
            }
            println!(
                "generated {} file(s) from {}",
                files.len(),
                args.api_dir.display()
            );
            Ok(ExitCode::SUCCESS)
        }
        Command::Check => {
            let mut stale = Vec::new();
            for file in &files {
                let target = args.root.join(&file.path);
                // A missing file is simply stale; an unreadable one is a
                // different failure and must not be swallowed into "stale".
                let current = match std::fs::read_to_string(&target) {
                    Ok(current) => current,
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
                    Err(e) => return Err(erris::report!("{}: {e}", target.display())),
                };
                if current != file.contents {
                    stale.push(file.path.display().to_string());
                }
            }

            if stale.is_empty() {
                println!("generated files are up to date");
                return Ok(ExitCode::SUCCESS);
            }

            eprintln!("generated files are stale; run `graphql-rpcgen generate`:");
            for path in &stale {
                eprintln!("  - {path}");
            }
            Ok(ExitCode::FAILURE)
        }
    }
}

/// The settings file this run reads: the one named on the command line, or
/// `rpcgen.toml` beside the root. Named even when it is absent, so a failure
/// can say which file it looked for.
fn config_path(args: &Args) -> PathBuf {
    args.config
        .clone()
        .unwrap_or_else(|| args.root.join(graphql_rpcgen::CONFIG_FILE))
}

/// Read the settings file. An explicit `--config` must exist; the default one
/// is optional, so a project with no custom scalars needs no file at all.
fn load_config(args: &Args) -> erris::Result<graphql_rpcgen::config::Config> {
    let path = config_path(args);
    if args.config.is_some() || path.exists() {
        return Ok(graphql_rpcgen::config::Config::from_toml_file(&path)?);
    }
    Ok(graphql_rpcgen::config::Config::default())
}

/// Skip rewriting unchanged files so build systems watching mtimes stay quiet.
fn write_if_changed(path: &Path, contents: &str) -> erris::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if std::fs::read_to_string(path).is_ok_and(|existing| existing == contents) {
        return Ok(());
    }
    std::fs::write(path, contents)?;
    println!("  wrote {}", path.display());
    Ok(())
}
