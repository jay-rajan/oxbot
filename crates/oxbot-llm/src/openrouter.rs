//! OpenRouter specifics: provider routing preferences (privacy), the model
//! list with its capability filter, and the key's limits.
//!
//! The model list (`GET /models`, no key sent) is the one download allowed
//! besides model calls; key info (`GET /key`) is read with the key.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::client::OpenAiClient;
use crate::error::{ErrorKind, LlmError};

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
    /// Only providers that honor every parameter sent. Requests then send
    /// only the parameters the model lists (see
    /// [`ModelInfo::supported_parameters`]), so this rules out providers
    /// without tools, not every provider.
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

/// Smallest context window a model needs to run oxbot's loop.
pub const MIN_CONTEXT: u32 = 64_000;

/// One model from the model list.
// Independent capability flags from the list, not states.
#[allow(clippy::struct_excessive_bools)]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ModelInfo {
    /// Its id, e.g. `vendor/model:free`.
    pub id: String,
    /// Its display name.
    pub name: String,
    /// Context window in tokens (0 if unknown).
    pub context_length: u32,
    /// Longest answer in tokens, when known.
    pub max_completion: Option<u32>,
    /// US dollars per input token.
    pub prompt_usd_per_token: f64,
    /// US dollars per output token.
    pub completion_usd_per_token: f64,
    /// Takes tools (`supported_parameters` has `tools`).
    pub tools: bool,
    /// Takes images.
    pub images_in: bool,
    /// Reads and writes text.
    pub text_io: bool,
    /// A free variant: id ends with `:free` and both prices are 0.
    pub free: bool,
    /// Request parameters its providers take, by OpenRouter's names (e.g.
    /// `tools`, `tool_choice`, `seed`). Empty: unknown (an older kept list,
    /// or a local server's).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub supported_parameters: Vec<String>,
}

impl ModelInfo {
    /// Whether oxbot's loop can use it: tools, text in and out, and a
    /// window of at least [`MIN_CONTEXT`] tokens.
    #[must_use]
    pub fn fits_loop(&self) -> bool {
        self.tools && self.text_io && self.context_length >= MIN_CONTEXT
    }

    /// Whether it takes the request parameter `name`. `false` when the
    /// list doesn't say.
    #[must_use]
    pub fn supports(&self, name: &str) -> bool {
        self.supported_parameters.iter().any(|p| p == name)
    }

    /// The longest answer to ask for: the model's own limit, at most
    /// `cap` (oxbot uses 4,096).
    #[must_use]
    pub fn max_tokens(&self, cap: u32) -> u32 {
        self.max_completion
            .filter(|m| *m > 0)
            .map_or(cap, |m| m.min(cap))
    }
}

/// The model list.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Catalog {
    /// Every model that could be read, in the service's order.
    pub models: Vec<ModelInfo>,
}

impl Catalog {
    /// Reads `GET /models`' answer (`{"data": [...]}`). Entries with odd ids
    /// or prices that aren't plain numbers (OpenRouter's own routers use
    /// `-1`) are left out; other fields default when missing, so local
    /// servers' shorter lists read too.
    ///
    /// # Errors
    /// The answer isn't a model list.
    pub fn parse(json: &[u8]) -> Result<Self, LlmError> {
        let v: Value = serde_json::from_slice(json).map_err(|e| {
            LlmError::new(
                ErrorKind::Protocol,
                format!("The model list couldn't be read: {e}"),
            )
        })?;
        let Some(data) = v.get("data").and_then(Value::as_array) else {
            return Err(LlmError::new(
                ErrorKind::Protocol,
                "The model list had no models.",
            ));
        };
        Ok(Self {
            models: data.iter().filter_map(model_info).collect(),
        })
    }

    /// Whether it was kept by an older oxbot that didn't keep each model's
    /// parameters: a model with tools but no parameter list. Read it again.
    #[must_use]
    pub fn lacks_parameters(&self) -> bool {
        self.models
            .iter()
            .any(|m| m.tools && m.supported_parameters.is_empty())
    }

    /// A model by id.
    #[must_use]
    pub fn get(&self, id: &str) -> Option<&ModelInfo> {
        self.models.iter().find(|m| m.id == id)
    }

    /// Free models the loop can use, in list order.
    pub fn free_for_loop(&self) -> impl Iterator<Item = &ModelInfo> {
        self.models.iter().filter(|m| m.free && m.fits_loop())
    }

