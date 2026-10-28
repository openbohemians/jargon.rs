//! Parsed schemas: a command's options as a tree of [`Property`] nodes.

use std::fmt;

use fancy_regex::Regex;
use indexmap::IndexMap;
use serde_json::{Map, Value};

use crate::Error;

/// The JSON Schema primitive types Jargon recognizes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Type {
    /// `"string"`, and the type of a node that names none and has neither
    /// `properties` nor `items`, or names one Jargon doesn't know.
    String,
    /// `"integer"`: a whole number.
    Integer,
    /// `"number"`: any number.
    Number,
    /// `"boolean"`: a flag, true when given without a value.
    Boolean,
    /// `"array"`: a list, an item each time its option is given (`-I a -I b`)
    /// or several split at a [`Separator`], or a variadic positional.
    Array,
    /// `"object"`: a group of options, given as `--group.key`.
    Object,
    /// `"null"`.
    Null,
}

impl Type {
    /// A `type` keyword's type; an unknown name is a string.
    fn from_name(name: &str) -> Type {
        match name.to_lowercase().as_str() {
            "integer" => Type::Integer,
            "number" => Type::Number,
            "boolean" => Type::Boolean,
            "array" => Type::Array,
            "object" => Type::Object,
            "null" => Type::Null,
            _ => Type::String,
        }
    }

    /// The type's name in a schema (`"integer"`), which messages use too.
    pub fn name(self) -> &'static str {
        match self {
            Type::String => "string",
            Type::Integer => "integer",
            Type::Number => "number",
            Type::Boolean => "boolean",
            Type::Array => "array",
            Type::Object => "object",
            Type::Null => "null",
        }
    }
}

impl fmt::Display for Type {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// What a `separator` keyword names: the characters at which an array
/// option's value is split into items (`"separator": "comma"` reads
/// `-f 1,3` as `[1, 3]`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Separator {
    /// `,`
    Comma,
    /// `:`
    Colon,
    /// `;`
    Semicolon,
    /// Any whitespace: spaces, tabs, newlines.
    Whitespace,
}

impl Separator {
    const ALL: [Separator; 4] = [
        Separator::Comma,
        Separator::Colon,
        Separator::Semicolon,
        Separator::Whitespace,
    ];

    /// The separator's name in a schema (`"comma"`), which help uses too.
    pub fn name(self) -> &'static str {
        match self {
            Separator::Comma => "comma",
            Separator::Colon => "colon",
            Separator::Semicolon => "semicolon",
            Separator::Whitespace => "whitespace",
        }
    }

    /// Whether a value splits at `c`.
    pub fn splits_at(self, c: char) -> bool {
        match self {
            Separator::Comma => c == ',',
            Separator::Colon => c == ':',
            Separator::Semicolon => c == ';',
            Separator::Whitespace => c.is_whitespace(),
        }
    }
}

/// Split an array option's value at its separators, trimming each piece and
/// dropping empty ones, so `"a, b,,c"` is `["a", "b", "c"]`.
pub(crate) fn split(value: &str, separators: &[Separator]) -> Vec<String> {
    value
        .split(|c| separators.iter().any(|s| s.splits_at(c)))
        .map(str::trim)
        .filter(|piece| !piece.is_empty())
        .map(String::from)
        .collect()
}

/// A `pattern` keyword: its regex and the source it was compiled from.
#[derive(Debug, Clone)]
pub struct Pattern {
    source: String,
    regex: Regex,
}

impl Pattern {
    /// The pattern as the schema wrote it.
    pub fn source(&self) -> &str {
        &self.source
    }

    /// Whether the pattern matches anywhere in `text`. An error means the
    /// match ran past the backtracking limit, which only patterns with
    /// lookaround or backreferences can.
    pub fn matches(&self, text: &str) -> Result<bool, fancy_regex::Error> {
        self.regex.is_match(text)
    }
}

