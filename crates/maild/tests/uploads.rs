//! G04: byte transport, durable drafts, principal isolation and real MIME acceptance.
use cordis::Context;
use maild::{
    api::{dispatch, JsonRpcRequest},
    drafts::DraftPatch,
    store::{DraftWrite, Store},
    uploads::{UploadError, CHUNK_BYTES, FILE_BYTES},
};
use serde_json::{json, Value};
use std::sync::Arc;
#[allow(dead_code)]
mod common;

fn draft(store: &Store, id: &str) {
    assert!(matches!(
        store
            .create_draft(id, "demo", &DraftPatch::default(), 1)
            .unwrap(),
        DraftWrite::Saved(_)
    ));
}
async fn rpc(
    ctx: &Context,
    method: &str,
    params: Value,
) -> Result<Value, maild::api::JsonRpcError> {
    let response = dispatch(
        ctx,
        &JsonRpcRequest {
            jsonrpc: "2.0".into(),
            method: method.into(),
            params,
            id: json!(1),
        },
    )
    .await;
    match response.error {
        Some(e) => Err(e),
        None => Ok(response.result.unwrap()),
    }
}
fn upload(store: &Store, id: &str, bytes: &[u8]) -> String {
    let u = store
        .begin_attachment_upload(
            "user",
            id,
            "新附件.bin",
            "application/octet-stream",
            bytes.len() as u64,
        )
        .unwrap();
    for (i, chunk) in bytes.chunks(CHUNK_BYTES).enumerate() {
        store
            .write_attachment_upload("user", &u.upload_id, (i * CHUNK_BYTES) as u64, chunk)
            .unwrap();
    }
    u.upload_id
}

#[test]
fn complete_is_atomic_versioned_and_idempotent() {
    let store = Store::open("").unwrap();
    draft(&store, "d");
    let id = upload(&store, "d", b"abc");
    let patch = DraftPatch {
        subject: Some("new subject".into()),
        ..Default::default()
    };
    store.update_draft("d", 1, &patch, 2).unwrap();
    assert!(matches!(
        store.finish_attachment_upload("user", &id, 1, 3),
        Err(UploadError::Draft(DraftWrite::Conflict { version: 2 }))
    ));
    assert!(store
        .get_draft("d")
        .unwrap()
        .unwrap()
        .attachments
        .is_empty());
    let done = store.finish_attachment_upload("user", &id, 2, 4).unwrap();
    assert_eq!(done.version, 3);
    assert_eq!(done.subject, "new subject");
    assert_eq!(done.attachments[0].upload_id.as_deref(), Some(id.as_str()));
    assert_eq!(
        store.finish_attachment_upload("user", &id, 2, 5).unwrap(),
        done
    );
    assert!(matches!(
        store.finish_attachment_upload("frontend:other", &id, 2, 5),
        Err(UploadError::Missing)
    ));
}

#[test]
fn offsets_retries_incomplete_and_cancellation_are_explicit() {
    let store = Store::open("").unwrap();
    draft(&store, "d");
    let u = store
        .begin_attachment_upload("user", "d", "x", "text/plain", 6)
        .unwrap();
    let id = &u.upload_id;
    assert!(matches!(
        store.write_attachment_upload("user", id, 1, b"abc"),
        Err(UploadError::State(0, 6))
    ));
    store
        .write_attachment_upload("user", id, 0, b"abc")
        .unwrap();
    assert_eq!(
        store
            .write_attachment_upload("user", id, 0, b"abc")
            .unwrap()
            .offset,
        3
    );
    assert!(matches!(
        store.write_attachment_upload("user", id, 0, b"bad"),
        Err(UploadError::State(3, 6))
    ));
    assert!(matches!(
        store.write_attachment_upload("user", id, 2, b"cde"),
        Err(UploadError::State(3, 6))
    ));
    assert!(matches!(
        store.finish_attachment_upload("user", id, 1, 2),
        Err(UploadError::State(3, 6))
    ));
    assert!(matches!(
        store.write_attachment_upload("other", id, 3, b"def"),
        Err(UploadError::Missing)
    ));
    store.cancel_attachment_upload("user", id).unwrap();
    assert!(matches!(
        store.write_attachment_upload("user", id, 3, b"def"),
        Err(UploadError::Missing)
    ));
    let empty = upload(&store, "d", b"");
    let done = store
        .finish_attachment_upload("user", &empty, 1, 2)
        .unwrap();
    assert_eq!(done.attachments[0].size, 0);
    assert!(done.attachments[0].stored);
}

