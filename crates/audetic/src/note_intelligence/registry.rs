//! Processor routing is independently extensible, without changes to note persistence.
use anyhow::Result;
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Processor {
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
            template_id: "general_note".into(),
        };
        let mut registry = Self {
            entries: BTreeMap::new(),
            fallback,
        };
        for (kind, template) in [
            ("meeting", "standard_meeting"),
            ("dictation", "cleaned_dictation"),
            ("conversation", "conversation"),
            ("request", "request_intent"),
            ("shopping-list", "shopping_items"),
            ("shopping_list", "shopping_items"),
            ("general", "general_note"),
        ] {
            registry
                .register(
                    kind,
                    Processor {
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
        assert_eq!(
            registry.resolve("dictation").template_id,
            "cleaned_dictation"
        );
        assert_eq!(registry.resolve("conversation").template_id, "conversation");
        assert_eq!(registry.resolve("request").template_id, "request_intent");
        assert_eq!(
            registry.resolve("shopping-list").template_id,
            "shopping_items"
        );
        assert_eq!(registry.resolve("new-kind").template_id, "general_note");
        registry
            .register(
                "new-kind",
                Processor {
                    template_id: "action_items".into(),
                },
            )
            .unwrap();
        assert_eq!(registry.resolve("new-kind").template_id, "action_items");
    }
}