    /// Paid models the loop can use, cheapest first (input plus output
    /// price), ties by id.
    #[must_use]
    pub fn paid_for_loop(&self) -> Vec<&ModelInfo> {
        let mut paid: Vec<&ModelInfo> = self
            .models
            .iter()
            .filter(|m| !m.free && m.fits_loop())
            .collect();
        paid.sort_by(|a, b| {
            let price = |m: &ModelInfo| m.prompt_usd_per_token + m.completion_usd_per_token;
            price(a).total_cmp(&price(b)).then_with(|| a.id.cmp(&b.id))
        });
        paid
    }
}

fn model_info(m: &Value) -> Option<ModelInfo> {
    let id = m
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| valid_id(id))?
        .to_owned();
    let pricing = m.get("pricing");
    let price = |field: &str| -> Option<f64> {
        match pricing.and_then(|p| p.get(field)) {
            None | Some(Value::Null) => Some(0.0),
            Some(Value::String(s)) => s.trim().parse::<f64>().ok(),
            Some(v) => v.as_f64(),
        }
        .filter(|p| p.is_finite() && *p >= 0.0)
    };
    let prompt = price("prompt")?;
    let completion = price("completion")?;
    let top = m.get("top_provider");
    let num = |v: Option<&Value>| {
        v.and_then(Value::as_u64)
            .map(|n| u32::try_from(n).unwrap_or(u32::MAX))
    };
    let context_length =
        num(m.get("context_length")).or_else(|| num(top.and_then(|t| t.get("context_length"))));
    let params = strings(m.get("supported_parameters"));
    let arch = m.get("architecture");
    let inputs = strings(arch.and_then(|a| a.get("input_modalities")));
    let outputs = strings(arch.and_then(|a| a.get("output_modalities")));
    // A list without modalities (local servers) is taken as text.
    let text_io = (inputs.is_empty() || inputs.contains(&"text"))
        && (outputs.is_empty() || outputs.contains(&"text"));
    let name = m
        .get("name")
        .and_then(Value::as_str)
        .map_or_else(|| id.clone(), clean_name);
    Some(ModelInfo {
        free: id.ends_with(":free") && prompt == 0.0 && completion == 0.0,
        name,
        context_length: context_length.unwrap_or(0),
        max_completion: num(top.and_then(|t| t.get("max_completion_tokens"))),
        prompt_usd_per_token: prompt,
        completion_usd_per_token: completion,
        tools: params.contains(&"tools"),
        images_in: inputs.contains(&"image"),
        text_io,
        supported_parameters: params
            .iter()
            .filter(|p| valid_param(p))
            .take(MAX_PARAMS)
            .map(|p| (*p).to_owned())
            .collect(),
        id,
    })
}

fn strings(v: Option<&Value>) -> Vec<&str> {
    v.and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default()
}

/// Most parameters kept per model (OpenRouter lists about 20).
const MAX_PARAMS: usize = 64;

/// Parameter names are short snake-case words.
fn valid_param(p: &str) -> bool {
    !p.is_empty()
        && p.len() <= 64
        && p.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

/// Model ids go into requests and the page: plain characters only.
fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 200
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._:/@+-".contains(&b))
}

/// One line, no control characters, at most 200 characters.
fn clean_name(name: &str) -> String {
    name.chars()
        .filter(|c| !c.is_control())
        .take(200)
        .collect::<String>()
        .trim()
        .to_owned()
}

/// Requests used, allowed and left today.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Quota {
    /// Used today.
    pub used: u32,
    /// Allowed per day.
    pub limit: u32,
    /// Left today.
    pub remaining: u32,
}

/// The key's limits and use, from `GET /key`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct KeyInfo {
    /// The key's spending limit in US dollars (`None`: no limit).
    pub limit_usd: Option<f64>,
    /// What's left of that limit.
    pub remaining_usd: Option<f64>,
    /// Spent today (UTC) in US dollars.
    pub usage_daily_usd: f64,
    /// Whether the account has never bought credits.
    pub is_free_tier: bool,
    /// Free-model requests today (UTC), when OpenRouter says.
    pub free_daily: Option<Quota>,
}

