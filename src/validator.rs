//! Validation of data against a schema, usable without the parser: the same
//! checks [`Cli::parse`](crate::Cli::parse) applies to what it parsed.

use std::collections::HashSet;
use std::sync::LazyLock;

use fancy_regex::Regex;
use indexmap::IndexMap;
use serde_json::{Map, Value};

use crate::schema::{Property, Schema, Type};
use crate::values::{self, format_num};

/// Validate `data` against `schema`, returning every error (none when it
/// is valid).
pub fn validate(data: &Map<String, Value>, schema: &Schema) -> Vec<String> {
    validate_except(data, schema, &HashSet::new())
}

/// [`validate`], except the fields in `skip` (paths such as
/// `server.port`): those whose argument didn't convert to their type,
/// which the parser has already reported.
pub(crate) fn validate_except(
    data: &Map<String, Value>,
    schema: &Schema,
    skip: &HashSet<String>,
) -> Vec<String> {
    let mut errors = Vec::new();
    let root = schema.resolve(&schema.root);
    let Some(props) = &root.properties else {
        return errors;
    };

    check_additional_properties(data, root, &mut errors, "", props);
    for (name, prop) in props {
        validate_property(
            data,
            name,
            schema.resolve(prop),
            &mut errors,
            "",
            schema,
            skip,
        );
    }
    errors
}

fn validate_property(
    data: &Map<String, Value>,
    name: &str,
    prop: &Property,
    errors: &mut Vec<String>,
    prefix: &str,
    schema: &Schema,
    skip: &HashSet<String>,
) {
    let full_name = if prefix.is_empty() {
        name.to_owned()
    } else {
        format!("{prefix}.{name}")
    };
    if skip.contains(&full_name) {
        return;
    }
    let Some(value) = data.get(name) else {
        if prop.required {
            errors.push(format!("Missing required field: {full_name}"));
        }
        return;
    };

    validate_type_match(value, prop, errors, &full_name);
    validate_const(value, prop, errors, &full_name);
    validate_enum(value, prop, errors, &full_name);
    validate_numeric(value, prop, errors, &full_name);
    validate_string(value, prop, errors, &full_name);
    validate_array(value, prop, errors, &full_name);
    validate_object(value, prop, errors, &full_name, schema, skip);
}

fn validate_array_item(value: &Value, prop: &Property, errors: &mut Vec<String>, label: &str) {
    validate_type_match(value, prop, errors, label);
    validate_enum(value, prop, errors, label);
    validate_numeric(value, prop, errors, label);
    validate_string(value, prop, errors, label);
}

fn validate_type_match(value: &Value, prop: &Property, errors: &mut Vec<String>, label: &str) {
    if !valid_type(value, prop.kind) {
        errors.push(format!(
            "Invalid type for {label}: expected {}, got {}",
            prop.kind,
            values::type_name(value)
        ));
    }
}

fn validate_const(value: &Value, prop: &Property, errors: &mut Vec<String>, label: &str) {
    if let Some(constant) = &prop.constant
        && !values::same(value, constant)
    {
        errors.push(format!(
            "Value for {label} must be {}",
            values::display(constant)
        ));
    }
}

fn validate_enum(value: &Value, prop: &Property, errors: &mut Vec<String>, label: &str) {
    if let Some(choices) = &prop.enum_values
        && !choices.iter().any(|choice| values::same(value, choice))
    {
        let formatted: Vec<String> = choices.iter().map(values::display).collect();
        errors.push(format!(
            "Invalid value for {label}: must be one of {}",
            formatted.join(", ")
        ));
    }
}

fn validate_array(value: &Value, prop: &Property, errors: &mut Vec<String>, label: &str) {
    let (Type::Array, Some(items)) = (prop.kind, value.as_array()) else {
        return;
    };

    if let Some(min) = prop.min_items
        && (items.len() as u64) < min
    {
        errors.push(format!("{label} must have at least {min} items"));
    }
    if let Some(max) = prop.max_items
        && (items.len() as u64) > max
    {
        errors.push(format!("{label} must have at most {max} items"));
    }
    validate_unique_items(items, prop, errors, label);

    if let Some(item_prop) = &prop.items {
        for (i, item) in items.iter().enumerate() {
            validate_array_item(item, item_prop, errors, &format!("{label}[{i}]"));
        }
    }
}