#[test]
fn preserve_reorder_detach_delete_and_cross_draft_reference() {
    let store = Store::open("").unwrap();
    draft(&store, "d");
    draft(&store, "other");
    let a = upload(&store, "d", b"a");
    store.finish_attachment_upload("user", &a, 1, 2).unwrap();
    let b = upload(&store, "d", b"b");
    store.finish_attachment_upload("user", &b, 2, 3).unwrap();
    let patch: DraftPatch =
        serde_json::from_value(json!({"attachments": [{"upload_id": b}, {"upload_id": a}]}))
            .unwrap();
    assert!(matches!(
        store.update_draft("other", 1, &patch, 4).unwrap(),
        DraftWrite::AttachmentPartMissing { .. }
    ));
    assert_eq!(store.get_draft("other").unwrap().unwrap().version, 1);
    store.update_draft("d", 3, &patch, 4).unwrap();
    assert_eq!(
        store.get_draft("d").unwrap().unwrap().attachments[0]
            .upload_id
            .as_ref(),
        Some(&b)
    );
    assert_eq!(
        store.draft_file_content("d", &a).unwrap().unwrap().content,
        b"a"
    );
    let patch: DraftPatch =
        serde_json::from_value(json!({"attachments": [{"upload_id": a}]})).unwrap();
    store.update_draft("d", 4, &patch, 5).unwrap();
    assert!(store.draft_file_content("d", &b).unwrap().is_none());
    let unfinished = upload(&store, "d", b"x");
    store.delete_draft("d", Some(5)).unwrap();
    assert!(store.draft_file_content("d", &a).unwrap().is_none());
    assert!(matches!(
        store.finish_attachment_upload("user", &unfinished, 5, 6),
        Err(UploadError::Missing)
    ));
}

#[test]
fn file_and_draft_limits_do_not_mutate_saved_data() {
    let store = Store::open("").unwrap();
    draft(&store, "d");
    assert!(matches!(
        store.begin_attachment_upload("user", "d", "x", "text/plain", FILE_BYTES + 1),
        Err(UploadError::Limit(..))
    ));
    let data = vec![7; 13 * 1024 * 1024];
    let a = upload(&store, "d", &data);
    store.finish_attachment_upload("user", &a, 1, 2).unwrap();
    let b = upload(&store, "d", &data);
    assert!(matches!(
        store.finish_attachment_upload("user", &b, 2, 3),
        Err(UploadError::Limit("attachment_total_bytes", _))
    ));
    assert_eq!(store.get_draft("d").unwrap().unwrap().version, 2);
    store.cancel_attachment_upload("user", &b).unwrap();
    for _ in 1..50 {
        let id = upload(&store, "d", b"");
        let version = store.get_draft("d").unwrap().unwrap().version;
        store
            .finish_attachment_upload("user", &id, version, 4)
            .unwrap();
    }
    let id = upload(&store, "d", b"");
    assert!(matches!(
        store.finish_attachment_upload("user", &id, 51, 5),
        Err(UploadError::Limit("attachments", 50))
    ));
}

#[test]
fn completed_bytes_survive_restart_backup_and_restore() {
    let temp = tempfile::tempdir().unwrap();
    let db = temp.path().join("mail.db");
    let backup = temp.path().join("backup.db");
    let rollback = temp.path().join("rollback.db");
    let store = Store::open(db.to_str().unwrap()).unwrap();
    draft(&store, "d");
    let id = upload(&store, "d", b"persistent bytes");
    store.finish_attachment_upload("user", &id, 1, 2).unwrap();
    let incomplete = upload(&store, "d", b"not saved");
    maild::store::backup_database(&db, &backup).unwrap();
    drop(store);
    let store = Store::open(db.to_str().unwrap()).unwrap();
    assert!(matches!(
        store.finish_attachment_upload("user", &incomplete, 2, 3),
        Err(UploadError::Missing)
    ));
    assert_eq!(
        store
            .finish_attachment_upload("user", &id, 1, 3)
            .unwrap()
            .version,
        2
    );
    store.delete_draft("d", None).unwrap();
    drop(store);
    maild::store::restore_database(&db, &backup, &rollback).unwrap();
    let store = Store::open(db.to_str().unwrap()).unwrap();
    assert_eq!(
        store.draft_file_content("d", &id).unwrap().unwrap().content,
        b"persistent bytes"
    );
}

