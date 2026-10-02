use async_trait::async_trait;
use audetic_core::jobs_client::Segment;

use std::collections::VecDeque;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Mutex,
};

use crate::agents::{AgentRunOutput, AgentRunRequest};
use crate::db::audio_note_artifacts::{ArtifactStatus, AudioNoteArtifactRepository};

use super::*;

struct FakeAgent {
    outputs: Mutex<VecDeque<AgentRunOutput>>,
    calls: AtomicUsize,
    templates: Mutex<Vec<String>>,
    transcripts: Mutex<Vec<String>>,
}

impl FakeAgent {
    fn new(outputs: Vec<AgentRunOutput>) -> Self {
        Self {
            outputs: Mutex::new(outputs.into()),
            calls: AtomicUsize::new(0),
            templates: Mutex::new(vec![]),
            transcripts: Mutex::new(vec![]),
        }
    }
}

#[async_trait]
impl AgentRunner for FakeAgent {
    async fn run(&self, request: AgentRunRequest) -> Result<AgentRunOutput> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let template: serde_json::Value =
            serde_json::from_slice(&std::fs::read(request.paths.template_path)?)?;
        self.templates.lock().unwrap().push(
            template["id"]
                .as_str()
                .unwrap_or("classification")
                .to_string(),
        );
        self.transcripts
            .lock()
            .unwrap()
            .push(std::fs::read_to_string(request.paths.transcript_path)?);
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        self.outputs
            .lock()
            .unwrap()
            .pop_front()
            .context("unexpected extra agent execution")
    }
}

fn output(text: &str) -> AgentRunOutput {
    AgentRunOutput {
        success: true,
        exit_code: Some(0),
        stdout: text.into(),
        stderr: String::new(),
        timed_out: false,
    }
}

fn classified(kind: &str) -> AgentRunOutput {
    output(&serde_json::json!({"version":1,"kind":kind,"confidence":0.9,"title":"Generated title","topics":["test"],"participants":[],"metadata":{"custom":42}}).to_string())
}

fn setup(with_agent: bool, title: Option<&str>) -> (tempfile::TempDir, PathBuf, i64) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test.db");
    let conn = crate::db::init_db_at(&path).unwrap();
    if with_agent {
        // The runner is injected. An available executable only makes the profile
        // resolvable; no real subprocess or cloud call is made by these tests.
        conn.execute("INSERT INTO agent_profiles (name,kind,executable,args_json,prompt_mode,default_profile,enabled) VALUES ('fake','fake',?1,'[]','stdin',1,1)", [std::env::current_exe().unwrap().to_string_lossy().as_ref()]).unwrap();
    }
    let audio = dir.path().join("note.wav");
    let transcript = dir.path().join("note.txt");
    std::fs::write(&transcript, "Original raw transcript").unwrap();
    let id = AudioNoteRepository::insert(&conn, title, &audio.to_string_lossy()).unwrap();
    AudioNoteRepository::complete(
        &conn,
        id,
        &transcript.to_string_lossy(),
        "Original raw transcript",
        None,
        45,
    )
    .unwrap();
    (dir, path, id)
}

#[tokio::test]
async fn successful_enrichment_routes_and_dispatches_once_under_concurrency() {
    let (dir, path, id) = setup(true, None);
    let runner = FakeAgent::new(vec![
        classified("meeting"),
        output("# Meeting\nSummary, participants, decisions and action items"),
    ]);
    let events = Mutex::new(vec![]);
    let dispatch = |payload| events.lock().unwrap().push(payload);
    let registry = ProcessorRegistry::default();
    let (a, b) = tokio::join!(
        enrich_with_runner(id, &path, &runner, &registry, &dispatch),
        enrich_with_runner(id, &path, &runner, &registry, &dispatch)
    );
    a.unwrap();
    b.unwrap();
    enrich_with_runner(id, &path, &runner, &registry, &dispatch)
        .await
        .unwrap();
    assert_eq!(runner.calls.load(Ordering::SeqCst), 2);
    assert_eq!(
        *runner.templates.lock().unwrap(),
        vec!["classification", "standard_meeting"]
    );
    let conn = crate::db::init_db_at(&path).unwrap();
    let note = AudioNoteRepository::get(&conn, id).unwrap().unwrap();
    assert_eq!(note.status, "completed");
    assert_eq!(note.enrichment_status, "completed");
    assert_eq!(note.title.as_deref(), Some("Generated title"));
    assert_eq!(
        note.transcript_text.as_deref(),
        Some("Original raw transcript")
    );
    let artifacts = AudioNoteArtifactRepository::list_for_note(&conn, id).unwrap();
    assert_eq!(artifacts.len(), 1);
    assert_eq!(artifacts[0].kind, "meeting_minutes");
    let events = events.lock().unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].note_id, id);
    assert_eq!(events[0].transcript_text, "Original raw transcript");
    assert!(events[0].transcript_path.starts_with(dir.path()));
    assert_eq!(
        events[0].classification.as_ref().unwrap()["kind"],
        "meeting"
    );
}

