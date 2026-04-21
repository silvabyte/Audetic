use anyhow::Result;
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
pub struct VoiceToTextData {
    pub text: String,
    pub audio_path: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", content = "payload")]
pub enum WorkflowData {
    VoiceToText(VoiceToTextData),
    // Will support more types later
}

#[derive(Debug)]
pub enum WorkflowType {
    VoiceToText,
}

impl WorkflowType {
    pub fn parse(s: &str) -> Result<WorkflowType> {
        match s {
            "VoiceToText" => Ok(WorkflowType::VoiceToText),
            _ => anyhow::bail!("Invalid workflow type: {}", s),
        }
    }

    pub fn to_str(&self) -> &str {
        match self {
            WorkflowType::VoiceToText => "VoiceToText",
        }
    }
}

#[derive(Debug)]
pub struct Workflow {
    pub id: Option<i64>,
    pub workflow_type: WorkflowType,
    pub data: WorkflowData,
    pub created_at: Option<String>,
}

impl Workflow {
    pub fn to_row(&self) -> Result<(String, String)> {
        Ok((
            self.workflow_type.to_str().to_string(),
            serde_json::to_string(&self.data)?,
        ))
    }

    pub fn from_row(
        id: i64,
        workflow_type: String,
        json: String,
        created_at: String,
    ) -> Result<Workflow> {
        Ok(Workflow {
            id: Some(id),
            workflow_type: WorkflowType::parse(&workflow_type)?,
            data: serde_json::from_str(&json)?,
            created_at: Some(created_at),
        })
    }

    pub fn new(workflow_type: WorkflowType, data: WorkflowData) -> Self {
        Workflow {
            id: None,
            workflow_type,
            data,
            created_at: None,
        }
    }
}
