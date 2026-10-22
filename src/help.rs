//! Help text, generated from the schemas.

use std::borrow::Cow;
use std::collections::HashSet;

use crate::schema::{Property, Schema, Type};
use crate::values;
use crate::{Cli, Subcommand};

impl Cli {
    /// The program's help: its commands when it has subcommands, its
    /// options for a flat CLI, or a bare usage line otherwise.
    pub fn help(&self) -> String {
        if !self.subcommands.is_empty() {
            self.help_with_subcommands()
        } else if let Some(schema) = &self.schema {
            self.help_text(schema, None)
        } else {
            format!("Usage: {} <command> [options]", self.program_name)
        }
    }

    /// One subcommand's help; a nested one is named with spaces
    /// (`"remote add"`).
    pub fn help_for(&self, subcommand: &str) -> String {
        let (name, rest) = match subcommand.split_once(' ') {
            Some((name, rest)) => (name, Some(rest)),
            None => (subcommand, None),
        };
        match self.subcommands.get(name) {
            Some(Subcommand::Cli(cli)) => match rest {
                Some(rest) => cli.help_for(rest),
                None => cli.help(),
            },
            Some(Subcommand::Schema(schema)) => self.help_text(schema, Some(name)),
            None => format!("Unknown subcommand: {name}"),
        }
    }

    fn help_with_subcommands(&self) -> String {
        let mut lines = vec![
            format!("Usage: {} <command> [options]", self.program_name),
            String::new(),
            "Commands:".into(),
        ];

        let width = name_width(self.subcommands.keys());
        for (name, subcommand) in &self.subcommands {
            match subcommand {
                Subcommand::Schema(schema) => lines.push(command_line(
                    name,
                    schema.root.description.as_deref(),
                    2,
                    width,
                )),
                Subcommand::Cli(cli) => {
                    // A nested CLI is a group with no description of its
                    // own: its commands are listed under it, aligned among
                    // themselves.
                    lines.push(command_line(name, None, 2, width));
                    let sub_width = name_width(cli.subcommands.keys());
                    for (sub_name, sub) in &cli.subcommands {
                        let description = match sub {
                            Subcommand::Schema(schema) => schema.root.description.as_deref(),
                            Subcommand::Cli(_) => None,
                        };
                        lines.push(command_line(sub_name, description, 4, sub_width));
                    }
                }
            }
        }

        lines.push(String::new());
        lines.push(format!(
            "Run '{} <command> --help' for command-specific options.",
            self.program_name
        ));
        lines.join("\n")
    }

    fn help_text(&self, schema: &Schema, subcommand: Option<&str>) -> String {
        let positional = &schema.positional;
        let root = schema.resolve(&schema.root);
        let props = root.properties.as_ref();
        let mut lines = Vec::new();

        let mut usage = match subcommand {
            Some(name) => vec![format!("Usage: {} {name}", self.program_name)],
            None => vec![format!("Usage: {}", self.program_name)],
        };
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
            lines.push("Arguments:".into());
            for name in positional {
                if let Some(prop) = props.and_then(|props| props.get(name)) {
                    lines.push(format!(
                        "  {name}    {}",
                        prop.description.as_deref().unwrap_or_default()
                    ));
                }
            }
            lines.push(String::new());
        }

        lines.push("Options:".into());
        for (name, prop) in props.into_iter().flatten() {
            if !positional.contains(name) {
                option_lines(&mut lines, name, prop, "", schema, &HashSet::new());
            }
        }
        lines.join("\n")
    }
}

fn name_width<'a>(names: impl Iterator<Item = &'a String>) -> usize {
    names.map(|name| name.chars().count()).max().unwrap_or(0)
}

/// One command's listing: its name padded to `width`, then its description
/// when it has one.
fn command_line(name: &str, description: Option<&str>, indent: usize, width: usize) -> String {
    let entry = format!("{}{name:<width$}", " ".repeat(indent));
    match description {
        Some(description) => format!("{entry}  {description}"),
        None => entry.trim_end().to_owned(),
    }
}

/// An option's lines, then its nested options'. `expanding` holds the
/// `$ref`s descended through on this path: a ref already in it is
/// recursive (a tree-shaped schema), so it is listed but not expanded
/// again, which would never end.
fn option_lines(
    lines: &mut Vec<String>,
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

    lines.push(option_line(&full_name, prop));
    if let Some(description) = prop.description.as_deref().filter(|d| !d.is_empty()) {
        lines.push(format!("      {description}"));
    }
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
            option_lines(lines, nested_name, nested_prop, &full_name, schema, &inner);
        }
    }
}

/// An option's flag, its value's type, and whether it is required or has a
/// default.
fn option_line(full_name: &str, prop: &Property) -> String {
    let required = if prop.required { " (required)" } else { "" };
    let default = prop
        .default
        .as_ref()
        .map(|d| format!(" [default: {}]", values::display(d)))
        .unwrap_or_default();
    let flag = match &prop.short {
        Some(short) => format!("-{short}, --{full_name}"),
        None => format!("    --{full_name}"),
    };
    if prop.kind == Type::Boolean {
        format!("  {flag}{required}{default}")
    } else {
        format!(
            "  {flag}=<{}>{required}{default}",
            prop.kind.name().to_lowercase()
        )
    }
}
