//! Bounded `model/list` projection for the pinned original app-server.
//! This is presentation/selection data, never account or execution authority.
use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use serde_json::Value;

const MAX_MODELS: usize = 256;
const MAX_PAGES: usize = 16;

/// Exact supported wire efforts. Ultra is deliberately not re-labelled as Max.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Effort {
    None,
    Minimal,
    Low,
    Medium,
    High,
    XHigh,
    Max,
}

impl Effort {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Minimal => "minimal",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::XHigh => "xhigh",
            Self::Max => "max",
        }
    }
    fn parse(value: &str) -> Option<Self> {
        match value {
            "none" => Some(Self::None),
            "minimal" => Some(Self::Minimal),
            "low" => Some(Self::Low),
            "medium" => Some(Self::Medium),
            "high" => Some(Self::High),
            "xhigh" => Some(Self::XHigh),
            "max" => Some(Self::Max),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EffortOption {
    pub effort: Effort,
    pub description: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Model {
    /// The actual picker identifier; not assumed equal to `model`.
    pub id: String,
    /// Exact wire model returned by the pinned server.
    pub model: String,
    pub display_name: String,
    pub description: String,
    pub efforts: Vec<EffortOption>,
    /// None when the original default is unsupported; never silently replace it.
    pub default_effort: Option<Effort>,
    pub is_default: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModelChoice {
    id: String,
    model: String,
    effort: Effort,
}
impl ModelChoice {
    pub fn id(&self) -> &str {
        &self.id
    }
    pub fn model(&self) -> &str {
        &self.model
    }
    pub fn effort(&self) -> Effort {
        self.effort
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CatalogFault {
    Shape,
    Limit,
    Cursor,
    Duplicate,
    NotComplete,
    UnsupportedChoice,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Catalog {
    models: Vec<Model>,
}
impl Catalog {
    pub fn models(&self) -> &[Model] {
        &self.models
    }
    pub fn select(&self, id: &str, effort: Effort) -> Result<ModelChoice, CatalogFault> {
        let row = self
            .models
            .iter()
            .find(|row| row.id == id && row.efforts.iter().any(|e| e.effort == effort))
            .ok_or(CatalogFault::UnsupportedChoice)?;
        Ok(ModelChoice {
            id: row.id.clone(),
            model: row.model.clone(),
            effort,
        })
    }
}

#[derive(Default)]
pub struct CatalogBuilder {
    models: Vec<Model>,
    ids: HashSet<String>,
    cursors: HashSet<String>,
    next: Option<String>,
    pages: usize,
    complete: bool,
}
impl CatalogBuilder {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn next_cursor(&self) -> Option<&str> {
        self.next.as_deref()
    }
    /// The actor correlates the RPC ID and current native scope before calling.
    /// A malformed page never partially mutates the catalog.
    pub fn push_page(
        &mut self,
        requested_cursor: Option<&str>,
        result: &Value,
    ) -> Result<(), CatalogFault> {
        if self.complete || requested_cursor != self.next.as_deref() {
            return Err(CatalogFault::Cursor);
        }
        if self.pages >= MAX_PAGES {
            return Err(CatalogFault::Limit);
        }
        let rows = result
            .get("data")
            .and_then(Value::as_array)
            .ok_or(CatalogFault::Shape)?;
        if self.ids.len().saturating_add(rows.len()) > MAX_MODELS {
            return Err(CatalogFault::Limit);
        }
        let next = match result.get("nextCursor") {
            Some(Value::Null) => None,
            Some(Value::String(cursor)) if bounded(cursor, 2048) => Some(cursor.clone()),
            _ => return Err(CatalogFault::Shape),
        };
        if next.as_ref().is_some_and(|c| self.cursors.contains(c)) {
            return Err(CatalogFault::Cursor);
        }
        let mut ids = self.ids.clone();
        let mut admitted = Vec::new();
        for row in rows {
            let id = string(row, "id", 256)?;
            if !ids.insert(id.to_owned()) {
                return Err(CatalogFault::Duplicate);
            }
            if row
                .get("hidden")
                .and_then(Value::as_bool)
                .ok_or(CatalogFault::Shape)?
            {
                continue;
            }
            let model = string(row, "model", 256)?;
            let display_name = string(row, "displayName", 256)?;
            let description = row
                .get("description")
                .and_then(Value::as_str)
                .ok_or(CatalogFault::Shape)?;
            if description.len() > 8192 {
                return Err(CatalogFault::Limit);
            }
            let options = row
                .get("supportedReasoningEfforts")
                .and_then(Value::as_array)
                .ok_or(CatalogFault::Shape)?;
            if options.len() > 32 {
                return Err(CatalogFault::Limit);
            }
            let mut seen = HashSet::new();
            let mut efforts = Vec::new();
            for option in options {
                let wire = string(option, "reasoningEffort", 64)?;
                if !seen.insert(wire) {
                    return Err(CatalogFault::Duplicate);
                }
                let description = option
                    .get("description")
                    .and_then(Value::as_str)
                    .ok_or(CatalogFault::Shape)?;
                if description.len() > 2048 {
                    return Err(CatalogFault::Limit);
                }
                if let Some(effort) = Effort::parse(wire) {
                    efforts.push(EffortOption {
                        effort,
                        description: description.to_owned(),
                    });
                }
            }
            let default = string(row, "defaultReasoningEffort", 64)?;
            let is_default = row
                .get("isDefault")
                .and_then(Value::as_bool)
                .ok_or(CatalogFault::Shape)?;
            // This connection is text-only. Catalog input support grants no image/tool action.
            let modalities = row
                .get("inputModalities")
                .and_then(Value::as_array)
                .ok_or(CatalogFault::Shape)?;
            if modalities.len() > 16 || modalities.iter().any(|v| !v.is_string()) {
                return Err(CatalogFault::Shape);
            }
            if efforts.is_empty() || !modalities.iter().any(|v| v.as_str() == Some("text")) {
                continue;
            }
            let default_effort =
                Effort::parse(default).filter(|e| efforts.iter().any(|o| o.effort == *e));
            admitted.push(Model {
                id: id.to_owned(),
                model: model.to_owned(),
                display_name: display_name.to_owned(),
                description: description.to_owned(),
                efforts,
                default_effort,
                is_default,
            });
        }
        self.ids = ids;
        self.models.extend(admitted);
        self.pages += 1;
        self.complete = next.is_none();
        if let Some(cursor) = &next {
            self.cursors.insert(cursor.clone());
        }
        self.next = next;
        Ok(())
    }
    pub fn finish(self) -> Result<Catalog, CatalogFault> {
        if !self.complete {
            return Err(CatalogFault::NotComplete);
        }
        Ok(Catalog {
            models: self.models,
        })
    }
}

fn string<'a>(value: &'a Value, key: &str, max: usize) -> Result<&'a str, CatalogFault> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|s| bounded(s, max))
        .ok_or(CatalogFault::Shape)
}
fn bounded(value: &str, max: usize) -> bool {
    !value.is_empty() && value.len() <= max && !value.chars().any(char::is_control)
}

#[cfg(test)]
#[path = "catalog/tests.rs"]
mod tests;
