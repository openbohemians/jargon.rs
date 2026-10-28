use std::borrow::Cow;
use std::collections::HashSet;
use std::ffi::OsStr;
use std::io::{Cursor, Read};
use std::path::Path;

use indexmap::IndexMap;
use serde_json::{Map, Value};

use crate::completion::{Completer, Shell};
use crate::config::{ConfigParser, deep_merge};
use crate::schema::{self, Property, Schema, Type};
use crate::validator::{validate, validate_except};
use crate::values;
use crate::{Error, ParseError, Parsed};

/// A command's schema, or a nested CLI with subcommands of its own.
#[derive(Debug)]
#[allow(
    clippy::large_enum_variant,
    reason = "a program has a handful of subcommands"
)]
pub enum Subcommand {
    /// A command with options of its own.
    Schema(Schema),
    /// A command with subcommands of its own, as `remote` is in
    /// `git remote add`.
    Cli(Cli),
}

impl From<Schema> for Subcommand {
    fn from(schema: Schema) -> Subcommand {
        Subcommand::Schema(schema)
    }
}

impl From<Cli> for Subcommand {
    fn from(cli: Cli) -> Subcommand {
        Subcommand::Cli(cli)
    }
}

/// Looks up an environment variable.
type EnvLookup<'a> = dyn Fn(&str) -> Option<String> + 'a;

/// What [`Cli::parse_with`] reads besides the arguments. Each field left as
/// None uses the process's own: the real environment and standard input.
#[derive(Default)]
pub struct ParseOptions<'a> {
    /// Option defaults, such as from a config file. They rank below
    /// arguments and environment variables, and above the schema's
    /// defaults.
    ///
    /// With subcommands, an object under a subcommand's name is that
    /// subcommand's section, and any other key is global. A subcommand
    /// takes the global keys it has an option for, then its section's
    /// keys over them, every one of them. A nested CLI's section holds its
    /// own subcommands' sections the same way.
    pub defaults: Option<&'a Map<String, Value>>,
    /// The input read for the `-@` form (options as JSON).
    pub stdin: Option<&'a mut dyn Read>,
    /// Looks up an environment variable.
    pub env: Option<&'a EnvLookup<'a>>,
}

/// The words after the program's name in a command line, each checked for
/// UTF-8.
pub(crate) fn arguments<I>(args: I) -> Result<Vec<String>, String>
where
    I: IntoIterator,
    I::Item: AsRef<OsStr>,
{
    args.into_iter()
        .skip(1)
        .map(|arg| {
            let arg = arg.as_ref();
            arg.to_str()
                .map(String::from)
                .ok_or_else(|| format!("Argument '{}' is not valid UTF-8", arg.to_string_lossy()))
        })
        .collect()
}

/// Option defaults worked out for a CLI or command, borrowed where
/// nothing needed changing.
type Defaults<'a> = Option<Cow<'a, Map<String, Value>>>;

/// One parse's inputs, passed down to nested CLIs.
struct Context<'a> {
    /// Option defaults from further up, for any command with the option.
    globals: Option<&'a Map<String, Value>>,
    /// The option defaults given for this CLI or command: with
    /// subcommands, an object under a subcommand's name is that
    /// subcommand's section, and any other key is global.
    section: Option<&'a Map<String, Value>>,
    env: &'a EnvLookup<'a>,
    input: &'a mut dyn Read,
}

/// A command-line parser driven by one or more schemas. A CLI is either
/// flat (one schema) or has subcommands, each a schema or a nested CLI.
/// Build one with [`Cli::from_json`] (or a file) or [`Cli::new`],
/// then [`parse`](Cli::parse) arguments into a [`Parsed`], or get the
/// validated data straight from [`json`](Cli::json).
pub struct Cli {
    pub(crate) schema: Option<Schema>,
    pub(crate) program_name: String,
    pub(crate) subcommands: IndexMap<String, Subcommand>,
    default_subcommand: Option<String>,
    subcommand_key: String,
    bare_assignment: bool,
    pub(crate) completers: IndexMap<String, Completer>,
    pub(crate) config_formats: IndexMap<String, ConfigParser>,
}

impl std::fmt::Debug for Cli {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Cli")
            .field("program_name", &self.program_name)
            .field("schema", &self.schema)
            .field("subcommands", &self.subcommands)
            .field("default_subcommand", &self.default_subcommand)
            .field("subcommand_key", &self.subcommand_key)
            .field("bare_assignment", &self.bare_assignment)
            .field("completers", &self.completers.keys().collect::<Vec<_>>())
            .field(
                "config_formats",
                &self.config_formats.keys().collect::<Vec<_>>(),
            )
            .finish()
    }
}

// ---- construction ---------------------------------------------------------

impl Cli {
    /// An empty CLI to attach subcommands to.
    pub fn new(program_name: impl Into<String>) -> Cli {
        Cli {
            schema: None,
            program_name: program_name.into(),
            subcommands: IndexMap::new(),
            default_subcommand: None,
            subcommand_key: "subcommand".into(),
            bare_assignment: true,
            completers: IndexMap::new(),
            config_formats: IndexMap::new(),
        }
    }

    /// A flat CLI for one schema.
    pub fn with_schema(schema: Schema, program_name: impl Into<String>) -> Cli {
        Cli {
            schema: Some(schema),
            ..Cli::new(program_name)
        }
    }

    /// A CLI from a JSON schema. Several objects in a row (relaxed JSONL)
    /// are subcommands, each named by its `name`, with `$id` mixins.
    pub fn from_json(json: &str, program_name: impl Into<String>) -> Result<Cli, Error> {
        if multi_json(json) {
            let mut cli = Cli::new(program_name);
            cli.load_multi_docs(split_json_documents(json)?)?;
            Ok(cli)
        } else {
            Ok(Cli::with_schema(Schema::from_json(json)?, program_name))
        }
    }

    /// A CLI from schema documents already parsed, such as YAML read with a
    /// library of your choice: one document is the CLI's schema, and
    /// several are subcommands, each named by its `name`, with `$id`
    /// mixins, as for [`from_json`](Cli::from_json).
    ///
    /// ```
    /// use serde_json::json;
    ///
    /// let cli = jargonaut::Cli::from_values(
    ///     [
    ///         json!({"$id": "global", "properties": {"verbose": {"type": "boolean"}}}),
    ///         json!({"name": "fetch", "allOf": [{"$ref": "global"}], "properties": {"url": {"type": "string"}}}),
    ///     ],
    ///     "app",
    /// )?;
    /// let parsed = cli.parse_from(["app", "fetch", "--verbose", "--url", "x"]);
    /// assert_eq!(parsed.subcommand.as_deref(), Some("fetch"));
    /// # Ok::<(), jargonaut::Error>(())
    /// ```
    pub fn from_values(
        docs: impl IntoIterator<Item = Value>,
        program_name: impl Into<String>,
    ) -> Result<Cli, Error> {
        let mut docs: Vec<Value> = docs.into_iter().collect();
        if docs.len() > 1 {
            let mut cli = Cli::new(program_name);
            cli.load_multi_docs(docs)?;
            Ok(cli)
        } else {
            let doc = docs.pop().unwrap_or(Value::Null);
            Ok(Cli::with_schema(Schema::from_value(&doc)?, program_name))
        }
    }

