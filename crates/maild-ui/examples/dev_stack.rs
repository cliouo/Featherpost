//! 预置假数据的临时 daemon，用来手工体验 / 评审默认 UI 壳，**不碰你机器上的真实安装**。
//!
//! ```sh
//! cargo run -p maild-ui --example dev_stack
//! # 另一个终端，按它打印出来的命令起壳：
//! cargo run -p maild-ui -- --socket /tmp/maild-ui-dev-<pid>/runtime/mail.sock
//! ```
//!
//! 两个账户指向一个关着的端口；另一个受管账户只连本机假 IMAP/SMTP，可验收真实发送。
//! 前两个账户显示为离线、正文来自缓存——这正是
//! UX-SPEC §8「单账户离线：可读缓存」那一行，不是 bug。Ctrl-C 结束；临时目录留在 /tmp 供检查。

#[allow(dead_code)]
#[path = "../../maild/tests/common/fake_mail.rs"]
mod fake_mail;

use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use maild::events::{MailAttachment, MailMessage};
use maild::folders::FolderRole;
use maild::store::Store;
use maild::{run_daemon_until, DaemonOptions};
use maild_paths::AppPaths;

use maild_ui::FRONTEND_ID;

fn secure_dir(path: &Path) {
    std::fs::create_dir_all(path).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
}

fn mail(
    account: &str,
    folder: &str,
    uid: u32,
    sender: &str,
    subject: &str,
    body: &str,
    when: u64,
) -> MailMessage {
    MailMessage {
        account: account.into(),
        uid: uid.to_string(),
        sender: sender.into(),
        recipient: format!("{account}@example.test"),
        subject: subject.into(),
        body: body.into(),
        received_at: when,
        date_at: Some(when - 120),
        folder: folder.into(),
        seen: true,
        ..Default::default()
    }
}

fn seed_with_remote_host(store: &Store, remote_host: &str) {
    seed(store, remote_host);
}

