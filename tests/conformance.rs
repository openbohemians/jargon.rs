//! The conformance cases recorded from the Crystal implementation's spec
//! suite (`tests/conformance`, copied in by `scripts/sync-conformance.sh`):
//! each is a call with its inputs and what Crystal Jargon returned, replayed
//! here against this implementation.

use std::collections::HashMap;
use std::io::Cursor;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

use jargonaut::{Cli, ParseOptions, Schema};
use serde_json::{Map, Value, json};

fn cases(kind: &str) -> Vec<Value> {
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!("tests/conformance/{kind}.json"));
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// Cases where this implementation deliberately differs from Crystal's,
/// by spec name, with the reason.
const DIFFERENCES: &[(&str, &str)] = &[(
    "validation > reports a pattern that exhausts the regex match limit rather than raising",
    "a pattern without lookaround or backreferences runs in linear time here, so it never hits a limit",
)];

fn text<'a>(value: &'a Value, key: &str) -> &'a str {
    value[key].as_str().unwrap_or_default()
}

/// A YAML source's documents, which it carries in JSON as well, since YAML
/// isn't supported here.
fn documents(op: &Value) -> Vec<Value> {
    match &op["documents"] {
        Value::Array(docs) => docs.clone(),
        _ => panic!("YAML that didn't parse: {op}"),
    }
}

/// A schema file's content as JSON. A YAML file's documents become objects
/// one after another, which read as several documents too.
fn source(op: &Value) -> String {
    match op.get("documents") {
        None => text(op, "text").to_owned(),
        Some(Value::Array(docs)) if docs.is_empty() => "null".to_owned(),
        Some(Value::Array(docs)) => {
            let docs: Vec<String> = docs.iter().map(Value::to_string).collect();
            docs.join("\n")
        }
        Some(_) => panic!("YAML that didn't parse: {op}"),
    }
}

/// Whether a CLI's calls include YAML that didn't parse, which only a
/// YAML parser can report on.
fn unparsed_yaml(recipe: &Value) -> bool {
    recipe.as_array().is_some_and(|ops| {
        ops.iter()
            .any(|op| op.get("documents").is_some_and(Value::is_null) || unparsed_yaml(&op["cli"]))
    })
}

/// Why a case can't be replayed here, if it can't.
fn skip_reason(case: &Value) -> Option<&'static str> {
    if unparsed_yaml(&case["cli"]) {
        return Some("YAML that doesn't parse, and YAML isn't supported here");
    }
    let spec = case["spec"].as_str()?;
    DIFFERENCES
        .iter()
        .find(|(name, _)| *name == spec)
        .map(|(_, why)| *why)
}

/// A file holding `content`, for the calls that read schema files; removed
/// when dropped.
struct TempFile(PathBuf);

impl TempFile {
    fn new(content: &str) -> TempFile {
        static COUNT: AtomicUsize = AtomicUsize::new(0);
        let n = COUNT.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "jargonaut-conformance-{}-{n}.json",
            std::process::id()
        ));
        std::fs::write(&path, content).expect("write a schema file");
        TempFile(path)
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Rebuild a case's CLI from its recorded calls.
fn build(recipe: &Value) -> Result<Cli, jargonaut::Error> {
    let mut cli: Option<Cli> = None;
    for op in recipe.as_array().expect("a list of calls") {
        let (program, name) = (text(op, "program"), text(op, "name"));
        match text(op, "op") {
            "from_json" => cli = Some(Cli::from_json(text(op, "text"), program)?),
            "from_yaml" => cli = Some(Cli::from_values(documents(op), program)?),
            "from_file" => {
                let file = TempFile::new(&source(op));
                cli = Some(Cli::from_file(&file.0, program)?);
            }
            "from_schema" => {
                cli = Some(Cli::with_schema(
                    Schema::from_json(text(op, "text"))?,
                    program,
                ))
            }
            "new" => cli = Some(Cli::new(program)),
            other => {
                let cli = cli.as_mut().expect("a call before the CLI exists");
                match other {
                    "subcommand_json" => {
                        cli.subcommand_json(name, text(op, "text"))?;
                    }
                    // A YAML subcommand is its first document.
                    "subcommand_yaml" => {
                        let first = documents(op).into_iter().next().unwrap_or(Value::Null);
                        cli.subcommand(name, Schema::from_value(&first)?);
                    }
                    "subcommand_schema" => {
                        cli.subcommand(name, Schema::from_json(text(op, "text"))?);
                    }
                    "subcommand_file" => {
                        let file = TempFile::new(&source(op));
                        if op["name"].is_null() {
                            cli.subcommands_file(&file.0)?;
                        } else {
                            cli.subcommand_file(name, &file.0)?;
                        }
                    }
                    "subcommand_cli" => {
                        cli.subcommand(name, build(&op["cli"])?);
                    }
                    "default_subcommand" => {
                        cli.set_default_subcommand(name);
                    }
                    "subcommand_key" => {
                        cli.set_subcommand_key(text(op, "key"));
                    }
                    "bare_assignment" => {
                        cli.set_bare_assignment(op["value"].as_bool().expect("a boolean"));
                    }
                    unknown => panic!("unknown call '{unknown}'"),
                }
            }
        }
    }
    Ok(cli.expect("a CLI"))
}

