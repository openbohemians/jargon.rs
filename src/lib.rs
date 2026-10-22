//! Jargon builds a command-line parser from a JSON Schema: the schema is
//! the single source of truth for a program's options, their types,
//! validation, defaults, help, and shell completion.
//!
//! ```
//! let cli = jargon::Cli::from_json(r#"{
//!     "type": "object",
//!     "properties": {
//!         "name": {"type": "string", "short": "n"},
//!         "count": {"type": "integer", "default": 1}
//!     },
//!     "required": ["name"]
//! }"#, "greet")?;
//!
//! let parsed = cli.parse(&["-n", "World"]);
//! assert!(parsed.is_valid());
//! assert_eq!(parsed.get("name").unwrap(), "World");
//! assert_eq!(parsed.get("count").unwrap(), 1);
//! # Ok::<(), jargon::Error>(())
//! ```
//!
//! This is a port of the [Crystal implementation](https://github.com/trans/jargon),
//! held to the same behavior by the conformance cases its spec suite
//! records (`tests/conformance`).

mod cli;
mod completion;
mod config;
mod help;
mod parsed;
mod run;
mod schema;
mod utility;
pub mod validator;
mod values;
#[cfg(feature = "yaml")]
mod yaml;

use std::fmt;

pub use cli::{Cli, ParseOptions, Subcommand};
pub use completion::{COMPLETE_VERB, CompletionContext, Shell};
pub use config::ConfigOptions;
pub use parsed::Parsed;
pub use schema::{Pattern, Property, Schema, Type};
pub use utility::{flatten, merge};
pub use validator::validate;

/// Why a schema, or a CLI built from schemas, couldn't be made.
#[derive(Debug)]
#[non_exhaustive]
pub enum Error {
    /// A schema document isn't valid JSON or YAML.
    Syntax(String),
    /// A schema is well-formed but can't be used, such as a `$ref` to a
    /// mixin that doesn't exist.
    Schema(String),
    /// A call that doesn't fit the CLI, such as validating against a
    /// subcommand it doesn't have.
    Usage(String),
    /// A schema file couldn't be read.
    Io(std::io::Error),
}

impl Error {
    pub(crate) fn syntax(error: impl fmt::Display) -> Error {
        Error::Syntax(error.to_string())
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Syntax(message) | Error::Schema(message) | Error::Usage(message) => {
                f.write_str(message)
            }
            Error::Io(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Io(error) => Some(error),
            _ => None,
        }
    }
}

impl From<std::io::Error> for Error {
    fn from(error: std::io::Error) -> Error {
        Error::Io(error)
    }
}

/// The errors from [`Cli::json`], for a program that treats invalid input
/// as fatal. It displays as the messages, one per line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    pub errors: Vec<String>,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.errors.join("\n"))
    }
}

impl std::error::Error for ParseError {}