fn validate_unique_items(items: &[Value], prop: &Property, errors: &mut Vec<String>, label: &str) {
    if !prop.unique_items {
        return;
    }
    let mut seen = HashSet::new();
    for item in items {
        if !seen.insert(values::to_json(item)) {
            errors.push(format!(
                "{label} must have unique items (duplicate: {})",
                values::display(item)
            ));
            break;
        }
    }
}

fn validate_object(
    value: &Value,
    prop: &Property,
    errors: &mut Vec<String>,
    label: &str,
    schema: &Schema,
    skip: &HashSet<String>,
) {
    let (Type::Object, Some(nested_props), Some(nested_data)) =
        (prop.kind, &prop.properties, value.as_object())
    else {
        return;
    };

    check_additional_properties(nested_data, prop, errors, label, nested_props);
    for (nested_name, nested_prop) in nested_props {
        validate_property(
            nested_data,
            nested_name,
            schema.resolve(nested_prop),
            errors,
            label,
            schema,
            skip,
        );
    }
}

/// Numeric constraints. `label` is the field or item path messages use.
fn validate_numeric(value: &Value, prop: &Property, errors: &mut Vec<String>, label: &str) {
    if !matches!(prop.kind, Type::Integer | Type::Number) {
        return;
    }
    let Some(num) = value.as_f64() else {
        return;
    };

    if let Some(min) = prop.minimum
        && num < min
    {
        errors.push(format!("Value for {label} must be >= {}", format_num(min)));
    }
    if let Some(max) = prop.maximum
        && num > max
    {
        errors.push(format!("Value for {label} must be <= {}", format_num(max)));
    }
    if let Some(min) = prop.exclusive_minimum
        && num <= min
    {
        errors.push(format!("Value for {label} must be > {}", format_num(min)));
    }
    if let Some(max) = prop.exclusive_maximum
        && num >= max
    {
        errors.push(format!("Value for {label} must be < {}", format_num(max)));
    }
    if let Some(multiple) = prop.multiple_of {
        // Compare the quotient to the nearest integer within a few ulps:
        // float modulo misreports decimal multiples (0.3 % 0.1 is about 0.1).
        let quotient = num / multiple;
        if (quotient - quotient.round()).abs() > 4.0 * f64::EPSILON * quotient.abs().max(1.0) {
            errors.push(format!(
                "Value for {label} must be a multiple of {}",
                format_num(multiple)
            ));
        }
    }
}

/// String constraints (minLength, maxLength, pattern, format). `label` is
/// the field or item path messages use.
fn validate_string(value: &Value, prop: &Property, errors: &mut Vec<String>, label: &str) {
    let (Type::String, Some(text)) = (prop.kind, value.as_str()) else {
        return;
    };
    let length = text.chars().count() as u64;

    if let Some(min) = prop.min_length
        && length < min
    {
        errors.push(format!(
            "Value for {label} must be at least {min} characters"
        ));
    }
    if let Some(max) = prop.max_length
        && length > max
    {
        errors.push(format!(
            "Value for {label} must be at most {max} characters"
        ));
    }
    if let Some(pattern) = &prop.pattern {
        match pattern.matches(text) {
            Ok(true) => {}
            Ok(false) => errors.push(format!(
                "Value for {label} must match pattern: {}",
                pattern.source()
            )),
            Err(e) => errors.push(format!(
                "Value for {label} could not be checked against pattern: {e}"
            )),
        }
    }
    if let Some(format) = &prop.format
        && !valid_format(text, format)
    {
        errors.push(format!("Value for {label} must be a valid {format}"));
    }
}

fn check_additional_properties(
    data: &Map<String, Value>,
    prop: &Property,
    errors: &mut Vec<String>,
    prefix: &str,
    known: &IndexMap<String, Property>,
) {
    if prop.additional_properties != Some(false) {
        return;
    }
    for key in data.keys() {
        if !known.contains_key(key) {
            let full_name = if prefix.is_empty() {
                key.clone()
            } else {
                format!("{prefix}.{key}")
            };
            errors.push(format!(
                "Unknown property '{full_name}': additionalProperties is false"
            ));
        }
    }
}

