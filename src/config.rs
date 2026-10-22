//! Option defaults from config files, in the XDG locations.

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use crate::Cli;

/// The file types a config can be, in the order they're looked for.
#[cfg(feature = "yaml")]
const EXTENSIONS: &[&str] = &["yaml", "yml", "json"];
#[cfg(not(feature = "yaml"))]
const EXTENSIONS: &[&str] = &["json"];

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
        load(&self.config_paths(options), options)
    }

    /// The files [`load_config_with`](Cli::load_config_with) looks for,
    /// highest precedence first. For a program `app`, each of `app` and
    /// `app/config` with `.yaml`, `.yml`, and `.json` (JSON only, without
    /// the `yaml` feature): in `./.config` when `options.project` is set,
    /// then in `$XDG_CONFIG_HOME`, or `~/.config` when that is unset or
    /// not absolute.
    pub fn config_paths(&self, options: ConfigOptions) -> Vec<PathBuf> {
        search_paths(
            &self.program_name,
            options.project.then(|| Path::new(".")),
            std::env::var_os("XDG_CONFIG_HOME"),
            std::env::home_dir(),
        )
    }
}

/// The config paths for `program`, with the project's under `project` and
/// the user's under `xdg`, or `home`'s `.config` failing that.
fn search_paths(
    program: &str,
    project: Option<&Path>,
    xdg: Option<OsString>,
    home: Option<PathBuf>,
) -> Vec<PathBuf> {
    // Per the XDG spec, an empty or relative XDG_CONFIG_HOME is ignored.
    let user = xdg
        .map(PathBuf::from)
        .filter(|dir| dir.is_absolute())
        .or_else(|| home.map(|home| home.join(".config")));
    let dirs = project
        .map(|dir| dir.join(".config"))
        .into_iter()
        .chain(user);
    let bases: Vec<PathBuf> = dirs
        .flat_map(|dir| [dir.join(program), dir.join(program).join("config")])
        .collect();
    bases
        .iter()
        .flat_map(|base| EXTENSIONS.iter().map(move |ext| with_extension(base, ext)))
        .collect()
}

/// `base` with `.ext` appended, keeping any dot already in its name
/// (`my.app` becomes `my.app.yaml`).
fn with_extension(base: &Path, ext: &str) -> PathBuf {
    let mut path = base.as_os_str().to_owned();
    path.push(".");
    path.push(ext);
    PathBuf::from(path)
}

fn load(paths: &[PathBuf], options: ConfigOptions) -> Option<Map<String, Value>> {
    if !options.merge {
        let path = paths.iter().find(|path| path.exists())?;
        return read(path, options.warnings);
    }
    // The lowest precedence first, so each config overrides the ones before.
    let mut merged = Map::new();
    for path in paths.iter().rev().filter(|path| path.exists()) {
        if let Some(config) = read(path, options.warnings) {
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
fn read(path: &Path, warnings: bool) -> Option<Map<String, Value>> {
    match parse(path) {
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

/// A config file's options; None for an empty file, which configures
/// nothing.
fn parse(path: &Path) -> Result<Option<Map<String, Value>>, String> {
    let content = fs::read_to_string(path).map_err(|error| error.to_string())?;
    let config = if crate::cli::is_yaml_path(path) {
        yaml_document(&content)?
    } else {
        serde_json::from_str(&content).map_err(|error| error.to_string())?
    };
    match config {
        Value::Null => Ok(None),
        Value::Object(options) => Ok(Some(options)),
        _ => Err("expected a mapping of option defaults at the top level".into()),
    }
}

#[cfg(feature = "yaml")]
fn yaml_document(content: &str) -> Result<Value, String> {
    crate::yaml::document(content).map_err(|error| error.to_string())
}

#[cfg(not(feature = "yaml"))]
fn yaml_document(_content: &str) -> Result<Value, String> {
    Err("YAML configs need jargon's `yaml` feature".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A scratch directory, removed when dropped.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(name: &str) -> TempDir {
            let dir =
                std::env::temp_dir().join(format!("jargon-config-{}-{name}", std::process::id()));
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

    #[test]
    #[cfg(feature = "yaml")]
    fn searches_the_project_then_the_user() {
        let paths = search_paths(
            "myapp",
            Some(Path::new(".")),
            Some("/xdg".into()),
            Some("/home/me".into()),
        );
        assert_eq!(
            strings(&paths),
            [
                "./.config/myapp.yaml",
                "./.config/myapp.yml",
                "./.config/myapp.json",
                "./.config/myapp/config.yaml",
                "./.config/myapp/config.yml",
                "./.config/myapp/config.json",
                "/xdg/myapp.yaml",
                "/xdg/myapp.yml",
                "/xdg/myapp.json",
                "/xdg/myapp/config.yaml",
                "/xdg/myapp/config.yml",
                "/xdg/myapp/config.json",
            ]
        );
        let user = search_paths("my.app", None, None, Some("/home/me".into()));
        assert_eq!(strings(&user)[0], "/home/me/.config/my.app.yaml");
        assert_eq!(user.len(), 6);
    }

    #[test]
    fn ignores_an_empty_or_relative_xdg_config_home() {
        let home = || Some(PathBuf::from("/home/me"));
        let first =
            |xdg: &str| strings(&search_paths("myapp", None, Some(xdg.into()), home()))[0].clone();
        let fallback = format!("/home/me/.config/myapp.{}", EXTENSIONS[0]);
        assert_eq!(first(""), fallback);
        assert_eq!(first("relative/dir"), fallback);
        assert_eq!(
            first("/abs/xdg"),
            format!("/abs/xdg/myapp.{}", EXTENSIONS[0])
        );
        assert!(search_paths("myapp", None, None, None).is_empty());
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
        );

        assert_eq!(
            load(&paths, QUIET),
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
            load(&paths, first),
            json!({"host": "project", "db": {"host": "prod"}, "extra": true})
                .as_object()
                .cloned()
        );
    }

    #[test]
    #[cfg(feature = "yaml")]
    fn reads_yaml_first() {
        let dir = TempDir::new("yaml");
        dir.write("myapp.yaml", "source: yaml\nport: 8080\ndebug: yes\n");
        dir.write("myapp.json", r#"{"source": "json"}"#);
        dir.write("blank.yaml", "");
        assert_eq!(parse(&dir.0.join("blank.yaml")), Ok(None));
        let paths = search_paths("myapp", None, Some(dir.0.clone().into()), None);
        let first = ConfigOptions {
            merge: false,
            ..QUIET
        };
        assert_eq!(
            load(&paths, first),
            json!({"source": "yaml", "port": 8080, "debug": true})
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
        assert_eq!(parse(&dir.0.join("empty.json")), Ok(None));
        assert_eq!(
            parse(&dir.0.join("list.json")),
            Err("expected a mapping of option defaults at the top level".into())
        );
        assert!(parse(&dir.0.join("broken.json")).is_err());

        for name in ["empty", "list", "broken", "missing"] {
            let paths = search_paths(name, None, Some(dir.0.clone().into()), None);
            assert_eq!(load(&paths, QUIET), None, "{name}");
        }
    }
}
