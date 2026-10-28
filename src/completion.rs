//! Shell completion. A small script per shell (a "shim") hands the words
//! typed so far, and which one the cursor is on, back to the program itself
//! through the hidden `__complete` verb; the program answers from its
//! schemas and any completers it registered, so static completion (flags,
//! enums, subcommands) and dynamic completion (the program's own data)
//! share one path.

use std::collections::HashSet;
use std::ffi::OsStr;
use std::fmt;
use std::io::Write;
use std::str::FromStr;
use std::sync::LazyLock;

use fancy_regex::Regex;
use indexmap::IndexMap;

use crate::schema::{Property, Schema, Type};
use crate::values;
use crate::{Cli, Error, Subcommand};

/// The hidden subcommand the shims call. Programs never write it; it is
/// between a shim and [`Cli::handle_completion`].
pub const COMPLETE_VERB: &str = "__complete";

/// A function that completes a field's value.
pub(crate) type Completer = Box<dyn Fn(&CompletionContext) -> Vec<String> + Send + Sync>;

/// A shell Jargon writes completion scripts for. More may come, so a
/// `match` on one needs a `_` arm.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Shell {
    /// Bash.
    Bash,
    /// Zsh.
    Zsh,
    /// Fish.
    Fish,
}

impl Shell {
    /// The shell's name, as `--completions` takes it.
    pub fn name(self) -> &'static str {
        match self {
            Shell::Bash => "bash",
            Shell::Zsh => "zsh",
            Shell::Fish => "fish",
        }
    }
}

impl fmt::Display for Shell {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

impl FromStr for Shell {
    type Err = Error;

    fn from_str(name: &str) -> Result<Shell, Error> {
        match name {
            "bash" => Ok(Shell::Bash),
            "zsh" => Ok(Shell::Zsh),
            "fish" => Ok(Shell::Fish),
            _ => Err(Error::Usage(format!(
                "Unknown shell '{name}'. Supported: bash, zsh, fish"
            ))),
        }
    }
}

/// What a completer is given: the word being completed (`partial`), every
/// word as the shell split them (the program's name first), the subcommand
/// being completed (None at the top level), and the options typed so far,
/// read leniently, for narrowing the candidates.
///
/// More fields may come in later versions, so it is read, not built. To
/// test a completer, call [`Cli::complete`].
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct CompletionContext {
    /// The word being completed, as typed so far; empty at a fresh word.
    pub partial: String,
    /// The whole command line as the shell split it, the program's name
    /// first.
    pub words: Vec<String>,
    /// The subcommand being completed, dot-separated when nested
    /// (`"remote.add"`), as completer paths are; None at the top level.
    pub subcommand: Option<String>,
    /// The options typed so far, by field, each value as typed: a flag
    /// given without a value reads `"true"`, and a flag the schema doesn't
    /// have is kept as typed (`"--verbose"`).
    pub arguments: IndexMap<String, String>,
}

// ---- registration and dispatch --------------------------------------------

impl Cli {
    /// Register a completer for a field's values, named by its dotted path:
    /// the field for a flat CLI, `subcommand.field` for a subcommand's
    /// (`a.b.field` when nested). A path that matches no field is an
    /// error, so typos and renames fail at once.
    pub fn completer<F>(&mut self, path: &str, completer: F) -> Result<&mut Cli, Error>
    where
        F: Fn(&CompletionContext) -> Vec<String> + Send + Sync + 'static,
    {
        let field = path.rsplit('.').next().unwrap_or(path);
        let known = self
            .completer_schema(path)
            .and_then(|schema| schema.root.properties.as_ref())
            .is_some_and(|props| props.contains_key(field));
        if !known {
            return Err(Error::Usage(format!(
                "completer path '{path}' does not match any field in the schema"
            )));
        }
        self.completers.insert(path.to_owned(), Box::new(completer));
        Ok(self)
    }

    fn completer_schema(&self, path: &str) -> Option<&Schema> {
        let parts: Vec<&str> = path.split('.').collect();
        let subcommands = &parts[..parts.len() - 1];
        if subcommands.is_empty() {
            return self.schema.as_ref();
        }

        let mut cli = self;
        let mut found = None;
        for name in subcommands {
            match cli.subcommands.get(*name)? {
                Subcommand::Cli(sub) => cli = sub,
                Subcommand::Schema(schema) => found = Some(schema),
            }
        }
        found.or(cli.schema.as_ref())
    }

