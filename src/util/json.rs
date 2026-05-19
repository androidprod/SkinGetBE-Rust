//! JSON utilities using serde_json

use crate::Result;
use serde_json::{json, Value};

/// JSON utilities
pub struct Json;

impl Json {
    /// Parse JSON string
    pub fn parse(data: &str) -> Result<Value> {
        serde_json::from_str(data).map_err(Into::into)
    }

    /// Convert value to JSON string
    pub fn stringify(value: &Value) -> Result<String> {
        serde_json::to_string(value).map_err(Into::into)
    }

    /// Pretty print JSON
    pub fn pretty_print(value: &Value) -> Result<String> {
        serde_json::to_string_pretty(value).map_err(Into::into)
    }

    /// Create a JSON object
    pub fn object() -> Value {
        json!({})
    }

    /// Create a JSON array
    pub fn array() -> Value {
        Value::Array(vec![])
    }
}
