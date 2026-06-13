use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq)]
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

pub fn parse_list_output(output: &str) -> Vec<ContainerSummary> {
    let value = serde_json::from_str::<Value>(output).unwrap_or(Value::Null);
    match value {
        Value::Array(items) => items.into_iter().filter_map(parse_summary).collect(),
        Value::Object(_) => parse_summary(value).into_iter().collect(),
        _ => Vec::new(),
    }
}

pub fn parse_inspect_output(output: &str) -> Option<Value> {
    let value = serde_json::from_str::<Value>(output).ok()?;
    match value {
        Value::Array(items) => items.into_iter().next(),
        other => Some(other),
    }
}

fn parse_summary(value: Value) -> Option<ContainerSummary> {
    let id = field(&value, &["id", "ID", "identifier", "name"])?;
    let name = field(&value, &["name", "Name"]);
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
