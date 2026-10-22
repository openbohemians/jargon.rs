use std::collections::HashSet;

use serde_json::{Map, Value};

use crate::Error;

fn parse_object(json: &str) -> Result<Map<String, Value>, Error> {
    match serde_json::from_str(json).map_err(Error::syntax)? {
        Value::Object(map) => Ok(map),
        _ => Err(Error::Schema("A schema must be an object".into())),
    }
}

/// Merge a global schema's properties into a subcommand's schema; where
/// both define a property, the subcommand's wins.
pub fn merge(sub: &str, global: &str) -> Result<String, Error> {
    let mut sub = parse_object(sub)?;
    let global = parse_object(global)?;

    let mut properties = match global.get("properties") {
        Some(Value::Object(props)) => props.clone(),
        _ => Map::new(),
    };
    if let Some(Value::Object(props)) = sub.get("properties") {
        properties.extend(props.clone());
    }
    sub.insert("properties".into(), Value::Object(properties));
    Ok(Value::Object(sub).to_string())
}

/// Flatten a schema by inlining every local `$ref` and dropping `$defs`
/// (`definitions`), for consumers that don't support `$ref`, such as some
/// AI tool definitions. A recursive `$ref` (a definition that contains
/// itself) has no finite inline form, so it is an error.
pub fn flatten(schema: &str) -> Result<String, Error> {
    let json: Value = serde_json::from_str(schema).map_err(Error::syntax)?;
    let defs = ["$defs", "definitions"]
        .iter()
        .find_map(|key| json.get(key).and_then(Value::as_object))
        .cloned()
        .unwrap_or_default();
    Ok(flatten_node(&json, &defs, &HashSet::new())?.to_string())
}

/// `expanding` holds the definitions being inlined on this path, to catch a
/// `$ref` back into one of them.
fn flatten_node(
    node: &Value,
    defs: &Map<String, Value>,
    expanding: &HashSet<String>,
) -> Result<Value, Error> {
    match node {
        Value::Object(map) => {
            if let Some(reference) = map.get("$ref").and_then(Value::as_str) {
                if reference.starts_with("#/$defs/") || reference.starts_with("#/definitions/") {
                    let name = reference.rsplit('/').next().unwrap_or_default();
                    if let Some(resolved) = defs.get(name) {
                        if expanding.contains(name) {
                            return Err(Error::Schema(format!(
                                "Cannot flatten recursive $ref '{reference}': the definition contains itself"
                            )));
                        }
                        let mut inner = expanding.clone();
                        inner.insert(name.to_owned());
                        return flatten_node(resolved, defs, &inner);
                    }
                }
                // A ref that can't be resolved stays as it is.
                return Ok(node.clone());
            }

            map.iter()
                .filter(|(key, _)| *key != "$defs" && *key != "definitions")
                .map(|(key, value)| Ok((key.clone(), flatten_node(value, defs, expanding)?)))
                .collect::<Result<Map<_, _>, _>>()
                .map(Value::Object)
        }
        Value::Array(items) => items
            .iter()
            .map(|item| flatten_node(item, defs, expanding))
            .collect::<Result<Vec<_>, _>>()
            .map(Value::Array),
        other => Ok(other.clone()),
    }
}
