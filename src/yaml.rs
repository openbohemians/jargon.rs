//! YAML documents as JSON values, through Oyl.

use oyl::{Dialect, JsonOptions, Parser};
use serde_json::Value;

use crate::Error;

/// Each document in `text`, as JSON. Scalars are typed by YAML 1.1 (`yes`
/// is true, `0777` is octal), as Crystal's YAML reads them, so a schema or
/// config reads the same in both implementations; merge keys are expanded
/// and aliases followed.
pub(crate) fn documents(text: &str) -> Result<Vec<Value>, Error> {
    let mut parser = Parser::new(text).map_err(Error::syntax)?;
    parser.dialect(Dialect::yaml11());
    let mut docs = Vec::new();
    while let Some(json) = parser
        .next_json(&JsonOptions::default())
        .map_err(Error::syntax)?
    {
        docs.push(serde_json::from_str(json).map_err(Error::syntax)?);
    }
    Ok(docs)
}

/// The first document in `text`; null when there is none.
pub(crate) fn document(text: &str) -> Result<Value, Error> {
    Ok(documents(text)?.into_iter().next().unwrap_or(Value::Null))
}
