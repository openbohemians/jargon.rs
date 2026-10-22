//! Option defaults from config files, in the XDG locations.

use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use indexmap::IndexMap;
use serde_json::{Map, Value};

use crate::Cli;

/// Reads a config file's text as JSON.
pub(crate) type ConfigParser = Box<dyn Fn(&str) -> Result<Value, String> + Send + Sync>;

/// Where [`Cli::load_config_with`] looks for config files, and what it does
/// with them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConfigOptions {
    /// Merge every config found, a project's over the user's, nested
    /// objects key by key (true, the default); or take just the first one
    /// found.
    pub merge: bool,
    /// Also read the project's configs, in `./.config`. They're off by
    /// default because the current directory may be one someone else
    /// controls, such as a freshly cloned repository, and its configs
    /// would take precedence over the user's own.
    pub project: bool,
    /// Print a warning to standard error for a config that can't be read
    /// (true, the default). The config is skipped either way.
    pub warnings: bool,
}

impl Default for ConfigOptions {
    fn default() -> ConfigOptions {
        ConfigOptions {
            merge: true,
            project: false,
            warnings: true,
        }
    }
}

impl Cli {
    /// The user's config for this program, to pass as
    /// [`ParseOptions::defaults`](crate::ParseOptions::defaults): every
    /// config found in [`config_paths`](Cli::config_paths), merged. None
    /// when there isn't one.
    pub fn load_config(&self) -> Option<Map<String, Value>> {
        self.load_config_with(ConfigOptions::default())
    }

    /// [`load_config`](Cli::load_config) with [`ConfigOptions`].
    pub fn load_config_with(&self, options: ConfigOptions) -> Option<Map<String, Value>> {
        load(&self.config_paths(options), options, &self.config_formats)
    }

    /// The files [`load_config_with`](Cli::load_config_with) looks for,
    /// highest precedence first. For a program `app`, `app.json` and
    /// `app/config.json` (and the same with each extension added by
    /// [`config_format`](Cli::config_format), ahead of `.json`): in
    /// `./.config` when `options.project` is set, then in
    /// `$XDG_CONFIG_HOME`, or `~/.config` when that is unset or not
    /// absolute.
    pub fn config_paths(&self, options: ConfigOptions) -> Vec<PathBuf> {
        search_paths(
            &self.program_name,
            options.project.then(|| Path::new(".")),
            std::env::var_os("XDG_CONFIG_HOME"),
            std::env::home_dir(),
            &extensions(&self.config_formats),
        )
    }

    /// Read config files ending in `.ext` with `parse`, which turns a
    /// file's text into JSON: a YAML or TOML library's `from_str`, say.
    /// Configs are otherwise JSON only. An added format is looked for
    /// ahead of JSON, and ahead of formats added after it, so where a
    /// directory has both `app.toml` and `app.json`, `app.toml` takes
    /// precedence. Adding `json` replaces the JSON parser (with one that
    /// allows comments, say), and JSON keeps its place.
    ///
    /// ```
    /// # mod toml {
    /// #     pub fn from_str<T: serde::de::DeserializeOwned>(text: &str) -> serde_json::Result<T> {
    /// #         serde_json::from_str(text)
    /// #     }
    /// # }
    /// let mut cli = jargonaut::Cli::from_json(r#"{"type": "object"}"#, "app")?;
    /// cli.config_format("toml", toml::from_str);
    /// let config = cli.load_config(); // reads ~/.config/app.toml too
    /// # Ok::<(), jargonaut::Error>(())
    /// ```
    ///
    /// # Panics
    ///
    /// When `ext` is empty or has a dot or a path separator in it: it is
    /// the extension alone, `"toml"`.
    pub fn config_format<F, E>(&mut self, ext: &str, parse: F) -> &mut Cli
    where
        F: Fn(&str) -> Result<Value, E> + Send + Sync + 'static,
        E: fmt::Display,
    {
        assert!(
            !ext.is_empty() && !ext.contains(|c| c == '.' || std::path::is_separator(c)),
            "a config format's extension is the extension alone, such as \"toml\"; got {ext:?}"
        );
        self.config_formats.insert(
            ext.to_owned(),
            Box::new(move |text| parse(text).map_err(|error| error.to_string())),
        );
        self
    }
}

/// The config file extensions, in the order they're looked for: those
/// added, then JSON.
fn extensions(formats: &IndexMap<String, ConfigParser>) -> Vec<&str> {
    formats
        .keys()
        .map(String::as_str)
        .filter(|ext| *ext != "json")
        .chain(["json"])
        .collect()
}

/// The config paths for `program`, with the project's under `project` and
/// the user's under `xdg`, or `home`'s `.config` failing that.
fn search_paths(
    program: &str,
    project: Option<&Path>,
    xdg: Option<OsString>,
    home: Option<PathBuf>,
    extensions: &[&str],
) -> Vec<PathBuf> {
    // Per the XDG spec, an empty or relative XDG_CONFIG_HOME is ignored.
    let user = xdg
        .map(PathBuf::from)
        .filter(|dir| dir.is_absolute())
        .or_else(|| home.map(|home| home.join(".config")));
    project
        .map(|dir| dir.join(".config"))
        .into_iter()
        .chain(user)
        .flat_map(|dir| [dir.join(program), dir.join(program).join("config")])
        .flat_map(|base| {
            extensions.iter().map(move |ext| {
                // Appended, so a dot in the program's name stays (`my.app.json`).
                let mut path = base.clone().into_os_string();
                path.push(format!(".{ext}"));
                PathBuf::from(path)
            })
        })
        .collect()
}