    /// A CLI from a JSON schema file, as [`from_json`](Cli::from_json)
    /// reads one.
    pub fn from_file(
        path: impl AsRef<Path>,
        program_name: impl Into<String>,
    ) -> Result<Cli, Error> {
        Cli::from_json(&std::fs::read_to_string(path)?, program_name)
    }

    /// Add a subcommand: a schema, or a nested CLI.
    pub fn subcommand(
        &mut self,
        name: impl Into<String>,
        subcommand: impl Into<Subcommand>,
    ) -> &mut Cli {
        self.subcommands.insert(name.into(), subcommand.into());
        self
    }

    /// Add a subcommand from a JSON schema.
    pub fn subcommand_json(
        &mut self,
        name: impl Into<String>,
        json: &str,
    ) -> Result<&mut Cli, Error> {
        Ok(self.subcommand(name, Schema::from_json(json)?))
    }

    /// Add a subcommand from a JSON schema file. A file of several objects
    /// becomes a nested CLI named `name`, with a subcommand per document.
    pub fn subcommand_file(
        &mut self,
        name: impl Into<String>,
        path: impl AsRef<Path>,
    ) -> Result<&mut Cli, Error> {
        let name = name.into();
        let docs = read_documents(path.as_ref())?;
        if docs.len() > 1 {
            let mut nested = Cli::new(name.clone());
            nested.load_multi_docs(docs)?;
            Ok(self.subcommand(name, nested))
        } else {
            let doc = docs.into_iter().next().unwrap_or(Value::Null);
            Ok(self.subcommand(name, Schema::from_value(&doc)?))
        }
    }

    /// Add the subcommands in a JSON file of several objects, each named
    /// by its `name`.
    pub fn subcommands_file(&mut self, path: impl AsRef<Path>) -> Result<&mut Cli, Error> {
        let docs = read_documents(path.as_ref())?;
        if docs.len() < 2 {
            return Err(Error::Usage(
                "A single-document file needs a subcommand name: use subcommand_file(name, path)"
                    .into(),
            ));
        }
        self.load_multi_docs(docs)?;
        Ok(self)
    }

    /// Set the subcommand used when the arguments name none.
    pub fn set_default_subcommand(&mut self, name: impl Into<String>) -> &mut Cli {
        self.default_subcommand = Some(name.into());
        self
    }

    /// Set the JSON key that names the subcommand when the options come as
    /// JSON on standard input (the `-@` form). It is `"subcommand"` unless
    /// set.
    pub fn set_subcommand_key(&mut self, key: impl Into<String>) -> &mut Cli {
        self.subcommand_key = key.into();
        self
    }

    /// Whether a bare `key=value` word (no dashes) may assign a property;
    /// on unless turned off. Off, such a word is a positional when a slot
    /// is open and an unexpected argument otherwise, for programs whose
    /// operands contain `=`. An open positional slot takes such a word
    /// literally either way, and `--flag=value` always works. It applies to
    /// this CLI and its schema subcommands; a nested CLI has its own
    /// setting. A colon never assigns: `key:value` is an ordinary word.
    pub fn set_bare_assignment(&mut self, enabled: bool) -> &mut Cli {
        self.bare_assignment = enabled;
        self
    }

    /// The name help and completion scripts use for the program.
    pub fn program_name(&self) -> &str {
        &self.program_name
    }

    /// The schema of a flat CLI; None for one with subcommands.
    pub fn schema(&self) -> Option<&Schema> {
        self.schema.as_ref()
    }

    /// The subcommands, in the order they were added.
    pub fn subcommands(&self) -> &IndexMap<String, Subcommand> {
        &self.subcommands
    }

    /// The subcommand used when the arguments name none, if one is set.
    pub fn default_subcommand(&self) -> Option<&str> {
        self.default_subcommand.as_deref()
    }

    /// The JSON key that names the subcommand in the `-@` form.
    pub fn subcommand_key(&self) -> &str {
        &self.subcommand_key
    }

    /// Whether a bare `key=value` word assigns a property; see
    /// [`set_bare_assignment`](Cli::set_bare_assignment).
    pub fn bare_assignment(&self) -> bool {
        self.bare_assignment
    }

    /// Register subcommands from schema documents: each has a `name`, or is
    /// a mixin with an `$id` that others include through `allOf`.
    fn load_multi_docs(&mut self, docs: Vec<Value>) -> Result<(), Error> {
        let mut registry: IndexMap<String, Map<String, Value>> = IndexMap::new();
        let mut commands = Vec::new();

        for doc in docs {
            let Value::Object(map) = doc else {
                return Err(Error::Schema(
                    "Each schema document must be an object".into(),
                ));
            };
            let id = map.get("$id").and_then(Value::as_str).map(String::from);
            let name = map.get("name").and_then(Value::as_str).map(String::from);
            match (id, name) {
                (_, Some(name)) => commands.push((name, map)),
                (Some(id), None) => {
                    registry.insert(id, map);
                }
                (None, None) => {
                    return Err(Error::Schema(
                        "Schema must have either 'name' (subcommand) or '$id' (mixin)".into(),
                    ));
                }
            }
        }

        for (name, map) in commands {
            let resolved = resolve_all_of(&map, &registry)?;
            let schema = Schema::from_value(&Value::Object(resolved))?;
            self.subcommands.insert(name, Subcommand::Schema(schema));
        }
        Ok(())
    }
}

/// A schema file's documents.
fn read_documents(path: &Path) -> Result<Vec<Value>, Error> {
    let content = std::fs::read_to_string(path)?;
    if multi_json(&content) {
        split_json_documents(&content)
    } else {
        Ok(vec![serde_json::from_str(&content).map_err(Error::syntax)?])
    }
}

/// Whether `content` holds more than one top-level JSON object.
fn multi_json(content: &str) -> bool {
    let stripped = content.trim();
    if !stripped.starts_with('{') {
        return false;
    }

    let mut count = 0;
    let mut depth = 0;
    let mut in_string = false;
    let mut escape_next = false;
    for c in stripped.chars() {
        if escape_next {
            escape_next = false;
            continue;
        }
        match c {
            '\\' => escape_next = in_string,
            '"' => in_string = !in_string,
            '{' if !in_string => {
                depth += 1;
                if depth == 1 {
                    count += 1;
                }
            }
            '}' if !in_string => depth -= 1,
            _ => {}
        }
    }
    count > 1
}

/// The JSON objects written one after another in `content`.
fn split_json_documents(content: &str) -> Result<Vec<Value>, Error> {
    let mut docs = Vec::new();
    let mut remaining = content.trim();
    while let Some(start) = remaining.find('{') {
        remaining = &remaining[start..];

        let mut depth = 0;
        let mut in_string = false;
        let mut escape_next = false;
        let mut end = remaining.len() - 1;
        for (i, c) in remaining.char_indices() {
            if escape_next {
                escape_next = false;
                continue;
            }
            match c {
                '\\' => escape_next = in_string,
                '"' => in_string = !in_string,
                '{' if !in_string => depth += 1,
                '}' if !in_string => {
                    depth -= 1;
                    if depth == 0 {
                        end = i;
                        break;
                    }
                }
                _ => {}
            }
        }

        docs.push(serde_json::from_str(&remaining[..=end]).map_err(Error::syntax)?);
        remaining = remaining[end + 1..].trim();
    }
    Ok(docs)
}