/// One node of a parsed schema: a single option, field, or positional, or
/// the root object. It holds its type, validation constraints, CLI hints
/// (`short`, `env`), nested `properties` and `items`, and any `x-`
/// extensions a program defines.
///
/// It is read, not built: make one with [`Property::from_value`], or a
/// whole [`Schema`]. More fields may come with new keywords.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Property {
    /// The field's key, `"items"` for an array's items, or `"root"`.
    pub name: String,
    /// The `type` keyword, or the type `properties` or `items` imply.
    pub kind: Type,
    /// The `description` keyword, which help shows beside the option.
    pub description: Option<String>,
    /// Whether the enclosing object's `required` list names this field.
    pub required: bool,
    /// The `default` keyword: the value when no argument, environment
    /// variable, or caller's default gives one.
    pub default: Option<Value>,
    /// The `enum` keyword.
    pub enum_values: Option<Vec<Value>>,
    /// An object's `properties`, in the order the schema lists them.
    pub properties: Option<IndexMap<String, Property>>,
    /// An array's `items`: the schema each element must meet.
    pub items: Option<Box<Property>>,
    /// The `$ref` keyword.
    pub reference: Option<String>,
    /// A one-letter flag (`-v`).
    pub short: Option<String>,
    /// An environment variable that supplies the value.
    pub env: Option<String>,
    /// The `separator` keyword: where an array's value splits into items.
    /// None takes each value as one item.
    pub separator: Option<Vec<Separator>>,
    /// The `minimum` keyword: the least value allowed.
    pub minimum: Option<f64>,
    /// The `maximum` keyword: the greatest value allowed.
    pub maximum: Option<f64>,
    /// The `exclusiveMinimum` keyword: values must be greater.
    pub exclusive_minimum: Option<f64>,
    /// The `exclusiveMaximum` keyword: values must be less.
    pub exclusive_maximum: Option<f64>,
    /// The `multipleOf` keyword.
    pub multiple_of: Option<f64>,
    /// The `minLength` keyword, in characters.
    pub min_length: Option<u64>,
    /// The `maxLength` keyword, in characters.
    pub max_length: Option<u64>,
    /// The `minItems` keyword.
    pub min_items: Option<u64>,
    /// The `maxItems` keyword.
    pub max_items: Option<u64>,
    /// The `uniqueItems` keyword.
    pub unique_items: bool,
    /// The `pattern` keyword, compiled.
    pub pattern: Option<Pattern>,
    /// The `const` keyword.
    pub constant: Option<Value>,
    /// The `format` keyword (`email`, `uri`, `date`, `path`, ...); a format
    /// Jargon doesn't know accepts any string.
    pub format: Option<String>,
    /// The `additionalProperties` keyword, when it is a boolean: false
    /// makes keys the schema doesn't name an error.
    pub additional_properties: Option<bool>,
    /// A hint that the command runs a long-lived service.
    pub service: bool,
    /// Keys starting `x-`, kept as written for the program to read; parsing
    /// and validation ignore them.
    pub extensions: Map<String, Value>,
}

impl Property {
    /// Build a property from a JSON Schema node. `name` is the field's key
    /// (or `"root"`); `required` is the enclosing object's `required` list.
    pub fn from_value(name: &str, json: &Value, required: &[String]) -> Result<Property, Error> {
        let Some(obj) = json.as_object() else {
            return Err(Error::Schema(format!("Schema '{name}' must be an object")));
        };
        let kind = resolve_type(name, obj)?;

        Ok(Property {
            name: name.to_owned(),
            kind,
            description: string(obj, "description"),
            required: required.iter().any(|field| field == name),
            default: obj.get("default").cloned(),
            enum_values: obj.get("enum").and_then(Value::as_array).cloned(),
            properties: parse_properties(kind, obj)?,
            items: parse_items(kind, obj)?,
            reference: string(obj, "$ref"),
            short: string(obj, "short"),
            env: string(obj, "env"),
            separator: parse_separator(name, kind, obj)?,
            minimum: obj.get("minimum").and_then(Value::as_f64),
            maximum: obj.get("maximum").and_then(Value::as_f64),
            exclusive_minimum: obj.get("exclusiveMinimum").and_then(Value::as_f64),
            exclusive_maximum: obj.get("exclusiveMaximum").and_then(Value::as_f64),
            multiple_of: obj.get("multipleOf").and_then(Value::as_f64),
            min_length: obj.get("minLength").and_then(Value::as_u64),
            max_length: obj.get("maxLength").and_then(Value::as_u64),
            min_items: obj.get("minItems").and_then(Value::as_u64),
            max_items: obj.get("maxItems").and_then(Value::as_u64),
            unique_items: obj
                .get("uniqueItems")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            pattern: parse_pattern(obj)?,
            constant: obj.get("const").cloned(),
            format: string(obj, "format"),
            additional_properties: obj.get("additionalProperties").and_then(Value::as_bool),
            service: obj.get("service").and_then(Value::as_bool).unwrap_or(false),
            extensions: obj
                .iter()
                .filter(|(key, _)| key.starts_with("x-"))
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect(),
        })
    }
}