fn valid_type(value: &Value, expected: Type) -> bool {
    match expected {
        Type::String => value.is_string(),
        Type::Integer => value.as_i64().is_some(),
        Type::Number => value.is_number(),
        Type::Boolean => value.is_boolean(),
        Type::Array => value.is_array(),
        Type::Object => value.is_object(),
        Type::Null => value.is_null(),
    }
}

fn valid_format(value: &str, format: &str) -> bool {
    match format {
        "email" => valid_format_email(value),
        "uri" | "url" => valid_format_uri(value),
        "uuid" => valid_format_uuid(value),
        "date" => valid_format_date(value),
        "time" => valid_format_time(value),
        "date-time" => valid_format_date_time(value),
        "ipv4" => valid_format_ipv4(value),
        "ipv6" => valid_format_ipv6(value),
        "hostname" => valid_format_hostname(value),
        "path" => true, // Any string is a path; `~` expands when parsed
        _ => true,      // Unknown formats pass, as JSON Schema says
    }
}

/// Compile one of the fixed format regexes.
fn regex(source: &str) -> Regex {
    Regex::new(source).expect("a built-in format regex compiles")
}

/// A format check; one that exhausts the backtracking limit fails.
fn format_match(value: &str, regex: &Regex) -> bool {
    regex.is_match(value).unwrap_or(false)
}

/// The RFC 5321 length cap keeps the repeated label group's work bounded.
fn valid_format_email(value: &str) -> bool {
    static EMAIL: LazyLock<Regex> = LazyLock::new(|| regex(r"\A[^@\s]+@[^@\s.]+(\.[^@\s.]+)+\z"));
    value.len() <= 254 && format_match(value, &EMAIL)
}

fn valid_format_uri(value: &str) -> bool {
    static URI: LazyLock<Regex> = LazyLock::new(|| regex(r"(?i)\A[a-z][a-z0-9+.-]*://.+\z"));
    format_match(value, &URI)
}

fn valid_format_uuid(value: &str) -> bool {
    static UUID: LazyLock<Regex> = LazyLock::new(|| {
        regex(r"(?i)\A[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}\z")
    });
    format_match(value, &UUID)
}

fn valid_format_date(value: &str) -> bool {
    static DATE: LazyLock<Regex> = LazyLock::new(|| regex(r"\A[0-9]{4}-[0-9]{2}-[0-9]{2}\z"));
    format_match(value, &DATE)
}

fn valid_format_time(value: &str) -> bool {
    static TIME: LazyLock<Regex> =
        LazyLock::new(|| regex(r"\A[0-9]{2}:[0-9]{2}:[0-9]{2}(Z|[+-][0-9]{2}:[0-9]{2})?\z"));
    format_match(value, &TIME)
}

fn valid_format_date_time(value: &str) -> bool {
    static DATE_TIME: LazyLock<Regex> = LazyLock::new(|| {
        regex(
            r"\A[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}(Z|[+-][0-9]{2}:[0-9]{2})?\z",
        )
    });
    format_match(value, &DATE_TIME)
}

fn valid_format_ipv4(value: &str) -> bool {
    let parts: Vec<&str> = value.split('.').collect();
    parts.len() == 4
        && parts.iter().all(|part| {
            (1..=3).contains(&part.len())
                && part.bytes().all(|b| b.is_ascii_digit())
                && part.parse::<u16>().is_ok_and(|n| n <= 255)
        })
}

fn valid_format_ipv6(value: &str) -> bool {
    static IPV6: LazyLock<Regex> =
        LazyLock::new(|| regex(r"(?i)\A([0-9a-f]{0,4}:){2,7}[0-9a-f]{0,4}\z"));
    format_match(value, &IPV6)
}

/// RFC 1035 caps a name at 253 bytes.
fn valid_format_hostname(value: &str) -> bool {
    static HOSTNAME: LazyLock<Regex> = LazyLock::new(|| {
        regex(r"(?i)\A[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?(\.[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?)*\z")
    });
    value.len() <= 253 && format_match(value, &HOSTNAME)
}
