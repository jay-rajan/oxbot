//! OpenRouter specifics: provider routing preferences (privacy).

use serde::{Deserialize, Serialize};

/// Whether providers may keep and learn from requests.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DataCollection {
    /// Providers may keep data (OpenRouter's default).
    Allow,
    /// Only providers that don't keep data.
    Deny,
}

/// OpenRouter's `provider` object: which providers may serve a request.
/// Serializes to OpenRouter's own shape; unset fields are left out.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderPrefs {
    /// Whether providers may keep data.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data_collection: Option<DataCollection>,
    /// Only zero-data-retention endpoints.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub zdr: Option<bool>,
    /// Whether OpenRouter may try other providers of the same model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allow_fallbacks: Option<bool>,
    /// Only providers that honor every parameter sent (tools,
    /// `parallel_tool_calls`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub require_parameters: Option<bool>,
    /// Providers to try first, in order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub order: Vec<String>,
    /// Providers never to use.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ignore: Vec<String>,
}

impl ProviderPrefs {
    /// For private bots: no data collection, zero retention, only providers
    /// that honor tools; other providers of the model may still answer.
    #[must_use]
    pub fn private_bot() -> Self {
        Self {
            data_collection: Some(DataCollection::Deny),
            zdr: Some(true),
            allow_fallbacks: Some(true),
            require_parameters: Some(true),
            ..Self::default()
        }
    }

    /// For shared bots: OpenRouter's default data rules, only providers
    /// that honor tools.
    #[must_use]
    pub fn shared_bot() -> Self {
        Self {
            require_parameters: Some(true),
            allow_fallbacks: Some(true),
            ..Self::default()
        }
    }

    /// The same preferences without the zero-retention requirement. Keeps
    /// `data_collection` as it is: a private request stays "deny".
    #[must_use]
    pub fn without_zdr(&self) -> Self {
        Self {
            zdr: None,
            ..self.clone()
        }
    }

    /// Whether these preferences refuse providers that keep data.
    #[must_use]
    pub fn denies_data_collection(&self) -> bool {
        self.data_collection == Some(DataCollection::Deny)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn private_prefs_serialize_to_openrouter_shape() {
        let v = serde_json::to_value(ProviderPrefs::private_bot()).unwrap();
        assert_eq!(
            v,
            json!({"data_collection": "deny", "zdr": true, "allow_fallbacks": true, "require_parameters": true})
        );
    }

    #[test]
    fn dropping_zdr_keeps_deny() {
        let p = ProviderPrefs::private_bot().without_zdr();
        assert!(p.denies_data_collection());
        assert_eq!(p.zdr, None);
        let v = serde_json::to_value(&p).unwrap();
        assert_eq!(v["data_collection"], "deny");
        assert!(v.get("zdr").is_none());
    }

    #[test]
    fn shared_prefs_leave_data_rules_to_openrouter() {
        let p = ProviderPrefs::shared_bot();
        assert!(!p.denies_data_collection());
        let v = serde_json::to_value(&p).unwrap();
        assert!(v.get("data_collection").is_none());
        assert_eq!(v["require_parameters"], true);
    }
}