fn string(obj: &Map<String, Value>, key: &str) -> Option<String> {
    obj.get(key).and_then(Value::as_str).map(String::from)
}

/// A keyword holding a list of names (`required`, `positional`).
pub(crate) fn string_list(obj: &Map<String, Value>, key: &str) -> Result<Vec<String>, Error> {
    let Some(value) = obj.get(key) else {
        return Ok(Vec::new());
    };
    value
        .as_array()
        .and_then(|items| {
            items
                .iter()
                .map(|item| item.as_str().map(String::from))
                .collect()
        })
        .ok_or_else(|| Error::Schema(format!("'{key}' must be an array of strings")))
}

/// When `type` is omitted, `properties` or `items` imply it, as in JSON
/// Schema: an omitted type isn't a string when the node has a shape.
fn resolve_type(name: &str, obj: &Map<String, Value>) -> Result<Type, Error> {
    let declared = obj.get("type").and_then(Value::as_str);
    let implied = if obj.contains_key("properties") {
        "object"
    } else if obj.contains_key("items") {
        "array"
    } else {
        "string"
    };
    let kind = Type::from_name(declared.unwrap_or(implied));

    if let Some(declared) = declared {
        if kind != Type::Object && obj.contains_key("properties") {
            return Err(Error::Schema(format!(
                "Schema '{name}' declares type '{declared}' but has 'properties' (did you mean type: object?)"
            )));
        }
        if kind != Type::Array && obj.contains_key("items") {
            return Err(Error::Schema(format!(
                "Schema '{name}' declares type '{declared}' but has 'items' (did you mean type: array?)"
            )));
        }
    }
    Ok(kind)
}

/// `separator`: one name or a list of them, on an array.
fn parse_separator(
    name: &str,
    kind: Type,
    obj: &Map<String, Value>,
) -> Result<Option<Vec<Separator>>, Error> {
    let Some(value) = obj.get("separator") else {
        return Ok(None);
    };
    if obj.contains_key("$ref") {
        return Err(Error::Schema(format!(
            "Schema '{name}' has 'separator' beside '$ref' (put it in the referenced schema)"
        )));
    }
    if kind != Type::Array {
        return Err(Error::Schema(format!(
            "Schema '{name}' has 'separator' but isn't an array (did you mean type: array?)"
        )));
    }

    let entries = value
        .as_array()
        .map_or(std::slice::from_ref(value), Vec::as_slice);
    let mut separators: Vec<Separator> = Vec::new();
    for entry in entries {
        let word = entry
            .as_str()
            .map_or_else(|| entry.to_string(), String::from);
        let Some(separator) = Separator::ALL.into_iter().find(|s| s.name() == word) else {
            let names: Vec<_> = Separator::ALL.iter().map(|s| s.name()).collect();
            return Err(Error::Schema(format!(
                "Schema '{name}' has unknown separator '{word}' (use {})",
                names.join(", ")
            )));
        };
        if !separators.contains(&separator) {
            separators.push(separator);
        }
    }
    Ok((!separators.is_empty()).then_some(separators))
}