#[test]
fn v8_migration_preserves_draft_and_rejects_reserved_index() {
    let temp = tempfile::tempdir().unwrap();
    let db = temp.path().join("mail.db");
    let store = Store::open(db.to_str().unwrap()).unwrap();
    draft(&store, "d");
    drop(store);
    let conn = rusqlite::Connection::open(&db).unwrap();
    conn.execute_batch("DROP INDEX idx_draft_attachments_upload; ALTER TABLE draft_attachments DROP COLUMN upload_id; ALTER TABLE draft_attachments DROP COLUMN upload_owner; ALTER TABLE draft_attachments DROP COLUMN content; ALTER TABLE drafts DROP COLUMN send_state; ALTER TABLE drafts DROP COLUMN sent_archive; PRAGMA user_version=8;").unwrap();
    drop(conn);
    let store = Store::open(db.to_str().unwrap()).unwrap();
    assert_eq!(store.get_draft("d").unwrap().unwrap().version, 1);
    drop(store);
    let conn = rusqlite::Connection::open(&db).unwrap();
    conn.execute_batch("DROP INDEX idx_draft_attachments_upload; ALTER TABLE draft_attachments DROP COLUMN upload_id; ALTER TABLE draft_attachments DROP COLUMN upload_owner; ALTER TABLE draft_attachments DROP COLUMN content; CREATE INDEX idx_draft_attachments_upload ON drafts(id); ALTER TABLE drafts DROP COLUMN send_state; ALTER TABLE drafts DROP COLUMN sent_archive; PRAGMA user_version=8;").unwrap();
    drop(conn);
    assert!(matches!(
        Store::open(db.to_str().unwrap()),
        Err(maild::store::StoreOpenError::InvalidSchema(_))
    ));
    let conn = rusqlite::Connection::open(&db).unwrap();
    assert_eq!(
        conn.pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))
            .unwrap(),
        8
    );
}

#[tokio::test]
async fn rpc_rejects_paths_headers_malformed_chunks_and_unknown_keys() {
    let ctx = Context::new();
    let store = Arc::new(Store::open("").unwrap());
    draft(&store, "d");
    ctx.provide(store.clone());
    for filename in ["../secret", "/etc/hosts", "a\\b", "..", "a\r\nBcc: x@y"] {
        assert_eq!(
            rpc(
                &ctx,
                "beginAttachmentUpload",
                json!({"draft_id":"d","filename":filename,"mime_type":"text/plain","size":1})
            )
            .await
            .unwrap_err()
            .code,
            -32602
        );
    }
    assert_eq!(
        rpc(
            &ctx,
            "beginAttachmentUpload",
            json!({"draft_id":"d","filename":"x","mime_type":"text/plain\r\nEvil: yes","size":1})
        )
        .await
        .unwrap_err()
        .code,
        -32602
    );
    assert_eq!(rpc(&ctx,"beginAttachmentUpload",json!({"draft_id":"d","filename":"x","mime_type":"text/plain","size":1,"path":"/etc/hosts"})).await.unwrap_err().code,-32602);
    let id = upload(&store, "d", b"x");
    for data in [
        "?bad!".to_owned(),
        "a".repeat(CHUNK_BYTES.div_ceil(3) * 4 + 1),
    ] {
        assert_eq!(
            rpc(
                &ctx,
                "writeAttachmentUpload",
                json!({"upload_id":id,"offset":0,"data":data})
            )
            .await
            .unwrap_err()
            .code,
            -32602
        );
    }
    assert_eq!(store.get_draft("d").unwrap().unwrap().version, 1);
}

