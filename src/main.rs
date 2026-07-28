//! `mgit` -- the command-line entry point.
//!
//! This file does argument parsing and nothing else. Every subcommand is one
//! call into `mini_git::commands`, so the CLI can be read as a table of
//! contents for the tool, and the commands stay callable from tests and the
//! benchmark without going through a shell.

use anyhow::Result;
use clap::{Parser, Subcommand};
use mini_git::commands;
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "mgit",
    version,
    about = "A mini Git that writes Git's real on-disk object format",
    long_about = "A minimal version control system. Objects are stored exactly as Git \
                  stores them -- zlib-compressed, framed as \"<type> <size>\\0<content>\", \
                  named by the SHA-1 of the uncompressed bytes -- so real git can read \
                  what this tool writes and vice versa."
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Create an empty repository
    Init {
        /// Directory to initialize (defaults to the current directory)
        #[arg(default_value = ".")]
        path: PathBuf,
    },

    /// Compute the object id of a file, optionally storing it
    #[command(name = "hash-object")]
    HashObject {
        /// File to hash
        path: Option<PathBuf>,
        /// Read the content from standard input instead of a file
        #[arg(long)]
        stdin: bool,
        /// Write the object into the object store
        #[arg(short = 'w')]
        write: bool,
        /// Object type to record in the header
        #[arg(short = 't', default_value = "blob")]
        object_type: String,
    },

    /// Print an object's contents, type, or size
    #[command(name = "cat-file")]
    CatFile {
        /// Object id (full or abbreviated), branch name, or HEAD
        object: String,
        /// Pretty-print the contents (the default)
        #[arg(short = 'p', group = "mode")]
        pretty: bool,
        /// Print the object type
        #[arg(short = 't', group = "mode")]
        show_type: bool,
        /// Print the payload size in bytes
        #[arg(short = 's', group = "mode")]
        size: bool,
    },

    /// Stage files for the next commit
    Add {
        /// Files or directories to stage (use `.` for everything)
        #[arg(required = true)]
        paths: Vec<PathBuf>,
    },

    /// Record the staged snapshot as a new commit
    Commit {
        /// Commit message
        #[arg(short = 'm', long)]
        message: String,
    },

    /// Show commit history, newest first
    Log {
        /// Limit the number of commits shown
        #[arg(short = 'n', long)]
        max_count: Option<usize>,
        /// One line per commit
        #[arg(long)]
        oneline: bool,
    },

    /// Show the state of the working tree and the index
    Status,

    /// Show line-level changes
    Diff {
        /// Compare the index with HEAD instead of the working tree with the index
        #[arg(long, alias = "cached")]
        staged: bool,
        /// Compare the working tree with this commit
        revision: Option<String>,
    },

    /// Restore files from a commit, or switch to a branch
    Checkout {
        /// Branch name, commit id, or HEAD
        revision: String,
        /// Files to restore, after `--`. With no paths, switches to the branch.
        #[arg(last = true)]
        paths: Vec<PathBuf>,
    },

    /// List, create, or delete branches
    Branch {
        /// Name of the branch to create (omit to list)
        name: Option<String>,
        /// Commit the new branch should point at (defaults to HEAD)
        start_point: Option<String>,
        /// Delete the named branch instead
        #[arg(short = 'd', long)]
        delete: Option<String>,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Init { path } => commands::init::run(&path),
        Command::HashObject {
            path,
            stdin,
            write,
            object_type,
        } => commands::hash_object::run(path.as_deref(), stdin, write, &object_type),
        Command::CatFile {
            object,
            pretty,
            show_type,
            size,
        } => {
            use commands::cat_file::Mode;
            // `-p` is the default when no mode flag is given, which is how the
            // command is used in practice.
            let _ = pretty;
            let mode = if show_type {
                Mode::Type
            } else if size {
                Mode::Size
            } else {
                Mode::Pretty
            };
            commands::cat_file::run(&object, mode)
        }
        Command::Add { paths } => commands::add::run(&paths),
        Command::Commit { message } => commands::commit::run(&message),
        Command::Log { max_count, oneline } => commands::log::run(max_count, oneline),
        Command::Status => commands::status::run(),
        Command::Diff { staged, revision } => commands::diff::run(staged, revision.as_deref()),
        Command::Checkout { revision, paths } => commands::checkout::run(&revision, &paths),
        Command::Branch {
            name,
            start_point,
            delete,
        } => commands::branch::run(name.as_deref(), start_point.as_deref(), delete.as_deref()),
    }
}