/// Error messages must match, except where they quote the JSON parser,
/// whose wording is the language's own.
fn same_errors(expected: &Value, actual: &[String]) -> bool {
    const PARSER_PREFIX: &str = "Invalid JSON from stdin: ";
    let expected = expected.as_array().expect("a list of errors");
    expected.len() == actual.len()
        && expected.iter().zip(actual).all(|(expected, actual)| {
            let expected = expected.as_str().unwrap_or_default();
            if expected.starts_with(PARSER_PREFIX) {
                actual.starts_with(PARSER_PREFIX)
            } else {
                expected == actual
            }
        })
}

/// Collects the cases that didn't match, and fails with them at the end.
#[derive(Default)]
struct Report {
    ran: usize,
    skipped: Vec<String>,
    failures: Vec<String>,
}

impl Report {
    fn check(&mut self, case: &Value, ok: bool, detail: impl FnOnce() -> String) {
        self.ran += 1;
        if !ok {
            self.failures.push(format!(
                "{}\n{}",
                case["spec"].as_str().unwrap_or("?"),
                detail()
            ));
        }
    }

    fn skip(&mut self, case: &Value, why: &str) {
        self.skipped
            .push(format!("{} ({why})", case["spec"].as_str().unwrap_or("?")));
    }

    fn finish(self, kind: &str) {
        eprintln!(
            "{kind}: {} ran, {} skipped, {} failed",
            self.ran,
            self.skipped.len(),
            self.failures.len()
        );
        for skipped in &self.skipped {
            eprintln!("  skipped: {skipped}");
        }
        if !self.failures.is_empty() {
            let shown: Vec<&str> = self.failures.iter().take(25).map(String::as_str).collect();
            panic!(
                "{} of {} {kind} cases failed:\n\n{}",
                self.failures.len(),
                self.ran,
                shown.join("\n\n")
            );
        }
    }
}

#[test]
fn parse() {
    let mut report = Report::default();
    for case in cases("parse") {
        if let Some(why) = skip_reason(&case) {
            report.skip(&case, why);
            continue;
        }
        let expect = &case["expect"];
        let defaults = match case.get("defaults") {
            None => None,
            Some(Value::Object(map)) => Some(map),
            // The defaults parameter takes an object, so a non-object can't
            // be passed at all.
            Some(_) => {
                report.skip(&case, "defaults that aren't an object can't be passed");
                continue;
            }
        };
        let cli = match build(&case["cli"]) {
            Ok(cli) => cli,
            Err(e) => {
                report.check(&case, false, || format!("building the CLI failed: {e}"));
                continue;
            }
        };

        let env: HashMap<String, String> = case["env"]
            .as_object()
            .map(|env| {
                env.iter()
                    .map(|(k, v)| (k.clone(), v.as_str().unwrap_or_default().to_owned()))
                    .collect()
            })
            .unwrap_or_default();
        let lookup = |name: &str| env.get(name).cloned();
        let mut stdin = Cursor::new(text(&case, "stdin").as_bytes().to_vec());
        let args: Vec<&str> = case["args"]
            .as_array()
            .expect("args")
            .iter()
            .map(|a| a.as_str().unwrap())
            .collect();

        let command_line = std::iter::once(cli.program_name()).chain(args.iter().copied());
        let parsed = cli.parse_with(
            command_line,
            ParseOptions {
                defaults,
                stdin: Some(&mut stdin),
                env: Some(&lookup),
            },
        );
        let actual = json!({
            "data": parsed.data,
            "errors": parsed.errors,
            "subcommand": parsed.subcommand,
            "help": parsed.help_requested,
            "help_subcommand": parsed.help_subcommand,
            "completion_shell": parsed.completion_shell.map(|shell| shell.name()),
        });
        let ok = expect["data"] == actual["data"]
            && same_errors(&expect["errors"], &parsed.errors)
            && ["subcommand", "help", "help_subcommand", "completion_shell"]
                .iter()
                .all(|k| expect[k] == actual[k]);
        report.check(&case, ok, || {
            format!(
                "  args: {}\n  expected: {expect}\n  actual:   {actual}",
                case["args"]
            )
        });
    }
    report.finish("parse");
}