    /// Answer a completion request (the hidden `__complete` verb a shim
    /// calls) by writing the candidates to `out`, one per line. `args` is
    /// the command line, the program's name first, as for
    /// [`parse_from`](Cli::parse_from). Returns whether it was a completion
    /// request; call it before anything slow, and exit when it returns
    /// true. [`run`](Cli::run) does all this itself.
    pub fn handle_completion<I>(&self, args: I, out: &mut dyn Write) -> std::io::Result<bool>
    where
        I: IntoIterator,
        I::Item: AsRef<OsStr>,
    {
        // Completion is best effort: a word that isn't UTF-8 is read lossily.
        let args: Vec<String> = args
            .into_iter()
            .skip(1)
            .map(|arg| arg.as_ref().to_string_lossy().into_owned())
            .collect();
        self.answer_completion(&args, out)
    }

    /// [`handle_completion`](Cli::handle_completion) for the words after
    /// the program's name.
    pub(crate) fn answer_completion(
        &self,
        args: &[String],
        out: &mut dyn Write,
    ) -> std::io::Result<bool> {
        if args.first().map(String::as_str) != Some(COMPLETE_VERB) {
            return Ok(false);
        }
        let cword = args
            .get(1)
            .and_then(|n| values::parse_int(n))
            .and_then(|n| usize::try_from(n).ok())
            .unwrap_or(0);
        let words: Vec<&str> = args.iter().skip(2).map(String::as_str).collect();
        for candidate in self.complete(&words, cword) {
            writeln!(out, "{candidate}")?;
        }
        Ok(true)
    }

    /// The completions for a command line: `words` as the shell split them,
    /// the program's name first, and `cword` the index of the word under
    /// the cursor (`words.len()` when starting a new word).
    pub fn complete<S: AsRef<str>>(&self, words: &[S], cword: usize) -> Vec<String> {
        let words: Vec<String> = words.iter().map(|w| w.as_ref().to_owned()).collect();
        let partial = words.get(cword).cloned().unwrap_or_default();
        let engine = Engine {
            cli: self,
            words: &words,
        };
        // The engine's cursor counts from the word after the program's
        // name, so it is -1 on the name itself.
        let cursor = isize::try_from(cword).unwrap_or(isize::MAX) - 1;
        engine.complete_cli(self, words.get(1..).unwrap_or(&[]), cursor, "", &partial)
    }

    // ---- shims ------------------------------------------------------------

    /// The completion script for `shell`, which calls back into this
    /// program.
    pub fn completion_script(&self, shell: Shell) -> String {
        self.completion_script_for(shell, &self.program_name)
    }

    /// The completion script for `shell`, calling `command` instead of the
    /// program: a separate, lighter binary that answers `__complete`, say.
    pub fn completion_script_for(&self, shell: Shell, command: &str) -> String {
        match shell {
            Shell::Bash => self.bash_script(command),
            Shell::Zsh => self.zsh_script(command),
            Shell::Fish => self.fish_script(command),
        }
    }