/// A subcommand's schema with its `allOf` merged in: mixins by `$ref`, and
/// inline schemas.
fn resolve_all_of(
    schema: &Map<String, Value>,
    registry: &IndexMap<String, Map<String, Value>>,
) -> Result<Map<String, Value>, Error> {
    let Some(all_of) = schema.get("allOf") else {
        return Ok(schema.clone());
    };
    let Some(all_of) = all_of.as_array() else {
        return Err(Error::Schema("'allOf' must be an array".into()));
    };

    let mut merged = Map::new();
    for item in all_of {
        let Some(item) = item.as_object() else {
            return Err(Error::Schema("Each 'allOf' item must be an object".into()));
        };
        match item.get("$ref").and_then(Value::as_str) {
            Some(reference) => {
                let mixin = registry
                    .get(reference)
                    .ok_or_else(|| Error::Schema(format!("Unknown $ref: {reference}")))?;
                merge_schema(&mut merged, mixin)?;
            }
            None => merge_schema(&mut merged, item)?,
        }
    }

    // The subcommand's own keys are one more conjunct of the allOf, so its
    // `properties` and `required` combine with the mixins'; its other keys
    // override.
    for (key, value) in schema {
        match key.as_str() {
            "allOf" => {}
            "properties" | "required" => {
                let mut own = Map::new();
                own.insert(key.clone(), value.clone());
                merge_schema(&mut merged, &own)?;
            }
            _ => {
                merged.insert(key.clone(), value.clone());
            }
        }
    }

    if merged.contains_key("properties") && !merged.contains_key("type") {
        merged.insert("type".into(), Value::from("object"));
    }
    Ok(merged)
}

/// Merge `source` into `target`: properties and required lists combine,
/// other keys keep the first value.
fn merge_schema(target: &mut Map<String, Value>, source: &Map<String, Value>) -> Result<(), Error> {
    for (key, value) in source {
        match key.as_str() {
            "$id" => {}
            "properties" => {
                let Some(props) = value.as_object() else {
                    return Err(Error::Schema("'properties' must be an object".into()));
                };
                let mut merged = match target.get("properties") {
                    Some(Value::Object(existing)) => existing.clone(),
                    _ => Map::new(),
                };
                merged.extend(props.clone());
                target.insert(key.clone(), Value::Object(merged));
            }
            "required" => {
                let Some(names) = value.as_array() else {
                    return Err(Error::Schema(
                        "'required' must be an array of strings".into(),
                    ));
                };
                let mut required = match target.get("required") {
                    Some(Value::Array(existing)) => existing.clone(),
                    _ => Vec::new(),
                };
                for name in names {
                    if !required.iter().any(|r| values::same(r, name)) {
                        required.push(name.clone());
                    }
                }
                target.insert(key.clone(), Value::Array(required));
            }
            _ => {
                if !target.contains_key(key) {
                    target.insert(key.clone(), value.clone());
                }
            }
        }
    }
    Ok(())
}

// ---- parsing --------------------------------------------------------------

impl Cli {
    /// Parse the program's command line into a [`Parsed`]: the data plus
    /// any errors. Invalid input is never a failure here; check
    /// [`Parsed::is_valid`]. An argument that isn't UTF-8 is one of the
    /// errors.
    ///
    /// # Panics
    ///
    /// When the CLI has neither a schema nor subcommands.
    pub fn parse(&self) -> Parsed {
        self.parse_with(std::env::args_os(), ParseOptions::default())
    }

    /// Parse a command line, the program's name first, as
    /// [`std::env::args_os`] gives it; the name itself is skipped.
    ///
    /// ```
    /// let cli = jargonaut::Cli::from_json(r#"{"properties": {"name": {}}}"#, "greet")?;
    /// let parsed = cli.parse_from(["greet", "--name", "World"]);
    /// assert_eq!(parsed.get("name").unwrap(), "World");
    /// # Ok::<(), jargonaut::Error>(())
    /// ```
    ///
    /// # Panics
    ///
    /// When the CLI has neither a schema nor subcommands.
    pub fn parse_from<I>(&self, args: I) -> Parsed
    where
        I: IntoIterator,
        I::Item: AsRef<OsStr>,
    {
        self.parse_with(args, ParseOptions::default())
    }

