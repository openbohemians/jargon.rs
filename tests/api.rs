//! The Rust API around parsing, beyond what the conformance cases cover.

use std::io::Cursor;

use jargonaut::{Cli, ConfigOptions, ParseOptions};
use serde::Deserialize;
use serde_json::{Map, Value, json};

const SCHEMA: &str = r#"{
    "type": "object",
    "properties": {
        "name": {"type": "string", "short": "n"},
        "count": {"type": "integer", "default": 1},
        "server": {
            "type": "object",
            "properties": {"host": {"type": "string"}, "port": {"type": "integer"}}
        },
        "token": {"type": "string", "env": "APP_TOKEN"}
    },
    "required": ["name"]
}"#;

fn cli() -> Cli {
    Cli::from_json(SCHEMA, "app").expect("the schema loads")
}

#[test]
fn deserializes_into_a_struct() {
    #[derive(Deserialize, Debug, PartialEq)]
    struct Server {
        host: Option<String>,
        port: Option<i64>,
    }
    #[derive(Deserialize, Debug, PartialEq)]
    struct Args {
        name: String,
        count: i64,
        server: Option<Server>,
    }

    let parsed = cli().parse_from(["app", "-n", "web", "--server.port", "8080"]);
    let args: Args = parsed.deserialize().expect("the data fits Args");
    let server = Some(Server {
        host: None,
        port: Some(8080),
    });
    assert_eq!(
        args,
        Args {
            name: "web".into(),
            count: 1,
            server
        }
    );
}

#[test]
fn json_returns_the_data_or_the_errors() {
    let data = cli().json_from(["app", "--name", "x"]).expect("valid");
    assert_eq!(Value::Object(data), json!({"name": "x", "count": 1}));

    let error = cli().json_from(["app", "--count", "two"]).expect_err("invalid");
    assert_eq!(
        error.to_string(),
        "Invalid integer value 'two' for count\nMissing required field: name"
    );
}

#[test]
fn options_supply_defaults_environment_and_stdin() {
    let defaults: Map<String, Value> =
        json!({"server": {"host": "localhost", "port": 1}, "count": 5})
            .as_object()
            .cloned()
            .unwrap();
    let env = |name: &str| (name == "APP_TOKEN").then(|| "secret".to_owned());
    let options = ParseOptions {
        defaults: Some(&defaults),
        env: Some(&env),
        ..ParseOptions::default()
    };
    let parsed = cli().parse_with(["app", "-n", "a", "--server.port", "8080"], options);
    assert_eq!(
        Value::Object(parsed.data),
        json!({"name": "a", "server": {"port": 8080, "host": "localhost"}, "token": "secret", "count": 5})
    );
    assert_eq!(
        defaults["server"]["port"], 1,
        "the caller's defaults are untouched"
    );

    let mut stdin = Cursor::new(br#"{"name": "from-json"}"#.to_vec());
    let parsed = cli().parse_with(
        ["app", "-"],
        ParseOptions {
            stdin: Some(&mut stdin),
            ..ParseOptions::default()
        },
    );
    assert_eq!(parsed.get("name"), Some(&json!("from-json")));
}

#[test]
fn reports_bad_schemas_as_errors() {
    let error = Cli::from_json(r#"{"type": "string", "properties": {}}"#, "app")
        .expect_err("a type mismatch");
    assert!(matches!(error, jargonaut::Error::Schema(_)));
    let error = Cli::from_json("{nope", "app").expect_err("not JSON");
    assert!(matches!(error, jargonaut::Error::Syntax(_)));
    let error = Cli::from_json(r#"{"properties": {"a": {"pattern": "("}}}"#, "app")
        .expect_err("a bad pattern");
    assert!(
        error.to_string().starts_with("Invalid pattern '('"),
        "{error}"
    );
}

#[test]
fn takes_the_program_name_first() {
    let mut stdout = Vec::new();
    let answered = cli()
        .handle_completion(["app", "__complete", "1", "app", "--na"], &mut stdout)
        .unwrap();
    assert!(answered);
    assert_eq!(String::from_utf8(stdout).unwrap(), "--name\n");

    let args: Vec<String> = vec!["app".into(), "-n".into(), "x".into()];
    assert_eq!(cli().parse_from(&args).get("name"), Some(&json!("x")));
    assert!(!cli().handle_completion(&args, &mut Vec::new()).unwrap());
}

#[cfg(unix)]
#[test]
fn an_argument_that_isnt_utf8_is_an_error() {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;

    let args = [OsStr::new("app"), OsStr::new("-n"), OsStr::from_bytes(b"caf\xe9")];
    let parsed = cli().parse_from(args);
    assert_eq!(parsed.errors, ["Argument 'caf\u{FFFD}' is not valid UTF-8"]);
}

#[test]
fn help_for_an_unknown_subcommand_is_an_error() {
    let mut cli = Cli::new("git");
    cli.subcommand_json("remote", r#"{"type": "object"}"#).unwrap();
    assert!(cli.help_for("remote").is_ok());
    let error = cli.help_for("nope").expect_err("no such subcommand");
    assert!(matches!(error, jargonaut::Error::Usage(_)));
    assert_eq!(error.to_string(), "Unknown subcommand: nope");
}

#[test]
#[should_panic(expected = "CLI has no schema and no subcommands defined")]
fn parsing_with_nothing_to_parse_against_panics() {
    Cli::new("empty").parse_from(["empty", "x"]);
}

#[test]
fn config_paths_include_the_project_only_when_asked() {
    let cli = Cli::from_json(r#"{"type": "object"}"#, "myapp").unwrap();
    let user = cli.config_paths(ConfigOptions::default());
    assert!(user.iter().all(|path| !path.starts_with(".")));
    let all = cli.config_paths(ConfigOptions {
        project: true,
        ..ConfigOptions::default()
    });
    assert!(all.len() > user.len());
    assert!(all[0].starts_with("./.config"));
    assert_eq!(all[all.len() - user.len()..], user[..]);
}
