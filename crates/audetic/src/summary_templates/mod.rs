//! Built-in meeting summary templates.
//!
//! Templates are intentionally data-only: they describe the Markdown sections
//! an agent should produce without knowing anything about HTTP, CLI args, or
//! persistence. User-editable templates can later use the same shape from disk
//! or the database.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactKind {
    MeetingMinutes,
    Summary,
    ActionItems,
    TalkingPoints,
    MindMap,
}

impl ArtifactKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::MeetingMinutes => "meeting_minutes",
            Self::Summary => "summary",
            Self::ActionItems => "action_items",
            Self::TalkingPoints => "talking_points",
            Self::MindMap => "mind_map",
        }
    }
}

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
    pub kind: ArtifactKind,
    pub requires_timestamps: bool,
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
                "paragraph" | "list" | "table" | "string" | "timeline" | "mermaid" => {}
                other => anyhow::bail!("unsupported template section format `{other}`"),
            }
        }
        Ok(())
    }

    pub fn markdown_skeleton(&self) -> String {
        let mut out = String::from("# <Concise meeting title>\n\n");
        for section in &self.sections {
            out.push_str(&format!("## {}\n\n", section.title));
            match section.format.as_str() {
                "timeline" => {
                    out.push_str("- [00:00] <Topic> - <What was discussed>\n\n");
                }
                "mermaid" => {
                    out.push_str("```mermaid\nmindmap\n  root((<Meeting topic>))\n```\n\n");
                }
                _ => {}
            }
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
            name: "Meeting Minutes".into(),
            description: "A complete record of the discussion, decisions, and follow-ups.".into(),
            kind: ArtifactKind::MeetingMinutes,
            requires_timestamps: false,
            sections: vec![
                section("Attendees & Context", "Identify participants when the transcript supports it, then state the purpose and context of the meeting.", "paragraph"),
                section("Discussion", "Record the major topics in the order they were discussed, preserving important rationale and disagreement.", "paragraph"),
                section("Decisions", "List decisions made or clearly proposed during the meeting.", "list"),
                SummaryTemplateSection {
                    title: "Action Items".into(),
                    instruction: "List tasks, owners, due dates, and evidence from the transcript. If unknown, write `Unassigned` or `No due date`.".into(),
                    format: "table".into(),
                    item_format: Some("| Owner | Task | Due | Evidence |".into()),
                },
                section("Open Questions", "List unresolved questions, risks, and items that need a later decision.", "list"),
            ],
        },
        SummaryTemplate {
            id: "concise_summary".into(),
            name: "Concise Summary".into(),
            description: "The meeting's essential context and takeaways in a quick read.".into(),
            kind: ArtifactKind::Summary,
            requires_timestamps: false,
            sections: vec![
                section("Overview", "Summarize the purpose, outcome, and most important context in no more than two short paragraphs.", "paragraph"),
                section("Key Takeaways", "List the few facts, decisions, risks, or insights a reader must retain.", "list"),
            ],
        },
        SummaryTemplate {
            id: "action_items".into(),
            name: "Action Items".into(),
            description: "A focused follow-up list with owners and evidence.".into(),
            kind: ArtifactKind::ActionItems,
            requires_timestamps: false,
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
            id: "talking_points".into(),
            name: "Talking Points".into(),
            description: "Timestamped chapters that turn the recording into a navigable timeline.".into(),
            kind: ArtifactKind::TalkingPoints,
            requires_timestamps: true,
            sections: vec![SummaryTemplateSection {
                title: "Talking Points".into(),
                instruction: "Create 3-12 chronological chapters from the timestamped transcript. Every item must use exactly `- [MM:SS] Topic - one-sentence description` (or `[H:MM:SS]` after one hour). Use the timestamp where that topic begins. Do not add nested bullets or omit timestamps.".into(),
                format: "timeline".into(),
                item_format: Some("- [MM:SS] Topic - one-sentence description".into()),
            }],
        },
        SummaryTemplate {
            id: "mind_map".into(),
            name: "Mind Map".into(),
            description: "A visual hierarchy of the conversation's themes and supporting ideas.".into(),
            kind: ArtifactKind::MindMap,
            requires_timestamps: false,
            sections: vec![SummaryTemplateSection {
                title: "Mind Map".into(),
                instruction: "Return one valid Mermaid `mindmap` diagram in a fenced `mermaid` block. Use a concise meeting topic as the root, 3-7 major branches, and short transcript-grounded child labels. Do not use HTML, click directives, icons, or paragraph-length nodes.".into(),
                format: "mermaid".into(),
                item_format: None,
            }],
        },
        SummaryTemplate {
            id: "project_sync".into(),
            name: "Project Sync".into(),
            description: "Status, blockers, decisions, and next steps for project meetings.".into(),
            kind: ArtifactKind::MeetingMinutes,
            requires_timestamps: false,
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
            kind: ArtifactKind::MeetingMinutes,
            requires_timestamps: false,
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
            kind: ArtifactKind::MeetingMinutes,
            requires_timestamps: false,
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

    #[test]
    fn builtins_have_unique_ids() {
        let templates = list_templates();
        for (index, template) in templates.iter().enumerate() {
            assert!(
                templates[..index]
                    .iter()
                    .all(|other| other.id != template.id),
                "duplicate template id: {}",
                template.id
            );
        }
    }

    #[test]
    fn specialized_templates_emit_machine_readable_skeletons() {
        let timeline = get_template("talking_points").unwrap().markdown_skeleton();
        assert!(timeline.contains("- [00:00] <Topic>"));

        let mind_map = get_template("mind_map").unwrap().markdown_skeleton();
        assert!(mind_map.contains("```mermaid\nmindmap"));
    }
}