    /// [`parse_from`](Cli::parse_from) with defaults, or with standard
    /// input or the environment supplied (see [`ParseOptions`]). Pass
    /// [`std::env::args_os`] to parse the program's own command line.
    ///
    /// # Panics
    ///
    /// When the CLI has neither a schema nor subcommands.
    pub fn parse_with<I>(&self, args: I, options: ParseOptions<'_>) -> Parsed
    where
        I: IntoIterator,
        I::Item: AsRef<OsStr>,
    {
        match arguments(args) {
            Ok(args) => self.parse_arguments(&args, options),
            Err(error) => Parsed::error(error),
        }
    }

    /// Parse the words after the program's name.
    pub(crate) fn parse_arguments(&self, args: &[String], options: ParseOptions<'_>) -> Parsed {
        let process_env = |name: &str| std::env::var(name).ok();
        let mut process_stdin = std::io::stdin();
        let mut context = Context {
            globals: None,
            section: options.defaults,
            env: options.env.unwrap_or(&process_env),
            input: match options.stdin {
                Some(input) => input,
                None => &mut process_stdin,
            },
        };
        self.parse_args(args, &mut context)
    }

    /// Parse the program's command line and return just the data, or the
    /// errors when it isn't valid.
    pub fn json(&self) -> Result<Map<String, Value>, ParseError> {
        self.json_with(std::env::args_os(), ParseOptions::default())
    }

    /// [`json`](Cli::json) for a command line, the program's name first,
    /// as for [`parse_from`](Cli::parse_from).
    pub fn json_from<I>(&self, args: I) -> Result<Map<String, Value>, ParseError>
    where
        I: IntoIterator,
        I::Item: AsRef<OsStr>,
    {
        self.json_with(args, ParseOptions::default())
    }

    /// [`json_from`](Cli::json_from) with [`ParseOptions`].
    pub fn json_with<I>(
        &self,
        args: I,
        options: ParseOptions<'_>,
    ) -> Result<Map<String, Value>, ParseError>
    where
        I: IntoIterator,
        I::Item: AsRef<OsStr>,
    {
        let parsed = self.parse_with(args, options);
        if parsed.is_valid() {
            Ok(parsed.data)
        } else {
            Err(ParseError {
                errors: parsed.errors,
            })
        }
    }

    /// Validate data against the CLI's schema, or a subcommand's (its name
    /// space-separated when nested, `"remote add"`).
    pub fn validate(
        &self,
        data: &Map<String, Value>,
        subcommand: Option<&str>,
    ) -> Result<Vec<String>, Error> {
        let Some(path) = subcommand else {
            let schema = self
                .schema
                .as_ref()
                .ok_or_else(|| Error::Usage("No schema available".into()))?;
            return Ok(validate(data, schema));
        };
        let (name, rest) = match path.split_once(' ') {
            Some((name, rest)) => (name, Some(rest)),
            None => (path, None),
        };
        match self.subcommands.get(name) {
            Some(Subcommand::Cli(cli)) => cli.validate(data, rest),
            Some(Subcommand::Schema(schema)) => Ok(validate(data, schema)),
            None => Err(Error::Usage(format!("Unknown subcommand: {name}"))),
        }
    }

    /// Validate a parse's data against the schema of the subcommand it
    /// chose.
    pub fn validate_parsed(&self, parsed: &Parsed) -> Result<Vec<String>, Error> {
        self.validate(&parsed.data, parsed.subcommand.as_deref())
    }

    fn parse_args(&self, args: &[String], context: &mut Context<'_>) -> Parsed {
        if !self.subcommands.is_empty() {
            self.parse_with_subcommands(args, context)
        } else if let Some(schema) = &self.schema {
            self.parse_flat(args, schema, None, context)
        } else {
            panic!("CLI has no schema and no subcommands defined");
        }
    }

    fn parse_with_subcommands(&self, args: &[String], context: &mut Context<'_>) -> Parsed {
        // `prog -@`: all options as JSON, the subcommand among them.
        if args == [STDIN_JSON] {
            return self.parse_from_stdin_full(context);
        }

        if matches!(args.first().map(String::as_str), Some("--help" | "-h")) {
            return Parsed {
                help_requested: true,
                ..Parsed::default()
            };
        }

        if let Some(parsed) = check_completions_flag(args) {
            return parsed;
        }

        if let Some(first) = args.first()
            && let Some(name) = self.resolve_subcommand(first)
            && let Some(subcommand) = self.subcommands.get(name)
        {
            return self.dispatch_subcommand(subcommand, name, &args[1..], context);
        }

        if let Some(name) = &self.default_subcommand
            && let Some(subcommand) = self.subcommands.get(name)
        {
            return self.dispatch_subcommand(subcommand, name, args, context);
        }

        match args.first() {
            None => Parsed::error("No subcommand specified"),
            Some(first) => Parsed::error(format!("Unknown subcommand: {first}")),
        }
    }

    fn dispatch_subcommand(
        &self,
        subcommand: &Subcommand,
        name: &str,
        args: &[String],
        context: &mut Context<'_>,
    ) -> Parsed {
        let (globals, section) = self.subcommand_defaults(name, context);
        let context = &mut Context {
            globals: globals.as_deref(),
            section,
            env: context.env,
            input: &mut *context.input,
        };
        match subcommand {
            Subcommand::Cli(cli) => {
                let parsed = cli.parse_args(args, context);
                let full_name = join_names(name, parsed.subcommand.as_deref());
                let help_subcommand = parsed
                    .help_requested
                    .then(|| join_names(name, parsed.help_subcommand.as_deref()));
                Parsed {
                    data: parsed.data,
                    errors: parsed.errors,
                    subcommand: Some(full_name),
                    help_requested: parsed.help_requested,
                    help_subcommand,
                    completion_shell: None,
                }
            }
            Subcommand::Schema(schema) => {
                let parsed = self.parse_flat(args, schema, Some(name), context);
                Parsed {
                    data: parsed.data,
                    errors: parsed.errors,
                    subcommand: Some(name.to_owned()),
                    help_requested: parsed.help_requested,
                    help_subcommand: parsed.help_requested.then(|| name.to_owned()),
                    completion_shell: None,
                }
            }
        }
    }

    /// The JSON form, given an outer CLI's JSON for this one: the
    /// subcommand's name in it if this CLI has subcommands, its options if
    /// not.
    fn parse_json_scoped(&self, context: &mut Context<'_>) -> Parsed {
        if !self.subcommands.is_empty() {
            self.parse_from_stdin_full(context)
        } else if let Some(schema) = &self.schema {
            match read_stdin_json(context) {
                Ok(data) => finish_json(data, schema, context),
                Err(message) => Parsed::error(message),
            }
        } else {
            panic!("CLI has no schema and no subcommands defined");
        }
    }

    fn parse_from_stdin_full(&self, context: &mut Context<'_>) -> Parsed {
        let mut data = match read_stdin_json(context) {
            Ok(data) => data,
            Err(message) => return Parsed::error(message),
        };

        let named = data
            .shift_remove(&self.subcommand_key)
            .and_then(|v| v.as_str().map(String::from));
        let Some(name) = named.or_else(|| self.default_subcommand.clone()) else {
            return Parsed::error(format!("No '{}' specified in JSON", self.subcommand_key));
        };
        let Some((name, subcommand)) = self
            .resolve_subcommand(&name)
            .and_then(|n| self.subcommands.get_key_value(n))
        else {
            return Parsed::error(format!("Unknown subcommand: {name}"));
        };
        let (globals, section) = self.subcommand_defaults(name, context);

        match subcommand {
            Subcommand::Cli(cli) => {
                // The nested CLI reads the rest of the JSON as its own input.
                let mut nested_input = Cursor::new(Value::Object(data).to_string().into_bytes());
                let mut nested = Context {
                    globals: globals.as_deref(),
                    section,
                    env: context.env,
                    input: &mut nested_input,
                };
                let parsed = cli.parse_json_scoped(&mut nested);
                let full_name = join_names(name, parsed.subcommand.as_deref());
                Parsed {
                    subcommand: Some(full_name),
                    ..Parsed::new(parsed.data, parsed.errors)
                }
            }
            Subcommand::Schema(schema) => {
                let scoped = Context {
                    globals: globals.as_deref(),
                    section,
                    env: context.env,
                    input: &mut *context.input,
                };
                let mut parsed = finish_json(data, schema, &scoped);
                parsed.subcommand = Some(name.clone());
                parsed
            }
        }
    }

    /// Split this CLI's section of the defaults for subcommand `name`: the
    /// keys that aren't a subcommand's section are global, over the
    /// globals from further up, and `name`'s section is the subcommand's
    /// own.
    fn subcommand_defaults<'c>(
        &self,
        name: &str,
        context: &Context<'c>,
    ) -> (Defaults<'c>, Option<&'c Map<String, Value>>) {
        let Some(section) = context.section else {
            return (context.globals.map(Cow::Borrowed), None);
        };
        let own: Map<String, Value> = section
            .iter()
            .filter(|(key, value)| !(self.subcommands.contains_key(*key) && value.is_object()))
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect();
        let globals = if own.is_empty() {
            context.globals.map(Cow::Borrowed)
        } else {
            let mut globals = context.globals.cloned().unwrap_or_default();
            deep_merge(&mut globals, own);
            Some(Cow::Owned(globals))
        };
        (globals, section.get(name).and_then(Value::as_object))
    }

