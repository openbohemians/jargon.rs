use serde::de::DeserializeOwned;
use serde_json::{Map, Value};

use crate::Shell;

/// The outcome of parsing arguments: the parsed `data`, any validation
/// `errors`, and the subcommand that was chosen, plus any request for help
/// or a completion script, for the program to act on. Such a request isn't
/// a validation outcome, so check `help_requested` and `completion_shell`
/// before treating `errors` as final.
///
/// More fields may come in later versions, so it is read, not built.
#[derive(Debug, Clone, Default, PartialEq)]
#[non_exhaustive]
pub struct Parsed {
    /// The options, with defaults filled in, keyed by field. Nested
    /// objects are nested maps.
    pub data: Map<String, Value>,
    /// The validation errors, as messages for the user, in the order found.
    pub errors: Vec<String>,
    /// The chosen subcommand, space-separated when nested (`"remote add"`);
    /// None for a CLI without subcommands, or when none matched.
    pub subcommand: Option<String>,
    /// True when `--help` or `-h` was given, unless the schema has an
    /// option of its own by that name.
    pub help_requested: bool,
    /// Whose help was asked for; None is the program's own.
    pub help_subcommand: Option<String>,
    /// The shell given to `--completions`, if any.
    pub completion_shell: Option<Shell>,
}

impl Parsed {
    pub(crate) fn new(data: Map<String, Value>, errors: Vec<String>) -> Parsed {
        Parsed {
            data,
            errors,
            ..Parsed::default()
        }
    }

    pub(crate) fn error(message: impl Into<String>) -> Parsed {
        Parsed {
            errors: vec![message.into()],
            ..Parsed::default()
        }
    }

    /// True when parsing found no errors. A help or completion request has
    /// none either, so check those first when it matters.
    pub fn is_valid(&self) -> bool {
        self.errors.is_empty()
    }

    /// True when `--completions <shell>` was given.
    pub fn completion_requested(&self) -> bool {
        self.completion_shell.is_some()
    }

    /// A top-level value.
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.data.get(key)
    }

    /// The data as compact JSON.
    pub fn to_json(&self) -> String {
        Value::Object(self.data.clone()).to_string()
    }

    /// The data as indented JSON.
    pub fn to_pretty_json(&self) -> String {
        serde_json::to_string_pretty(&self.data).unwrap_or_default()
    }

    /// The data as a value of your own type, such as a struct deriving
    /// `serde::Deserialize` whose fields are the options.
    pub fn deserialize<T: DeserializeOwned>(&self) -> serde_json::Result<T> {
        serde_json::from_value(Value::Object(self.data.clone()))
    }
}
