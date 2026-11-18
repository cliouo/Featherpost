use maild::{
    compose::OutgoingMessage,
    drafts::DraftPatch,
    store::{DraftWrite, Store},
    transport::{Transport, TransportConfig},
};
fn saved(outcome: DraftWrite) -> maild::drafts::Draft {
    match outcome {
        DraftWrite::Saved(d) => *d,
        other => panic!("unexpected {other:?}"),
    }
}
#[test]
fn send_fence_rejects_stale_edits_uploads_and_duplicate_sends() {
    let store = Store::open("").unwrap();
    saved(
        store
            .create_draft("d", "a", &DraftPatch::default(), 1)
            .unwrap(),
    );
    let upload = store
        .begin_attachment_upload("user", "d", "x", "text/plain", 0)
        .unwrap();
    let locked = saved(store.claim_draft_send("d", 1, 2).unwrap());
    assert!(matches!(
        store.claim_draft_send("d", locked.version, 3).unwrap(),
        DraftWrite::SendLocked
    ));
    assert!(matches!(
        store
            .update_draft(
                "d",
                locked.version,
                &DraftPatch {
                    body: Some("changed".into()),
                    ..Default::default()
                },
                3
            )
            .unwrap(),
        DraftWrite::SendLocked
    ));
    assert!(matches!(
        store.delete_draft("d", None).unwrap(),
        DraftWrite::SendLocked
    ));
    assert!(matches!(
        store.finish_attachment_upload("user", &upload.upload_id, locked.version, 3),
        Err(maild::uploads::UploadError::Draft(DraftWrite::SendLocked))
    ));
    let sent = saved(
        store
            .settle_draft_send("d", locked.version, "sent", Some(false), 4)
            .unwrap(),
    );
    assert_eq!(sent.send_state, "sent");
    assert_eq!(sent.sent_archive, Some(false));
    assert!(matches!(
        store.claim_draft_send("d", sent.version, 5).unwrap(),
        DraftWrite::SendLocked
    ));
    assert!(matches!(
        store.resolve_draft_send("d", sent.version, 5).unwrap(),
        DraftWrite::SendLocked
    ));
    saved(store.delete_draft("d", Some(sent.version)).unwrap());
}
#[test]
fn restart_recovers_uncertainty_and_requires_an_explicit_resolution() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("mail.db");
    let store = Store::open(path.to_str().unwrap()).unwrap();
    saved(
        store
            .create_draft("d", "a", &DraftPatch::default(), 1)
            .unwrap(),
    );
    store.claim_draft_send("d", 1, 2).unwrap();
    drop(store);
    let store = Store::open(path.to_str().unwrap()).unwrap();
    let d = store.get_draft("d").unwrap().unwrap();
    assert_eq!(d.send_state, "unknown");
    assert!(matches!(
        store.claim_draft_send("d", d.version, 3).unwrap(),
        DraftWrite::SendLocked
    ));
    let resolved = saved(store.resolve_draft_send("d", d.version, 3).unwrap());
    assert_eq!(resolved.send_state, "failed");
    assert_eq!(
        saved(store.claim_draft_send("d", resolved.version, 4).unwrap()).send_state,
        "sending"
    );
}
#[test]
fn switching_accounts_is_versioned_and_keeps_uploaded_bytes() {
    let s = Store::open("").unwrap();
    saved(s.create_draft("d", "a", &DraftPatch::default(), 1).unwrap());
    let u = s
        .begin_attachment_upload("user", "d", "x", "text/plain", 1)
        .unwrap();
    s.write_attachment_upload("user", &u.upload_id, 0, b"x")
        .unwrap();
    s.finish_attachment_upload("user", &u.upload_id, 1, 2)
        .unwrap();
    let d = saved(
        s.update_draft(
            "d",
            2,
            &DraftPatch {
                account: Some("b".into()),
                from: Some("b@example.test".into()),
                ..Default::default()
            },
            3,
        )
        .unwrap(),
    );
    assert_eq!(d.account, "b");
    assert_eq!(
        s.draft_file_content("d", &u.upload_id)
            .unwrap()
            .unwrap()
            .content,
        b"x"
    );
    assert!(matches!(
        s.update_draft("d", 2, &DraftPatch::default(), 4).unwrap(),
        DraftWrite::Conflict { .. }
    ));
}
#[test]
fn v9_migrates_without_reinterpreting_existing_drafts() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("mail.db");
    let s = Store::open(path.to_str().unwrap()).unwrap();
    saved(
        s.create_draft(
            "d",
            "a",
            &DraftPatch {
                body: Some("keep".into()),
                ..Default::default()
            },
            1,
        )
        .unwrap(),
    );
    drop(s);
    let db = rusqlite::Connection::open(&path).unwrap();
    db.execute_batch("ALTER TABLE drafts DROP COLUMN send_state; ALTER TABLE drafts DROP COLUMN sent_archive; PRAGMA user_version=9;").unwrap();
    drop(db);
    let s = Store::open(path.to_str().unwrap()).unwrap();
    let d = s.get_draft("d").unwrap().unwrap();
    assert_eq!(d.body, "keep");
    assert_eq!(d.version, 1);
    assert_eq!(d.send_state, "editing");
}
async fn smtp(outcome: &str) -> Result<String, maild::transport::DeliveryFailure> {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let outcome = outcome.to_owned();
    let server = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        let (mut r, mut w) = socket.into_split();
        w.write_all(b"220 fixture\r\n").await.unwrap();
        let mut reader = BufReader::new(&mut r);
        let mut in_data = false;
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line).await.unwrap() == 0 {
                break;
            }
            if in_data {
                if line == ".\r\n" {
                    if outcome == "unknown" {
                        break;
                    }
                    w.write_all(if outcome == "reject" {
                        b"550 refused\r\n"
                    } else {
                        b"250 accepted\r\n"
                    })
                    .await
                    .unwrap();
                    break;
                }
                continue;
            }
            if line.starts_with("DATA") {
                in_data = true;
                w.write_all(b"354 proceed\r\n").await.unwrap();
            } else {
                w.write_all(b"250 ok\r\n").await.unwrap();
            }
        }
    });
    let result = Transport::from_config(TransportConfig {
        server: "127.0.0.1".into(),
        port,
        ..Default::default()
    })
    .send_message_checked(&OutgoingMessage {
        from: "me@example.test".into(),
        to: vec!["you@example.test".into()],
        subject: "test".into(),
        body: "content".into(),
        ..Default::default()
    })
    .await;
    server.await.unwrap();
    result
}
#[tokio::test]
async fn accepted_data_survives_a_closed_quit_connection() {
    assert!(smtp("accept").await.is_ok());
}
#[tokio::test]
async fn lost_data_response_is_unknown_but_explicit_refusal_is_safe_to_retry() {
    assert!(smtp("unknown").await.unwrap_err().uncertain);
    assert!(!smtp("reject").await.unwrap_err().uncertain);
}
