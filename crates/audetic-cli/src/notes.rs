//! Audio Notes is an HTTP consumer; the daemon owns capture and persistence.
use anyhow::{bail, Context, Result};
use audetic_core::url::{
    api_url, audio_note_path, audio_note_process_path, audio_note_retry_path, paths,
};
use reqwest::{Client, Method, RequestBuilder};
use serde_json::{json, Value};
use std::path::Path;

use crate::args::{CaptureArgs, NotesCliArgs, NotesCommand};
use crate::client::{json_or_error, CONNECT_HINT};

pub async fn handle_notes_command(args: NotesCliArgs) -> Result<()> {
    let client = Client::new();
    let request = match args.command {
        NotesCommand::Start(options) => capture_request(&client, paths::AUDIO_NOTES_START, options),
        NotesCommand::Toggle(options) => {
            capture_request(&client, paths::AUDIO_NOTES_TOGGLE, options)
        }
        NotesCommand::Stop => client.post(api_url(paths::AUDIO_NOTES_STOP)),
        NotesCommand::Cancel => client.post(api_url(paths::AUDIO_NOTES_CANCEL)),
        NotesCommand::Confirm { start, end } => client
            .post(api_url(paths::AUDIO_NOTES_CONFIRM))
            .json(&trim_body(start.as_deref(), end.as_deref())?),
        NotesCommand::Status => client.get(api_url(paths::AUDIO_NOTES_STATUS)),
        NotesCommand::List {
            limit,
            offset,
            query,
            kind,
        } => list_request(&client, limit, offset, query.as_deref(), kind.as_deref()),
        NotesCommand::Show { id } => note_request(&client, Method::GET, id, audio_note_path)?,
        NotesCommand::Delete { id } => note_request(&client, Method::DELETE, id, audio_note_path)?,
        NotesCommand::Retry { id } => {
            note_request(&client, Method::POST, id, audio_note_retry_path)?
        }
        NotesCommand::Process { id } => {
            note_request(&client, Method::POST, id, audio_note_process_path)?
        }
        NotesCommand::Import { path, title } => import_request(&client, &path, title).await?,
        NotesCommand::Copy { id } => {
            let response = note_request(&client, Method::GET, id, audio_note_path)?
                .send()
                .await
                .context(CONNECT_HINT)?;
            let note = json_or_error(response, "read audio note").await?;
            let text = note
                .get("transcript_text")
                .and_then(Value::as_str)
                .filter(|text| !text.is_empty())
                .context("Audio note has no transcript yet")?;
            audetic_core::clipboard::copy_to_clipboard_sync(text)?;
            println!("Copied audio note #{id} transcript.");
            return Ok(());
        }
    };
    let response = request.send().await.context(CONNECT_HINT)?;
    // Preserve extensible classification metadata and enrichment errors in output.
    println!(
        "{}",
        serde_json::to_string_pretty(&json_or_error(response, "audio notes").await?)?
    );
    Ok(())
}

fn capture_request(client: &Client, path: &str, options: CaptureArgs) -> RequestBuilder {
    let mut body = json!({
        "title": options.title,
        "capture_source": options.capture_source.as_str(),
        "review_before_processing": options.review_before_processing,
        "copy_to_clipboard": options.copy_to_clipboard,
    });
    // Omission allows the daemon to honor an explicitly configured preference.
    if let Some(auto_paste) = options.auto_paste {
        body["auto_paste"] = json!(auto_paste);
    }
    client.post(api_url(path)).json(&body)
}

fn note_request(
    client: &Client,
    method: Method,
    id: i64,
    path: fn(i64) -> String,
) -> Result<RequestBuilder> {
    if id <= 0 {
        bail!("Audio note ID must be positive");
    }
    Ok(client.request(method, api_url(&path(id))))
}

fn list_request(
    client: &Client,
    limit: usize,
    offset: usize,
    query: Option<&str>,
    kind: Option<&str>,
) -> RequestBuilder {
    let mut request = client
        .get(api_url(paths::AUDIO_NOTES))
        .query(&[("limit", limit), ("offset", offset)]);
    if let Some(query) = query {
        request = request.query(&[("query", query)]);
    }
    if let Some(kind) = kind {
        request = request.query(&[("kind", kind)]);
    }
    request
}

async fn import_request(
    client: &Client,
    path: &Path,
    title: Option<String>,
) -> Result<RequestBuilder> {
    let file = tokio::fs::File::open(path)
        .await
        .with_context(|| format!("Failed to open {}", path.display()))?;
    let metadata = file.metadata().await?;
    if !metadata.is_file() {
        bail!("Not a regular file: {}", path.display());
    }
    let filename = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("upload")
        .to_owned();
    let mime = path
        .extension()
        .and_then(|e| e.to_str())
        .and_then(audetic_core::jobs_client::mime_type_for_extension)
        .unwrap_or("application/octet-stream");
    let body = reqwest::Body::wrap_stream(tokio_util::io::ReaderStream::new(file));
    let part = reqwest::multipart::Part::stream_with_length(body, metadata.len())
        .file_name(filename)
        .mime_str(mime)?;
    let mut form = reqwest::multipart::Form::new().part("file", part);
    if let Some(title) = title {
        form = form.text("title", title);
    }
    Ok(client
        .post(api_url(paths::AUDIO_NOTES_IMPORT))
        .multipart(form))
}