    /// Bash splits words at `=` and `:` (they are in COMP_WORDBREAKS), so
    /// `--mode=fa` arrives as `--mode`, `=`, `fa`. The shim rejoins pieces
    /// with no whitespace between them in COMP_LINE, so the program sees the
    /// words zsh and fish report, then trims each candidate to the part
    /// after the last break, which is all readline replaces.
    fn bash_script(&self, command: &str) -> String {
        let program = &self.program_name;
        let func = format!("_{}_complete", sanitize(program));
        let command = sh_quote(command);
        [
            format!("{func}() {{"),
            "    local brk=${COMP_WORDBREAKS//[^=:]} rest=$COMP_LINE word gap i j=-1 cword=0 prefix=".into(),
            "    local -a words=()".into(),
            "    for ((i = 0; i < ${#COMP_WORDS[@]}; i++)); do".into(),
            "        word=${COMP_WORDS[i]}".into(),
            r#"        if [[ -z $word ]]; then gap=$rest; else gap=${rest%%"$word"*}; rest=${rest#*"$word"}; fi"#.into(),
            "        if [[ -n $brk && $j -ge 0 && -z $gap && ( $word == [$brk]* || ${words[j]} == *[$brk] ) ]]; then"
                .into(),
            "            words[j]+=$word".into(),
            "        else".into(),
            "            words[++j]=$word".into(),
            "        fi".into(),
            "        ((i == COMP_CWORD)) && cword=$j".into(),
            "    done".into(),
            r#"    [[ -n $brk && ${words[cword]} == *[$brk]* ]] && prefix=${words[cword]%"${words[cword]##*[$brk]}"}"#
                .into(),
            format!(r#"    readarray -t COMPREPLY < <({command} {COMPLETE_VERB} "$cword" "${{words[@]}}")"#),
            r#"    if [[ -n $prefix ]]; then COMPREPLY=("${COMPREPLY[@]#"$prefix"}"); fi"#.into(),
            "}".into(),
            format!("complete -F {func} {}", sh_quote(program)),
        ]
        .join("\n")
    }

    fn zsh_script(&self, command: &str) -> String {
        let program = &self.program_name;
        let func = format!("_{}", sanitize(program));
        // `#compdef` is a comment compinit reads, so quoting can't make an
        // unusual name safe there; the `compdef` call covers it.
        let header = if plain_word(program) {
            format!("#compdef {program}\n")
        } else {
            String::new()
        };
        let lines = [
            format!("{header}{func}() {{"),
            "    local -a candidates".into(),
            format!(
                r#"    candidates=("${{(@f)$({} {COMPLETE_VERB} $((CURRENT - 1)) $words)}}")"#,
                sh_quote(command)
            ),
            "    compadd -U -- $candidates".into(),
            "}".into(),
            format!("compdef {func} {}", sh_quote(program)),
        ];
        lines.join("\n")
    }

    fn fish_script(&self, command: &str) -> String {
        let program = &self.program_name;
        let func = format!("__{}_complete", sanitize(program));
        let lines = [
            format!("function {func}"),
            "    set -l tokens (commandline --current-process --tokenize --cut-at-cursor)".into(),
            "    set -l current (commandline --current-token)".into(),
            format!(
                "    {} {COMPLETE_VERB} (count $tokens) $tokens $current",
                fish_quote(command)
            ),
            "end".into(),
            format!("complete -c {} -f -a '({func})'", fish_quote(program)),
        ];
        lines.join("\n")
    }
}

fn sanitize(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Words made only of these characters need no quoting in bash, zsh, or
/// fish, so ordinary command names and paths are written as they are.
fn plain_word(word: &str) -> bool {
    static PLAIN: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"\A[A-Za-z0-9_./+:@,-]+\z").expect("the plain-word regex compiles")
    });
    PLAIN.is_match(word).unwrap_or(false)
}

/// A word quoted for bash or zsh (POSIX single quotes).
fn sh_quote(word: &str) -> String {
    if plain_word(word) {
        word.to_owned()
    } else {
        format!("'{}'", word.replace('\'', r"'\''"))
    }
}

/// A word quoted for fish, whose single quotes take `\\` and `\'` escapes.
fn fish_quote(word: &str) -> String {
    if plain_word(word) {
        word.to_owned()
    } else {
        format!("'{}'", word.replace('\\', r"\\").replace('\'', r"\'"))
    }
}

// ---- candidates ------------------------------------------------------------

/// One completion request: the CLI whose completers apply, and the words.
struct Engine<'a> {
    cli: &'a Cli,
    words: &'a [String],
}