#[cfg(unix)]
#[tokio::test]
async fn real_socket_cli_large_upload_and_smtp_mime_round_trip() {
    use maild_client::{ComposeRequest, DraftAttachment, Endpoint, RpcClient};
    let server = common::fake_mail::FakeMail::start().await.unwrap();
    let root = tempfile::tempdir().unwrap();
    let socket = root.path().join("api.sock");
    let ctx = Context::new();
    let store = Arc::new(Store::open("").unwrap());
    draft(&store, "d");
    ctx.provide(store.clone());
    ctx.provide(Arc::new(maild::transport::Transport::from_config(
        maild::transport::TransportConfig {
            server: "127.0.0.1".into(),
            port: server.smtp_addr.port(),
            ..Default::default()
        },
    )));
    let listener = tokio::net::UnixListener::bind(&socket).unwrap();
    let (stop, done) = tokio::sync::oneshot::channel();
    let task = tokio::spawn(maild::api::serve_unix(listener, ctx, async {
        let _ = done.await;
    }));
    let bytes: Vec<u8> = (0..3 * 1024 * 1024 + 17).map(|i| (i % 251) as u8).collect();
    let path = root.path().join("原稿.bin");
    std::fs::write(&path, &bytes).unwrap();
    let result = tokio::process::Command::new(maild_brand::bin_exe_ctl!())
        .args([
            "--socket",
            socket.to_str().unwrap(),
            "--json",
            "draft",
            "attach-file",
            "d",
            "--version",
            "1",
            path.to_str().unwrap(),
        ])
        .output()
        .await
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let output: Value = serde_json::from_slice(&result.stdout).unwrap();
    assert!(output.to_string().len() < 2000, "no bytes in draft reply");
    let saved = store.get_draft("d").unwrap().unwrap();
    let id = saved.attachments[0].upload_id.clone().unwrap();
    assert_eq!(
        store.draft_file_content("d", &id).unwrap().unwrap().content,
        bytes
    );
    let alias = root.path().join("link.bin");
    std::os::unix::fs::symlink(&path, &alias).unwrap();
    let rejected = tokio::process::Command::new(maild_brand::bin_exe_ctl!())
        .args([
            "--socket",
            socket.to_str().unwrap(),
            "draft",
            "attach-file",
            "d",
            "--version",
            "2",
            alias.to_str().unwrap(),
        ])
        .output()
        .await
        .unwrap();
    assert!(!rejected.status.success());
    assert_eq!(store.get_draft("d").unwrap().unwrap().version, 2);
    let client = RpcClient::new(Endpoint::Unix {
        path: socket.clone(),
    })
    .unwrap();
    let compose = ComposeRequest {
        from: Some("me@example.test".into()),
        to: vec!["you@example.test".into()],
        bcc: vec!["secret@example.test".into()],
        body: "attached".into(),
        attachments: vec![DraftAttachment {
            draft_id: Some("d".into()),
            upload_id: Some(id.clone()),
            ..Default::default()
        }],
        ..Default::default()
    };
    client
        .call("send", serde_json::to_value(compose).unwrap())
        .await
        .unwrap();
    let messages = server.messages();
    assert_eq!(messages.len(), 1);
    let mime = mailparse::parse_mail(&messages[0]).unwrap();
    let file = mime
        .subparts
        .iter()
        .find(|p| p.ctype.mimetype == "application/octet-stream")
        .unwrap();
    assert_eq!(file.get_body_raw().unwrap(), bytes);
    assert!(!String::from_utf8_lossy(&messages[0]).contains("secret@example.test"));
    store.delete_draft("d", None).unwrap();
    assert_eq!(client.call("send",json!({"from":"me@example.test","to":"you@example.test","attachments":[{"draft_id":"d","upload_id":id}]})).await.unwrap_err().rpc_code,Some(-32003));
    assert_eq!(
        server.messages().len(),
        1,
        "missing attachment must not send partial mail"
    );
    let _ = stop.send(());
    task.await.unwrap().unwrap();
}

#[test]
fn quoted_filename_survives_mime_parameters() {
    for filename in [
        "report \"final\".txt",
        "report\"; name=\"evil.txt",
        "原稿 \"终版\".pdf",
    ] {
        let raw = maild::compose::assemble(&maild::compose::OutgoingMessage {
            from: "a@example.test".into(),
            to: vec!["b@example.test".into()],
            attachments: vec![maild::compose::OutgoingAttachment {
                filename: filename.into(),
                mime_type: "application/octet-stream".into(),
                content: b"data".to_vec(),
            }],
            ..Default::default()
        });
        let parsed = mailparse::parse_mail(raw.as_bytes()).unwrap();
        let part = parsed
            .subparts
            .iter()
            .find(|p| p.ctype.mimetype == "application/octet-stream")
            .unwrap();
        assert_eq!(
            part.get_content_disposition()
                .params
                .get("filename")
                .unwrap(),
            filename
        );
    }
}

#[test]
fn malformed_restored_upload_rows_are_not_treated_as_sendable() {
    for sql in [
        "UPDATE draft_attachments SET mime_type = 'text/plain' || char(13,10) || 'X-Injected: yes'",
        "UPDATE draft_attachments SET content = zeroblob(22020096), size = 22020096",
        "UPDATE draft_attachments SET content = zeroblob(20971520), size = 0",
    ] {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("mail.db");
        let store = Store::open(path.to_str().unwrap()).unwrap();
        draft(&store, "d");
        let id = upload(&store, "d", b"x");
        store.finish_attachment_upload("user", &id, 1, 2).unwrap();
        drop(store);
        let db = rusqlite::Connection::open(&path).unwrap();
        db.execute_batch(sql).unwrap();
        drop(db);
        let store = Store::open(path.to_str().unwrap()).unwrap();
        assert!(
            store.draft_file_content("d", &id).is_err(),
            "persisted invariant must be rechecked: {sql}"
        );
        assert!(!store.get_draft("d").unwrap().unwrap().attachments[0].stored);
        let empty = upload(&store, "d", b"");
        assert!(matches!(
            store.finish_attachment_upload("user", &empty, 2, 3),
            Err(UploadError::Draft(DraftWrite::AttachmentNotStored { .. }))
        ));
        assert_eq!(store.get_draft("d").unwrap().unwrap().version, 2);
    }
}