    fn parse_flat(
        &self,
        args: &[String],
        schema: &Schema,
        path: Option<&str>,
        context: &mut Context<'_>,
    ) -> Parsed {
        // `prog [sub] -@`: this schema's options as JSON, unless the schema
        // claims -@ for an option of its own, as it may -h.
        if args == [STDIN_JSON] && !build_short_map(schema).contains_key("@") {
            return match read_stdin_json(context) {
                Ok(data) => finish_json(data, schema, context),
                Err(message) => Parsed::error(message),
            };
        }

        if help_requested(args, schema) {
            return Parsed {
                help_requested: true,
                help_subcommand: path.map(String::from),
                ..Parsed::default()
            };
        }

        // `--completions` belongs to the program, not its subcommands.
        if path.is_none()
            && let Some(parsed) = check_completions_flag(args)
        {
            return parsed;
        }

        let mut data = Map::new();
        let mut errors = Vec::new();
        self.consume_tokens(args, schema, &mut data, &mut errors, context);
        init_empty_variadic(schema, &mut data, &mut errors);
        apply_env_vars(&mut data, &mut errors, schema, context);
        // Before defaults fill anything in, every value came from a token.
        let unconverted = unconverted_fields(&data, schema.resolve(&schema.root), schema, "");
        apply_user_defaults(&mut data, command_defaults(schema, context).as_deref());
        apply_defaults(&mut data, schema);
        errors.extend(validate_except(&data, schema, &unconverted));
        Parsed::new(data, errors)
    }

    /// Walk the argument words, filling `data` and `errors`.
    fn consume_tokens(
        &self,
        args: &[String],
        schema: &Schema,
        data: &mut Map<String, Value>,
        errors: &mut Vec<String>,
        context: &Context<'_>,
    ) {
        let short_to_long = build_short_map(schema);
        let positional = &schema.positional;
        let mut filled = 0;
        let mut options_ended = false;
        let mut i = 0;

        while i < args.len() {
            let arg = &args[i];
            if !options_ended && arg == "--" {
                // POSIX end of options: every word after it is an operand,
                // even one that looks like a flag. The marker is dropped.
                options_ended = true;
                i += 1;
            } else if !options_ended && short_flag(arg) {
                i += handle_short_flag(arg, args, i, data, errors, &short_to_long, schema, context);
            } else if !options_ended && arg.starts_with("--") {
                i += handle_long_flag(arg, args, i, data, errors, schema, context);
            } else if filled < positional.len() {
                let (consumed, slots) = handle_positional(
                    arg,
                    args,
                    i,
                    filled,
                    data,
                    errors,
                    schema,
                    options_ended,
                    context,
                );
                i += consumed;
                filled += slots;
            } else {
                i += self.handle_extra_arg(arg, data, errors, schema, context);
            }
        }
    }

    fn handle_extra_arg(
        &self,
        arg: &str,
        data: &mut Map<String, Value>,
        errors: &mut Vec<String>,
        schema: &Schema,
        context: &Context<'_>,
    ) -> usize {
        if let Some((key, value, error)) = self.parse_argument(arg, schema, context) {
            errors.extend(error);
            set_option_value(data, &key, Some(value), errors, schema);
        } else if self.bare_assignment && arg.contains('=') {
            // An assignment to a property that doesn't exist: suggest one.
            let unknown = arg.split('=').next().unwrap_or_default();
            errors.push(unknown_option_error(
                unknown,
                &available_options(schema),
                "",
                None,
            ));
        } else {
            errors.push(format!("Unexpected argument '{arg}'"));
        }
        1
    }

    /// A bare `key=value` word assigning a known property.
    fn parse_argument(
        &self,
        arg: &str,
        schema: &Schema,
        context: &Context<'_>,
    ) -> Option<(String, Value, Option<String>)> {
        if !self.bare_assignment {
            return None;
        }
        let (key, value) = arg.split_once('=')?;
        find_property(key, schema)?;
        let (coerced, error) = coerce_value(key, value, schema, context);
        Some((key.to_owned(), coerced, error))
    }

    /// The subcommand `input` names: exactly, or as an unambiguous prefix
    /// of at least 3 characters.
    pub(crate) fn resolve_subcommand(&self, input: &str) -> Option<&str> {
        if let Some((name, _)) = self.subcommands.get_key_value(input) {
            return Some(name);
        }
        if input.chars().count() < 3 {
            return None;
        }
        let mut matches = self
            .subcommands
            .keys()
            .filter(|name| name.starts_with(input));
        match (matches.next(), matches.next()) {
            (Some(name), None) => Some(name),
            _ => None,
        }
    }
}

fn join_names(name: &str, rest: Option<&str>) -> String {
    match rest {
        Some(rest) => format!("{name} {rest}"),
        None => name.to_owned(),
    }
}

fn check_completions_flag(args: &[String]) -> Option<Parsed> {
    if args.len() < 2 || args[0] != "--completions" {
        return None;
    }
    Some(match args[1].parse::<Shell>() {
        Ok(shell) => Parsed {
            completion_shell: Some(shell),
            ..Parsed::default()
        },
        Err(error) => Parsed::error(error.to_string()),
    })
}

/// Standard input as a JSON object; anything else that is valid JSON is
/// an empty object.
fn read_stdin_json(context: &mut Context<'_>) -> Result<Map<String, Value>, String> {
    let mut text = String::new();
    if let Err(error) = context.input.read_to_string(&mut text) {
        return Err(format!("Invalid JSON from stdin: {error}"));
    }
    match serde_json::from_str(&text) {
        Ok(Value::Object(map)) => Ok(map),
        Ok(_) => Ok(Map::new()),
        Err(error) => Err(format!("Invalid JSON from stdin: {error}")),
    }
}

/// Fill in and validate options that came as JSON.
fn finish_json(mut data: Map<String, Value>, schema: &Schema, context: &Context<'_>) -> Parsed {
    let mut errors = Vec::new();
    apply_env_vars(&mut data, &mut errors, schema, context);
    apply_user_defaults(&mut data, command_defaults(schema, context).as_deref());
    apply_defaults(&mut data, schema);
    errors.extend(validate(&data, schema));
    Parsed::new(data, errors)
}

/// The defaults a command takes: the global ones it has an option for,
/// then its section's, all of them, as a CLI without subcommands takes
/// all of its defaults.
fn command_defaults<'c>(schema: &Schema, context: &Context<'c>) -> Defaults<'c> {
    let properties = schema.properties();
    let mut defaults: Map<String, Value> = context
        .globals
        .into_iter()
        .flatten()
        .filter(|(key, _)| properties.is_some_and(|props| props.contains_key(*key)))
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect();
    if defaults.is_empty() {
        return context.section.map(Cow::Borrowed);
    }
    if let Some(section) = context.section {
        deep_merge(&mut defaults, section.clone());
    }
    Some(Cow::Owned(defaults))
}

/// Whether the arguments ask for the built-in help. A schema may claim
/// `--help` (a `help` property) or `-h` (a property with short `h`) for
/// itself, and then the flag is its own. A POSIX `--` ends the search:
/// what follows is an operand.
fn help_requested(args: &[String], schema: &Schema) -> bool {
    let props = schema.properties();
    for arg in args {
        match arg.as_str() {
            "--" => break,
            "--help" => return !props.is_some_and(|props| props.contains_key("help")),
            "-h" => {
                return !props
                    .is_some_and(|props| props.values().any(|p| p.short.as_deref() == Some("h")));
            }
            _ => {}
        }
    }
    false
}

/// A word that is one or more short flags: `-v`, `-abc`. A negative number
/// (`-5`, `-3.14`) is a value instead.
fn short_flag(arg: &str) -> bool {
    let mut chars = arg.chars();
    chars.next() == Some('-')
        && chars
            .next()
            .is_some_and(|c| c != '-' && !c.is_ascii_digit())
}