impl KeyInfo {
    /// Reads `GET /key`'s answer (`{"data": {...}}`).
    ///
    /// # Errors
    /// The answer isn't key info.
    pub fn parse(json: &[u8]) -> Result<Self, LlmError> {
        let v: Value = serde_json::from_slice(json).map_err(|e| {
            LlmError::new(
                ErrorKind::Protocol,
                format!("The key info couldn't be read: {e}"),
            )
        })?;
        let Some(d) = v.get("data").filter(|d| d.is_object()) else {
            return Err(LlmError::new(
                ErrorKind::Protocol,
                "The key info was empty.",
            ));
        };
        let money = |field: &str| {
            d.get(field)
                .and_then(Value::as_f64)
                .filter(|n| n.is_finite())
        };
        let count = |q: &Value, field: &str| {
            q.get(field)
                .and_then(Value::as_u64)
                .map(|n| u32::try_from(n).unwrap_or(u32::MAX))
        };
        let free_daily = d.get("free_model_daily_requests").and_then(|q| {
            Some(Quota {
                used: count(q, "used")?,
                limit: count(q, "limit")?,
                remaining: count(q, "remaining")?,
            })
        });
        Ok(Self {
            limit_usd: money("limit"),
            remaining_usd: money("limit_remaining"),
            usage_daily_usd: money("usage_daily").unwrap_or(0.0),
            is_free_tier: d
                .get("is_free_tier")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            free_daily,
        })
    }
}

impl OpenAiClient {
    /// Fetches the model list. Sends no key.
    ///
    /// # Errors
    /// Network, HTTP or format errors.
    pub async fn models(&self) -> Result<Catalog, LlmError> {
        let body = self.get_json("models", false).await?;
        Catalog::parse(&body)
    }