fn seed(store: &Store, remote_host: &str) {
    let day = 86_400;
    let base = 1_757_500_000_u64; // 2025-09-10 前后
    for (account, folders) in [
        (
            "personal",
            vec![
                ("INBOX", "收件箱", Some(FolderRole::Inbox)),
                ("Sent", "已发送", Some(FolderRole::Sent)),
                ("Archive", "归档", Some(FolderRole::Archive)),
                ("&XfJT0ZAB-", "订阅", None),
            ],
        ),
        (
            "work",
            vec![
                ("INBOX", "收件箱", Some(FolderRole::Inbox)),
                ("Sent", "已发送", Some(FolderRole::Sent)),
                ("Junk", "垃圾邮件", Some(FolderRole::Spam)),
            ],
        ),
    ] {
        for (name, display, role) in folders {
            store.upsert_folder(account, name, display, role).unwrap();
        }
    }
    let mut m1 = mail(
        "personal",
        "INBOX",
        101,
        "沿海来信 <letters@coast.example>",
        "沿海来信 · 九月号",
        "这一期我们聊聊潮汐表、渔港的早市，以及一封迟到了三十年的信。\n\n—— 编辑部",
        base,
    );
    m1.seen = false;
    m1.body_html =
        "<html><body style=\"font-family:serif\"><h1>沿海来信</h1><p>九月号</p></body></html>"
            .into();
    let mut m2 = mail(
        "personal",
        "INBOX",
        102,
        "印刷厂 <print@press.example>",
        "秋季刊印刷确认",
        "您好，附件是秋季刊的印刷确认单，请在周五前回复确认。\n\n印刷厂",
        base - day,
    );
    m2.message_id = "press-root@example.test".into();
    m2.has_attachments = true;
    // 字节与 `size` 一致（ADR-0032 的分块读取会核对），内容是确定性的假数据：
    // `<cli_ctl> attachment save`（`maild_brand::CLI_CTL`）能把它取回并逐字节对上。
    let confirmation: Vec<u8> = (0..184_320u32)
        .map(|index| (index.wrapping_mul(2_654_435_761) >> 13) as u8)
        .collect();
    m2.attachments = vec![MailAttachment {
        part_index: 2,
        filename: "秋季刊-印刷确认.pdf".into(),
        mime_type: "application/pdf".into(),
        size: confirmation.len() as u64,
        stored: true,
        content: Some(confirmation),
        ..Default::default()
    }];
    m2.flagged = true;
    let mut m3 = mail("personal", "INBOX", 103, "GitHub <noreply@github.example>", "[cordis] 2 new commits pushed to main",
        "feat(m4.5-g01): let a frontend declare what it may do instead of being user\n\nView it on GitHub.", base - 2 * day);
    m3.seen = false;
    let m4 = mail(
        "personal",
        "INBOX",
        104,
        "电费通知 <billing@utility.example>",
        "9 月电费账单已生成",
        "本月用电 236 kWh，应缴 142.60 元，将于 9 月 25 日自动扣款。",
        base - 3 * day,
    );
    let mut m5 = mail(
        "personal",
        "Sent",
        7,
        "personal@example.test",
        "Re: 秋季刊印刷确认",
        "已确认，按附件版本印刷。",
        base - day + 3600,
    );
    m5.message_id = "press-reply@example.test".into();
    m5.in_reply_to = "press-root@example.test".into();
    m5.references = "press-root@example.test".into();
    let m6 = mail(
        "personal",
        "Archive",
        3,
        "旧同事 <old@friend.example>",
        "上次说的那本书",
        "找到了，下周带给你。",
        base - 40 * day,
    );
    let m7 = mail(
        "personal",
        "&XfJT0ZAB-",
        12,
        "Weekly Rust <weekly@rust.example>",
        "This Week in Rust 612",
        "Hello and welcome to another issue of This Week in Rust!",
        base - 4 * day,
    );
    let mut w1 = mail(
        "work",
        "INBOX",
        501,
        "王芸 <yun.wang@corp.example>",
        "周会纪要与下周排期",
        "各位好，周会纪要见下：\n1. 前端迁移 Tauri 的 ADR 已接受；\n2. 下周排 03 骨架。\n\n王芸",
        base - 3600,
    );
    w1.seen = false;
    let w2 = mail(
        "work",
        "INBOX",
        502,
        "Jenkins <ci@corp.example>",
        "Build #4821 failed: cordis-nightly",
        "Stage 'clippy' failed. See console output.",
        base - 5 * 3600,
    );
    let w3 = mail(
        "work",
        "Junk",
        90,
        "promo@deals.example",
        "限时优惠！",
        "点击领取…",
        base - 6 * day,
    );
    // ADR-0033 验收 fixture：表格 + 内联 CSS + CJK + `cid:` PNG + 指向本机计数服务器的远程图片 +
    // 脚本 / 表单 / 各类链接 / meta refresh。计数服务器的命中数就是「默认零请求」的证据。
    let mut h1 = mail(
        "personal",
        "INBOX",
        105,
        "山海读书会 <club@shanhai.example>",
        "十月共读：《夜航西飞》——报名与场地（HTML 版式）",
        "十月共读书目《夜航西飞》。报名截止 10 月 8 日，场地在城西图书馆三层。",
        base + 3600,
    );
    h1.seen = false;
    h1.body_html = format!(
        r##"<!DOCTYPE html><html><head><meta http-equiv="refresh" content="0;url=https://evil.example/">
<script>alert('script ran')</script>
<style>body{{font-family:Georgia,serif;margin:0;background:#f4efe6}} .card{{max-width:640px;margin:24px auto;background:#fff;border:1px solid #d8cdb8;padding:28px}}
h1{{font-size:22px;color:#2f4a3a;margin:0 0 8px}} .meta{{color:#7a6b55;font-size:13px}} table.plan td{{padding:6px 10px;border-bottom:1px solid #eee}}
.cta{{display:inline-block;background:#2f4a3a;color:#fff !important;padding:10px 18px;text-decoration:none;border-radius:4px}}</style></head>
<body><div class="card">
<img src="cid:club-logo@shanhai" width="96" height="32" alt="山海读书会">
<h1>十月共读：《夜航西飞》</h1>
<p class="meta">柏瑞尔·马卡姆 · 1942 · 非虚构</p>
<table class="plan" cellpadding="0" cellspacing="0" width="100%">
<tr><td width="30%"><b>报名截止</b></td><td>10 月 8 日（周三）</td></tr>
<tr><td><b>第一次讨论</b></td><td>10 月 12 日 14:00 · 城西图书馆三层</td></tr>
<tr><td><b>领读</b></td><td>周砚 · <a href="mailto:yan.zhou@shanhai.example">yan.zhou@shanhai.example</a></td></tr>
</table>
<p>这一次我们读一位女飞行员在肯尼亚的童年与横越大西洋的夜航。请在下面的链接报名，并回复告知是否需要借阅纸书。</p>
<p><a class="cta" href="https://shanhai.example/signup?book=west-with-the-night&amp;t=oct">前往报名</a>
&nbsp;<a href="javascript:alert('js')">一个 javascript: 链接</a> &nbsp;<a href="file:///etc/hosts">一个 file: 链接</a></p>
<p><img src="http://{remote_host}/tracker.png" width="1" height="1" alt=""> <img src="http://{remote_host}/banner.png" width="320" height="60" alt="活动横幅（远程）"></p>
<form action="https://evil.example/post" method="post"><input name="q" value="表单不可提交"><button>提交</button></form>
<p class="meta">山海读书会 · 城西图书馆 · 每月一册</p>
</div></body></html>"##,
        remote_host = remote_host,
    );
    let logo_png = tiny_png();
    h1.attachments = vec![
        MailAttachment {
            part_index: 2,
            filename: "logo.png".into(),
            mime_type: "image/png".into(),
            content_id: "club-logo@shanhai".into(),
            inline: true,
            size: logo_png.len() as u64,
            stored: true,
            content: Some(logo_png),
        },
        MailAttachment {
            part_index: 3,
            filename: "十月共读-报名表.pdf".into(),
            mime_type: "application/pdf".into(),
            size: 40_960,
            stored: true,
            content: Some(
                (0..40_960u32)
                    .map(|i| (i.wrapping_mul(7919) >> 8) as u8)
                    .collect(),
            ),
            ..Default::default()
        },
        MailAttachment {
            part_index: 4,
            filename: "讨论录音.m4a".into(),
            mime_type: "audio/mp4".into(),
            size: 30 * 1024 * 1024,
            stored: false,
            content: None,
            ..Default::default()
        },
    ];
    h1.has_attachments = true;
    for message in [m1, m2, m3, m4, m5, m6, m7, h1, w1, w2, w3] {
        store.insert_message(&message).unwrap();
    }
    // 08-B recovery fixtures stay on offline accounts and never reach a real server.
    use maild::pending::{MessageFlag, OpState};
    store
        .queue_move_ops("personal", "INBOX", &["103".into()], "Archive", base + 7200)
        .unwrap();
    let moved = store
        .claim_next_op("personal", base + 7201)
        .unwrap()
        .unwrap();
    store
        .finish_op(
            &moved,
            OpState::Unknown,
            "服务器在移动后断开，未收到最终响应",
            base + 7202,
        )
        .unwrap();
    store
        .queue_flag_ops(
            "work",
            "INBOX",
            &["502".into()],
            MessageFlag::Flagged,
            true,
            base + 7300,
        )
        .unwrap();
    let flag = store.claim_next_op("work", base + 7301).unwrap().unwrap();
    store
        .finish_op(
            &flag,
            OpState::Failed,
            "服务器明确拒绝：此文件夹为只读",
            base + 7302,
        )
        .unwrap();
}

/// 一张 8×8 的深绿色 PNG（内联 logo 用），字节级合法，不需要图片库。
fn tiny_png() -> Vec<u8> {
    fn crc32(bytes: &[u8]) -> u32 {
        let mut crc = 0xFFFF_FFFFu32;
        for &byte in bytes {
            crc ^= byte as u32;
            for _ in 0..8 {
                crc = if crc & 1 == 1 {
                    (crc >> 1) ^ 0xEDB8_8320
                } else {
                    crc >> 1
                };
            }
        }
        !crc
    }
    fn adler32(bytes: &[u8]) -> u32 {
        let (mut a, mut b) = (1u32, 0u32);
        for &byte in bytes {
            a = (a + byte as u32) % 65521;
            b = (b + a) % 65521;
        }
        (b << 16) | a
    }
    fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        let mut body = kind.to_vec();
        body.extend_from_slice(data);
        out.extend_from_slice(&body);
        out.extend_from_slice(&crc32(&body).to_be_bytes());
    }
    let (width, height) = (8u32, 8u32);
    // 每行：filter 字节 0 + width 个 RGB 像素。
    let row: Vec<u8> = std::iter::once(0u8)
        .chain(std::iter::repeat_n([0x2f, 0x4a, 0x3a], width as usize).flatten())
        .collect();
    let raw: Vec<u8> = std::iter::repeat_n(row, height as usize).flatten().collect();
    // zlib：无压缩块（stored）。
    let mut zlib = vec![0x78, 0x01, 0x01];
    zlib.extend_from_slice(&(raw.len() as u16).to_le_bytes());
    zlib.extend_from_slice(&(!(raw.len() as u16)).to_le_bytes());
    zlib.extend_from_slice(&raw);
    zlib.extend_from_slice(&adler32(&raw).to_be_bytes());
    let mut png = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]); // 8 bit, RGB
    chunk(&mut png, b"IHDR", &ihdr);
    chunk(&mut png, b"IDAT", &zlib);
    chunk(&mut png, b"IEND", &[]);
    png
}

/// 本机远程图片计数服务器（ADR-0033 测试策略）：任何 GET 都回一张 PNG 并计数；
/// `GET /count` 回当前命中数。命中数就是「远程内容默认零请求」的证据。
async fn remote_image_counter(hits: Arc<AtomicUsize>) -> std::net::SocketAddr {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                continue;
            };
            let hits = hits.clone();
            tokio::spawn(async move {
                let mut buffer = vec![0u8; 4096];
                let read = stream.read(&mut buffer).await.unwrap_or(0);
                let request = String::from_utf8_lossy(&buffer[..read]).into_owned();
                let path = request.split_whitespace().nth(1).unwrap_or("/").to_string();
                let response = if path == "/count" {
                    let count = hits.load(Ordering::SeqCst).to_string();
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        count.len(),
                        count
                    )
                    .into_bytes()
                } else {
                    let count = hits.fetch_add(1, Ordering::SeqCst) + 1;
                    eprintln!("[remote-image-counter] 命中 #{count}: GET {path}");
                    let png = tiny_png();
                    let mut response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: image/png\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        png.len()
                    )
                    .into_bytes();
                    response.extend_from_slice(&png);
                    response
                };
                let _ = stream.write_all(&response).await;
                let _ = stream.shutdown().await;
            });
        }
    });
    addr
}

