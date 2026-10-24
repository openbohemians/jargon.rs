//! Help text, generated from the schemas.

use std::borrow::Cow;
use std::collections::HashSet;

use serde_json::Value;

use crate::schema::{Property, Schema, Type};
use crate::values;
use crate::{Cli, Error, Subcommand};

impl Cli {
    /// The program's help: its commands when it has subcommands, its
    /// options for a flat CLI, or a bare usage line otherwise.
    pub fn help(&self) -> String {
        self.help_as(&self.program_name)
    }

    /// One subcommand's help; a nested one is named with spaces
    /// (`"remote add"`). A subcommand the CLI doesn't have is an
    /// [`Error::Usage`], as it is for [`validate`](Cli::validate).
    pub fn help_for(&self, subcommand: &str) -> Result<String, Error> {
        self.subcommand_help_as(&self.program_name, subcommand)
    }

    /// The help, with `command` (the program's name, then the subcommands
    /// leading to this CLI when it is nested) in its usage lines.
    fn help_as(&self, command: &str) -> String {
        if !self.subcommands.is_empty() {
            self.help_with_subcommands(command)
        } else if let Some(schema) = &self.schema {
            help_text(schema, command)
        } else {
            format!("Usage: {command} <command> [options]")
        }
    }

    fn subcommand_help_as(&self, command: &str, subcommand: &str) -> Result<String, Error> {
        let (name, rest) = match subcommand.split_once(' ') {
            Some((name, rest)) => (name, Some(rest)),
            None => (subcommand, None),
        };
        let command = format!("{command} {name}");
        match self.subcommands.get(name) {
            Some(Subcommand::Cli(cli)) => match rest {
                Some(rest) => cli.subcommand_help_as(&command, rest),
                None => Ok(cli.help_as(&command)),
            },
            Some(Subcommand::Schema(schema)) => Ok(help_text(schema, &command)),
            None => Err(Error::Usage(format!("Unknown subcommand: {name}"))),
        }
    }

    fn help_with_subcommands(&self, command: &str) -> String {
        let mut lines = vec![
            format!("Usage: {command} <command> [options]"),
            String::new(),
            "Commands:".into(),
        ];

        let width = name_width(self.subcommands.keys());
        for (name, subcommand) in &self.subcommands {
            match subcommand {
                Subcommand::Schema(schema) => help_entry(
                    &mut lines,
                    &format!("  {name}"),
                    width + 4,
                    schema.root.description.as_deref(),
                ),
                Subcommand::Cli(cli) => {
                    // A nested CLI is a group with no description of its
                    // own: its commands are listed under it, aligned among
                    // themselves.
                    help_entry(&mut lines, &format!("  {name}"), width + 4, None);
                    let sub_width = name_width(cli.subcommands.keys());
                    for (sub_name, sub) in &cli.subcommands {
                        let description = match sub {
                            Subcommand::Schema(schema) => schema.root.description.as_deref(),
                            Subcommand::Cli(_) => None,
                        };
                        help_entry(
                            &mut lines,
                            &format!("    {sub_name}"),
                            sub_width + 6,
                            description,
                        );
                    }
                }
            }
        }

        lines.push(String::new());
        lines.push(format!(
            "Run '{command} <command> --help' for command-specific options."
        ));
        lines.join("\n")
    }
}

/// A schema's help, with `command` in its usage line.
fn help_text(schema: &Schema, command: &str) -> String {
    let positional = &schema.positional;
    let root = schema.resolve(&schema.root);
    let props = root.properties.as_ref();
    let mut lines = Vec::new();

    let mut usage = vec![format!("Usage: {command}")];
    for name in positional {
        let required = props
            .and_then(|props| props.get(name))
            .is_none_or(|prop| prop.required);
        usage.push(if required {
            format!("<{name}>")
        } else {
            format!("[{name}]")
        });
    }
    usage.push("[options]".into());
    lines.push(usage.join(" "));
    lines.push(String::new());

    if !positional.is_empty() {
        // Whether an argument is required shows in the usage line.
        lines.push("Arguments:".into());
        let entries: Vec<_> = positional
            .iter()
            .filter_map(|name| {
                let prop = schema.resolve(props?.get(name)?);
                Some((name.clone(), help_text_for(prop, false)))
            })
            .collect();
        help_columns(&mut lines, &entries);
        lines.push(String::new());
    }

    lines.push("Options:".into());
    let mut entries = Vec::new();
    for (name, prop) in props.into_iter().flatten() {
        if !positional.contains(name) {
            option_entries(&mut entries, name, prop, "", schema, &HashSet::new());
        }
    }
    help_columns(&mut lines, &entries);
    lines.join("\n")
}