/// Alone on a command line, or after the subcommand, it reads options as JSON
/// from stdin (`prog mark -@ < options.json`).
const STDIN_JSON: &str = "-@";

/// A word that would end a flag's value or a variadic list: a leading dash,
/// except on a negative number or a lone `-`, which by convention names
/// standard input or output as a file.
fn flag_like(arg: &str) -> bool {
    let mut chars = arg.chars();
    chars.next() == Some('-') && chars.next().is_some_and(|c| !c.is_ascii_digit())
}

fn boolean_value(arg: &str) -> bool {
    matches!(
        arg.to_lowercase().as_str(),
        "true" | "false" | "yes" | "no" | "on" | "off" | "1" | "0"
    )
}

#[allow(clippy::too_many_arguments)]
fn handle_short_flag(
    arg: &str,
    args: &[String],
    i: usize,
    data: &mut Map<String, Value>,
    errors: &mut Vec<String>,
    short_to_long: &IndexMap<String, String>,
    schema: &Schema,
    context: &Context<'_>,
) -> usize {
    if arg.chars().count() == 2 {
        handle_single_short_flag(
            &arg[1..],
            args,
            i,
            data,
            errors,
            short_to_long,
            schema,
            context,
        )
    } else {
        handle_combined_short_flags(arg, args, i, data, errors, short_to_long, schema, context)
    }
}

#[allow(clippy::too_many_arguments)]
fn handle_single_short_flag(
    short: &str,
    args: &[String],
    i: usize,
    data: &mut Map<String, Value>,
    errors: &mut Vec<String>,
    short_to_long: &IndexMap<String, String>,
    schema: &Schema,
    context: &Context<'_>,
) -> usize {
    match short_to_long.get(short) {
        Some(long) => {
            let (key, value, consumed, error) =
                parse_long_flag(&format!("--{long}"), args, i, schema, context);
            if let Some(key) = key {
                errors.extend(error);
                set_option_value(data, &key, value, errors, schema);
            }
            consumed
        }
        None if short == "@" => {
            errors.push(format!(
                "'{STDIN_JSON}' reads options as JSON from stdin, and takes no other arguments"
            ));
            1
        }
        None => {
            let shorts: Vec<String> = short_to_long.keys().cloned().collect();
            errors.push(unknown_option_error(short, &shorts, "-", None));
            1
        }
    }
}

/// A cluster of short flags, read as getopt reads one: each letter is a flag
/// until one that takes a value, which takes the rest of the cluster (`-w80`,
/// `-lw80`), or the next argument when it ends the cluster (`-lw 80`).
/// Returns how many words it took.
#[allow(clippy::too_many_arguments)]
fn handle_combined_short_flags(
    arg: &str,
    args: &[String],
    i: usize,
    data: &mut Map<String, Value>,
    errors: &mut Vec<String>,
    short_to_long: &IndexMap<String, String>,
    schema: &Schema,
    context: &Context<'_>,
) -> usize {
    for (n, c) in arg.char_indices().skip(1) {
        let short = c.to_string();
        let Some(long) = short_to_long.get(&short) else {
            let shorts: Vec<String> = short_to_long.keys().cloned().collect();
            let context = format!("in '{arg}'");
            errors.push(unknown_option_error(&short, &shorts, "-", Some(&context)));
            return 1;
        };
        if boolean_property(long, schema) {
            set_nested_value(data, long, Some(Value::Bool(true)), errors);
            continue;
        }

        let rest = &arg[n + c.len_utf8()..];
        if rest.is_empty() {
            return handle_single_short_flag(
                &short,
                args,
                i,
                data,
                errors,
                short_to_long,
                schema,
                context,
            );
        }
        let (coerced, error) = coerce_value(long, rest, schema, context);
        errors.extend(error);
        set_option_value(data, long, Some(coerced), errors, schema);
        return 1;
    }
    1
}

fn handle_long_flag(
    arg: &str,
    args: &[String],
    i: usize,
    data: &mut Map<String, Value>,
    errors: &mut Vec<String>,
    schema: &Schema,
    context: &Context<'_>,
) -> usize {
    let (key, value, consumed, error) = parse_long_flag(arg, args, i, schema, context);
    match key {
        Some(key) => {
            errors.extend(error);
            set_option_value(data, &key, value, errors, schema);
        }
        None => {
            let name = &arg.split('=').next().unwrap_or_default()[2..];
            errors.push(unknown_option_error(
                name,
                &available_options(schema),
                "--",
                None,
            ));
        }
    }
    consumed
}

/// A long flag and its value: the key (None for an unknown option), the
/// value (None when it is missing), how many words it took, and a
/// coercion or missing-value error.
fn parse_long_flag(
    arg: &str,
    args: &[String],
    index: usize,
    schema: &Schema,
    context: &Context<'_>,
) -> (Option<String>, Option<Value>, usize, Option<String>) {
    let key = &arg[2..];
    let base_key = key.split('=').next().unwrap_or_default();
    if find_property(base_key, schema).is_none() {
        return (None, None, 1, None);
    }

    let next = args.get(index + 1);
    if let Some((name, value)) = key.split_once('=') {
        let (coerced, error) = coerce_value(name, value, schema, context);
        (Some(name.to_owned()), Some(coerced), 1, error)
    } else if boolean_property(key, schema) {
        // `--flag true`: a boolean takes the next word only if it is one.
        match next {
            Some(next) if boolean_value(next) => {
                let (coerced, error) = coerce_value(key, next, schema, context);
                (Some(key.to_owned()), Some(coerced), 2, error)
            }
            _ => (Some(key.to_owned()), Some(Value::Bool(true)), 1, None),
        }
    } else {
        match next {
            // A negative number is a value, not a flag.
            Some(next) if !flag_like(next) => {
                let (coerced, error) = coerce_value(key, next, schema, context);
                (Some(key.to_owned()), Some(coerced), 2, error)
            }
            _ => (
                Some(key.to_owned()),
                None,
                1,
                Some(format!("Missing value for --{key}")),
            ),
        }
    }
}

/// Returns how many words the positional took, and how many slots it
/// filled.
#[allow(clippy::too_many_arguments)]
fn handle_positional(
    arg: &str,
    args: &[String],
    i: usize,
    slot: usize,
    data: &mut Map<String, Value>,
    errors: &mut Vec<String>,
    schema: &Schema,
    options_ended: bool,
    context: &Context<'_>,
) -> (usize, usize) {
    let names = &schema.positional;
    let key = &names[slot];
    if schema.variadic.as_ref() != Some(key) {
        set_positional(key, arg, data, errors, schema, context);
        return (1, 1);
    }

    // The variadic takes a run of words, and the positionals after it take
    // the run's last words, one each (`cp a b dir/`).
    let (consumed, words) = collect_run(args, i, options_ended);
    let after = &names[slot + 1..];
    let count = after.len().min(words.len());
    let (own, rest) = words.split_at(words.len() - count);
    set_variadic(key, own, data, errors, schema);
    for (name, word) in after.iter().zip(rest) {
        set_positional(name, word, data, errors, schema, context);
    }
    (consumed, 1 + after.len())
}

