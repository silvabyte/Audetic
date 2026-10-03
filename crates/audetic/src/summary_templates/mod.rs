//! Built-in audio note templates; meeting is a taxonomy, not an entity type.
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
    CleanedText,
    Intent,
    ShoppingItems,
}

impl ArtifactKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::MeetingMinutes => "meeting_minutes",
            Self::Summary => "summary",
            Self::ActionItems => "action_items",
            Self::TalkingPoints => "talking_points",
            Self::MindMap => "mind_map",
            Self::CleanedText => "cleaned_text",
            Self::Intent => "intent",
            Self::ShoppingItems => "shopping_items",
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
        let mut out = String::from("# <Concise audio note title>\n\n");
        for section in &self.sections {
            out.push_str(&format!("## {}\n\n", section.title));
            match section.format.as_str() {
                "timeline" => {
                    out.push_str("- [00:00] <Topic> - <What was discussed>\n\n");
                }
                "mermaid" => {
                    out.push_str("```mermaid\nmindmap\n  root((<Audio note topic>))\n```\n\n");
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
            id: "general_note".into(), name: "General Note".into(),
            description: "A thoughtful brief: the big picture, key ideas, decisions, and grounded next steps.".into(),
            kind: ArtifactKind::Summary,
            requires_timestamps: false,
            sections: briefing_sections(),
        },
        SummaryTemplate {
            id: "cleaned_dictation".into(), name: "Cleaned Dictation".into(),
            description: "Readable text preserving the speaker's meaning.".into(),
            kind: ArtifactKind::CleanedText,
            requires_timestamps: false,
            sections: vec![section("Cleaned Text", "Remove filler and correct punctuation. Preserve meaning, voice, and all substantive details.", "paragraph")],
        },
        SummaryTemplate {
            id: "conversation".into(), name: "Conversation".into(),
            description: "A topic-led account that preserves context, differing views, and follow-ups.".into(),
            kind: ArtifactKind::Summary,
            requires_timestamps: false,
            sections: briefing_sections(),
        },
        SummaryTemplate {
            id: "request_intent".into(), name: "Request Intent".into(),
            description: "Structured intent for explicit review; never execute requests.".into(),
            kind: ArtifactKind::Intent,
            requires_timestamps: false,
            sections: vec![section("Intent", "Extract the requested outcome, actions, and any shopping items without acting on them.", "paragraph")],
        },
        SummaryTemplate {
            id: "shopping_items".into(), name: "Shopping Items".into(),
            description: "Structured shopping items for explicit review; never purchase anything.".into(),
            kind: ArtifactKind::ShoppingItems,
            requires_timestamps: false,
            sections: vec![section("Shopping Items", "Extract only evidenced shopping items, quantities, and units without acting on them.", "list")],
        },
        SummaryTemplate {
            id: "standard_meeting".into(),
            name: "Meeting Minutes".into(),
            description: "A complete record of the discussion, decisions, and follow-ups.".into(),
            kind: ArtifactKind::MeetingMinutes,
            requires_timestamps: false,
            sections: briefing_sections(),
        },
        SummaryTemplate {
            id: "concise_summary".into(),
            name: "Concise Summary".into(),
            description: "The audio note's essential context and takeaways in a quick read.".into(),
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
                    instruction: "Extract explicit commitments and requested follow-ups with transcript evidence. Label suggested tasks as proposed; do not turn possibilities into commitments. Include owner, task, due date, and priority only when stated; otherwise use Unassigned, Not stated, or No due date. Preserve relative deadlines as spoken.".into(),
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
            description: "A visual hierarchy of the audio note's themes and supporting ideas.".into(),
            kind: ArtifactKind::MindMap,
            requires_timestamps: false,
            sections: vec![SummaryTemplateSection {
                title: "Mind Map".into(),
                instruction: "Return one valid Mermaid `mindmap` diagram in a fenced `mermaid` block. Use a concise topic as the root and 3-7 distinct topic branches (fewer for a short note). Keep each label to 2-8 words, use at most 3 levels below the root and at most 30 nodes total. Prioritize relationships, decisions, constraints, and next steps over a chronological transcript dump. Distinguish proposals from agreed decisions. Preserve important numbers and names only when evidenced. Use simple plain-text labels with no embedded newlines or special shape syntax. Do not use HTML, click directives, icons, initialization directives, or paragraph-length nodes.".into(),
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

fn briefing_sections() -> Vec<SummaryTemplateSection> {
    vec![
        section("At a glance", "Write 2-4 sentences explaining what this note is about, why it matters, and its actual outcome or current state. Identify participants or roles only when supported. Lead with substance, not 'This meeting discussed'. Scale the whole brief to the source: a short voice note needs only a few sentences; a complex conversation needs a detailed, scannable record. Do not pad short notes to fill every section.", "paragraph"),
        section("Key ideas", "Replace this placeholder heading with specific, descriptive H2 topic headings, one per substantive theme. Group related discussion even when it occurred at different times. Under each heading, give concise context and reasoning, then selective bullets for important facts, numbers, constraints, trade-offs, or disagreement. Use descriptive H3 subtopics when a theme contains several distinct issues, and short bold lead-ins on detailed bullets so the reader can scan the hierarchy. Retain consequential detail and who said what, including small scheduling or logistical commitments that affect next steps. Preserve conditions and uncertainty; reported claims are not verified facts. When the transcript is ambiguous or contradictory on a consequential detail, identify that uncertainty locally instead of silently repairing it. Avoid repeating the overview or retelling every exchange.", "paragraph"),
        section("Decisions & direction", "List only settled decisions as decisions, including rationale when stated. Clearly label proposals, recommendations, and conditional possibilities as such; never promote them to agreements. If no decisions or direction are evidenced, omit this section entirely.", "list"),
        SummaryTemplateSection {
            title: "Next steps".into(),
            instruction: "Extract explicit commitments and requested follow-ups. Start each task with a concrete verb, retain its owner and timing only when evidenced, and attach a short supporting quote or source timestamp. Use Unassigned or No due date for missing fields; preserve relative deadlines as spoken rather than inventing a calendar date. Label proposed follow-ups as proposed. Do not infer tasks merely to make the note actionable. Omit the section when there are none.".into(),
            format: "table".into(),
            item_format: Some("| Owner | Next step | When | Evidence |".into()),
        },
        section("Open questions", "Capture unresolved questions, blockers, contradictions, or dependencies that matter to the outcome. Distinguish what is unknown from what someone intends to investigate. Do not invent risks or advice. Omit the section when none are evidenced.", "list"),
    ]
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
    fn specialized_templates_have_typed_intent_and_skeletons() {
        let timeline = get_template("talking_points").unwrap();
        assert_eq!(timeline.kind, ArtifactKind::TalkingPoints);
        assert!(timeline.requires_timestamps);
        assert!(timeline.markdown_skeleton().contains("- [00:00] <Topic>"));

        let mind_map = get_template("mind_map").unwrap();
        assert_eq!(mind_map.kind, ArtifactKind::MindMap);
        assert!(mind_map.markdown_skeleton().contains("```mermaid\nmindmap"));

        assert_eq!(
            get_template("shopping_items").unwrap().kind,
            ArtifactKind::ShoppingItems
        );
    }
}
