//! Suggested model lists by kind of job. A bot's settings and the New bot
//! form offer them; picking one fills the bot's own ordered list (see
//! `LoopSettings::models`). Free models come first in each, then cheap
//! paid ones as a backup. The page gets each list with only the models on
//! the live model list, so a retired model drops out.

use oxbot_llm::Catalog;
use oxbot_protocol::ModelList;

/// One kind of job and its models, in the order they're tried.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JobKind {
    /// Its key.
    pub kind: &'static str,
    /// Its name.
    pub label: &'static str,
    /// What it suits.
    pub about: &'static str,
    /// Full `OpenRouter` ids, free ones first.
    pub models: &'static [&'static str],
}

/// The four kinds of job (models picked Nov 6, 2026: tools, a long window,
/// cheap input; no Anthropic or Google models).
pub const JOB_KINDS: [JobKind; 4] = [
    JobKind {
        kind: "research",
        label: "Research and writing",
        about: "long reading, sources, your voice",
        models: &[
            "nvidia/nemotron-3-ultra-550b-a55b:free",
            "nvidia/nemotron-3.5-super-vl:free",
            "dots-studio/dots-3-note-preview:free",
            "deepseek/deepseek-v4.1-flash",
            "z-ai/glm-5.3-flash",
        ],
    },
    JobKind {
        kind: "coaching",
        label: "Coaching and plans",
        about: "long chats, careful reasoning",
        models: &[
            "nvidia/nemotron-3-ultra-550b-a55b:free",
            "nvidia/nemotron-3.5-super-vl:free",
            "deepseek/deepseek-v4.1-flash",
            "z-ai/glm-5.3-flash",
        ],
    },
    JobKind {
        kind: "browsing",
        label: "Browsing and shopping",
        about: "many page steps, prices, carts",
        models: &[
            "nvidia/nemotron-3.5-super-vl:free",
            "nvidia/nemotron-3-ultra-550b-a55b:free",
            "dots-studio/dots-3-note-preview:free",
            "nvidia/nemotron-3.5-super-vl",
            "deepseek/deepseek-v4.1-flash",
        ],
    },
    JobKind {
        kind: "upkeep",
        label: "Light upkeep",
        about: "lists, reminders, one file",
        models: &[
            "nvidia/nemotron-3.5-lightning:free",
            "nvidia/nemotron-3.5-super-vl:free",
            "openai/gpt-oss-120b",
            "nvidia/nemotron-3.5-super-vl",
        ],
    },
];

/// The suggested lists as the page gets them: each with only the models
/// `catalog` has that can run bots, in order. A kind left with no model is
/// left out.
#[must_use]
pub fn suggested(catalog: &Catalog) -> Vec<ModelList> {
    JOB_KINDS
        .iter()
        .filter_map(|k| {
            let models: Vec<String> = k
                .models
                .iter()
                .filter(|id| catalog.get(id).is_some_and(oxbot_llm::ModelInfo::fits_loop))
                .map(|id| (*id).to_owned())
                .collect();
            (!models.is_empty()).then(|| ModelList {
                kind: k.kind.to_owned(),
                label: k.label.to_owned(),
                about: k.about.to_owned(),
                models,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use oxbot_llm::ModelInfo;

    fn model(id: &str, tools: bool) -> ModelInfo {
        let free = id.ends_with(":free");
        ModelInfo {
            id: id.into(),
            name: id.into(),
            context_length: 128_000,
            max_completion: Some(8000),
            prompt_usd_per_token: if free { 0.0 } else { 1e-7 },
            completion_usd_per_token: if free { 0.0 } else { 1e-7 },
            tools,
            images_in: false,
            text_io: true,
            free,
            supported_parameters: vec!["tools".into()],
        }
    }

    #[test]
    fn every_list_starts_free_and_has_a_paid_backup() {
        for k in JOB_KINDS {
            assert!(k.models[0].ends_with(":free"), "{}", k.kind);
            assert!(k.models.iter().any(|m| !m.ends_with(":free")), "{}", k.kind);
            assert!(k.models.len() <= crate::hub::MAX_BOT_MODELS, "{}", k.kind);
            let free_then_paid = k
                .models
                .iter()
                .skip_while(|m| m.ends_with(":free"))
                .all(|m| !m.ends_with(":free"));
            assert!(free_then_paid, "{}", k.kind);
        }
    }

    #[test]
    fn only_listed_models_that_can_run_bots_are_sent() {
        let catalog = Catalog {
            models: vec![
                model("nvidia/nemotron-3.5-super-vl:free", true),
                model("deepseek/deepseek-v4.1-flash", true),
                // No tools: can't run bots.
                model("nvidia/nemotron-3.5-lightning:free", false),
            ],
        };
        let lists = suggested(&catalog);
        let kinds: Vec<&str> = lists.iter().map(|l| l.kind.as_str()).collect();
        assert_eq!(kinds, ["research", "coaching", "browsing", "upkeep"]);
        assert_eq!(
            lists[0].models,
            [
                "nvidia/nemotron-3.5-super-vl:free",
                "deepseek/deepseek-v4.1-flash"
            ]
        );
        assert_eq!(lists[3].models, ["nvidia/nemotron-3.5-super-vl:free"]);
        assert_eq!(lists[0].label, "Research and writing");
        assert_eq!(suggested(&Catalog::default()), Vec::new());
    }
}