fn parse_properties(
    kind: Type,
    obj: &Map<String, Value>,
) -> Result<Option<IndexMap<String, Property>>, Error> {
    let (Type::Object, Some(props)) = (kind, obj.get("properties")) else {
        return Ok(None);
    };
    let Some(props) = props.as_object() else {
        return Err(Error::Schema("'properties' must be an object".into()));
    };
    let required = string_list(obj, "required")?;
    props
        .iter()
        .map(|(name, schema)| Ok((name.clone(), Property::from_value(name, schema, &required)?)))
        .collect::<Result<_, _>>()
        .map(Some)
}

fn parse_items(kind: Type, obj: &Map<String, Value>) -> Result<Option<Box<Property>>, Error> {
    match (kind, obj.get("items")) {
        (Type::Array, Some(items)) => {
            Ok(Some(Box::new(Property::from_value("items", items, &[])?)))
        }
        _ => Ok(None),
    }
}

fn parse_pattern(obj: &Map<String, Value>) -> Result<Option<Pattern>, Error> {
    let Some(source) = obj.get("pattern").and_then(Value::as_str) else {
        return Ok(None);
    };
    let regex = Regex::new(source)
        .map_err(|e| Error::Schema(format!("Invalid pattern '{source}': {e}")))?;
    Ok(Some(Pattern {
        source: source.to_owned(),
        regex,
    }))
}

/// A parsed JSON Schema describing one command: its `root` property (the
/// object whose properties are the command's options), the names of its
/// `positional` arguments, and the `definitions` (`$defs`) that `$ref`
/// resolves against.
///
/// It is read, not built: make one with [`Schema::from_json`] or
/// [`Schema::from_value`]. More fields may come with new keywords.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Schema {
    /// The whole document, as a property named `"root"`.
    pub root: Property,
    /// The document's `definitions` or `$defs`, by name.
    pub definitions: IndexMap<String, Property>,
    /// The `positional` keyword: the fields bare arguments fill, in order.
    pub positional: Vec<String>,
}

impl Schema {
    /// Build a schema from a JSON Schema document.
    pub fn from_json(json: &str) -> Result<Schema, Error> {
        let value: Value = serde_json::from_str(json).map_err(Error::syntax)?;
        Schema::from_value(&value)
    }

    /// Build a schema from an already-parsed document. The whole document is
    /// the root property; `positional` and `definitions`/`$defs` are lifted
    /// out beside it.
    pub fn from_value(json: &Value) -> Result<Schema, Error> {
        let Some(obj) = json.as_object() else {
            return Err(Error::Schema("A schema must be an object".into()));
        };
        let required = string_list(obj, "required")?;
        let positional = string_list(obj, "positional")?;
        let root = Property::from_value("root", json, &required)?;

        let definitions = match obj.get("definitions").or_else(|| obj.get("$defs")) {
            Some(defs) => defs
                .as_object()
                .ok_or_else(|| Error::Schema("'definitions' must be an object".into()))?
                .iter()
                .map(|(name, schema)| Ok((name.clone(), Property::from_value(name, schema, &[])?)))
                .collect::<Result<_, Error>>()?,
            None => IndexMap::new(),
        };

        Ok(Schema {
            root,
            definitions,
            positional,
        })
    }

    /// The definition a local `$ref` (`#/$defs/Name`) points to; None for a
    /// pointer that isn't local or names nothing.
    pub fn resolve_ref(&self, reference: &str) -> Option<&Property> {
        let parts: Vec<&str> = reference.strip_prefix("#/")?.split('/').collect();
        match parts.as_slice() {
            ["definitions" | "$defs", name, ..] => self.definitions.get(*name),
            _ => None,
        }
    }

    /// The property itself, or what its `$ref` points to.
    pub fn resolve<'a>(&'a self, prop: &'a Property) -> &'a Property {
        prop.reference
            .as_deref()
            .and_then(|r| self.resolve_ref(r))
            .unwrap_or(prop)
    }

    /// The root's properties, through its `$ref`.
    pub(crate) fn properties(&self) -> Option<&IndexMap<String, Property>> {
        self.resolve(&self.root).properties.as_ref()
    }
}