#[tokio::test]
async fn no_profile_is_retryable_without_losing_raw_note() {
    let (_dir, path, id) = setup(false, None);
    let runner = FakeAgent::new(vec![]);
    let result = enrich_with_runner(id, &path, &runner, &ProcessorRegistry::default(), &|_| {
        panic!("no event on failure")
    })
    .await;
    assert!(result.is_err());
    let conn = crate::db::init_db_at(&path).unwrap();
    let note = AudioNoteRepository::get(&conn, id).unwrap().unwrap();
    assert_eq!(note.status, "completed");
    assert_eq!(note.enrichment_status, "error");
    assert!(note.enrichment_error.unwrap().contains("agent profile"));
    assert_eq!(
        note.transcript_text.as_deref(),
        Some("Original raw transcript")
    );
    assert_eq!(runner.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn invalid_classification_and_timeout_are_retryable() {
    for failed in [
        output("not valid JSON"),
        AgentRunOutput {
            timed_out: true,
            success: false,
            ..output("")
        },
    ] {
        let (_dir, path, id) = setup(true, Some("Manual title"));
        let runner = FakeAgent::new(vec![
            failed,
            classified("unknown-taxonomy"),
            output("# Generic summary"),
        ]);
        let events = AtomicUsize::new(0);
        let dispatch = |_| {
            events.fetch_add(1, Ordering::SeqCst);
        };
        let registry = ProcessorRegistry::default();
        assert!(enrich_with_runner(id, &path, &runner, &registry, &dispatch)
            .await
            .is_err());
        let conn = crate::db::init_db_at(&path).unwrap();
        assert_eq!(
            AudioNoteRepository::get(&conn, id)
                .unwrap()
                .unwrap()
                .enrichment_status,
            "error"
        );
        assert!(AudioNoteArtifactRepository::list_for_note(&conn, id)
            .unwrap()
            .is_empty());
        enrich_with_runner(id, &path, &runner, &registry, &dispatch)
            .await
            .unwrap();
        let note = AudioNoteRepository::get(&conn, id).unwrap().unwrap();
        assert_eq!(note.title.as_deref(), Some("Manual title"));
        assert_eq!(note.classification.unwrap()["kind"], "unknown-taxonomy");
        assert_eq!(note.enrichment_status, "completed");
        assert!(note.enrichment_error.is_none());
        assert_eq!(
            runner.templates.lock().unwrap().last().unwrap(),
            "general_note"
        );
        assert_eq!(events.load(Ordering::SeqCst), 1);
    }
}

#[tokio::test]
async fn processor_failure_is_durable_and_never_dispatches() {
    let (_dir, path, id) = setup(true, None);
    let runner = FakeAgent::new(vec![
        classified("request"),
        output("{\"shell_command\":\"do not execute me\"}"),
    ]);
    assert!(enrich_with_runner(
        id,
        &path,
        &runner,
        &ProcessorRegistry::default(),
        &|_| panic!("failure must not dispatch")
    )
    .await
    .is_err());
    let conn = crate::db::init_db_at(&path).unwrap();
    let note = AudioNoteRepository::get(&conn, id).unwrap().unwrap();
    assert_eq!(note.status, "completed");
    assert_eq!(note.enrichment_status, "error");
    assert_eq!(note.classification.unwrap()["kind"], "request");
    let artifacts = AudioNoteArtifactRepository::list_for_note(&conn, id).unwrap();
    assert_eq!(artifacts.len(), 1);
    assert_eq!(artifacts[0].status, ArtifactStatus::Error);
    assert!(artifacts[0].error.as_deref().unwrap().contains("intent"));
}

#[tokio::test]
async fn shopping_produces_structured_data_not_execution() {
    let (_dir, path, id) = setup(true, None);
    let intent = serde_json::json!({"version":1,"intent":"Buy milk","shopping_items":[{"name":"milk","quantity":2,"unit":"liters"}],"metadata":{}});
    let runner = FakeAgent::new(vec![
        classified("shopping-list"),
        output(&intent.to_string()),
    ]);
    enrich_with_runner(id, &path, &runner, &ProcessorRegistry::default(), &|_| {})
        .await
        .unwrap();
    let conn = crate::db::init_db_at(&path).unwrap();
    let artifacts = AudioNoteArtifactRepository::list_for_note(&conn, id).unwrap();
    assert_eq!(artifacts[0].kind, "shopping_items");
    assert_eq!(artifacts[0].template_id.as_deref(), Some("shopping_items"));
    assert_eq!(artifacts[0].content_json.as_ref(), Some(&intent));
    assert_eq!(artifacts[0].status, ArtifactStatus::Completed);
    assert_eq!(runner.calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn artifact_kind_and_timestamped_input_come_from_the_template() {
    let (dir, path, id) = setup(true, None);
    let conn = crate::db::init_db_at(&path).unwrap();
    let transcript_path = dir.path().join("note.txt");
    let segments = vec![
        Segment {
            start: 3.8,
            end: 8.0,
            text: " Opening context ".into(),
        },
        Segment {
            start: 3661.2,
            end: 3665.0,
            text: "Closing thought".into(),
        },
    ];
    AudioNoteRepository::complete(
        &conn,
        id,
        &transcript_path.to_string_lossy(),
        "Original raw transcript",
        Some(&segments),
        3666,
    )
    .unwrap();
    drop(conn);
    let runner = FakeAgent::new(vec![output(
        "# Topics\n\n## Talking Points\n\n- [00:03] Opening - Context",
    )]);

    let artifact = generate_with_runner(
        id,
        GenerateArtifactRequest {
            template_id: "talking_points".into(),
            agent_profile_id: None,
            custom_context: None,
        },
        &path,
        &runner,
    )
    .await
    .unwrap();

    assert_eq!(artifact.kind, "talking_points");
    assert_eq!(artifact.template_id.as_deref(), Some("talking_points"));
    let transcripts = runner.transcripts.lock().unwrap();
    assert!(transcripts[0].contains("[00:03] Opening context"));
    assert!(transcripts[0].contains("[1:01:01] Closing thought"));
}

#[tokio::test]
async fn timestamp_requirement_is_enforced_before_artifact_insertion() {
    let (_dir, path, id) = setup(true, None);
    let runner = FakeAgent::new(vec![]);

    let error = generate_with_runner(
        id,
        GenerateArtifactRequest {
            template_id: "talking_points".into(),
            agent_profile_id: None,
            custom_context: None,
        },
        &path,
        &runner,
    )
    .await
    .unwrap_err();

    assert!(error.to_string().contains("requires timestamped"));
    assert_eq!(runner.calls.load(Ordering::SeqCst), 0);
    let conn = crate::db::init_db_at(&path).unwrap();
    assert!(AudioNoteArtifactRepository::list_for_note(&conn, id)
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn interrupted_running_claim_recovers_only_with_explicit_startup_sweep() {
    let (_dir, path, id) = setup(true, None);
    let conn = crate::db::init_db_at(&path).unwrap();
    assert!(AudioNoteRepository::claim_enrichment(&conn, id).unwrap());
    let runner = FakeAgent::new(vec![classified("dictation"), output("Cleaned text")]);
    let registry = ProcessorRegistry::default();
    enrich_with_runner(id, &path, &runner, &registry, &|_| {})
        .await
        .unwrap();
    assert_eq!(runner.calls.load(Ordering::SeqCst), 0);
    assert_eq!(
        AudioNoteRepository::sweep_interrupted_enrichment(&conn).unwrap(),
        1
    );
    enrich_with_runner(id, &path, &runner, &registry, &|_| {})
        .await
        .unwrap();
    assert_eq!(
        runner.templates.lock().unwrap().last().unwrap(),
        "cleaned_dictation"
    );
}

#[test]
fn structured_intent_rejects_wrong_types_and_unsafe_top_level_commands() {
    use crate::audio_note_artifacts::parse_actionable;
    assert!(parse_actionable("not JSON").is_err());
    assert!(parse_actionable(r#"{"version":1,"intent":"buy","shopping_items":[{"name":"milk","quantity":-2,"unit":null}],"metadata":{}}"#).is_err());
    assert!(parse_actionable(
        r#"{"version":1,"intent":"buy","shopping_items":[],"metadata":{},"command":"ls"}"#
    )
    .is_err());
}

#[tokio::test(start_paused = true)]
async fn hung_agent_interaction_becomes_visible_error() {
    struct HungAgent;
    #[async_trait]
    impl AgentRunner for HungAgent {
        async fn run(&self, _: AgentRunRequest) -> Result<AgentRunOutput> {
            std::future::pending().await
        }
    }
    let (_dir, path, id) = setup(true, None);
    assert!(enrich_with_runner(
        id,
        &path,
        &HungAgent,
        &ProcessorRegistry::default(),
        &|_| panic!("must not dispatch")
    )
    .await
    .is_err());
    let conn = crate::db::init_db_at(&path).unwrap();
    let note = AudioNoteRepository::get(&conn, id).unwrap().unwrap();
    assert_eq!(note.status, "completed");
    assert_eq!(note.enrichment_status, "error");
    assert!(note.enrichment_error.unwrap().contains("timed out"));
}

#[tokio::test]
async fn deletion_during_classification_or_processor_suppresses_results_and_events() {
    struct DeletingAgent<'a> {
        inner: &'a FakeAgent,
        path: &'a Path,
        id: i64,
        delete_on_call: usize,
    }
    #[async_trait]
    impl AgentRunner for DeletingAgent<'_> {
        async fn run(&self, request: AgentRunRequest) -> Result<AgentRunOutput> {
            let output = self.inner.run(request).await?;
            if self.inner.calls.load(Ordering::SeqCst) == self.delete_on_call {
                let conn = crate::db::init_db_at(self.path)?;
                assert_eq!(
                    AudioNoteRepository::soft_delete(&conn, self.id)?,
                    crate::db::audio_notes::SoftDeleteOutcome::Deleted
                );
            }
            Ok(output)
        }
    }
    for delete_on_call in [1, 2] {
        let (_dir, path, id) = setup(true, None);
        let inner = FakeAgent::new(vec![classified("general"), output("# Summary")]);
        let runner = DeletingAgent {
            inner: &inner,
            path: &path,
            id,
            delete_on_call,
        };
        let result = enrich_with_runner(id, &path, &runner, &ProcessorRegistry::default(), &|_| {
            panic!("deleted notes must not dispatch")
        })
        .await;
        assert!(result.unwrap_err().to_string().contains("removed"));
        let conn = crate::db::init_db_at(&path).unwrap();
        assert!(AudioNoteRepository::get(&conn, id).unwrap().is_none());
        let (status, enrichment, raw, classification): (String, String, String, Option<String>) = conn.query_row("SELECT status,enrichment_status,transcript_text,classification FROM audio_notes WHERE id=?1", [id], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).unwrap();
        assert_eq!(status, "completed");
        assert_eq!(enrichment, "error");
        assert_eq!(raw, "Original raw transcript");
        assert_eq!(inner.calls.load(Ordering::SeqCst), delete_on_call);
        let artifacts = AudioNoteArtifactRepository::list_for_note(&conn, id).unwrap();
        if delete_on_call == 1 {
            assert!(classification.is_none());
            assert!(artifacts.is_empty());
        } else {
            assert_eq!(artifacts.len(), 1);
            assert_eq!(artifacts[0].status, ArtifactStatus::Error);
            assert!(artifacts[0].content_markdown.is_none());
            assert!(artifacts[0].error.as_deref().unwrap().contains("removed"));
        }
    }
}

#[tokio::test]
async fn unavailable_executable_is_visible_without_running_a_fallback_heuristic() {
    let (_dir, path, id) = setup(true, None);
    let conn = crate::db::init_db_at(&path).unwrap();
    conn.execute(
        "UPDATE agent_profiles SET executable='/nonexistent/audetic-test-agent'",
        [],
    )
    .unwrap();
    let runner = FakeAgent::new(vec![]);
    assert!(enrich_with_runner(
        id,
        &path,
        &runner,
        &ProcessorRegistry::default(),
        &|_| panic!("must not dispatch")
    )
    .await
    .is_err());
    let note = AudioNoteRepository::get(&conn, id).unwrap().unwrap();
    assert_eq!(note.status, "completed");
    assert_eq!(note.enrichment_status, "error");
    assert!(note.classification.is_none());
    assert_eq!(runner.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn custom_registry_routes_without_changing_note_or_classification_schema() {
    let (_dir, path, id) = setup(true, None);
    let mut registry = ProcessorRegistry::default();
    registry
        .register(
            "project-handoff",
            registry::Processor {
                template_id: "action_items".into(),
            },
        )
        .unwrap();
    let runner = FakeAgent::new(vec![classified("project-handoff"), output("# Follow-ups")]);
    enrich_with_runner(id, &path, &runner, &registry, &|_| {})
        .await
        .unwrap();
    let conn = crate::db::init_db_at(&path).unwrap();
    let artifacts = AudioNoteArtifactRepository::list_for_note(&conn, id).unwrap();
    assert_eq!(artifacts[0].kind, "action_items");
    assert_eq!(artifacts[0].template_id.as_deref(), Some("action_items"));
    assert_eq!(
        AudioNoteRepository::get(&conn, id)
            .unwrap()
            .unwrap()
            .classification
            .unwrap()["kind"],
        "project-handoff"
    );
}

#[tokio::test]
async fn useful_request_actions_are_structured_data_with_no_implicit_execution() {
    let (_dir, path, id) = setup(true, None);
    let request = serde_json::json!({"version":1,"intent":"Send the launch brief","actions":[{"description":"Email the launch brief to Sam","assignee":"Alex","due":"tomorrow"}],"shopping_items":[],"metadata":{"recipient":"Sam"}});
    let runner = FakeAgent::new(vec![classified("request"), output(&request.to_string())]);
    enrich_with_runner(id, &path, &runner, &ProcessorRegistry::default(), &|_| {})
        .await
        .unwrap();
    let conn = crate::db::init_db_at(&path).unwrap();
    let artifacts = AudioNoteArtifactRepository::list_for_note(&conn, id).unwrap();
    assert_eq!(artifacts[0].kind, "intent");
    assert_eq!(artifacts[0].content_json.as_ref(), Some(&request));
    assert_eq!(runner.calls.load(Ordering::SeqCst), 2);
    let mut invalid = request;
    invalid["actions"][0]["description"] = serde_json::json!("");
    assert!(crate::audio_note_artifacts::parse_actionable(&invalid.to_string()).is_err());
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn blocked_stdin_timeout_kills_the_local_agent_process() {
    use crate::db::agent_profiles::PromptMode;
    let (dir, path, _) = setup(true, None);
    let pid_path = dir.path().join("child.pid");
    let conn = crate::db::init_db_at(&path).unwrap();
    let mut profile = resolve_profile(&conn, None).unwrap();
    profile.executable = "sh".into();
    profile.prompt_mode = PromptMode::Stdin;
    // The test child ignores stdin; exec ensures the recorded PID is the exact
    // child owned by run_agent, not a separate shell descendant.
    profile.args = vec![
        "-c".into(),
        "echo $$ > \"$1\"; exec sleep 60".into(),
        "audetic-test".into(),
        pid_path.to_string_lossy().into_owned(),
    ];
    let mut request = prepare_agent_request(
        &path,
        "timeout-test",
        profile,
        "x".repeat(1_000_000),
        "raw",
        serde_json::json!({}),
    )
    .unwrap();
    request.timeout_seconds = 1;
    let error = run_with_timeout(&LocalAgent, request).await.unwrap_err();
    assert!(error.to_string().contains("timed out"));
    let pid = std::fs::read_to_string(pid_path).unwrap();
    let proc_path = PathBuf::from(format!("/proc/{}", pid.trim()));
    for _ in 0..100 {
        if !proc_path.exists() {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("timed-out agent process {} was not reaped", pid.trim());
}

#[test]
fn run_files_are_absolute_and_isolated_between_databases_in_one_directory() {
    let (dir, path, _) = setup(true, None);
    let other_path = dir.path().join("other.db");
    crate::db::init_db_at(&other_path).unwrap();
    let conn = crate::db::init_db_at(&path).unwrap();
    let profile = resolve_profile(&conn, None).unwrap();
    let first = prepare_agent_request(
        &path,
        "same-id",
        profile.clone(),
        "one".into(),
        "raw one",
        serde_json::json!({}),
    )
    .unwrap();
    let other = prepare_agent_request(
        &other_path,
        "same-id",
        profile,
        "two".into(),
        "raw two",
        serde_json::json!({}),
    )
    .unwrap();
    assert_ne!(first.paths.run_dir, other.paths.run_dir);
    assert!(first.paths.prompt_path.is_absolute());
    assert!(other.paths.transcript_path.is_absolute());
    assert_eq!(
        std::fs::read_to_string(first.paths.transcript_path).unwrap(),
        "raw one"
    );
    assert_eq!(
        std::fs::read_to_string(other.paths.transcript_path).unwrap(),
        "raw two"
    );
}
