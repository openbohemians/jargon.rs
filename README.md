# Jargonaut

[![CI](https://github.com/openbohemians/jargon.rs/actions/workflows/ci.yml/badge.svg)](https://github.com/openbohemians/jargon.rs/actions/workflows/ci.yml)

Jargon for Rust. Define a command-line interface with JSON Schema: the schema is the single
source of truth for a program's options, their types, validation,
defaults, help, and shell completion. This is a port of [Jargon](https://github.com/openbohemians/jargon.cr)
for Crystal, held to the same behavior by the cases that implementation's
spec suite records. The project's site is
[jargon.openbohemians.org](https://jargon.openbohemians.org).

```sh
cargo add jargonaut
```

```rust
use jargonaut::Cli;

let cli = Cli::from_json(r#"{
    "type": "object",
    "properties": {
        "name": {"type": "string", "short": "n", "description": "Who to greet"},
        "count": {"type": "integer", "default": 1}
    },
    "required": ["name"]
}"#, "greet")?;

let parsed = cli.run(); // greet -n World
println!("{}", parsed.to_json()); // {"name":"World","count":1}
```

`run` prints the help for `--help` and the script for `--completions
bash` (or `zsh`, `fish`), answers the shell's completion requests, and
prints the errors for invalid input, exiting in each case; otherwise it
returns the parse. `parse` does the parsing alone, leaving the rest to
you. Both read the program's own command line; `run_from` and
`parse_from` take one instead, the program's name first, as Clap's
`parse_from` does (`cli.parse_from(["greet", "-n", "World"])`).
`parsed.deserialize::<Args>()` reads the data into your own type, such
as a struct deriving `serde::Deserialize`.

Jargon's job ends at a valid value: one with the type, format, and
constraints its schema gives it. What that value refers to, such as the
document a `query` names, and what to do when none or several match, is
up to your program, once `parse` returns.

Options can also come from config files:

```rust
use jargonaut::ParseOptions;

let config = cli.load_config(); // ~/.config/greet.json, say
let parsed = cli.run_with(std::env::args_os(), ParseOptions {
    defaults: config.as_ref(),
    ..ParseOptions::default()
});
```

With subcommands, a config can set options for all of them and for each
one. An object under a subcommand's name is that subcommand's section, and
any other key is global:

```json
{"verbose": true, "fetch": {"depth": 1}, "remote": {"add": {"name": "upstream"}}}
```

A subcommand takes the global keys it has an option for, then its own
section's keys over them, all of them, so a misspelled one is still
reported under `additionalProperties: false`. A nested CLI's section holds
its subcommands' sections. This applies to `defaults` from any source.

## Status

Ported so far, with every recorded case for it passing:

- Parsing: `--flag value`, `--flag=value`, `key=value`, short and combined
  short flags, positionals and variadics, `--`, negative numbers, nested
  `--server.port`, typo suggestions, and options as JSON on standard input
  (`-`)
- Subcommands: nested CLIs, defaults, abbreviations, and multi-document
  JSON with `$id` mixins and `allOf`
- Validation, as `jargonaut::validate` too, and defaults from the schema, the
  environment, and the caller (such as a config file)
- Help text, and `jargonaut::merge` and `jargonaut::flatten`
- Shell completion for bash, zsh, and fish, with completers for dynamic
  values
- Config files (`load_config`), in JSON or any format you add, with
  sections for subcommands, and `run`

That is all of the Crystal implementation's API, except YAML.

## From Crystal

| Crystal | Rust |
|---|---|
| `Jargon.cli("app", json: s)` | `Cli::from_json(s, "app")?` (also `from_file`) |
| `Jargon.new("app")` | `Cli::new("app")` |
| `cli.subcommand("x", json: s)` | `cli.subcommand_json("x", s)?` (also `_file`) |
| `cli.subcommand("x", other_cli)` | `cli.subcommand("x", other_cli)` (a `Cli` or a `Schema`) |
| `cli.default_subcommand("x")` | `cli.set_default_subcommand("x")` |
| `cli.parse` | `cli.parse()` |
| `cli.parse(args)` | `cli.parse_from(["app", ...])`, the program's name first |
| `cli.parse(args, input, defaults: d)` | `cli.parse_with(args, ParseOptions { stdin, defaults, env })` |
| `cli.json(args)` | `cli.json_from(args)`, a `Result` with a `ParseError` (also `json`, `json_with`) |
| `cli.help("remote add")` | `cli.help_for("remote add")?` |
| `cli.run(args)` | `cli.run_from(args)` (also `run`, `run_with`) |
| `cli.load_config(project: true)` | `cli.load_config_with(ConfigOptions { project: true, ..Default::default() })` |
| `Jargon.config_warnings = false` | `ConfigOptions { warnings: false, .. }` |
| `cli.config_format("toml") { \|text\| TOML.parse(text) }` | `cli.config_format("toml", toml::from_str)` |
| `Jargon.cli("app", yaml: s)` | `Cli::from_values(docs, "app")?`, the YAML read with a library of your choice |
| `cli.bash_completion` | `cli.completion_script(Shell::Bash)` |
| `cli.handle_completion(args)` | `if cli.handle_completion(std::env::args_os(), &mut stdout)? { exit(0) }` |
| `Jargon::Validator.validate(data, schema)` | `jargonaut::validate(&data, &schema)` |
| `result.valid?` | `parsed.is_valid()` |

Crystal's `args` leave the program's name out, as `ARGV` does; a Rust
command line starts with it, as `std::env::args_os` does. Building a CLI
returns a `Result` where Crystal raises. `defaults` is a
`serde_json::Map`, so it can only be an object. `ParseOptions` can also
supply the environment, which the environment variables in a schema
(`"env"`) are read from.

## YAML, TOML, and other formats

Jargon reads JSON itself and leaves other formats to the library of
your choice, so it doesn't pick one for you. For a schema in YAML, read
its documents into `serde_json::Value`s and pass them to
`Cli::from_values`: one document is the CLI's schema, and several are
subcommands, as for multi-document JSON. (`Schema::from_value` makes a
single subcommand's schema.)

For config files, register the format's parser, and `load_config` looks
for `greet.toml` alongside `greet.json`, ahead of it:

```rust
cli.config_format("toml", toml::from_str);
```

A parser is any function from the file's text to a `serde_json::Value`
whose error displays, so a YAML library's `from_str` works the same way.
A format can have several extensions, looked for in the order given:

```rust
cli.config_format(["yaml", "yml"], your_yaml_library::from_str);
```

Registering `json` replaces the JSON parser, with one that allows
comments, say.

## Conformance

`tests/conformance` holds cases recorded from the Crystal implementation's
spec suite: each is a call, its inputs, and what Crystal Jargon returned.
`cargo test` replays them. To update them, record them in a checkout of
the Crystal repository, then copy them here:

```sh
(cd ../jargon.cr && just conformance)
just sync ../jargon.cr
```

`tests/conformance/COMMIT` names the commit they came from.

Where this implementation deliberately differs:

- Patterns run on fancy-regex rather than PCRE. One without lookaround
  or backreferences runs in linear time, so it never hits a backtracking
  limit.
- No built-in YAML: schemas and config files are JSON unless you add a
  format (see above), so `.yaml` and `.yml` configs aren't looked for
  by default.
- Arguments are UTF-8 (`&str`).

## License

MIT