impl Engine<'_> {
    fn complete_cli(
        &self,
        cli: &Cli,
        args: &[String],
        cursor: isize,
        path: &str,
        partial: &str,
    ) -> Vec<String> {
        if !cli.subcommands.is_empty() {
            let default = cli.default_subcommand();

            // Completing the subcommand's name, which may instead be the
            // default subcommand's first word.
            if cursor <= 0 {
                let mut names: Vec<String> = cli.subcommands.keys().cloned().collect();
                if partial.starts_with('-') {
                    names.extend(["--help".into(), "-h".into()]);
                }
                let mut candidates = filter(names, partial);
                if let Some(default) = default {
                    candidates.extend(
                        self.complete_subcommand(cli, default, args, cursor, path, partial),
                    );
                }
                let mut seen = HashSet::new();
                candidates.retain(|candidate| seen.insert(candidate.clone()));
                return candidates;
            }

            // Descend into the chosen subcommand, which may be abbreviated,
            // or, when the first word names none, into the default
            // subcommand with every word, as `parse` does.
            if let Some(name) = args.first().and_then(|arg| cli.resolve_subcommand(arg)) {
                let rest = args.get(1..).unwrap_or(&[]);
                return self.complete_subcommand(cli, name, rest, cursor - 1, path, partial);
            }
            return match default {
                Some(default) => {
                    self.complete_subcommand(cli, default, args, cursor, path, partial)
                }
                None => Vec::new(),
            };
        }

        match &cli.schema {
            Some(schema) => self.complete_schema(schema, args, cursor, path, partial),
            None => Vec::new(),
        }
    }

    fn complete_subcommand(
        &self,
        cli: &Cli,
        name: &str,
        args: &[String],
        cursor: isize,
        path: &str,
        partial: &str,
    ) -> Vec<String> {
        let path = join(path, name);
        match cli.subcommands.get(name) {
            Some(Subcommand::Cli(sub)) => self.complete_cli(sub, args, cursor, &path, partial),
            Some(Subcommand::Schema(schema)) => {
                self.complete_schema(schema, args, cursor, &path, partial)
            }
            None => Vec::new(),
        }
    }

    fn complete_schema(
        &self,
        schema: &Schema,
        args: &[String],
        cursor: isize,
        path: &str,
        partial: &str,
    ) -> Vec<String> {
        // After a POSIX `--`, every word is an operand, never a flag.
        let typed = &args[..clamp(cursor, args.len())];
        if !typed.iter().any(|arg| arg == "--")
            && let Some(candidates) = self.flag_word_candidates(schema, args, cursor, path, partial)
        {
            return candidates;
        }

        match positional_at(schema, args, cursor) {
            Some(field) => self
                .value_candidates(schema, &field, path, partial, args)
                .unwrap_or_default(),
            None => Vec::new(),
        }
    }

    /// The candidates when the word under the cursor is a flag or a flag's
    /// value; None when it is an operand.
    fn flag_word_candidates(
        &self,
        schema: &Schema,
        args: &[String],
        cursor: isize,
        path: &str,
        partial: &str,
    ) -> Option<Vec<String>> {
        // The value half of a `--flag=value` word. The candidates keep the
        // `--flag=`, since the shell replaces the whole word.
        if partial.starts_with("--")
            && let Some(eq) = partial.find('=')
        {
            let Some(field) = field_for_flag(partial, schema).filter(|f| !boolean(schema, f))
            else {
                return Some(Vec::new());
            };
            let values = self
                .value_candidates(schema, &field, path, &partial[eq + 1..], args)
                .unwrap_or_default();
            return Some(
                values
                    .into_iter()
                    .map(|value| format!("{}{value}", &partial[..=eq]))
                    .collect(),
            );
        }

        // A flag's name.
        if partial == "-" || flag_token(partial) {
            return Some(flag_name_candidates(schema, partial));
        }

        // The value of the flag before it. The word is that value, so this
        // never falls through to positional completion: no enum or
        // completer just means no candidates.
        let previous = usize::try_from(cursor - 1).ok().and_then(|i| args.get(i));
        let field = previous.and_then(|token| value_flag_field(schema, token))?;
        Some(
            self.value_candidates(schema, &field, path, partial, args)
                .unwrap_or_default(),
        )
    }

    fn value_candidates(
        &self,
        schema: &Schema,
        field: &str,
        path: &str,
        partial: &str,
        args: &[String],
    ) -> Option<Vec<String>> {
        let prop = props_of(schema).and_then(|props| props.get(field));
        let head = separated_head(partial, prop);
        let item = &partial[head.len()..];

        let key = join(path, field);
        let candidates = if let Some(completer) = self.cli.completers.get(&key) {
            let context = CompletionContext {
                partial: item.to_owned(),
                words: self.words.to_vec(),
                subcommand: (!path.is_empty()).then(|| path.to_owned()),
                arguments: collect_arguments(schema, args),
            };
            completer(&context)
        } else {
            let prop = prop?;
            let choices = prop.enum_values.as_ref().or_else(|| {
                (prop.kind == Type::Array)
                    .then(|| prop.items.as_ref()?.enum_values.as_ref())
                    .flatten()
            })?;
            filter(choices.iter().map(values::display).collect(), item)
        };
        Some(
            candidates
                .into_iter()
                .map(|value| format!("{head}{value}"))
                .collect(),
        )
    }
}

