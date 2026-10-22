# Jargon for Rust

Define a command-line interface with JSON Schema: the schema is the single
source of truth for a program's options, their types, validation,
defaults, help, and shell completion. This is a port of [Jargon](https://github.com/trans/jargon)
for Crystal, held to the same behavior by the cases that implementation's
spec suite records.

```rust
use jargon::Cli;

let cli = Cli::from_json(r#"{
    "type": "object",
    "properties": {
        "name": {"type": "string", "short": "n", "description": "Who to greet"},
        "count": {"type": "integer", "default": 1}
    },
    "required": ["name"]
}"#, "greet")?;

let args: Vec<String> = std::env::args().skip(1).collect();
let parsed = cli.run(&args);
println!("{}", parsed.to_json()); // {"name":"World","count":1}
```

`run` prints the help for `--help` and the script for `--completions
bash` (or `zsh`, `fish`), answers the shell's completion requests, and
prints the errors for invalid input, exiting in each case; otherwise it
returns the parse. `parse` does the parsing alone, leaving the rest to
you. `parsed.deserialize::<Args>()` reads the data into your own type,
such as a struct deriving `serde::Deserialize`.

Options can also come from config files:

```rust
use jargon::ParseOptions;

let config = cli.load_config(); // ~/.config/greet.yaml, say
let parsed = cli.run_with(&args, ParseOptions {
    defaults: config.as_ref(),
    ..ParseOptions::default()
});
```

## Status

Ported so far, with every recorded case for it passing:

- Parsing: `--flag value`, `--flag=value`, `key=value`, short and combined
  short flags, positionals and variadics, `--`, negative numbers, nested
  `--server.port`, typo suggestions, and options as JSON on standard input
  (`-`)
- Subcommands: nested CLIs, defaults, abbreviations, and multi-document
  JSON or YAML with `$id` mixins and `allOf`
- Validation, as `jargon::validate` too, and defaults from the schema, the
  environment, and the caller (such as a config file)
- Help text, and `jargon::merge` and `jargon::flatten`
- Shell completion for bash, zsh, and fish, with completers for dynamic
  values
- Config files (`load_config`) and `run`

That is all of the Crystal implementation's API.

## From Crystal

| Crystal | Rust |
|---|---|
| `Jargon.cli("app", json: s)` | `Cli::from_json(s, "app")?` (also `from_yaml`, `from_file`) |
| `Jargon.new("app")` | `Cli::new("app")` |
| `cli.subcommand("x", json: s)` | `cli.subcommand_json("x", s)?` (also `_yaml`, `_file`) |
| `cli.subcommand("x", other_cli)` | `cli.subcommand("x", other_cli)` (a `Cli` or a `Schema`) |
| `cli.default_subcommand("x")` | `cli.set_default_subcommand("x")` |
| `cli.parse(args, input, defaults: d)` | `cli.parse_with(&args, ParseOptions { stdin, defaults, env })` |
| `cli.json(args)` | `cli.json(&args)`, a `Result` with a `ParseError` |
| `cli.help("remote add")` | `cli.help_for("remote add")` |
| `cli.run(args)` | `cli.run(&args)` (also `run_with`) |
| `cli.load_config(project: true)` | `cli.load_config_with(ConfigOptions { project: true, ..Default::default() })` |
| `Jargon.config_warnings = false` | `ConfigOptions { warnings: false, .. }` |
| `cli.bash_completion` | `cli.completion_script(Shell::Bash)` |
| `cli.handle_completion(args)` | `if cli.handle_completion(&args, &mut stdout)? { exit(0) }` |
| `Jargon::Validator.validate(data, schema)` | `jargon::validate(&data, &schema)` |
| `result.valid?` | `parsed.is_valid()` |

Building a CLI returns a `Result` where Crystal raises. `defaults` is a
`serde_json::Map`, so it can only be an object. `ParseOptions` can also
supply the environment, which the environment variables in a schema
(`"env"`) are read from.

YAML is the `yaml` feature, on by default, through
[Oyl](https://github.com/openbohemians/oyl.rs), which needs a C compiler
to build. The package is `jargon-cli`, since `jargon` is taken on
crates.io; in code it is `jargon`.

## Conformance

`tests/conformance` holds cases recorded from the Crystal implementation's
spec suite: each is a call, its inputs, and what Crystal Jargon returned.
`cargo test` replays them. To update them, record them in a checkout of
the Crystal repository, then copy them here:

```sh
(cd ../jargon && just conformance)
just sync ../jargon
```

`tests/conformance/COMMIT` names the commit they came from.

Where this implementation deliberately differs:

- A `pattern` without lookaround or backreferences runs in linear time, so
  it never hits a backtracking limit. Patterns are Rust regexes, whose
  `\d`, `\s`, and `\w` match Unicode, not just ASCII.
- In the built-in formats, `$` matches only at the end of the value, so
  `"a@b.co\n"` isn't an email.
- `inf`, `nan`, and numbers too large for a float are invalid numbers.
- Help shows a default that isn't a string as JSON.
- YAML is read as YAML 1.1, as Crystal reads it, except that dates stay
  strings (`2026-10-22`) and `0o17` is a string.
- Arguments are UTF-8 (`&str`).
- Without the `yaml` feature, config files are JSON only.

## License

MIT