fn load(
    paths: &[PathBuf],
    options: ConfigOptions,
    formats: &IndexMap<String, ConfigParser>,
) -> Option<Map<String, Value>> {
    if !options.merge {
        let path = paths.iter().find(|path| path.exists())?;
        return read(path, options.warnings, formats);
    }
    // The lowest precedence first, so each config overrides the ones before.
    let mut merged = Map::new();
    for path in paths.iter().rev().filter(|path| path.exists()) {
        if let Some(config) = read(path, options.warnings, formats) {
            deep_merge(&mut merged, config);
        }
    }
    (!merged.is_empty()).then_some(merged)
}

/// Merge `source` into `target`, objects in both recursively; otherwise
/// `source`'s value wins.
fn deep_merge(target: &mut Map<String, Value>, source: Map<String, Value>) {
    for (key, value) in source {
        match (target.get_mut(&key), value) {
            (Some(Value::Object(existing)), Value::Object(nested)) => deep_merge(existing, nested),
            (_, value) => {
                target.insert(key, value);
            }
        }
    }
}

/// A config file's options, warning about one that can't be read.
fn read(
    path: &Path,
    warnings: bool,
    formats: &IndexMap<String, ConfigParser>,
) -> Option<Map<String, Value>> {
    match parse(path, formats) {
        Ok(config) => config,
        Err(message) => {
            if warnings {
                eprintln!(
                    "Warning: Failed to parse config file '{}': {message}",
                    path.display()
                );
            }
            None
        }
    }
}