fn main() {
    std::env::set_var("MAILD_TEST_CREDENTIAL_STORE", "memory");
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    std::env::set_var("MAILD_TEST_ACCEPT_INVALID_CERTS", "1");
    let mail_server = runtime
        .block_on(fake_mail::FakeMail::start_imap_implicit_and_smtp_starttls())
        .unwrap();
    mail_server.add_mailbox("Sent", "\\Sent");
    let hits = Arc::new(AtomicUsize::new(0));
    let remote_addr = runtime.block_on(remote_image_counter(hits));
    let root = std::env::temp_dir().join(format!("maild-ui-dev-{}", std::process::id()));
    let paths = AppPaths::from_roots(
        &root.join("config"),
        &root.join("data"),
        Some(&root.join("runtime")),
        Some(&root.join("state")),
        &root,
    );
    for dir in [
        &paths.config_dir,
        &paths.data_dir,
        &paths.runtime_dir,
        &paths.plugin_dir,
        &paths.frontend_dir,
    ] {
        secure_dir(dir);
    }
    // 两个账户指向关着的端口：显示离线 + 缓存可读，不联网。
    std::fs::write(
        &paths.config_file,
        r#"
[plugins.store.config]
db_path = "mail.sqlite3"
[plugins.api.config]
transport = "unix"
listen = "mail.sock"
[plugins.frontend.config]
default = "builtin-egui"
[plugins.external.config]

[[accounts]]
account = "personal"
server = "127.0.0.1"
port = 1
username = "personal"
password = "dev"

[[accounts]]
account = "work"
server = "127.0.0.1"
port = 1
username = "work"
password = "dev"
"#,
    )
    .unwrap();
    {
        let store = Store::open(paths.data_dir.join("mail.sqlite3").to_str().unwrap()).unwrap();
        seed_with_remote_host(&store, &remote_addr.to_string());
    }
    // 装前端包：直接放进受管目录（等价于 `<cli_ctl> plugin install --frontend crates/maild-ui/package`）。
    let package = paths.frontend_dir.join(FRONTEND_ID);
    secure_dir(&package);
    let manifest = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("package/frontend.toml"),
    )
    .unwrap()
    .replace(maild_brand::BUNDLED_FRONTEND_ID, FRONTEND_ID);
    std::fs::write(package.join("frontend.toml"), manifest).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(
            package.join("frontend.toml"),
            std::fs::Permissions::from_mode(0o600),
        )
        .unwrap();
    }

    // A third, managed account delivers only to the local fixture. No real credentials or mailbox.
    {
        use maild::account_catalog::*;
        let id = "acct_aaaaaaaaaaaaaaaaaaaaaaaaaa";
        let profile = AccountProfile {
            id: id.into(),
            revision: 1,
            display_name: "本机测试邮箱".into(),
            address: "writer@example.test".into(),
            enabled: true,
            archived_at: None,
            incoming_credential_revision: Some(1),
            outgoing_credential_revision: None,
            incoming: IncomingSettings::Imap {
                server: "127.0.0.1".into(),
                port: mail_server.imap_addr.port(),
                security: SecurityMode::Implicit,
                username: "writer@example.test".into(),
                mailbox: "INBOX".into(),
                idle: true,
                poll_secs: 300,
            },
            outgoing: Some(OutgoingSettings::Smtp {
                server: "127.0.0.1".into(),
                port: mail_server.smtp_addr.port(),
                security: SecurityMode::StartTls,
                username: "writer@example.test".into(),
                credential: CredentialSharing::Incoming,
            }),
        };
        save_catalog(
            &paths.accounts_file,
            &AccountCatalog {
                format_version: CATALOG_FORMAT_VERSION,
                profiles: vec![profile],
            },
        )
        .unwrap();
        maild::credential::platform_store_for(&paths.accounts_file)
            .save(
                &CredentialKey::new(id, CredentialRole::Incoming, 1),
                b"dev-only",
            )
            .unwrap();
        let store = Store::open(paths.data_dir.join("mail.sqlite3").to_str().unwrap()).unwrap();
        store
            .upsert_folder(id, "Sent", "已发送", Some(FolderRole::Sent))
            .unwrap();
    }
    let socket = paths.runtime_dir.join("mail.sock");
    eprintln!("maild-ui dev stack");
    eprintln!("  root:   {}", root.display());
    eprintln!("  socket: {}", socket.display());
    eprintln!(
        "  remote-image-counter: http://{remote_addr}/  （GET /count 看命中数；ADR-0033 验收）"
    );
    eprintln!();
    eprintln!("  起壳（开发形态，需先 pnpm --dir packages/featherpost-studio dev）：");
    eprintln!("    cargo run -p maild-ui -- --socket {}", socket.display());
    eprintln!("  或内嵌形态（先 pnpm --dir packages/featherpost-studio build）：");
    eprintln!(
        "    cargo run -p maild-ui --features embedded -- --socket {}",
        socket.display()
    );
    eprintln!();
    let options = DaemonOptions::new(paths.config_file.clone(), paths).expect("绝对路径");
    if let Err(error) = runtime.block_on(run_daemon_until(options, std::future::pending())) {
        eprintln!("daemon 退出: {error}");
        std::process::exit(1);
    }
}
