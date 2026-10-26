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

/// A native catalog choice, not a guessed effort or an execution authority.
/// Default omits the original wire setting; it is never literal `none`.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    content = "effort",
    rename_all = "camelCase",
    deny_unknown_fields
)]
pub enum EffortSelection {
    Default,
    Explicit(Effort),
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
    /// Exact original default metadata, including an unqualified future mode.
    pub default_effort_wire: String,
    /// Parsed original default, independent of explicit preset membership.
    /// None means unqualified by this adapter, not unsupported by the provider.
    pub default_effort: Option<Effort>,
    pub is_default: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModelChoice {
    id: String,
    model: String,
    selection: EffortSelection,
    default_effort: Option<Effort>,
    default_effort_wire: String,
}
impl ModelChoice {
    pub fn id(&self) -> &str {
        &self.id
    }
    pub fn model(&self) -> &str {
        &self.model
    }
    pub fn selection(&self) -> EffortSelection {
        self.selection
    }
    pub fn explicit_effort(&self) -> Option<Effort> {
        match self.selection {
            EffortSelection::Default => None,
            EffortSelection::Explicit(effort) => Some(effort),
        }
    }
    /// Catalog metadata only; null effective readback must remain unspecified.
    pub fn advertised_default(&self) -> Option<Effort> {
        self.default_effort
    }
    pub fn advertised_default_wire(&self) -> &str {
        &self.default_effort_wire
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
    pub fn select(
        &self,
        id: &str,
        selection: EffortSelection,
    ) -> Result<ModelChoice, CatalogFault> {
        let row = self
            .models
            .iter()
            .find(|row| {
                row.id == id
                    && match selection {
                        EffortSelection::Default => row.default_effort.is_some(),
                        EffortSelection::Explicit(effort) => {
                            row.efforts.iter().any(|e| e.effort == effort)
                        }
                    }
            })
            .ok_or(CatalogFault::UnsupportedChoice)?;
        Ok(ModelChoice {
            id: row.id.clone(),
            model: row.model.clone(),
            selection,
            default_effort: row.default_effort,
            default_effort_wire: row.default_effort_wire.clone(),
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
            // The exact original producer maps default and presets independently.
            // An empty preset list is valid, and is not evidence that the
            // separately advertised default is an explicit selectable preset.
            let default_effort = Effort::parse(default);
            if (efforts.is_empty() && default_effort.is_none())
                || !modalities.iter().any(|v| v.as_str() == Some("text"))
            {
                continue;
            }
            admitted.push(Model {
                id: id.to_owned(),
                model: model.to_owned(),
                display_name: display_name.to_owned(),
                description: description.to_owned(),
                efforts,
                default_effort_wire: default.to_owned(),
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
