use serde::Serialize;
use serde_json::Value;
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ContainerSummary {
    pub id: String,
    pub name: Option<String>,
    pub status: Option<String>,
    pub running: bool,
}

impl ContainerSummary {
    pub fn matches(&self, needle: &str) -> bool {
        self.id == needle || self.name.as_deref() == Some(needle)
    }
}

#[derive(Debug, Error)]
pub enum StateError {
    #[error("failed to parse container list output: {0}")]
    ParseList(#[source] serde_json::Error),
}

pub fn parse_list_output(output: &str) -> Result<Vec<ContainerSummary>, StateError> {
    let value = serde_json::from_str::<Value>(output).map_err(StateError::ParseList)?;
    Ok(match value {
        Value::Array(items) => items.into_iter().filter_map(parse_summary).collect(),
        Value::Object(_) => parse_summary(value).into_iter().collect(),
        _ => Vec::new(),
    })
}

pub fn parse_inspect_output(output: &str) -> Option<Value> {
    let value = serde_json::from_str::<Value>(output).ok()?;
    match value {
        Value::Array(items) => items.into_iter().next(),
        other => Some(other),
    }
}

fn parse_summary(value: Value) -> Option<ContainerSummary> {
    let id = field(&value, &["id", "Id", "ID", "identifier", "name"])?;
    let name = field(&value, &["name", "Name"]).or_else(|| first_string_field(&value, &["Names"]));
    let status = field(&value, &["status", "Status", "state", "State"]);
    let running = value
        .get("running")
        .and_then(Value::as_bool)
        .or_else(|| {
            status
                .as_ref()
                .map(|value| value.eq_ignore_ascii_case("running"))
        })
        .unwrap_or(false);
    Some(ContainerSummary {
        id,
        name,
        status,
        running,
    })
}

fn field(value: &Value, names: &[&str]) -> Option<String> {
    names.iter().find_map(|name| {
        value.get(name).and_then(|value| match value {
            Value::String(value) => Some(value.clone()),
            Value::Number(value) => Some(value.to_string()),
            _ => None,
        })
    })
}

fn first_string_field(value: &Value, names: &[&str]) -> Option<String> {
    names.iter().find_map(|name| {
        value.get(name).and_then(|value| {
            value
                .as_array()
                .and_then(|values| values.first())
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn list_parse_error_bubbles_up() {
        let error = parse_list_output("not-json").unwrap_err();
        assert!(
            error
                .to_string()
                .contains("failed to parse container list output")
        );
    }

    #[test]
    fn parse_list_output_supports_podman_id_and_names() {
        let parsed = parse_list_output(
            &json!([
                {
                    "Id": "abc123",
                    "Names": ["orodruin-demo"],
                    "State": "running",
                    "Status": "Up 10 seconds"
                }
            ])
            .to_string(),
        )
        .unwrap();

        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].id, "abc123");
        assert_eq!(parsed[0].name.as_deref(), Some("orodruin-demo"));
        assert!(parsed[0].matches("orodruin-demo"));
    }
}