#[test]
fn validate() {
    let mut report = Report::default();
    for case in cases("validate") {
        if let Some(why) = skip_reason(&case) {
            report.skip(&case, why);
            continue;
        }
        let data: Map<String, Value> = case["data"].as_object().cloned().unwrap_or_default();
        let result = if let Some(schema) = case["schema"].as_str() {
            Schema::from_json(schema).map(|schema| jargonaut::validate(&data, &schema))
        } else {
            build(&case["cli"]).and_then(|cli| cli.validate(&data, case["subcommand"].as_str()))
        };
        let expect = &case["expect"];
        let ok = match (&result, expect.get("raises")) {
            (Ok(errors), None) => same_errors(expect, errors),
            (Err(e), Some(message)) => message == &e.to_string(),
            _ => false,
        };
        report.check(&case, ok, || {
            format!("  expected: {expect}\n  actual:   {result:?}")
        });
    }
    report.finish("validate");
}

#[test]
fn load() {
    let mut report = Report::default();
    for case in cases("load") {
        if let Some(why) = skip_reason(&case) {
            report.skip(&case, why);
            continue;
        }
        let result = match case["schema"].as_str() {
            Some(schema) => Schema::from_json(schema).map(|_| ()),
            None => build(&case["cli"]).map(|_| ()),
        };
        let expect = &case["expect"]["raises"];
        let ok = result.as_ref().is_err_and(|e| expect == &e.to_string());
        report.check(&case, ok, || {
            format!("  expected the error: {expect}\n  actual: {result:?}")
        });
    }
    report.finish("load");
}

#[test]
fn utility() {
    let mut report = Report::default();
    for case in cases("utility") {
        let result = if let Some([sub, global]) = case["merge"].as_array().map(Vec::as_slice) {
            jargonaut::merge(sub.as_str().unwrap(), global.as_str().unwrap())
        } else {
            jargonaut::flatten(text(&case, "flatten"))
        };
        let expect = &case["expect"];
        let ok = match (&result, expect.get("raises")) {
            // Compared as JSON values: the two write floats differently.
            (Ok(output), None) => {
                serde_json::from_str::<Value>(output).ok()
                    == serde_json::from_str(expect.as_str().unwrap()).ok()
            }
            (Err(e), Some(message)) => message == &e.to_string(),
            _ => false,
        };
        report.check(&case, ok, || {
            format!("  expected: {expect}\n  actual:   {result:?}")
        });
    }
    report.finish("utility");
}

#[test]
fn help() {
    let mut report = Report::default();
    for case in cases("help") {
        if let Some(why) = skip_reason(&case) {
            report.skip(&case, why);
            continue;
        }
        let result = build(&case["cli"]).and_then(|cli| match case["subcommand"].as_str() {
            Some(subcommand) => cli.help_for(subcommand),
            None => Ok(cli.help()),
        });
        let expect = &case["expect"];
        let ok = match (&result, expect.get("raises")) {
            (Ok(help), None) => expect.as_str() == Some(help.as_str()),
            (Err(e), Some(message)) => message == &e.to_string(),
            _ => false,
        };
        report.check(&case, ok, || match &result {
            Ok(help) => format!(
                "  expected:\n{}\n  actual:\n{help}",
                expect.as_str().unwrap_or_default()
            ),
            Err(e) => format!("  expected: {expect}\n  failed: {e}"),
        });
    }
    report.finish("help");
}

#[test]
fn complete() {
    let mut report = Report::default();
    for case in cases("complete") {
        if let Some(why) = skip_reason(&case) {
            report.skip(&case, why);
            continue;
        }
        let words: Vec<&str> = case["words"]
            .as_array()
            .expect("words")
            .iter()
            .map(|w| w.as_str().unwrap())
            .collect();
        let cword = case["cword"].as_u64().expect("cword") as usize;
        let result = build(&case["cli"]).map(|cli| cli.complete(&words, cword));
        let expect = &case["expect"];
        let ok = result
            .as_ref()
            .is_ok_and(|candidates| expect == &json!(candidates));
        report.check(&case, ok, || {
            format!(
                "  words: {words:?} cword {cword}\n  expected: {expect}\n  actual:   {result:?}"
            )
        });
    }
    report.finish("complete");
}

#[test]
fn shim() {
    let mut report = Report::default();
    for case in cases("shim") {
        if let Some(why) = skip_reason(&case) {
            report.skip(&case, why);
            continue;
        }
        let shell: jargonaut::Shell = text(&case, "shell").parse().expect("a shell");
        let result =
            build(&case["cli"]).map(|cli| cli.completion_script_for(shell, text(&case, "command")));
        let expect = &case["expect"];
        let ok = result
            .as_ref()
            .is_ok_and(|script| expect.as_str() == Some(script.as_str()));
        report.check(&case, ok, || {
            format!(
                "  expected:\n{}\n  actual:\n{result:?}",
                expect.as_str().unwrap_or_default()
            )
        });
    }
    report.finish("shim");
}