    /// Fetches the key's limits and today's use.
    ///
    /// # Errors
    /// Not signed in, the key was refused, or network/format errors.
    pub async fn key_info(&self) -> Result<KeyInfo, LlmError> {
        let body = self.get_json("key", true).await?;
        KeyInfo::parse(&body)
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

    fn catalog_json() -> String {
        json!({"data": [
            {"id": "a/tools:free", "name": "A (free)", "context_length": 131_072,
             "pricing": {"prompt": "0", "completion": "0"},
             "supported_parameters": ["tools", "tool_choice", "max_tokens"],
             "architecture": {"input_modalities": ["text", "image"], "output_modalities": ["text"]},
             "top_provider": {"context_length": 131_072, "max_completion_tokens": 8192}},
            {"id": "b/small:free", "name": "Small", "context_length": 8192,
             "pricing": {"prompt": "0", "completion": "0"}, "supported_parameters": ["tools"]},
            {"id": "c/no-tools:free", "name": "No tools", "context_length": 200_000,
             "pricing": {"prompt": "0", "completion": "0"}, "supported_parameters": ["max_tokens"]},
            {"id": "d/paid", "name": "Paid", "context_length": 200_000,
             "pricing": {"prompt": "0.000003", "completion": "0.000015"}, "supported_parameters": ["tools"]},
            {"id": "e/cheap", "name": "Cheap", "context_length": 100_000,
             "pricing": {"prompt": "0.0000001", "completion": "0.0000004"}, "supported_parameters": ["tools"]},
            {"id": "f/fake:free", "name": "Priced but named free", "context_length": 100_000,
             "pricing": {"prompt": "0.000001", "completion": "0"}, "supported_parameters": ["tools"]},
            {"id": "openrouter/auto", "name": "Auto", "context_length": 2_000_000,
             "pricing": {"prompt": "-1", "completion": "-1"}, "supported_parameters": ["tools"]},
            {"id": "g/image-out:free", "context_length": 100_000, "pricing": {"prompt": "0", "completion": "0"},
             "supported_parameters": ["tools"], "architecture": {"input_modalities": ["text"], "output_modalities": ["image"]}},
            {"id": "bad id <script>", "context_length": 100_000, "supported_parameters": ["tools"]},
            {"name": "no id"}
        ]})
        .to_string()
    }

    #[test]
    fn catalog_filters_free_models_that_fit_the_loop() {
        let c = Catalog::parse(catalog_json().as_bytes()).unwrap();
        let free: Vec<&str> = c.free_for_loop().map(|m| m.id.as_str()).collect();
        assert_eq!(free, ["a/tools:free"]);
        let a = c.get("a/tools:free").unwrap();
        assert!(a.images_in);
        assert_eq!(a.max_tokens(4096), 4096);
        assert_eq!(a.name, "A (free)");
        assert!(
            c.get("openrouter/auto").is_none(),
            "variable prices are left out"
        );
        assert!(c.get("bad id <script>").is_none());
        assert!(!c.get("f/fake:free").unwrap().free);
        assert!(!c.get("g/image-out:free").unwrap().fits_loop());
        assert_eq!(
            a.supported_parameters,
            ["tools", "tool_choice", "max_tokens"]
        );
        assert!(a.supports("tool_choice") && !a.supports("seed"));
    }

    #[test]
    fn odd_parameter_names_are_left_out() {
        let c = Catalog::parse(
            json!({"data": [{"id": "a/m:free", "pricing": {"prompt": "0", "completion": "0"},
                "supported_parameters": ["tools", "Bad Name", "<x>", 7, "top_k"]}]})
            .to_string()
            .as_bytes(),
        )
        .unwrap();
        assert_eq!(c.models[0].supported_parameters, ["tools", "top_k"]);
    }

    #[test]
    fn a_list_kept_without_parameters_reads_and_asks_to_be_read_again() {
        let fresh = Catalog::parse(catalog_json().as_bytes()).unwrap();
        assert!(!fresh.lacks_parameters());
        let old = r#"{"models":[{"id":"a/tools:free","name":"A","context_length":131072,
            "max_completion":null,"prompt_usd_per_token":0.0,"completion_usd_per_token":0.0,
            "tools":true,"images_in":false,"text_io":true,"free":true}]}"#;
        let old: Catalog = serde_json::from_str(old).unwrap();
        assert_eq!(old.models[0].supported_parameters, Vec::<String>::new());
        assert!(old.lacks_parameters());
    }

    #[test]
    fn paid_models_sorted_cheapest_first() {
        let c = Catalog::parse(catalog_json().as_bytes()).unwrap();
        let paid: Vec<&str> = c.paid_for_loop().iter().map(|m| m.id.as_str()).collect();
        assert_eq!(paid, ["e/cheap", "f/fake:free", "d/paid"]);
        let d = c.get("d/paid").unwrap();
        assert!((d.prompt_usd_per_token - 0.000_003).abs() < 1e-12);
    }

    #[test]
    fn catalog_round_trips_for_the_cache() {
        let c = Catalog::parse(catalog_json().as_bytes()).unwrap();
        let json = serde_json::to_string(&c).unwrap();
        assert_eq!(serde_json::from_str::<Catalog>(&json).unwrap(), c);
    }

    #[test]
    fn local_server_model_lists_read_too() {
        let c =
            Catalog::parse(br#"{"object":"list","data":[{"id":"llama3.1:8b","object":"model"}]}"#)
                .unwrap();
        let m = c.get("llama3.1:8b").unwrap();
        assert!(m.text_io);
        assert_eq!(m.context_length, 0);
        assert!(!m.free, "only OpenRouter's :free ids are free models");
    }

    #[test]
    fn bad_catalogs_are_errors() {
        assert_eq!(
            Catalog::parse(b"nope").unwrap_err().kind,
            ErrorKind::Protocol
        );
        assert_eq!(Catalog::parse(b"{}").unwrap_err().kind, ErrorKind::Protocol);
    }

    #[test]
    fn key_info_reads_limits_and_free_quota() {
        let k = KeyInfo::parse(
            json!({"data": {
                "label": "oxbot on Mac", "limit": 20.0, "limit_remaining": 12.5, "usage": 7.5,
                "usage_daily": 0.25, "is_free_tier": false,
                "free_model_daily_requests": {"used": 10, "limit": 1000, "remaining": 990}
            }})
            .to_string()
            .as_bytes(),
        )
        .unwrap();
        assert_eq!(k.limit_usd, Some(20.0));
        assert_eq!(k.remaining_usd, Some(12.5));
        assert!((k.usage_daily_usd - 0.25).abs() < f64::EPSILON);
        assert_eq!(
            k.free_daily,
            Some(Quota {
                used: 10,
                limit: 1000,
                remaining: 990
            })
        );

        let bare = KeyInfo::parse(br#"{"data":{"limit":null,"is_free_tier":true}}"#).unwrap();
        assert_eq!(bare.limit_usd, None);
        assert!(bare.is_free_tier);
        assert_eq!(bare.free_daily, None);
        assert!(KeyInfo::parse(br#"{"error":{}}"#).is_err());
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