/// What precedes the item being typed in an array's value: everything up to
/// its last separator, and the blanks after it (`pid, ` in `pid, co`). Empty
/// when the property names no separators.
fn separated_head<'a>(partial: &'a str, prop: Option<&Property>) -> &'a str {
    let Some(separators) = prop.and_then(|p| p.separator.as_deref()) else {
        return "";
    };
    let end = partial
        .char_indices()
        .rfind(|&(_, c)| separators.iter().any(|s| s.splits_at(c)))
        .map_or(0, |(i, c)| i + c.len_utf8());
    let rest = &partial[end..];
    &partial[..end + rest.len() - rest.trim_start().len()]
}

fn clamp(cursor: isize, len: usize) -> usize {
    usize::try_from(cursor).unwrap_or(0).min(len)
}

fn flag_name_candidates(schema: &Schema, partial: &str) -> Vec<String> {
    let mut tokens = vec!["--help".to_owned(), "-h".to_owned()];
    for (name, prop) in props_of(schema).into_iter().flatten() {
        if schema.positional.contains(name) {
            continue;
        }
        tokens.push(format!("--{name}"));
        if let Some(short) = &prop.short {
            tokens.push(format!("-{short}"));
        }
    }
    filter(tokens, partial)
}

/// The positional slot the cursor is in, counting past flags and the values
/// they take. From the variadic on, the parser gives the run's last words to
/// the positionals after it, so the word under the cursor, the last so far,
/// is one of theirs, or the variadic's when none follows it.
fn positional_at(schema: &Schema, args: &[String], cursor: isize) -> Option<String> {
    let names = &schema.positional;
    let limit = cursor.min(args.len() as isize);
    let slot = positional_index(schema, args, limit);
    let variadic = schema
        .variadic
        .as_ref()
        .and_then(|name| names.iter().position(|n| n == name));
    match variadic {
        Some(variadic) if slot >= variadic => {
            let after = names.len() - 1 - variadic;
            let run = slot - variadic + 1;
            Some(names[variadic + run.min(after)].clone())
        }
        _ => names.get(slot).cloned(),
    }
}

/// How many positional slots `args[..limit]` fills, skipping flags and the
/// values they take. As the parser does, an open slot takes any word that
/// isn't a flag (`k=v` and `-5` too), and every word after `--` is an
/// operand.
fn positional_index(schema: &Schema, args: &[String], limit: isize) -> usize {
    let limit = usize::try_from(limit).unwrap_or(0);
    let mut slot = 0;
    let mut i = 0;
    let mut options_ended = false;
    while i < limit {
        let token = &args[i];
        if !options_ended && token == "--" {
            options_ended = true;
            i += 1;
        } else if !options_ended && value_flag_field(schema, token).is_some() && i + 1 < limit {
            i += 2;
        } else if !options_ended && flag_token(token) {
            i += 1;
        } else {
            slot += 1;
            i += 1;
        }
    }
    slot
}

/// A leading dash marks a flag, except on a lone `-` or a negative number
/// like `-5`, which the parser reads as values.
fn flag_token(token: &str) -> bool {
    let mut chars = token.chars();
    chars.next() == Some('-') && chars.next().is_some_and(|c| !c.is_ascii_digit())
}

/// The field a flag sets when it takes the next word as its value (`--name`,
/// `-n`, or `-ln`, a cluster ending in it); None for booleans, a value in the
/// same word (`--name=value`, `-n5`), and words that aren't flags.
fn value_flag_field(schema: &Schema, token: &str) -> Option<String> {
    if !flag_token(token) {
        return None;
    }
    if token.starts_with("--") {
        if token.contains('=') {
            return None;
        }
        return field_for_flag(token, schema).filter(|field| !boolean(schema, field));
    }
    match short_cluster(schema, token) {
        (_, Some(field), None) => Some(field),
        _ => None,
    }
}