fn parse_timecode(value: &str) -> Result<f64> {
    let parts: Vec<_> = value.trim().split(':').collect();
    if parts.len() > 3 {
        bail!("Expected SS, MM:SS, or HH:MM:SS");
    }
    let mut seconds = 0.0;
    for part in parts {
        let component: f64 = part.parse().context("Invalid time component")?;
        if !component.is_finite() || component < 0.0 {
            bail!("Time must be finite and non-negative");
        }
        seconds = seconds * 60.0 + component;
    }
    if !seconds.is_finite() {
        bail!("Time is too large");
    }
    Ok(seconds)
}

fn trim_body(start: Option<&str>, end: Option<&str>) -> Result<Value> {
    let start = start.map(parse_timecode).transpose()?;
    let end = end.map(parse_timecode).transpose()?;
    if let Some(end) = end {
        if end <= start.unwrap_or(0.0) {
            bail!("--end must be after --start");
        }
    }
    let mut body = json!({});
    if let Some(start) = start {
        body["start_seconds"] = json!(start);
    }
    if let Some(end) = end {
        body["end_seconds"] = json!(end);
    }
    Ok(body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::args::{Cli, CliCommand};
    use clap::Parser;

    #[test]
    fn capture_request_uses_unified_api_and_safe_defaults() {
        let cli = Cli::try_parse_from(["audetic", "notes", "start"]).unwrap();
        let Some(CliCommand::Notes(NotesCliArgs {
            command: NotesCommand::Start(options),
        })) = cli.command
        else {
            panic!("wrong command")
        };
        let request = capture_request(&Client::new(), paths::AUDIO_NOTES_START, options)
            .build()
            .unwrap();
        assert_eq!(request.url().path(), "/api/audio-notes/start");
        let body: Value =
            serde_json::from_slice(request.body().unwrap().as_bytes().unwrap()).unwrap();
        assert_eq!(body["capture_source"], "microphone");
        assert_eq!(body["copy_to_clipboard"], false);
        assert_eq!(body["review_before_processing"], false);
        assert!(body.get("auto_paste").is_none());
    }

    #[test]
    fn removed_commands_are_not_accepted() {
        for command in ["history", "meeting", "transcribe"] {
            assert!(Cli::try_parse_from(["audetic", command]).is_err());
        }
    }

    #[test]
    fn list_query_is_encoded_not_interpolated() {
        let request = list_request(
            &Client::new(),
            5,
            10,
            Some("tea & coffee"),
            Some("shopping-list"),
        )
        .build()
        .unwrap();
        let pairs: std::collections::HashMap<_, _> = request.url().query_pairs().collect();
        assert_eq!(pairs.get("query").unwrap(), "tea & coffee");
        assert_eq!(pairs.get("kind").unwrap(), "shopping-list");
        assert_eq!(pairs.get("offset").unwrap(), "10");
    }

    #[test]
    fn trim_validation_preserves_fields_and_rejects_nonfinite_values() {
        assert_eq!(
            trim_body(Some("1:05.5"), Some("2:00")).unwrap(),
            json!({"start_seconds":65.5,"end_seconds":120.0})
        );
        for bad in ["NaN", "inf", "-1", "", "1:2:3:4"] {
            assert!(parse_timecode(bad).is_err());
        }
        assert!(trim_body(Some("5"), Some("4")).is_err());
        assert!(trim_body(None, Some("0")).is_err());
    }

    #[tokio::test]
    async fn capture_http_roundtrip_and_api_error_envelope() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut buffer = [0u8; 1024];
            loop {
                let count = socket.read(&mut buffer).await.unwrap();
                assert_ne!(count, 0);
                request.extend_from_slice(&buffer[..count]);
                if let Some(header_end) = request.windows(4).position(|bytes| bytes == b"\r\n\r\n")
                {
                    let headers = String::from_utf8_lossy(&request[..header_end]);
                    let length: usize = headers
                        .lines()
                        .find_map(|line| {
                            line.to_ascii_lowercase()
                                .strip_prefix("content-length: ")
                                .map(str::to_owned)
                        })
                        .unwrap()
                        .parse()
                        .unwrap();
                    if request.len() >= header_end + 4 + length {
                        break;
                    }
                }
            }
            let request = String::from_utf8(request).unwrap();
            assert!(request.starts_with("POST /api/audio-notes/toggle HTTP/1.1"));
            let body: Value =
                serde_json::from_str(request.split_once("\r\n\r\n").unwrap().1).unwrap();
            assert_eq!(body["capture_source"], "microphone_and_system");
            assert_eq!(body["auto_paste"], false);
            let body = r#"{"message":"Capture already awaiting review"}"#;
            socket.write_all(format!("HTTP/1.1 409 Conflict\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
        });
        let cli = Cli::try_parse_from([
            "audetic",
            "notes",
            "toggle",
            "--capture-source",
            "microphone_and_system",
            "--auto-paste=false",
        ])
        .unwrap();
        let Some(CliCommand::Notes(NotesCliArgs {
            command: NotesCommand::Toggle(options),
        })) = cli.command
        else {
            panic!("wrong command")
        };
        let client = Client::new();
        let mut request = capture_request(&client, paths::AUDIO_NOTES_TOGGLE, options)
            .build()
            .unwrap();
        request.url_mut().set_port(Some(port)).unwrap();
        let response = client.execute(request).await.unwrap();
        let error = json_or_error(response, "toggle").await.unwrap_err();
        assert_eq!(error.to_string(), "Capture already awaiting review");
        server.await.unwrap();
    }
}
