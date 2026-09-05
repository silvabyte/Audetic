use audetic_core::sync::{DeviceId, HubId, RecordId};
use bytes::Bytes;
use futures_util::{stream, StreamExt};
use sha2::{Digest, Sha256};

use crate::sync::identity::TAILSCALE_USER_LOGIN_HEADER;
use crate::sync::library::HubLibrary;
use crate::sync::protocol::{
    DictationPayload, DictationSnapshot, HubInfo, RecordKind, RecordingPayloadDescriptor,
    SnapshotBatch, HUB_ID_HEADER, PROTOCOL_VERSION, PROTOCOL_VERSION_HEADER,
};
use crate::sync::runtime::{HubRuntimeLauncher, TcpHubRuntimeLauncher};
use crate::sync::server::{HubServer, HubServerConfig};

use super::watchdog;

fn temp_file_count(root: &std::path::Path) -> usize {
    std::fs::read_dir(root)
        .map(|entries| entries.filter_map(Result::ok).count())
        .unwrap_or(0)
}

async fn wait_for_temp_file_count(root: &std::path::Path, expected: usize, label: &str) {
    watchdog(label, async {
        loop {
            if temp_file_count(root) == expected {
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await;
}

fn trusted_proxy_request(
    request: reqwest::RequestBuilder,
    hub_id: HubId,
    owner: &str,
) -> reqwest::RequestBuilder {
    request
        .header(TAILSCALE_USER_LOGIN_HEADER, owner)
        .header(HUB_ID_HEADER, hub_id.to_string())
        .header(PROTOCOL_VERSION_HEADER, PROTOCOL_VERSION.to_string())
}

#[tokio::test]
async fn real_tcp_reqwest_contract_covers_framing_disconnect_cleanup_and_shutdown() {
    let temp = tempfile::tempdir().unwrap();
    let db_path = temp.path().join("hub.db");
    crate::db::migrate_db_at(&db_path).unwrap();
    let hub_id = HubId::new();
    let owner = "Alice@Example.com";
    let server = HubServer::new(
        HubServerConfig::new(hub_id, owner)
            .unwrap()
            .with_library(HubLibrary::new(db_path.clone())),
    );
    let requested_address = "127.0.0.1:0".parse().unwrap();
    let (shutdown, receiver) = tokio::sync::oneshot::channel();
    let listener = TcpHubRuntimeLauncher
        .launch(server, requested_address, receiver)
        .await
        .unwrap();
    let address = listener
        .bound_address
        .expect("TCP launcher reports its actual bound address");
    assert_ne!(address.port(), 0);
    let server_task = tokio::spawn(listener.future);

    let raw_client = reqwest::Client::new();
    // trusted_proxy_request directly models Tailscale reverse-proxy output.
    // Its identity header is not application-controlled input.
    let handshake = trusted_proxy_request(
        raw_client.get(format!("http://{address}/v1/info")),
        hub_id,
        owner,
    )
    .send()
    .await
    .unwrap();
    assert_eq!(handshake.status(), reqwest::StatusCode::OK);
    assert_eq!(
        handshake.headers()[HUB_ID_HEADER],
        hub_id.to_string().as_str()
    );
    let info: HubInfo = handshake.json().await.unwrap();
    assert_eq!(info.hub_id, hub_id);

    let payload = vec![b'p'; 256 * 1024];
    let checksum = format!("{:x}", Sha256::digest(&payload));
    let record_id = RecordId::new();
    let snapshots = trusted_proxy_request(
        raw_client.post(format!("http://{address}/v1/snapshots")),
        hub_id,
        owner,
    )
    .json(&SnapshotBatch {
        snapshots: vec![DictationSnapshot {
            kind: RecordKind::Dictation,
            schema_version: 1,
            record_id,
            origin_device_id: DeviceId::new(),
            local_version: 1,
            created_at: "2026-09-05T00:00:00Z".into(),
            updated_at: "2026-09-05T00:00:00Z".into(),
            payload: DictationPayload {
                text: "tcp contract".into(),
                recording_payload: RecordingPayloadDescriptor::pending(
                    checksum.clone(),
                    payload.len() as u64,
                    "audio/wav".into(),
                ),
            },
        }
        .into()],
    })
    .send()
    .await
    .unwrap();
    assert_eq!(snapshots.status(), reqwest::StatusCode::OK);
    let upload = trusted_proxy_request(
        raw_client.put(format!("http://{address}/v1/blobs/{checksum}")),
        hub_id,
        owner,
    )
    .header("content-type", "audio/wav")
    .header("content-length", payload.len())
    .body(payload.clone())
    .send()
    .await
    .unwrap();
    assert_eq!(upload.status(), reqwest::StatusCode::CREATED);
    let range = trusted_proxy_request(
        raw_client.get(format!(
            "http://{address}/v1/dictations/{record_id}/payload"
        )),
        hub_id,
        owner,
    )
    .header("range", "bytes=7-31")
    .send()
    .await
    .unwrap();
    assert_eq!(range.status(), reqwest::StatusCode::PARTIAL_CONTENT);
    assert_eq!(range.content_length(), Some(25));
    assert_eq!(
        range.headers()["content-range"],
        format!("bytes 7-31/{}", payload.len())
    );
    assert_eq!(range.bytes().await.unwrap().as_ref(), &payload[7..=31]);

    let payload_url = format!("http://{address}/v1/dictations/{record_id}/payload");
    let mut download = trusted_proxy_request(raw_client.get(payload_url), hub_id, owner)
        .send()
        .await
        .unwrap();
    assert!(download.chunk().await.unwrap().is_some());
    drop(download);

    let interrupted_checksum = format!("{:x}", Sha256::digest(vec![b'i'; 32 * 1024]));
    let interrupted_record = RecordId::new();
    let interrupted_snapshot = trusted_proxy_request(
        raw_client.post(format!("http://{address}/v1/snapshots")),
        hub_id,
        owner,
    )
    .json(&SnapshotBatch {
        snapshots: vec![DictationSnapshot {
            kind: RecordKind::Dictation,
            schema_version: 1,
            record_id: interrupted_record,
            origin_device_id: DeviceId::new(),
            local_version: 1,
            created_at: "2026-09-05T00:00:01Z".into(),
            updated_at: "2026-09-05T00:00:01Z".into(),
            payload: DictationPayload {
                text: "interrupted TCP upload".into(),
                recording_payload: RecordingPayloadDescriptor::pending(
                    interrupted_checksum.clone(),
                    32 * 1024,
                    "audio/wav".into(),
                ),
            },
        }
        .into()],
    })
    .send()
    .await
    .unwrap();
    assert_eq!(interrupted_snapshot.status(), reqwest::StatusCode::OK);
    let first_chunk =
        stream::once(async { Ok::<_, std::io::Error>(Bytes::from(vec![b'i'; 4096])) });
    let never_finishes = stream::pending::<Result<Bytes, std::io::Error>>();
    let body = reqwest::Body::wrap_stream(first_chunk.chain(never_finishes));
    let upload_url = format!("http://{address}/v1/blobs/{interrupted_checksum}");
    let mut interrupted = tokio::spawn(async move {
        trusted_proxy_request(raw_client.put(upload_url), hub_id, owner)
            .header("content-type", "audio/wav")
            .header("content-length", 32 * 1024)
            .body(body)
            .send()
            .await
    });
    let server_temp_root = temp.path().join("sync/blobs/.tmp");
    tokio::select! {
        () = wait_for_temp_file_count(
            &server_temp_root,
            1,
            "waiting for interrupted upload temp file",
        ) => {}
        result = &mut interrupted => panic!("interrupted upload completed before cancellation: {result:?}"),
    }
    interrupted.abort();
    assert!(matches!(
        watchdog("joining interrupted real TCP upload", interrupted).await,
        Err(error) if error.is_cancelled()
    ));
    wait_for_temp_file_count(
        &server_temp_root,
        0,
        "waiting for interrupted upload cleanup",
    )
    .await;

    shutdown.send(()).unwrap();
    let result = watchdog("waiting for real TCP graceful shutdown", server_task)
        .await
        .unwrap();
    result.unwrap();
}