/// A config file's options, read by the format its extension names; None
/// for `null`, which configures nothing (as an empty YAML file reads).
fn parse(
    path: &Path,
    formats: &IndexMap<String, ConfigParser>,
) -> Result<Option<Map<String, Value>>, String> {
    let content = fs::read_to_string(path).map_err(|error| error.to_string())?;
    let ext = path.extension().and_then(OsStr::to_str).unwrap_or_default();
    let config = match formats.get(ext) {
        Some(parse) => parse(&content)?,
        None => serde_json::from_str(&content).map_err(|error| error.to_string())?,
    };
    match config {
        Value::Null => Ok(None),
        Value::Object(options) => Ok(Some(options)),
        _ => Err("expected a mapping of option defaults at the top level".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A scratch directory, removed when dropped.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(name: &str) -> TempDir {
            let dir = std::env::temp_dir()
                .join(format!("jargonaut-config-{}-{name}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            TempDir(dir)
        }

        fn write(&self, path: &str, content: &str) {
            let path = self.0.join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, content).unwrap();
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    const QUIET: ConfigOptions = ConfigOptions {
        merge: true,
        project: false,
        warnings: false,
    };

    fn strings(paths: &[PathBuf]) -> Vec<String> {
        paths.iter().map(|p| p.display().to_string()).collect()
    }

    const JSON: &[&str] = &["json"];

    fn no_formats() -> IndexMap<String, ConfigParser> {
        IndexMap::new()
    }

    /// A toy format: `key = value` lines, with JSON values.
    fn key_values(text: &str) -> Result<Value, String> {
        let mut options = Map::new();
        for line in text.lines().filter(|line| !line.trim().is_empty()) {
            let (key, value) = line
                .split_once('=')
                .ok_or_else(|| format!("no '=' in {line:?}"))?;
            let value = serde_json::from_str(value.trim()).map_err(|e| e.to_string())?;
            options.insert(key.trim().to_owned(), value);
        }
        Ok(Value::Object(options))
    }

    #[test]
    fn searches_the_project_then_the_user() {
        let paths = search_paths(
            "myapp",
            Some(Path::new(".")),
            Some("/xdg".into()),
            Some("/home/me".into()),
            JSON,
        );
        assert_eq!(
            strings(&paths),
            [
                "./.config/myapp.json",
                "./.config/myapp/config.json",
                "/xdg/myapp.json",
                "/xdg/myapp/config.json",
            ]
        );
        let user = search_paths("my.app", None, None, Some("/home/me".into()), JSON);
        assert_eq!(
            strings(&user),
            [
                "/home/me/.config/my.app.json",
                "/home/me/.config/my.app/config.json"
            ]
        );
    }

    #[test]
    fn ignores_an_empty_or_relative_xdg_config_home() {
        let home = || Some(PathBuf::from("/home/me"));
        let first = |xdg: &str| {
            strings(&search_paths("myapp", None, Some(xdg.into()), home(), JSON))[0].clone()
        };
        assert_eq!(first(""), "/home/me/.config/myapp.json");
        assert_eq!(first("relative/dir"), "/home/me/.config/myapp.json");
        assert_eq!(first("/abs/xdg"), "/abs/xdg/myapp.json");
        assert!(search_paths("myapp", None, None, None, JSON).is_empty());
    }

    #[test]
    fn merges_configs_with_the_project_winning() {
        let dir = TempDir::new("merge");
        dir.write(
            "user/myapp/config.json",
            r#"{"host": "user", "port": 8080, "db": {"host": "localhost", "port": 5432}}"#,
        );
        dir.write(
            "project/.config/myapp.json",
            r#"{"host": "project", "db": {"host": "prod"}, "extra": true}"#,
        );
        let paths = search_paths(
            "myapp",
            Some(&dir.0.join("project")),
            Some(dir.0.join("user").into()),
            None,
            JSON,
        );

        assert_eq!(
            load(&paths, QUIET, &no_formats()),
            json!({
                "host": "project",
                "port": 8080,
                "db": {"host": "prod", "port": 5432},
                "extra": true
            })
            .as_object()
            .cloned()
        );
        let first = ConfigOptions {
            merge: false,
            ..QUIET
        };
        assert_eq!(
            load(&paths, first, &no_formats()),
            json!({"host": "project", "db": {"host": "prod"}, "extra": true})
                .as_object()
                .cloned()
        );
    }

    #[test]
    fn skips_configs_that_are_empty_or_not_a_mapping() {
        let dir = TempDir::new("invalid");
        dir.write("empty.json", "null");
        dir.write("list.json", "[1, 2]");
        dir.write("broken.json", "{");
        assert_eq!(parse(&dir.0.join("empty.json"), &no_formats()), Ok(None));
        assert_eq!(
            parse(&dir.0.join("list.json"), &no_formats()),
            Err("expected a mapping of option defaults at the top level".into())
        );
        assert!(parse(&dir.0.join("broken.json"), &no_formats()).is_err());

        for name in ["empty", "list", "broken", "missing"] {
            let paths = search_paths(name, None, Some(dir.0.clone().into()), None, JSON);
            assert_eq!(load(&paths, QUIET, &no_formats()), None, "{name}");
        }
    }

    #[test]
    fn looks_for_added_formats_ahead_of_json() {
        let mut cli = Cli::new("myapp");
        cli.config_format("kv", key_values)
            .config_format("json", |text: &str| serde_json::from_str::<Value>(text))
            .config_format("ini", key_values);
        let exts = extensions(&cli.config_formats);
        assert_eq!(exts, ["kv", "ini", "json"]);
        assert_eq!(
            strings(&search_paths(
                "myapp",
                None,
                Some("/xdg".into()),
                None,
                &exts
            )),
            [
                "/xdg/myapp.kv",
                "/xdg/myapp.ini",
                "/xdg/myapp.json",
                "/xdg/myapp/config.kv",
                "/xdg/myapp/config.ini",
                "/xdg/myapp/config.json",
            ]
        );
    }

    #[test]
    fn reads_configs_in_added_formats() {
        let dir = TempDir::new("formats");
        dir.write("myapp.kv", "host = \"kv\"\nport = 8080\n");
        dir.write("myapp.json", r#"{"host": "json", "debug": true}"#);
        dir.write("broken.kv", "not a pair\n");
        let mut cli = Cli::new("myapp");
        cli.config_format("kv", key_values);
        let exts = extensions(&cli.config_formats);
        let paths = search_paths("myapp", None, Some(dir.0.clone().into()), None, &exts);

        assert_eq!(
            load(&paths, QUIET, &cli.config_formats),
            json!({"host": "kv", "port": 8080, "debug": true})
                .as_object()
                .cloned()
        );
        let first = ConfigOptions {
            merge: false,
            ..QUIET
        };
        assert_eq!(
            load(&paths, first, &cli.config_formats),
            json!({"host": "kv", "port": 8080}).as_object().cloned()
        );
        assert_eq!(
            parse(&dir.0.join("broken.kv"), &cli.config_formats),
            Err("no '=' in \"not a pair\"".into())
        );
    }

    #[test]
    fn a_json_format_replaces_the_json_parser() {
        let dir = TempDir::new("jsonc");
        dir.write("myapp.json", "// a comment\n{\"host\": \"jsonc\"}\n");
        let mut cli = Cli::new("myapp");
        let paths = search_paths("myapp", None, Some(dir.0.clone().into()), None, JSON);
        assert_eq!(load(&paths, QUIET, &cli.config_formats), None);

        cli.config_format("json", |text: &str| {
            let lines: Vec<&str> = text
                .lines()
                .filter(|line| !line.trim_start().starts_with("//"))
                .collect();
            serde_json::from_str::<Value>(&lines.join("\n"))
        });
        assert_eq!(extensions(&cli.config_formats), JSON);
        assert_eq!(
            load(&paths, QUIET, &cli.config_formats),
            json!({"host": "jsonc"}).as_object().cloned()
        );
    }

    #[test]
    #[should_panic(expected = "the extension alone")]
    fn a_format_is_named_by_its_extension_alone() {
        Cli::new("myapp").config_format(".toml", key_values);
    }
}