fn set_positional(
    key: &str,
    word: &str,
    data: &mut Map<String, Value>,
    errors: &mut Vec<String>,
    schema: &Schema,
    context: &Context<'_>,
) {
    let (coerced, error) = coerce_value(key, word, schema, context);
    errors.extend(error);
    set_nested_value(data, key, Some(coerced), errors);
}

/// The words a variadic takes from `start`, and how many words they span.
/// The run is greedy: it takes every word literally, `:` and `=` and
/// negative numbers included, and stops only at a real flag, which is then
/// parsed as one. A `--` in the run is dropped, and after `--` it takes
/// flags too.
fn collect_run(args: &[String], start: usize, mut passthrough: bool) -> (usize, Vec<String>) {
    let mut words = Vec::new();
    let mut i = start;
    while i < args.len() {
        let arg = &args[i];
        if !passthrough && arg == "--" {
            passthrough = true;
        } else if !passthrough && flag_like(arg) {
            break;
        } else {
            words.push(arg.clone());
        }
        i += 1;
    }
    (i - start, words)
}

fn set_variadic(
    key: &str,
    words: &[String],
    data: &mut Map<String, Value>,
    errors: &mut Vec<String>,
    schema: &Schema,
) {
    let prop = find_property(key, schema);
    let item_type = prop.and_then(|p| p.items.as_ref()).map(|items| items.kind);
    let mut items = Vec::new();
    for word in words {
        for piece in array_pieces(word, prop) {
            let (coerced, error) = coerce_scalar(key, &piece, item_type);
            errors.extend(error);
            items.push(coerced);
        }
    }
    set_nested_value(data, key, Some(Value::Array(items)), errors);
}

/// The items a word gives an array: the word itself, or its pieces when the
/// property names separators.
fn array_pieces(value: &str, prop: Option<&Property>) -> Vec<String> {
    match prop.and_then(|p| p.separator.as_deref()) {
        Some(separators) => schema::split(value, separators),
        None => vec![value.to_owned()],
    }
}

/// A variadic that isn't given is an empty list.
fn init_empty_variadic(schema: &Schema, data: &mut Map<String, Value>, errors: &mut Vec<String>) {
    if let Some(key) = &schema.variadic
        && nested_value(data, key).is_none()
    {
        set_nested_value(data, key, Some(Value::Array(Vec::new())), errors);
    }
}

fn build_short_map(schema: &Schema) -> IndexMap<String, String> {
    let mut map = IndexMap::new();
    for (name, prop) in schema.properties().into_iter().flatten() {
        if let Some(short) = &prop.short {
            map.insert(short.clone(), name.clone());
        }
    }
    map
}

fn available_options(schema: &Schema) -> Vec<String> {
    schema
        .properties()
        .map(|props| props.keys().cloned().collect())
        .unwrap_or_default()
}

fn boolean_property(key: &str, schema: &Schema) -> bool {
    find_property(key, schema).is_some_and(|p| p.kind == Type::Boolean)
}

/// A property by dotted path (`server.port`); see `Schema::find_property`.
fn find_property<'a>(key: &str, schema: &'a Schema) -> Option<&'a Property> {
    schema.find_property(key)
}

/// The fields whose value didn't convert to the field's type. A token is a
/// string, and converting one leaves a string in a number or boolean field
/// only when it fails, which it reports; validating the value as well
/// would report the same mistake again. So this runs on values that came
/// from tokens alone, before defaults are filled in.
fn unconverted_fields(
    data: &Map<String, Value>,
    parent: &Property,
    schema: &Schema,
    prefix: &str,
) -> HashSet<String> {
    let mut fields = HashSet::new();
    let Some(props) = &parent.properties else {
        return fields;
    };
    for (name, value) in data {
        let Some(prop) = props.get(name).map(|prop| schema.resolve(prop)) else {
            continue;
        };
        let path = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{prefix}.{name}")
        };
        let unconverted = match value {
            Value::String(_) => converted_type(prop.kind),
            Value::Array(items) => {
                prop.kind == Type::Array
                    && prop
                        .items
                        .as_deref()
                        .is_some_and(|item| converted_type(schema.resolve(item).kind))
                    && items.iter().any(Value::is_string)
            }
            Value::Object(nested) => {
                fields.extend(unconverted_fields(nested, prop, schema, &path));
                false
            }
            _ => false,
        };
        if unconverted {
            fields.insert(path);
        }
    }
    fields
}

fn converted_type(kind: Type) -> bool {
    matches!(kind, Type::Integer | Type::Number | Type::Boolean)
}

/// A word as the value of the property at `key`: numbers and booleans
/// coerced, a list split at its separators, `~` expanded in a
/// `format: path` string. A word that doesn't fit stays a string, with an
/// error.
fn coerce_value(
    key: &str,
    value: &str,
    schema: &Schema,
    context: &Context<'_>,
) -> (Value, Option<String>) {
    let prop = find_property(key, schema);
    match prop.map(|p| p.kind) {
        Some(kind @ (Type::Integer | Type::Number | Type::Boolean)) => {
            coerce_scalar(key, value, Some(kind))
        }
        Some(Type::Array) => {
            // The value is one item, or several when the property names
            // separators. Each is coerced to the `items` type, as a variadic
            // positional's are; the first that doesn't fit is reported.
            let item_type = prop.and_then(|p| p.items.as_ref()).map(|items| items.kind);
            let mut first_error = None;
            let items = array_pieces(value, prop)
                .into_iter()
                .map(|item| {
                    let (coerced, error) = coerce_scalar(key, &item, item_type);
                    first_error = first_error.take().or(error);
                    coerced
                })
                .collect();
            (Value::Array(items), first_error)
        }
        _ if prop.is_some_and(|p| p.format.as_deref() == Some("path")) => {
            (Value::String(expand_tilde(value, context)), None)
        }
        _ => (Value::String(value.to_owned()), None),
    }
}

/// A word as an integer, number, or boolean; any other type keeps it a
/// string. A word that doesn't fit stays a string, with an error.
fn coerce_scalar(key: &str, value: &str, kind: Option<Type>) -> (Value, Option<String>) {
    let unchanged = || Value::String(value.to_owned());
    match kind {
        Some(Type::Integer) => match values::parse_int(value) {
            Some(n) => (Value::from(n), None),
            None => (
                unchanged(),
                Some(format!("Invalid integer value '{value}' for {key}")),
            ),
        },
        Some(Type::Number) => match values::parse_float(value) {
            Some(f) => (Value::from(f), None),
            None => (
                unchanged(),
                Some(format!("Invalid number value '{value}' for {key}")),
            ),
        },
        Some(Type::Boolean) => match value.to_lowercase().as_str() {
            "true" | "1" | "yes" | "on" => (Value::Bool(true), None),
            "false" | "0" | "no" | "off" => (Value::Bool(false), None),
            _ => (
                unchanged(),
                Some(format!(
                    "Invalid boolean value '{value}' for {key}. Use: true/false, yes/no, on/off, 1/0"
                )),
            ),
        },
        _ => (unchanged(), None),
    }
}

