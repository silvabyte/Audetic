//! Processor routing is independently extensible, without changes to note persistence.
use anyhow::Result;
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Processor {
    pub artifact_kind: String,
    pub template_id: String,
}

#[derive(Debug, Clone)]
pub struct ProcessorRegistry {
    entries: BTreeMap<String, Processor>,
    fallback: Processor,
}

impl Default for ProcessorRegistry {
    fn default() -> Self {
        let fallback = Processor {
            artifact_kind: "summary".into(),
            template_id: "general_note".into(),
        };
        let mut registry = Self {
            entries: BTreeMap::new(),
            fallback,
        };
        for (kind, artifact, template) in [
            ("meeting", "summary", "standard_meeting"),
            ("dictation", "cleaned_text", "cleaned_dictation"),
            ("conversation", "summary", "conversation"),
            ("request", "intent", "request_intent"),
            ("shopping-list", "shopping_items", "request_intent"),
            ("shopping_list", "shopping_items", "request_intent"),
            ("general", "summary", "general_note"),
        ] {
            registry
                .register(
                    kind,
                    Processor {
                        artifact_kind: artifact.into(),
                        template_id: template.into(),
                    },
                )
                .expect("valid builtin registry entry");
        }
        registry
    }
}

impl ProcessorRegistry {
    pub fn register(&mut self, kind: &str, processor: Processor) -> Result<()> {
        anyhow::ensure!(
            super::classification::valid_kind(kind),
            "processor kind must be a slug"
        );
        anyhow::ensure!(
            !processor.artifact_kind.trim().is_empty(),
            "processor artifact kind cannot be blank"
        );
        crate::summary_templates::get_template(&processor.template_id)?.validate()?;
        self.entries.insert(kind.into(), processor);
        Ok(())
    }
    pub fn resolve(&self, kind: &str) -> &Processor {
        self.entries.get(kind).unwrap_or(&self.fallback)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn routes_builtins_and_extensible_unknown_kind() {
        let mut registry = ProcessorRegistry::default();
        assert_eq!(registry.resolve("meeting").template_id, "standard_meeting");
        assert_eq!(registry.resolve("dictation").artifact_kind, "cleaned_text");
        assert_eq!(registry.resolve("conversation").template_id, "conversation");
        assert_eq!(registry.resolve("request").artifact_kind, "intent");
        assert_eq!(
            registry.resolve("shopping-list").artifact_kind,
            "shopping_items"
        );
        assert_eq!(registry.resolve("new-kind").template_id, "general_note");
        registry
            .register(
                "new-kind",
                Processor {
                    artifact_kind: "tasks".into(),
                    template_id: "action_items".into(),
                },
            )
            .unwrap();
        assert_eq!(registry.resolve("new-kind").template_id, "action_items");
    }
}
