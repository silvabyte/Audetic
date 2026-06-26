//! Built-in meeting summary templates.
//!
//! Templates are intentionally data-only: they describe the Markdown sections
//! an agent should produce without knowing anything about HTTP, CLI args, or
//! persistence. User-editable templates can later use the same shape from disk
//! or the database.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct SummaryTemplateSection {
    pub title: String,
    pub instruction: String,
    pub format: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub item_format: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct SummaryTemplate {
    pub id: String,
    pub name: String,
    pub description: String,
    pub sections: Vec<SummaryTemplateSection>,
}

impl SummaryTemplate {
    pub fn validate(&self) -> anyhow::Result<()> {
        if self.id.trim().is_empty() {
            anyhow::bail!("template id cannot be empty");
        }
        if self.name.trim().is_empty() {
            anyhow::bail!("template name cannot be empty");
        }
        if self.sections.is_empty() {
            anyhow::bail!("template must have at least one section");
        }
        for section in &self.sections {
            if section.title.trim().is_empty() {
                anyhow::bail!("template section title cannot be empty");
            }
            match section.format.as_str() {
                "paragraph" | "list" | "table" | "string" => {}
                other => anyhow::bail!("unsupported template section format `{other}`"),
            }
        }
        Ok(())
    }

    pub fn markdown_skeleton(&self) -> String {
        let mut out = String::from("# <Concise meeting title>\n\n");
        for section in &self.sections {
            out.push_str(&format!("## {}\n\n", section.title));
        }
        out
    }

    pub fn instructions(&self) -> String {
        let mut out = String::new();
        for section in &self.sections {
            out.push_str(&format!(
                "- **{}** ({}): {}\n",
                section.title, section.format, section.instruction
            ));
            if let Some(item_format) = &section.item_format {
                out.push_str(&format!(
                    "  - Use this item/table format: `{item_format}`\n"
                ));
            }
        }
        out
    }
}

pub fn list_templates() -> Vec<SummaryTemplate> {
    vec![
        SummaryTemplate {
            id: "standard_meeting".into(),
            name: "Standard Meeting Notes".into(),
            description: "Executive summary, decisions, action items, and discussion highlights.".into(),
            sections: vec![
                section("Summary", "Provide a concise executive summary of the meeting.", "paragraph"),
                section("Key Decisions", "List decisions made or clearly proposed during the meeting.", "list"),
                SummaryTemplateSection {
                    title: "Action Items".into(),
                    instruction: "List tasks, owners, due dates, and evidence from the transcript. If unknown, write `Unassigned` or `No due date`.".into(),
                    format: "table".into(),
                    item_format: Some("| Owner | Task | Due | Evidence |".into()),
                },
                section("Discussion Highlights", "Capture the important arguments, insights, risks, and context.", "paragraph"),
            ],
        },
        SummaryTemplate {
            id: "action_items".into(),
            name: "Action Items".into(),
            description: "A focused follow-up list with owners and evidence.".into(),
            sections: vec![
                SummaryTemplateSection {
                    title: "Action Items".into(),
                    instruction: "Extract every explicit or implied follow-up. Include owner, task, due date, priority, and transcript evidence.".into(),
                    format: "table".into(),
                    item_format: Some("| Priority | Owner | Task | Due | Evidence |".into()),
                },
                section("Open Questions", "List unresolved questions or missing decisions.", "list"),
            ],
        },
        SummaryTemplate {
            id: "project_sync".into(),
            name: "Project Sync".into(),
            description: "Status, blockers, decisions, and next steps for project meetings.".into(),
            sections: vec![
                section("Status Snapshot", "Summarize current project status and progress since the last sync.", "paragraph"),
                section("Blockers / Risks", "List blockers, risks, and dependencies that need attention.", "list"),
                section("Decisions", "List project decisions and rationale.", "list"),
                section("Next Steps", "List concrete next steps with owners when available.", "list"),
            ],
        },
        SummaryTemplate {
            id: "retrospective".into(),
            name: "Retrospective".into(),
            description: "What worked, what did not, and changes to try next.".into(),
            sections: vec![
                section("What Worked", "List practices, moments, or decisions that helped.", "list"),
                section("What Did Not Work", "List pain points, failures, or friction.", "list"),
                section("Experiments", "List process changes or experiments proposed for next time.", "list"),
                section("Action Items", "List owners and follow-ups.", "list"),
            ],
        },
        SummaryTemplate {
            id: "daily_standup".into(),
            name: "Daily Standup".into(),
            description: "Yesterday, today, blockers, and follow-ups.".into(),
            sections: vec![
                section("Yesterday", "Summarize completed work mentioned by each participant.", "list"),
                section("Today", "Summarize planned work mentioned by each participant.", "list"),
                section("Blockers", "List blockers and who can help resolve them.", "list"),
            ],
        },
    ]
}

pub fn get_template(id: &str) -> anyhow::Result<SummaryTemplate> {
    list_templates()
        .into_iter()
        .find(|t| t.id == id)
        .ok_or_else(|| anyhow::anyhow!("unknown summary template `{id}`"))
}

fn section(title: &str, instruction: &str, format: &str) -> SummaryTemplateSection {
    SummaryTemplateSection {
        title: title.into(),
        instruction: instruction.into(),
        format: format.into(),
        item_format: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtins_validate() {
        for template in list_templates() {
            template.validate().unwrap();
        }
    }
}
