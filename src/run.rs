//! Running a program's command line: the help, completion, and error
//! handling every program does the same way.

use std::ffi::OsStr;
use std::io::Write;
use std::process;

use crate::cli::arguments;
use crate::{Cli, ParseOptions, Parsed};

/// What [`Cli::run`] does with a command line.
#[derive(Debug, PartialEq)]
enum Outcome {
    /// Hand the parse to the program.
    Continue(Parsed),
    /// Print this to standard output and exit 0.
    Print(String),
    /// Print this to standard error and exit 1.
    Fail(String),
}

impl Cli {
    /// Parse the program's command line, as [`parse`](Cli::parse) does,
    /// and handle what isn't the program's business: print the help for
    /// `--help`, the completions a shell asks for, or the script for
    /// `--completions SHELL`, then exit 0; or print the errors to standard
    /// error and exit 1. Otherwise return the parse, which is valid.
    ///
    /// ```no_run
    /// # let cli = jargonaut::Cli::from_json(r#"{"type": "object"}"#, "app")?;
    /// let parsed = cli.run();
    /// # Ok::<(), jargonaut::Error>(())
    /// ```
    ///
    /// # Panics
    ///
    /// When the CLI has neither a schema nor subcommands.
    pub fn run(&self) -> Parsed {
        self.run_with(std::env::args_os(), ParseOptions::default())
    }

    /// [`run`](Cli::run) for a command line, the program's name first, as
    /// for [`parse_from`](Cli::parse_from).
    ///
    /// # Panics
    ///
    /// When the CLI has neither a schema nor subcommands.
    pub fn run_from<I>(&self, args: I) -> Parsed
    where
        I: IntoIterator,
        I::Item: AsRef<OsStr>,
    {
        self.run_with(args, ParseOptions::default())
    }

    /// [`run_from`](Cli::run_from) with [`ParseOptions`]. Pass
    /// [`std::env::args_os`] to run the program's own command line.
    ///
    /// ```no_run
    /// use jargonaut::ParseOptions;
    /// # let cli = jargonaut::Cli::from_json(r#"{"type": "object"}"#, "app")?;
    ///
    /// let config = cli.load_config();
    /// let parsed = cli.run_with(std::env::args_os(), ParseOptions {
    ///     defaults: config.as_ref(),
    ///     ..ParseOptions::default()
    /// });
    /// # Ok::<(), jargonaut::Error>(())
    /// ```
    ///
    /// # Panics
    ///
    /// When the CLI has neither a schema nor subcommands.
    pub fn run_with<I>(&self, args: I, options: ParseOptions<'_>) -> Parsed
    where
        I: IntoIterator,
        I::Item: AsRef<OsStr>,
    {
        let outcome = match arguments(args) {
            Ok(args) => self.outcome(&args, options),
            Err(error) => Outcome::Fail(error + "\n"),
        };
        match outcome {
            Outcome::Continue(parsed) => parsed,
            Outcome::Print(text) => exit(&mut std::io::stdout(), &text, 0),
            Outcome::Fail(text) => exit(&mut std::io::stderr(), &text, 1),
        }
    }

    /// What to do with the words after the program's name.
    fn outcome(&self, args: &[String], options: ParseOptions<'_>) -> Outcome {
        let mut candidates = Vec::new();
        // Writing to a Vec can't fail.
        if let Ok(true) = self.answer_completion(args, &mut candidates) {
            return Outcome::Print(String::from_utf8_lossy(&candidates).into_owned());
        }

        let parsed = self.parse_arguments(args, options);
        if parsed.help_requested {
            let help = match &parsed.help_subcommand {
                Some(subcommand) => self.help_for(subcommand),
                None => Ok(self.help()),
            };
            match help {
                Ok(help) => Outcome::Print(help + "\n"),
                Err(error) => Outcome::Fail(error.to_string() + "\n"),
            }
        } else if let Some(shell) = parsed.completion_shell {
            Outcome::Print(self.completion_script(shell) + "\n")
        } else if !parsed.is_valid() {
            Outcome::Fail(parsed.errors.join("\n") + "\n")
        } else {
            Outcome::Continue(parsed)
        }
    }
}

/// Write `text` and exit with `code`, or with 1 if it can't be written
/// (such as to a closed pipe).
fn exit(out: &mut dyn Write, text: &str, code: i32) -> ! {
    let written = out.write_all(text.as_bytes()).and_then(|()| out.flush());
    process::exit(if written.is_ok() { code } else { 1 })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cli() -> Cli {
        Cli::from_json(
            r#"{
                "type": "object",
                "properties": {
                    "name": {"type": "string", "description": "Who to greet"},
                    "mode": {"type": "string", "enum": ["fast", "slow"]}
                },
                "required": ["name"]
            }"#,
            "greet",
        )
        .unwrap()
    }

    /// The outcome for the words after the program's name.
    fn outcome(cli: &Cli, args: &[&str]) -> Outcome {
        let args: Vec<String> = args.iter().map(|&arg| arg.to_owned()).collect();
        cli.outcome(&args, ParseOptions::default())
    }

    #[test]
    fn returns_a_valid_parse() {
        let Outcome::Continue(parsed) = outcome(&cli(), &["--name", "World"]) else {
            panic!("expected a parse");
        };
        assert_eq!(parsed.get("name").unwrap(), "World");
    }

    #[test]
    fn prints_help_and_completions() {
        let cli = cli();
        assert_eq!(
            outcome(&cli, &["--help"]),
            Outcome::Print(cli.help() + "\n")
        );
        assert_eq!(
            outcome(&cli, &["--completions", "fish"]),
            Outcome::Print(cli.completion_script(crate::Shell::Fish) + "\n")
        );
        assert_eq!(
            outcome(&cli, &["__complete", "2", "greet", "--mode", ""]),
            Outcome::Print("fast\nslow\n".into())
        );
    }

    #[test]
    fn fails_with_the_errors() {
        let cli = cli();
        let Outcome::Fail(text) = outcome(&cli, &["--mode", "medium"]) else {
            panic!("expected a failure");
        };
        assert_eq!(
            text,
            cli.parse_from(["greet", "--mode", "medium"]).errors.join("\n") + "\n"
        );
        assert_eq!(
            outcome(&cli, &["--completions", "tcsh"]),
            Outcome::Fail("Unknown shell 'tcsh'. Supported: bash, zsh, fish\n".into())
        );
    }

    #[test]
    fn prints_a_subcommands_help() {
        let mut cli = Cli::new("git");
        cli.subcommand_json("remote", r#"{"type": "object"}"#)
            .unwrap();
        assert_eq!(
            outcome(&cli, &["remote", "--help"]),
            Outcome::Print(cli.help_for("remote").unwrap() + "\n")
        );
    }
}