fn name_width<'a>(names: impl Iterator<Item = &'a String>) -> usize {
    names.map(|name| name.chars().count()).max().unwrap_or(0)
}

/// An option's entry, then its nested options'. `expanding` holds the
/// `$ref`s descended through on this path: a ref already in it is
/// recursive (a tree-shaped schema), so it is listed but not expanded
/// again, which would never end.
fn option_entries(
    entries: &mut Vec<(String, String)>,
    name: &str,
    prop: &Property,
    prefix: &str,
    schema: &Schema,
    expanding: &HashSet<String>,
) {
    let reference = prop.reference.as_deref();
    let prop = schema.resolve(prop);
    let full_name = if prefix.is_empty() {
        name.to_owned()
    } else {
        format!("{prefix}.{name}")
    };

    entries.push((help_flag(&full_name, prop), help_text_for(prop, true)));
    if reference.is_some_and(|r| expanding.contains(r)) {
        return;
    }

    if prop.kind == Type::Object
        && let Some(nested) = &prop.properties
    {
        let inner = match reference {
            Some(r) => {
                let mut inner = expanding.clone();
                inner.insert(r.to_owned());
                Cow::Owned(inner)
            }
            None => Cow::Borrowed(expanding),
        };
        for (nested_name, nested_prop) in nested {
            option_entries(
                entries,
                nested_name,
                nested_prop,
                &full_name,
                schema,
                &inner,
            );
        }
    }
}

/// An option's flag and, unless it's a boolean, its value's type.
fn help_flag(full_name: &str, prop: &Property) -> String {
    let flag = match &prop.short {
        Some(short) => format!("-{short}, --{full_name}"),
        None => format!("    --{full_name}"),
    };
    if prop.kind == Type::Boolean {
        flag
    } else {
        format!("{flag}=<{}>", prop.kind.name())
    }
}

/// What help says about an argument or option: its description, the values
/// an enum allows, whether it's required (when `required`), and its default.
fn help_text_for(prop: &Property, required: bool) -> String {
    let mut parts = Vec::new();
    if let Some(description) = prop.description.as_deref().map(str::trim_end)
        && !description.is_empty()
    {
        parts.push(description.to_owned());
    }
    if let Some(values) = prop.enum_values.as_ref().filter(|v| !v.is_empty()) {
        let values: Vec<_> = values.iter().map(help_value).collect();
        parts.push(format!("[{}]", values.join("|")));
    }
    if required && prop.required {
        parts.push("(required)".into());
    }
    if let Some(default) = &prop.default {
        parts.push(format!("[default: {}]", help_value(default)));
    }
    parts.join(" ")
}

/// A string as itself, anything else as JSON (`[1, 2]`, `null`). An empty
/// string is `""`, so that it shows.
fn help_value(value: &Value) -> String {
    match value {
        Value::String(s) if s.is_empty() => values::to_json(value),
        other => values::display(other),
    }
}

/// The furthest into a line that the second column of `Arguments:` and
/// `Options:` starts.
const HELP_COLUMN_LIMIT: usize = 32;

/// Lays out `(name, text)` entries in two columns: each text starts two
/// spaces past the widest name, or at [`HELP_COLUMN_LIMIT`] if that is
/// nearer.
fn help_columns(lines: &mut Vec<String>, entries: &[(String, String)]) {
    let column = (name_width(entries.iter().map(|(name, _)| name)) + 4).min(HELP_COLUMN_LIMIT);
    for (name, text) in entries {
        help_entry(lines, &format!("  {name}"), column, Some(text));
    }
}

/// Adds one listing: `entry`, then `text` from `column` on, or on the next
/// line if `entry` is too wide to leave two spaces before the column. A
/// text of several lines has each one at the column.
fn help_entry(lines: &mut Vec<String>, entry: &str, column: usize, text: Option<&str>) {
    let text = text.map(str::trim_end).unwrap_or_default();
    if text.is_empty() {
        lines.push(entry.to_owned());
        return;
    }

    let indent = " ".repeat(column);
    let mut text_lines = text.split('\n');
    let first = text_lines.next().unwrap_or_default();
    if entry.chars().count() + 2 <= column {
        lines.push(format!("{entry:<column$}{first}").trim_end().to_owned());
    } else {
        lines.push(entry.to_owned());
        lines.push(format!("{indent}{first}").trim_end().to_owned());
    }
    for line in text_lines {
        lines.push(format!("{indent}{line}").trim_end().to_owned());
    }
}