/// Read a word of short flags as the parser does: each letter is a flag
/// until one that takes a value, which takes the rest of the word (`-lw80`),
/// or the next word when it ends this one (`-lw`, value None). Returns the
/// boolean fields, then the value-taking field and its value. An unknown
/// letter ends the reading.
fn short_cluster(schema: &Schema, token: &str) -> (Vec<String>, Option<String>, Option<String>) {
    let mut booleans = Vec::new();
    for (n, c) in token.char_indices().skip(1) {
        let Some(field) = field_for_flag(&format!("-{c}"), schema) else {
            break;
        };
        if boolean(schema, &field) {
            booleans.push(field);
        } else {
            let rest = &token[n + c.len_utf8()..];
            return (
                booleans,
                Some(field),
                (!rest.is_empty()).then(|| rest.to_owned()),
            );
        }
    }
    (booleans, None, None)
}

fn collect_arguments(schema: &Schema, args: &[String]) -> IndexMap<String, String> {
    let mut result = IndexMap::new();
    let mut i = 0;
    while i < args.len() {
        let token = &args[i];
        if token == "--" {
            break; // the rest are operands
        }
        if flag_token(token) {
            i += collect_flag(schema, args, i, &mut result);
        } else {
            if let Some((key, value)) = token.split_once('=') {
                result.insert(key.to_owned(), value.to_owned());
            }
            i += 1;
        }
    }
    result
}

/// Collect the fields a flag word sets, with the next word when it is the
/// flag's value. Returns how many words it took.
fn collect_flag(
    schema: &Schema,
    args: &[String],
    i: usize,
    result: &mut IndexMap<String, String>,
) -> usize {
    let token = &args[i];
    let (booleans, field, value) = flag_fields(schema, token);
    let none_set = booleans.is_empty();
    for name in booleans {
        result.insert(name, "true".to_owned());
    }
    match (field, value) {
        (Some(field), Some(value)) => {
            result.insert(field, value);
            1
        }
        (Some(field), None) if i + 1 < args.len() => {
            result.insert(field, args[i + 1].clone());
            2
        }
        (field, _) => {
            // An unknown flag is kept under its word.
            if field.is_some() || none_set {
                result.insert(field.unwrap_or_else(|| token.clone()), "true".to_owned());
            }
            1
        }
    }
}

/// The fields a flag word sets, as `short_cluster` returns them, for a long
/// flag (`--all`, `--name`, `--name=value`) too.
fn flag_fields(schema: &Schema, token: &str) -> (Vec<String>, Option<String>, Option<String>) {
    let Some(name) = token.strip_prefix("--") else {
        return short_cluster(schema, token);
    };
    if let Some((name, value)) = name.split_once('=') {
        return (Vec::new(), Some(name.to_owned()), Some(value.to_owned()));
    }
    match field_for_flag(token, schema) {
        Some(field) if boolean(schema, &field) => (vec![field], None, None),
        field => (Vec::new(), field, None),
    }
}

/// The root's own properties: completion doesn't follow a root `$ref`.
fn props_of(schema: &Schema) -> Option<&IndexMap<String, Property>> {
    schema.root.properties.as_ref()
}

fn field_for_flag(token: &str, schema: &Schema) -> Option<String> {
    let props = props_of(schema);
    if let Some(name) = token.strip_prefix("--") {
        let name = name.split('=').next().unwrap_or_default();
        props?.contains_key(name).then(|| name.to_owned())
    } else if let Some(short) = token.strip_prefix('-') {
        props?
            .iter()
            .find(|(_, prop)| prop.short.as_deref() == Some(short))
            .map(|(field, _)| field.clone())
    } else {
        None
    }
}

fn boolean(schema: &Schema, field: &str) -> bool {
    props_of(schema)
        .and_then(|props| props.get(field))
        .is_some_and(|p| p.kind == Type::Boolean)
}

fn join(path: &str, name: &str) -> String {
    if path.is_empty() {
        name.to_owned()
    } else {
        format!("{path}.{name}")
    }
}

fn filter(candidates: Vec<String>, partial: &str) -> Vec<String> {
    if partial.is_empty() {
        return candidates;
    }
    candidates
        .into_iter()
        .filter(|candidate| candidate.starts_with(partial))
        .collect()
}