fn expand_tilde(path: &str, context: &Context<'_>) -> String {
    let home = || {
        (context.env)("HOME")
            .or_else(|| std::env::home_dir().map(|p| p.to_string_lossy().into_owned()))
    };
    if path == "~" {
        home().unwrap_or_else(|| path.to_owned())
    } else if let Some(rest) = path.strip_prefix("~/") {
        home().map_or_else(|| path.to_owned(), |home| format!("{home}/{rest}"))
    } else {
        path.to_owned()
    }
}

/// Store an option's value. An array option given more than once collects
/// the items of each (`-I a -I b`), as getopt programs do; any other option
/// keeps the last value given.
fn set_option_value(
    data: &mut Map<String, Value>,
    key: &str,
    value: Option<Value>,
    errors: &mut Vec<String>,
    schema: &Schema,
) {
    let value = match (value, nested_value(data, key)) {
        (Some(Value::Array(items)), Some(Value::Array(earlier)))
            if find_property(key, schema).is_some_and(|p| p.kind == Type::Array) =>
        {
            Some(Value::Array(earlier.iter().cloned().chain(items).collect()))
        }
        (value, _) => value,
    };
    set_nested_value(data, key, value, errors);
}

fn nested_value<'a>(data: &'a Map<String, Value>, key: &str) -> Option<&'a Value> {
    let parts: Vec<&str> = key.split('.').collect();
    let (last, path) = parts.split_last().expect("split yields at least one part");
    let mut current = data;
    for part in path {
        current = current.get(*part)?.as_object()?;
    }
    current.get(*last)
}

/// Store a value under a dotted key, making the objects on the way
/// (`server.port` makes `{"server": {"port": ...}}`). None stores nothing;
/// a value that isn't an object in the way is an error.
fn set_nested_value(
    data: &mut Map<String, Value>,
    key: &str,
    value: Option<Value>,
    errors: &mut Vec<String>,
) {
    let Some(value) = value else {
        return;
    };
    let parts: Vec<&str> = key.split('.').collect();
    let (last, path) = parts.split_last().expect("split yields at least one part");

    let mut current = data;
    for part in path {
        let entry = current
            .entry(*part)
            .or_insert_with(|| Value::Object(Map::new()));
        match entry {
            Value::Object(map) => current = map,
            _ => {
                errors.push(format!(
                    "Cannot set nested property {key}: {part} is not an object"
                ));
                return;
            }
        }
    }
    current.insert((*last).to_owned(), value);
}

fn apply_env_vars(
    data: &mut Map<String, Value>,
    errors: &mut Vec<String>,
    schema: &Schema,
    context: &Context<'_>,
) {
    for (name, prop) in schema.properties().into_iter().flatten() {
        // An argument wins over the environment.
        if data.contains_key(name) {
            continue;
        }
        let Some(var) = &schema.resolve(prop).env else {
            continue;
        };
        let Some(value) = (context.env)(var) else {
            continue;
        };
        let (coerced, error) = coerce_value(name, &value, schema, context);
        errors.extend(error);
        data.insert(name.clone(), coerced);
    }
}

/// Fill the keys missing from `data`, recursing into objects on both sides
/// so `--server.port 1` keeps a default `server.host`.
fn apply_user_defaults(data: &mut Map<String, Value>, defaults: Option<&Map<String, Value>>) {
    let Some(defaults) = defaults else {
        return;
    };
    for (key, value) in defaults {
        match data.get_mut(key) {
            Some(Value::Object(existing)) => {
                if let Value::Object(nested) = value {
                    apply_user_defaults(existing, Some(nested));
                }
            }
            Some(_) => {}
            None => {
                data.insert(key.clone(), value.clone());
            }
        }
    }
}

fn apply_defaults(data: &mut Map<String, Value>, schema: &Schema) {
    for (name, prop) in schema.properties().into_iter().flatten() {
        apply_property_defaults(data, name, prop, schema, &HashSet::new());
    }
}

/// `expanding` holds the `$ref`s descended through on this path. A default
/// on a ref already in it belongs to a recursive (tree-shaped) schema and
/// would nest copies of itself forever, so it is skipped; data the user
/// supplied is still descended, which ends with the data.
fn apply_property_defaults(
    data: &mut Map<String, Value>,
    name: &str,
    prop: &Property,
    schema: &Schema,
    expanding: &HashSet<String>,
) {
    let reference = prop.reference.as_deref();
    let recursive = reference.is_some_and(|r| expanding.contains(r));
    let prop = schema.resolve(prop);

    if !data.contains_key(name)
        && !recursive
        && let Some(default) = &prop.default
    {
        data.insert(name.to_owned(), default.clone());
    }

    if prop.kind == Type::Object
        && let Some(nested_props) = &prop.properties
        && let Some(Value::Object(nested_data)) = data.get_mut(name)
    {
        let inner = match reference {
            Some(r) => {
                let mut inner = expanding.clone();
                inner.insert(r.to_owned());
                Cow::Owned(inner)
            }
            None => Cow::Borrowed(expanding),
        };
        for (nested_name, nested_prop) in nested_props {
            apply_property_defaults(nested_data, nested_name, nested_prop, schema, &inner);
        }
    }
}

/// An unknown option's message, with a suggestion when one is close.
fn unknown_option_error(
    input: &str,
    candidates: &[String],
    prefix: &str,
    context: Option<&str>,
) -> String {
    let display = format!("{prefix}{input}");
    let context = context.map(|c| format!(" {c}")).unwrap_or_default();

    if candidates.is_empty() {
        let kind = if prefix == "-" {
            "short flags"
        } else {
            "options"
        };
        format!("Unknown option '{display}'{context}: no {kind} defined")
    } else if let Some(suggestion) = find_suggestion(input, candidates) {
        format!("Unknown option '{display}'{context}. Did you mean '{prefix}{suggestion}'?")
    } else {
        let formatted: Vec<String> = candidates.iter().map(|c| format!("{prefix}{c}")).collect();
        format!(
            "Unknown option '{display}'{context}. Available: {}",
            formatted.join(", ")
        )
    }
}

/// The closest candidate to a mistyped name: at most 2 edits away, and
/// within 30% of the longer name's length. None for one-character input.
fn find_suggestion<'a>(input: &str, candidates: &'a [String]) -> Option<&'a str> {
    let input_len = input.chars().count();
    if input_len < 2 {
        return None;
    }

    let mut best: Option<(&str, usize)> = None;
    for candidate in candidates {
        let candidate_len = candidate.chars().count();
        // The distance is at least the difference in length, so skip
        // candidates that can't qualify before comparing them.
        if input_len.abs_diff(candidate_len) > 2 {
            continue;
        }
        let distance = levenshtein(input, candidate);
        let max_allowed = (input_len.max(candidate_len) as f64 * 0.3) as usize;
        if distance <= 2 && distance <= max_allowed && best.is_none_or(|(_, d)| distance < d) {
            best = Some((candidate, distance));
        }
    }
    best.map(|(candidate, _)| candidate)
}

fn levenshtein(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut previous: Vec<usize> = (0..=b.len()).collect();
    let mut current = vec![0; b.len() + 1];
    for (i, ca) in a.chars().enumerate() {
        current[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let cost = usize::from(ca != *cb);
            current[j + 1] = (current[j] + 1)
                .min(previous[j + 1] + 1)
                .min(previous[j] + cost);
        }
        std::mem::swap(&mut previous, &mut current);
    }
    previous[b.len()]
}
