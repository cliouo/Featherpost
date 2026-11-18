//! maild 的独立 JSON-RPC 客户端（HTTP / Unix socket / named pipe）。

use std::fs::File;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use maild_client::{
    discover_endpoint, generate_token, validate_draft_input, AccountDraftInput, ClientError,
    CompatVerdict, DiscoveryOptions, ErrorKind, IncomingInput, OutgoingInput, RpcClient,
    PluginPackageKind, UpdatePatchInput, PROTOCOL_VERSION,
};
use serde_json::{json, Value};

const MAX_BODY_BYTES: u64 = 512 * 1024;

#[derive(Parser)]
#[command(
    name = maild_brand::CLI_CTL,
    version,
    about = format!("{} headless API client", maild_brand::PRODUCT_NAME)
)]
struct Cli {
    /// Daemon TOML config used to discover the API transport and HTTP token.
    #[arg(long, value_name = "PATH")]
    config: Option<PathBuf>,
    /// Override the HTTP JSON-RPC endpoint (default: http://LISTEN/rpc).
    #[arg(
        long,
        value_name = "URL",
        conflicts_with_all = ["socket", "pipe"]
    )]
    url: Option<String>,
    /// Connect through a Unix domain socket instead of config discovery.
    #[arg(
        long,
        value_name = "PATH",
        conflicts_with_all = ["url", "pipe"]
    )]
    socket: Option<PathBuf>,
    /// Connect through a Windows named pipe instead of config discovery.
    #[arg(
        long,
        value_name = "NAME",
        conflicts_with_all = ["url", "socket"]
    )]
    pipe: Option<String>,
    /// Print the JSON-RPC result as JSON.
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Show daemon version, account states, stored message count, and resource usage.
    Status,
    /// Wake the background receive loop to sync right now (all enabled accounts by default).
    Sync {
        /// Only wake this account (name/id as shown by `accounts`).
        #[arg(long)]
        account: Option<String>,
    },
    /// List configured accounts and connection states.
    Accounts,
    /// List stored messages, newest first.
    Messages {
        #[arg(long, default_value_t = 50, value_parser = clap::value_parser!(u64).range(1..=500))]
        limit: u64,
        #[arg(long, default_value_t = 0)]
        offset: u64,
    },
    /// Read one stored message in full.
    Message {
        #[arg(long)]
        account: String,
        /// Mailbox name as returned in summaries; UIDs are only unique within a mailbox.
        #[arg(long)]
        folder: Option<String>,
        #[arg(long)]
        uid: String,
    },
    /// Save a stored attachment part to a local file (ADR-0032).
    Attachment {
        #[command(subcommand)]
        command: AttachmentCommand,
    },
    /// Show or change the remote-content policy and trusted senders (ADR-0032).
    ReadingPolicy {
        #[command(subcommand)]
        command: ReadingPolicyCommand,
    },
    /// Work with daemon-owned drafts (ADR-0034).
    Draft {
        #[command(subcommand)]
        command: DraftCommand,
    },
    /// Read mutable runtime rules settings.
    Settings,
    /// Apply a rules settings patch; `persisted` reports whether it reached rules_state.toml.
    SetSettings {
        /// JSON object containing RulesConfig fields to patch.
        #[arg(long, value_name = "JSON")]
        rules_json: String,
        /// Ask the daemon to start persisting rules to rules_state.toml next to its config.
        #[arg(long)]
        persist: bool,
    },
    /// Query native notification availability and authorization without prompting.
    NotificationStatus,
    /// Explicitly ask the operating system for notification authorization.
    RequestNotificationAuthorization,
    /// List installed frontend packages and the configured default.
    Frontends,
    /// List installed external process plugins and their lifecycle state.
    Plugins,
    /// Reveal (and rotate) one plugin's auth token; print it exactly once.
    PluginToken {
        /// Package id (`plugins/<id>/` or `frontends/<id>/` with --frontend).
        id: String,
        /// Target a frontend package instead of an external plugin.
        #[arg(long)]
        frontend: bool,
    },
    /// Send one plain-text message through the configured transport.
    Send {
        #[arg(long)]
        from: String,
        #[arg(long)]
        to: String,
        #[arg(long, default_value = "")]
        subject: String,
        #[arg(long, conflicts_with = "body_file")]
        body: Option<String>,
        /// Read the body from a file; use '-' for stdin.
        #[arg(long, value_name = "PATH", conflicts_with = "body")]
        body_file: Option<PathBuf>,
    },
    /// Generate a cryptographically random bearer token for the config file.
    GenerateToken,
    /// Manage mailbox accounts on the agent (add/test/list/edit/enable/disable/remove).
    Account {
        #[command(subcommand)]
        command: AccountCommand,
    },
    /// Install/upgrade/rollback/remove plugins and toggle supervised execution.
    Plugin {
        #[command(subcommand)]
        command: PluginCommand,
    },
}

#[derive(Subcommand)]
enum AttachmentCommand {
    /// Stream one part in bounded chunks and write it to --out (never overwrites).
    Save {
        #[arg(long)]
        account: String,
        /// Mailbox name exactly as returned in summaries (required: no folder disambiguation).
        #[arg(long)]
        folder: String,
        #[arg(long)]
        uid: String,
        /// `part_index` from the message's attachment list.
        #[arg(long)]
        part: u32,
        /// Destination path; fails if it already exists.
        #[arg(long, value_name = "PATH")]
        out: PathBuf,
    },
}

/// 草稿子命令（ADR-0034 决策 10）。`set` / `attach` / `detach` / `rm` 都要 `--version`：
/// 过期的写入会被 daemon 拒绝，这正是「过期更新不覆盖」在 CLI 上的样子。
#[derive(Subcommand)]
enum DraftCommand {
    /// List drafts, most recently updated first.
    List {
        #[arg(long)]
        account: Option<String>,
        #[arg(long, default_value_t = 50, value_parser = clap::value_parser!(u64).range(1..=200))]
        limit: u64,
        #[arg(long, default_value_t = 0)]
        offset: u64,
    },
    /// Print one draft in full.
    Show {
        id: String,
    },
    /// Create a draft. Every field is optional but `--account`.
    New {
        #[arg(long)]
        account: String,
        #[arg(long)]
        from: Option<String>,
        /// Repeatable.
        #[arg(long = "to")]
        to: Vec<String>,
        #[arg(long = "cc")]
        cc: Vec<String>,
        #[arg(long = "bcc")]
        bcc: Vec<String>,
        #[arg(long)]
        subject: Option<String>,
        #[arg(long, conflicts_with = "body_file")]
        body: Option<String>,
        /// Read the body from a file; use '-' for stdin.
        #[arg(long, value_name = "PATH", conflicts_with = "body")]
        body_file: Option<PathBuf>,
    },
    /// Patch a draft. Absent flags leave that field untouched.
    Set {
        id: String,
        /// The version you last read. A stale value is refused, never merged.
        #[arg(long)]
        version: u64,
        #[arg(long)]
        from: Option<String>,
        /// Repeatable; replaces the whole list. Pass --clear-to to empty it.
        #[arg(long = "to")]
        to: Vec<String>,
        #[arg(long)]
        clear_to: bool,
        #[arg(long = "cc")]
        cc: Vec<String>,
        #[arg(long)]
        clear_cc: bool,
        #[arg(long = "bcc")]
        bcc: Vec<String>,
        #[arg(long)]
        clear_bcc: bool,
        #[arg(long)]
        subject: Option<String>,
        #[arg(long, conflicts_with = "body_file")]
        body: Option<String>,
        #[arg(long, value_name = "PATH", conflicts_with = "body")]
        body_file: Option<PathBuf>,
    },
    /// Append one stored attachment part to a draft (bytes stay in the store).
    Attach {
        id: String,
        #[arg(long)]
        version: u64,
        /// Source account; defaults to the draft's own account.
        #[arg(long)]
        account: Option<String>,
        #[arg(long)]
        folder: String,
        #[arg(long)]
        uid: String,
        #[arg(long)]
        part: u32,
    },
    /// Upload a local file and atomically attach it to this draft.
    AttachFile {
        id: String,
        #[arg(long)]
        version: u64,
        file: PathBuf,
        #[arg(long, default_value = "application/octet-stream")]
        mime_type: String,
    },
    /// Retry a completed upload after conflict or an uncertain finish response.
    FinishUpload {
        upload_id: String,
        #[arg(long)]
        version: u64,
    },
    /// Release an unfinished upload.
    CancelUpload { upload_id: String },
    /// Remove the attachment at --position from a draft.
    Detach {
        id: String,
        #[arg(long)]
        version: u64,
        #[arg(long)]
        position: usize,
    },
    /// Discard a draft.
    Rm {
        id: String,
        /// Refuse if the draft changed elsewhere. Omit to discard unconditionally.
        #[arg(long)]
        version: Option<u64>,
    },
}

#[derive(Subcommand)]
enum ReadingPolicyCommand {
    /// Print the default remote-content policy and the trusted-sender list.
    Show,
    /// Set the default policy for remote images/styles/fonts: block | allow.
    Set {
        #[arg(long, value_name = "block|allow")]
        remote_content: String,
    },
    /// Trust one or more sender addresses (a@b.c) or exact domains (b.c).
    Trust {
        #[arg(required = true)]
        values: Vec<String>,
    },
    /// Remove one or more entries from the trusted-sender list.
    Untrust {
        #[arg(required = true)]
        values: Vec<String>,
    },
}

#[derive(Subcommand)]
enum PluginCommand {
    /// Install a package directory; upgrades automatically when the id exists
    /// (requires a strictly higher version). The agent validates and publishes
    /// atomically — never edit managed directories by hand.
    Install {
        /// Package directory containing plugin.toml / frontend.toml.
        #[arg(long)]
        source: PathBuf,
        /// Target a frontend package instead of an external plugin.
        #[arg(long)]
        frontend: bool,
    },
    /// Remove an installed package (running supervised children stop first).
    Remove {
        id: String,
        #[arg(long)]
        frontend: bool,
    },
    /// Swap the current version with the single-depth rollback slot.
    Rollback {
        id: String,
        #[arg(long)]
        frontend: bool,
    },
    /// Start a supervised plugin immediately.
    Enable { id: String },
    /// Stop a supervised plugin immediately.
    Disable { id: String },
}

#[derive(Subcommand)]
enum AccountCommand {
    /// List account profiles (managed + legacy).
    List {
        /// Include archived (removed) accounts.
        #[arg(long)]
        all: bool,
    },
    /// Probe endpoints with submitted credentials without saving anything.
    Test {
        #[command(flatten)]
        spec: AccountSpec,
    },
    /// Add an account: probe, save credentials, publish and connect immediately.
    Add {
        #[command(flatten)]
        spec: AccountSpec,
    },
    /// Probe a plugin-backed account without prompting for transport credentials.
    ProviderTest {
        #[command(flatten)]
        spec: ProviderAccountSpec,
    },
    /// Add a plugin-backed account with no credentials crossing the provider seam.
    ProviderAdd {
        #[command(flatten)]
        spec: ProviderAccountSpec,
    },
    /// Edit non-secret fields and/or rotate passwords.
    Edit {
        /// Account id (`acct_…`, see `account list`).
        #[arg(long)]
        id: String,
        #[arg(long)]
        display_name: Option<String>,
        #[arg(long)]
        address: Option<String>,
        #[arg(long)]
        imap_host: Option<String>,
        #[arg(long)]
        imap_port: Option<u16>,
        #[arg(long)]
        imap_security: Option<String>,
        #[arg(long)]
        imap_username: Option<String>,
        #[arg(long)]
        imap_mailbox: Option<String>,
        /// Rotate the IMAP password (TTY prompt or stdin line).
        #[arg(long)]
        update_imap_password: bool,
        /// Remove the outgoing (SMTP) configuration.
        #[arg(long, conflicts_with = "smtp_host")]
        remove_smtp: bool,
        #[arg(long)]
        smtp_host: Option<String>,
        #[arg(long)]
        smtp_port: Option<u16>,
        #[arg(long)]
        smtp_security: Option<String>,
        #[arg(long)]
        smtp_username: Option<String>,
        #[arg(long)]
        smtp_credential: Option<String>,
        /// Rotate the separate SMTP password (TTY prompt or stdin line).
        #[arg(long)]
        update_smtp_password: bool,
    },
    /// Enable an account (mounts its receive runtime immediately).
    Enable { id: String },
    /// Disable an account (stops its receive runtime, keeps credentials).
    Disable { id: String },
    /// Remove an account (archived; stored messages are retained).
    Remove { id: String },
    /// Detect provider servers for an address or domain (built-in presets, no network by default).
    Discover {
        /// Email address (`alice@example.com`) or bare domain (`example.com`).
        input: String,
        /// Allow a bounded public-network reachability probe for unknown domains.
        #[arg(long)]
        network: bool,
    },
}

/// Shared endpoint specification for `add` / `test`.
#[derive(clap::Args)]
struct AccountSpec {
    #[arg(long)]
    display_name: String,
    #[arg(long)]
    address: String,
    #[arg(long)]
    imap_host: String,
    #[arg(long, default_value_t = 993)]
    imap_port: u16,
    /// implicit | starttls
    #[arg(long, default_value = "implicit")]
    imap_security: String,
    #[arg(long)]
    imap_username: String,
    #[arg(long, default_value = "INBOX")]
    imap_mailbox: String,
    /// Disable IDLE and poll every --imap-poll-secs instead.
    #[arg(long)]
    no_imap_idle: bool,
    #[arg(long, default_value_t = 60)]
    imap_poll_secs: u64,
    #[arg(long)]
    smtp_host: Option<String>,
    #[arg(long, default_value_t = 587)]
    smtp_port: u16,
    /// implicit | starttls
    #[arg(long, default_value = "starttls")]
    smtp_security: String,
    #[arg(long)]
    smtp_username: Option<String>,
    /// incoming (reuse IMAP password) | separate
    #[arg(long, default_value = "incoming")]
    smtp_credential: String,
}

/// Shared plugin transport specification for `provider-add` / `provider-test`.
#[derive(clap::Args)]
struct ProviderAccountSpec {
    #[arg(long)]
    display_name: String,
    #[arg(long)]
    address: String,
    /// Installed transport provider package id.
    #[arg(long)]
    provider: String,
    /// Poll interval for delegated incoming mail.
    #[arg(long, default_value_t = 60, value_parser = clap::value_parser!(u64).range(1..))]
    poll_secs: u64,
    /// Delegate incoming mail only; omit the outgoing provider endpoint.
    #[arg(long)]
    receive_only: bool,
}

#[derive(Debug)]
struct AppError {
    code: u8,
    kind: &'static str,
    message: String,
    rpc_code: Option<i64>,
    /// 错误对象的 `data`。daemon 往这里放可据以行动的东西——草稿冲突时的当前版本号、
    /// 超限时的字段与上限（ADR-0034）——`--json` 原样透出，脚本才能据此重试。
    rpc_data: Option<Value>,
}

impl AppError {
    fn connection(message: impl Into<String>) -> Self {
        AppError {
            code: 3,
            kind: "connection",
            message: message.into(),
            rpc_code: None,
            rpc_data: None,
        }
    }

    fn rpc(message: impl Into<String>) -> Self {
        AppError {
            code: 4,
            kind: "rpc",
            message: message.into(),
            rpc_code: None,
            rpc_data: None,
        }
    }

    fn human_message(&self) -> String {
        let detail = match &self.rpc_data {
            Some(Value::Object(fields)) if !fields.is_empty() => {
                let pairs: Vec<String> = fields
                    .iter()
                    .map(|(key, value)| format!("{key}={value}"))
                    .collect();
                format!(" [{}]", pairs.join(" "))
            }
            _ => String::new(),
        };
        match self.rpc_code {
            Some(code) => format!("{} ({code}){detail}", self.message),
            None => format!("{}{detail}", self.message),
        }
    }
}

impl From<ClientError> for AppError {
    fn from(error: ClientError) -> Self {
        AppError {
            code: match error.kind {
                ErrorKind::Connection => 3,
                ErrorKind::Rpc => 4,
            },
            kind: error.kind.as_str(),
            message: error.message,
            rpc_code: error.rpc_code,
            rpc_data: error.rpc_data,
        }
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    let json_output = cli.json;
    match run(cli).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            if json_output {
                eprintln!("{}", json_error_line(&error));
            } else {
                eprintln!(
                    "{}: {}",
                    maild_brand::CLI_CTL,
                    terminal_text(&error.human_message())
                );
            }
            ExitCode::from(error.code)
        }
    }
}

async fn run(cli: Cli) -> Result<(), AppError> {
    if matches!(cli.command, Command::GenerateToken) {
        println!("{}", generate_token()?);
        return Ok(());
    }

    let endpoint = discover_endpoint(&DiscoveryOptions {
        config_path: cli.config,
        url: cli.url,
        socket: cli.socket,
        pipe: cli.pipe,
    })?;
    let client = RpcClient::new(endpoint)?;
    warn_on_protocol_skew(&client).await;
    if let Command::Account { command } = cli.command {
        return run_account(client, command, cli.json).await;
    }
    if let Command::PluginToken { id, frontend } = cli.command {
        // reveal 即轮换（ADR-0017）：明文只在本次 stdout 出现，旧凭证对新连接失效。
        let token = client
            .reveal_plugin_token(&id, frontend.then_some("frontend"))
            .await?;
        if cli.json {
            println!("{}", json!({ "token": token }));
        } else {
            eprintln!("凭证已轮换；旧凭证对新连接立即失效。请立即写入插件自己的配置，勿入 shell 历史：");
            println!("{token}");
        }
        return Ok(());
    }
    if let Command::Plugin { command } = cli.command {
        return run_plugin(client, command, cli.json).await;
    }
    if let Command::Attachment { command } = cli.command {
        return run_attachment(client, command, cli.json).await;
    }
    if let Command::ReadingPolicy { command } = cli.command {
        return run_reading_policy(client, command, cli.json).await;
    }
    if let Command::Draft { command } = cli.command {
        return run_draft(client, command, cli.json).await;
    }
    let (method, params) = match cli.command {
        Command::Status => ("status", json!({})),
        Command::Sync { account } => match account {
            Some(account) => ("syncMessages", json!({ "account": account })),
            None => ("syncMessages", json!({})),
        },
        Command::Accounts => ("listAccounts", json!({})),
        Command::Messages { limit, offset } => {
            ("listMessages", json!({ "limit": limit, "offset": offset }))
        }
        Command::Message {
            account,
            folder,
            uid,
        } => {
            let mut params = json!({ "account": account, "uid": uid });
            if let Some(folder) = folder {
                params["folder"] = json!(folder);
            }
            ("getMessage", params)
        }
        Command::Settings => ("getSettings", json!({})),
        Command::SetSettings { rules_json, persist } => {
            let rules = parse_rules_json(&rules_json)?;
            let mut params = json!({ "rules": rules });
            if persist {
                params["persist"] = json!(true);
            }
            ("setSettings", params)
        }
        Command::NotificationStatus => ("getNotificationStatus", json!({})),
        Command::RequestNotificationAuthorization => {
            ("requestNotificationAuthorization", json!({}))
        }
        Command::Frontends => ("listFrontends", json!({})),
        Command::Plugins => ("listExternalPlugins", json!({})),
        Command::Send {
            from,
            to,
            subject,
            body,
            body_file,
        } => {
            let body = match body_file {
                Some(path) => read_body(&path)?,
                None => body.unwrap_or_default(),
            };
            (
                "send",
                json!({ "from": from, "to": to, "subject": subject, "body": body }),
            )
        }
        Command::GenerateToken => unreachable!(),
        Command::Account { .. } => unreachable!("account commands handled before dispatch"),
        Command::PluginToken { .. } => unreachable!("plugin-token handled before dispatch"),
        Command::Plugin { .. } => unreachable!("plugin commands handled before dispatch"),
        Command::Attachment { .. } => unreachable!("attachment commands handled before dispatch"),
        Command::ReadingPolicy { .. } => {
            unreachable!("reading-policy commands handled before dispatch")
        }
        Command::Draft { .. } => unreachable!("draft commands handled before dispatch"),
    };

    let result = client.call(method, params).await?;
    if cli.json {
        println!(
            "{}",
            serde_json::to_string(&result)
                .map_err(|e| AppError::rpc(format!("JSON 输出失败: {e}")))?
        );
    } else {
        print_text(method, &result);
    }
    Ok(())
}

/// ADR-0016：内核与客户端协议代不一致时在 stderr 提示。不改变退出码与输出，
/// 保持脚本可用；握手失败不额外制造噪音（连不上等故障由后续命令本身报错）。
async fn warn_on_protocol_skew(client: &RpcClient) {
    let verdict = match client.agent_compat(PROTOCOL_VERSION).await {
        Ok(verdict) => verdict,
        Err(_) => return,
    };
    match verdict {
        CompatVerdict::Compatible => {}
        CompatVerdict::KernelOutdated => eprintln!(
            "警告: 后台内核协议代较旧（期望 {PROTOCOL_VERSION}），结果可能不可靠；请重启后台内核"
        ),
        CompatVerdict::AppOutdated => {
            eprintln!(
                "警告: {} 较旧（内核协议代更新），建议更新 {}",
                maild_brand::CLI_CTL,
                maild_brand::PRODUCT_NAME
            )
        }
    }
}

fn parse_rules_json(input: &str) -> Result<Value, AppError> {
    let rules = serde_json::from_str::<Value>(input)
        .map_err(|error| AppError::connection(format!("rules JSON 无效: {error}")))?;
    if !rules.is_object() {
        return Err(AppError::connection("rules JSON 必须是对象"));
    }
    Ok(rules)
}

fn json_error_line(error: &AppError) -> String {
    let mut body = json!({
        "kind": error.kind,
        "message": error.message,
        "rpc_code": error.rpc_code,
    });
    // `data` 只在 daemon 给了的时候出现：既有输出的形状逐字节不变。
    if let Some(data) = &error.rpc_data {
        body["data"] = data.clone();
    }
    serde_json::to_string(&json!({ "error": body }))
        .expect("CLI error JSON is always serializable")
}

fn read_body(path: &Path) -> Result<String, AppError> {
    let reader: Box<dyn Read> =
        if path == Path::new("-") {
            Box::new(io::stdin())
        } else {
            Box::new(File::open(path).map_err(|e| {
                AppError::connection(format!("读取正文失败 {}: {e}", path.display()))
            })?)
        };
    let mut bytes = Vec::new();
    reader
        .take(MAX_BODY_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| AppError::connection(format!("读取正文失败: {e}")))?;
    if bytes.len() as u64 > MAX_BODY_BYTES {
        return Err(AppError::connection(format!(
            "正文超过 {MAX_BODY_BYTES} bytes"
        )));
    }
    String::from_utf8(bytes).map_err(|_| AppError::connection("正文必须是 UTF-8"))
}

fn print_text(method: &str, result: &Value) {
    match method {
        "status" => {
            println!("version: {}", result["version"].as_str().unwrap_or("?"));
            println!(
                "messages: {}",
                result["message_count"].as_i64().unwrap_or(0)
            );
            print_accounts(&result["accounts"]);
            print_runtime(&result["runtime"]);
        }
        "syncMessages" => {
            let triggered = result["triggered"]
                .as_array()
                .map(|names| {
                    names
                        .iter()
                        .filter_map(Value::as_str)
                        .map(terminal_text)
                        .collect::<Vec<_>>()
                        .join(", ")
                })
                .unwrap_or_default();
            println!("triggered: {triggered}");
            println!("online: {}", result["online_count"].as_u64().unwrap_or(0));
        }
        "listAccounts" => print_accounts(&result["accounts"]),
        "listMessages" => {
            println!("total: {}", result["total"].as_i64().unwrap_or(0));
            if let Some(messages) = result["messages"].as_array() {
                for message in messages {
                    println!(
                        "{} [{}] {} - {}",
                        terminal_text(message["uid"].as_str().unwrap_or("?")),
                        terminal_text(message["account"].as_str().unwrap_or("?")),
                        terminal_text(message["sender"].as_str().unwrap_or("?")),
                        terminal_text(message["subject"].as_str().unwrap_or(""))
                    );
                }
            }
        }
        "getMessage" => print_message(result),
        "getSettings" | "setSettings" => print_settings(result),
        "getNotificationStatus" | "requestNotificationAuthorization" => {
            println!(
                "provider: {}",
                terminal_text(result["provider"].as_str().unwrap_or("?"))
            );
            println!(
                "availability: {}",
                terminal_text(result["availability"].as_str().unwrap_or("unknown"))
            );
            println!(
                "authorization: {}",
                terminal_text(result["authorization"].as_str().unwrap_or("unknown"))
            );
            println!("fresh: {}", result["fresh"].as_bool().unwrap_or(false));
        }
        "listFrontends" => {
            println!(
                "default: {}",
                terminal_text(result["default"].as_str().unwrap_or("?"))
            );
            if let Some(frontends) = result["frontends"].as_array() {
                for frontend in frontends {
                    println!(
                        "  {} {} {} [{}]{}",
                        terminal_text(frontend["id"].as_str().unwrap_or("?")),
                        terminal_text(frontend["name"].as_str().unwrap_or("?")),
                        terminal_text(frontend["version"].as_str().unwrap_or("?")),
                        terminal_text(frontend["lifecycle"].as_str().unwrap_or("?")),
                        if frontend["builtin"].as_bool() == Some(true) {
                            " builtin"
                        } else {
                            ""
                        }
                    );
                    // ADR-0030 决策 1：声明要可审计。装一个前端之前得能看见它要什么——
                    // 尤其是 plugins_manage，它等于「我可以代表用户安装任意受管包」。
                    let permissions = frontend["permissions"]
                        .as_array()
                        .map(|values| {
                            values
                                .iter()
                                .filter_map(|value| value.as_str())
                                .map(terminal_text)
                                .collect::<Vec<_>>()
                        })
                        .unwrap_or_default();
                    if !permissions.is_empty() {
                        println!("    permissions: {}", permissions.join(", "));
                    }
                }
            }
        }
        "listExternalPlugins" => {
            if let Some(plugins) = result["plugins"].as_array() {
                println!("plugins: {}", plugins.len());
                for plugin in plugins {
                    println!(
                        "  {} {} [{}] enabled={} restarts={}",
                        terminal_text(plugin["id"].as_str().unwrap_or("?")),
                        terminal_text(plugin["state"].as_str().unwrap_or("unknown")),
                        terminal_text(plugin["lifecycle"].as_str().unwrap_or("?")),
                        plugin["enabled"].as_bool().unwrap_or(false),
                        plugin["restart_count"].as_u64().unwrap_or(0)
                    );
                }
            }
        }
        "send" => println!("sent"),
        "commitPluginInstall" => match result["action"].as_str() {
            Some("installed") => println!(
                "已安装 {} {}",
                terminal_text(result["id"].as_str().unwrap_or("?")),
                terminal_text(result["version"].as_str().unwrap_or("?"))
            ),
            Some("upgraded") => println!(
                "已升级 {}: {} -> {}（旧版在回滚位，可用 rollback 切回）",
                terminal_text(result["id"].as_str().unwrap_or("?")),
                terminal_text(result["from_version"].as_str().unwrap_or("?")),
                terminal_text(result["version"].as_str().unwrap_or("?"))
            ),
            _ => println!("{result}"),
        },
        "removePlugin" => println!(
            "已移除 {}",
            terminal_text(result["removed"].as_str().unwrap_or("?"))
        ),
        "rollbackPlugin" => println!(
            "{} 已切换到 {}（再执行一次切回另一版）",
            terminal_text(result["id"].as_str().unwrap_or("?")),
            terminal_text(result["version"].as_str().unwrap_or("?"))
        ),
        "setPluginEnabled" => {
            let state = if result["enabled"].as_bool().unwrap_or(false) {
                "已启用"
            } else {
                "已停用"
            };
            println!("{} {}", terminal_text(result["id"].as_str().unwrap_or("?")), state);
        }
        _ => println!("{result}"),
    }
}

/// `status.runtime`（ADR-0041 决策 6）。旧 daemon 没有这个对象时什么都不打。
fn print_runtime(runtime: &Value) {
    let Some(runtime) = runtime.as_object() else {
        return;
    };
    for key in [
        "rss_bytes",
        "phys_footprint_bytes",
        "store_db_bytes",
        "store_wal_bytes",
        "stored_messages",
        "stored_attachment_bytes",
    ] {
        match runtime.get(key).and_then(Value::as_u64) {
            Some(value) => println!("{key}: {value}"),
            None => println!("{key}: n/a"),
        }
    }
}

fn print_discovery(discovery: &maild_client::ProviderDiscovery) {
    println!("provider: {}", terminal_text(&discovery.display_name));
    println!(
        "source: {} ({})",
        terminal_text(&discovery.source),
        terminal_text(&discovery.confidence)
    );
    println!(
        "imap: {}:{} {}",
        terminal_text(&discovery.incoming.server),
        discovery.incoming.port,
        terminal_text(&discovery.incoming.security)
    );
    println!(
        "smtp: {}:{} {}",
        terminal_text(&discovery.outgoing.server),
        discovery.outgoing.port,
        terminal_text(&discovery.outgoing.security)
    );
    if !discovery.suggested_username.is_empty() {
        println!("username: {}", terminal_text(&discovery.suggested_username));
    }
    println!("auth: {}", terminal_text(&discovery.auth_kind));
    if !discovery.notes.is_empty() {
        println!("notes: {}", terminal_text(&discovery.notes));
    }
    if let Some(url) = &discovery.help_url {
        println!("help: {}", terminal_text(url));
    }
}

const EARLIEST_PLAUSIBLE_DATE_SECS: u64 = 31_536_000;
const LATEST_PLAUSIBLE_DATE_SECS: u64 = 253_402_300_799;

fn format_unix_with_utc_hint(secs: u64) -> String {
    if !(EARLIEST_PLAUSIBLE_DATE_SECS..=LATEST_PLAUSIBLE_DATE_SECS).contains(&secs) {
        return secs.to_string();
    }
    let days = (secs / 86_400) as i64;
    let seconds_of_day = secs % 86_400;
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { y + 1 } else { y };
    format!(
        "{secs} ({year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z)",
        seconds_of_day / 3600,
        (seconds_of_day % 3600) / 60,
        seconds_of_day % 60,
    )
}

fn print_settings(result: &Value) {
    let rules = &result["rules"];
    println!(
        "show_sensitive: {}",
        rules["show_sensitive"].as_bool().unwrap_or(true)
    );
    println!(
        "timezone_offset_minutes: {}",
        rules["timezone_offset_minutes"].as_i64().unwrap_or(0)
    );
    if rules["follow_system_timezone"].as_bool() == Some(true) {
        println!("follow_system_timezone: true");
    }
    let keywords = rules["silent_keywords"]
        .as_array()
        .map(|values| {
            values
                .iter()
                .filter_map(Value::as_str)
                .map(terminal_text)
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default();
    println!("silent_keywords: {keywords}");
    if let Some(windows) = rules["quiet_hours"].as_array() {
        for window in windows {
            println!(
                "quiet_hours: {}-{}",
                terminal_text(window["start"].as_str().unwrap_or("?")),
                terminal_text(window["end"].as_str().unwrap_or("?"))
            );
        }
    }
    if rules["notify_new_mail"].as_bool() == Some(true) {
        println!("notify_new_mail: true");
    }
    if let Some(muted) = rules["muted_accounts"].as_array().filter(|muted| !muted.is_empty()) {
        let muted = muted
            .iter()
            .filter_map(Value::as_str)
            .map(terminal_text)
            .collect::<Vec<_>>()
            .join(", ");
        println!("muted_accounts: {muted}");
    }
    match result["persisted"].as_bool() {
        Some(false) => println!("persisted: false"),
        Some(true) => println!("persisted: true"),
        None => {}
    }
}

fn print_message(result: &Value) {
    let message = &result["message"];
    println!(
        "account: {}",
        terminal_text(message["account"].as_str().unwrap_or("?"))
    );
    println!(
        "uid: {}",
        terminal_text(message["uid"].as_str().unwrap_or("?"))
    );
    println!(
        "from: {}",
        terminal_text(message["sender"].as_str().unwrap_or("?"))
    );
    println!(
        "subject: {}",
        terminal_text(message["subject"].as_str().unwrap_or(""))
    );
    println!(
        "received_at: {}",
        format_unix_with_utc_hint(message["received_at"].as_u64().unwrap_or(0))
    );
    if let Some(date_at) = message["date_at"].as_u64() {
        println!("date_at: {}", format_unix_with_utc_hint(date_at));
    }
    if let Some(decision) = result.get("remote_content") {
        println!(
            "remote_content: {} ({})",
            terminal_text(decision["decision"].as_str().unwrap_or("?")),
            terminal_text(decision["reason"].as_str().unwrap_or("?"))
        );
    }
    if let Some(attachments) = message["attachments"].as_array() {
        for attachment in attachments {
            println!(
                "attachment: part={} {} {} {} bytes{}{}",
                attachment["part_index"].as_u64().unwrap_or(0),
                terminal_text(attachment["filename"].as_str().unwrap_or("")),
                terminal_text(attachment["mime_type"].as_str().unwrap_or("")),
                attachment["size"].as_u64().unwrap_or(0),
                if attachment["inline"].as_bool() == Some(true) {
                    " inline"
                } else {
                    ""
                },
                if attachment["stored"].as_bool() == Some(true) {
                    ""
                } else {
                    " (not stored)"
                }
            );
        }
    }
    println!();
    let body = terminal_multiline_text(message["body"].as_str().unwrap_or(""));
    print!("{body}");
    if !body.ends_with('\n') {
        println!();
    }
}

fn print_accounts(accounts: &Value) {
    let Some(accounts) = accounts.as_array() else {
        println!("accounts: 0");
        return;
    };
    println!("accounts: {}", accounts.len());
    for account in accounts {
        let detail = terminal_text(account["detail"].as_str().unwrap_or(""));
        if detail.is_empty() {
            println!(
                "  {} {}",
                terminal_text(account["account"].as_str().unwrap_or("?")),
                terminal_text(account["state"].as_str().unwrap_or("?"))
            );
        } else {
            println!(
                "  {} {} ({detail})",
                terminal_text(account["account"].as_str().unwrap_or("?")),
                terminal_text(account["state"].as_str().unwrap_or("?"))
            );
        }
    }
}

fn terminal_text(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .collect()
}

fn terminal_multiline_text(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    let mut chars = value.chars().peekable();
    while let Some(character) = chars.next() {
        match character {
            '\r' => {
                output.push('\n');
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
            }
            '\n' => output.push('\n'),
            character if character.is_control() => output.push(' '),
            character => output.push(character),
        }
    }
    output
}

// —— 账户子命令运行时（M3.7-01） ——

/// ADR-0019 插件事务 CLI：CLI 只把包复制进 `.incoming/<rand>` staging 并经 RPC
/// 指挥 Agent 校验发布；受管目录的其余写入全部归 Agent。
async fn run_plugin(
    client: RpcClient,
    command: PluginCommand,
    json_output: bool,
) -> Result<(), AppError> {
    match command {
        PluginCommand::Install { source, frontend } => {
            let kind = if frontend {
                PluginPackageKind::Frontend
            } else {
                PluginPackageKind::External
            };
            let staged = maild_client::stage_plugin_package(&source, kind)
                .map_err(AppError::rpc)?;
            let params = if frontend {
                json!({ "kind": "frontend", "staging": staged.staging() })
            } else {
                json!({ "staging": staged.staging() })
            };
            let result = client.call("commitPluginInstall", params).await?;
            print_result("commitPluginInstall", &result, json_output)?;
        }
        PluginCommand::Remove { id, frontend } => {
            let result = client
                .call(
                    "removePlugin",
                    plugin_id_params(&id, frontend.then_some("frontend")),
                )
                .await?;
            print_result("removePlugin", &result, json_output)?;
        }
        PluginCommand::Rollback { id, frontend } => {
            let result = client
                .call(
                    "rollbackPlugin",
                    plugin_id_params(&id, frontend.then_some("frontend")),
                )
                .await?;
            print_result("rollbackPlugin", &result, json_output)?;
        }
        PluginCommand::Enable { id } => {
            let result = client
                .call("setPluginEnabled", json!({ "id": id, "enabled": true }))
                .await?;
            print_result("setPluginEnabled", &result, json_output)?;
        }
        PluginCommand::Disable { id } => {
            let result = client
                .call("setPluginEnabled", json!({ "id": id, "enabled": false }))
                .await?;
            print_result("setPluginEnabled", &result, json_output)?;
        }
    }
    Ok(())
}

/// ADR-0032 决策 7：分块读到同目录临时文件，实收字节数对得上 `size` 才发布；目标已存在即失败
/// 且不覆盖（`hard_link` 在目标存在时失败，是原子的 no-clobber 发布）。
async fn run_attachment(
    client: RpcClient,
    command: AttachmentCommand,
    json_output: bool,
) -> Result<(), AppError> {
    let AttachmentCommand::Save {
        account,
        folder,
        uid,
        part,
        out,
    } = command;
    if out.exists() {
        return Err(AppError::connection(format!(
            "目标已存在，不覆盖: {}",
            out.display()
        )));
    }
    let parent = out
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    let saved = client
        .read_attachment(&account, &folder, &uid, part)
        .await?;
    let temp = parent.join(format!(
        ".maild-attachment-{}-{}.tmp",
        std::process::id(),
        generate_token()?
    ));
    let publish = (|| -> io::Result<()> {
        use std::io::Write as _;
        let mut file = File::options().write(true).create_new(true).open(&temp)?;
        file.write_all(&saved.bytes)?;
        file.sync_all()?;
        drop(file);
        std::fs::hard_link(&temp, &out)?;
        std::fs::remove_file(&temp)
    })();
    if let Err(error) = publish {
        let _ = std::fs::remove_file(&temp);
        return Err(AppError::connection(format!(
            "写入 {} 失败（已有目标未被改动）: {error}",
            out.display()
        )));
    }
    if json_output {
        println!(
            "{}",
            json!({
                "path": out,
                "bytes": saved.bytes.len(),
                "attachment": {
                    "part_index": saved.attachment.part_index,
                    "filename": saved.attachment.filename,
                    "mime_type": saved.attachment.mime_type,
                    "size": saved.attachment.size,
                },
            })
        );
    } else {
        println!(
            "saved: {} ({} bytes, {})",
            out.display(),
            saved.bytes.len(),
            terminal_text(&saved.attachment.mime_type)
        );
    }
    Ok(())
}

/// 草稿子命令（ADR-0034）。`attach` / `detach` 是一次读-改-写：先取当前列表，
/// 再带着同一个 `--version` 整体写回——版本号守的是整行，因此中途被别人改过会被拒绝。
async fn run_draft(
    client: RpcClient,
    command: DraftCommand,
    json_output: bool,
) -> Result<(), AppError> {
    use maild_client::{DraftAttachmentInput, DraftPatchInput};

    match command {
        DraftCommand::List {
            account,
            limit,
            offset,
        } => {
            let result = client
                .list_drafts(account.as_deref(), Some(limit), Some(offset))
                .await?;
            if json_output {
                println!(
                    "{}",
                    serde_json::to_string(&json!({
                        "drafts": result.drafts,
                        "total": result.total,
                    }))
                    .map_err(|error| AppError::rpc(format!("JSON 输出失败: {error}")))?
                );
                return Ok(());
            }
            println!("drafts: {} of {}", result.drafts.len(), result.total);
            for draft in &result.drafts {
                println!(
                    "  {} v{} [{}] {} → {}",
                    draft.id,
                    draft.version,
                    terminal_text(&draft.account),
                    terminal_text(if draft.subject.is_empty() {
                        "（无主题）"
                    } else {
                        &draft.subject
                    }),
                    terminal_text(&draft.to.join(", ")),
                );
                if !draft.snippet.is_empty() {
                    println!("    {}", terminal_text(&draft.snippet));
                }
                if draft.attachment_count > 0 {
                    println!("    attachments: {}", draft.attachment_count);
                }
            }
            Ok(())
        }
        DraftCommand::Show { id } => {
            let draft = client.get_draft(&id).await?;
            print_draft(&draft, json_output)
        }
        DraftCommand::New {
            account,
            from,
            to,
            cc,
            bcc,
            subject,
            body,
            body_file,
        } => {
            let patch = DraftPatchInput {
                from,
                to: (!to.is_empty()).then_some(to),
                cc: (!cc.is_empty()).then_some(cc),
                bcc: (!bcc.is_empty()).then_some(bcc),
                subject,
                body: read_optional_body(body, body_file)?,
                ..DraftPatchInput::default()
            };
            let draft = client.create_draft(&account, &patch).await?;
            print_draft(&draft, json_output)
        }
        DraftCommand::Set {
            id,
            version,
            from,
            to,
            clear_to,
            cc,
            clear_cc,
            bcc,
            clear_bcc,
            subject,
            body,
            body_file,
        } => {
            let patch = DraftPatchInput {
                from,
                to: address_patch(to, clear_to),
                cc: address_patch(cc, clear_cc),
                bcc: address_patch(bcc, clear_bcc),
                subject,
                body: read_optional_body(body, body_file)?,
                ..DraftPatchInput::default()
            };
            let draft = client.update_draft(&id, version, &patch).await?;
            print_draft(&draft, json_output)
        }
        DraftCommand::Attach {
            id,
            version,
            account,
            folder,
            uid,
            part,
        } => {
            let current = client.get_draft(&id).await?;
            let mut attachments = attachment_inputs(&current);
            attachments.push(DraftAttachmentInput {
                upload_id: None,
                account,
                folder,
                uid,
                part_index: part,
            });
            let patch = DraftPatchInput {
                attachments: Some(attachments),
                ..DraftPatchInput::default()
            };
            let draft = client.update_draft(&id, version, &patch).await?;
            print_draft(&draft, json_output)
        }
        DraftCommand::AttachFile {
            id,
            version,
            file,
            mime_type,
        } => {
            let draft = upload_draft_file(&client, &id, version, &file, &mime_type).await?;
            print_draft(&draft, json_output)
        }
        DraftCommand::FinishUpload { upload_id, version } => {
            let draft = client.finish_attachment_upload(&upload_id, version).await?;
            print_draft(&draft, json_output)
        }
        DraftCommand::CancelUpload { upload_id } => {
            client.cancel_attachment_upload(&upload_id).await?;
            if json_output {
                println!("{}", json!({"cancelled": true}));
            } else {
                println!("cancelled: {upload_id}");
            }
            Ok(())
        }
        DraftCommand::Detach {
            id,
            version,
            position,
        } => {
            let current = client.get_draft(&id).await?;
            if position >= current.attachments.len() {
                return Err(AppError::rpc(format!(
                    "草稿只有 {} 条附件，没有第 {position} 条",
                    current.attachments.len()
                )));
            }
            let mut attachments = attachment_inputs(&current);
            attachments.remove(position);
            let patch = DraftPatchInput {
                attachments: Some(attachments),
                ..DraftPatchInput::default()
            };
            let draft = client.update_draft(&id, version, &patch).await?;
            print_draft(&draft, json_output)
        }
        DraftCommand::Rm { id, version } => {
            let draft = client.delete_draft(&id, version).await?;
            if json_output {
                println!("{}", json!({ "deleted": true, "id": draft.id }));
            } else {
                println!("deleted: {} (v{})", draft.id, draft.version);
            }
            Ok(())
        }
    }
}

/// `--to a --to b` → 替换整张列表；`--clear-to` → 清空；两者都没有 → 不动。
fn address_patch(values: Vec<String>, clear: bool) -> Option<Vec<String>> {
    if clear {
        return Some(Vec::new());
    }
    (!values.is_empty()).then_some(values)
}

fn read_optional_body(
    body: Option<String>,
    body_file: Option<PathBuf>,
) -> Result<Option<String>, AppError> {
    match (body, body_file) {
        (_, Some(path)) => Ok(Some(read_body(&path)?)),
        (Some(body), None) => Ok(Some(body)),
        (None, None) => Ok(None),
    }
}

async fn upload_draft_file(
    client: &RpcClient,
    id: &str,
    version: u64,
    path: &Path,
    mime: &str,
) -> Result<maild_client::DraftView, AppError> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    if std::fs::symlink_metadata(path)
        .map_err(|e| AppError::rpc(e.to_string()))?
        .file_type()
        .is_symlink()
    {
        return Err(AppError::rpc("附件不能是符号链接"));
    }
    let mut file = options
        .open(path)
        .map_err(|e| AppError::rpc(format!("无法读取附件: {e}")))?;
    let metadata = file.metadata().map_err(|e| AppError::rpc(e.to_string()))?;
    if !metadata.is_file() {
        return Err(AppError::rpc("附件必须是普通文件"));
    }
    let filename = path
        .file_name()
        .and_then(|v| v.to_str())
        .ok_or_else(|| AppError::rpc("附件名必须是 UTF-8"))?;
    let upload = client
        .begin_attachment_upload(id, filename, mime, metadata.len())
        .await?;
    let transfer: Result<(), AppError> = async {
        let mut buffer = vec![0; 512 * 1024];
        let mut offset = 0;
        loop {
            let n = file
                .read(&mut buffer)
                .map_err(|e| AppError::rpc(e.to_string()))?;
            if n == 0 {
                break;
            }
            let receipt = client
                .write_attachment_upload(&upload.upload_id, offset, &buffer[..n])
                .await?;
            offset = receipt.offset;
        }
        if offset != metadata.len() {
            return Err(AppError::rpc("附件读取期间长度变化，请重新选择文件"));
        }
        Ok(())
    }
    .await;
    if let Err(error) = transfer {
        let _ = client.cancel_attachment_upload(&upload.upload_id).await;
        return Err(error);
    }
    client.finish_attachment_upload(&upload.upload_id, version).await.map_err(|e| {
        let mut error = AppError::from(e);
        error.message = format!("{}；上传 {} 已保留，可用 draft finish-upload --version <当前版本> 重试，或 cancel-upload 取消", error.message, upload.upload_id);
        error
    })
}

/// 把读回来的引用摊回写入形状：`attach` / `detach` 要整体重写这张列表。
fn attachment_inputs(draft: &maild_client::DraftView) -> Vec<maild_client::DraftAttachmentInput> {
    draft
        .attachments
        .iter()
        .map(|attachment| maild_client::DraftAttachmentInput {
            upload_id: attachment.upload_id.clone(),
            account: if attachment.upload_id.is_some() {
                None
            } else {
                Some(attachment.account.clone())
            },
            folder: attachment.folder.clone(),
            uid: attachment.uid.clone(),
            part_index: attachment.part_index,
        })
        .collect()
}

fn print_draft(draft: &maild_client::DraftView, json_output: bool) -> Result<(), AppError> {
    if json_output {
        println!(
            "{}",
            serde_json::to_string(draft)
                .map_err(|error| AppError::rpc(format!("JSON 输出失败: {error}")))?
        );
        return Ok(());
    }
    println!("id: {}", draft.id);
    println!("version: {}", draft.version);
    println!("account: {}", terminal_text(&draft.account));
    if !draft.from.is_empty() {
        println!("from: {}", terminal_text(&draft.from));
    }
    for (label, values) in [("to", &draft.to), ("cc", &draft.cc), ("bcc", &draft.bcc)] {
        if !values.is_empty() {
            println!("{label}: {}", terminal_text(&values.join(", ")));
        }
    }
    println!("subject: {}", terminal_text(&draft.subject));
    println!(
        "updated_at: {}",
        format_unix_with_utc_hint(draft.updated_at)
    );
    for (index, attachment) in draft.attachments.iter().enumerate() {
        println!(
            "attachment[{index}]: {} ({} bytes, {}){}",
            terminal_text(&attachment.filename),
            attachment.size,
            terminal_text(&attachment.mime_type),
            if attachment.stored {
                ""
            } else {
                " [源 part 已不可读]"
            },
        );
    }
    println!("--");
    println!("{}", terminal_multiline_text(&draft.body));
    Ok(())
}

async fn run_reading_policy(
    client: RpcClient,
    command: ReadingPolicyCommand,
    json_output: bool,
) -> Result<(), AppError> {
    let result = match command {
        ReadingPolicyCommand::Show => client.reading_policy().await?,
        ReadingPolicyCommand::Set { remote_content } => {
            client
                .set_reading_policy(&maild_client::ReadingPolicyPatch {
                    remote_content: Some(remote_content),
                    ..Default::default()
                })
                .await?
        }
        ReadingPolicyCommand::Trust { values } => {
            client
                .set_reading_policy(&maild_client::ReadingPolicyPatch {
                    trust: values,
                    ..Default::default()
                })
                .await?
        }
        ReadingPolicyCommand::Untrust { values } => {
            client
                .set_reading_policy(&maild_client::ReadingPolicyPatch {
                    untrust: values,
                    ..Default::default()
                })
                .await?
        }
    };
    if json_output {
        println!(
            "{}",
            json!({
                "policy": {
                    "remote_content": result.policy.remote_content,
                    "trusted_senders": result.policy.trusted_senders,
                },
                "persisted": result.persisted,
            })
        );
        return Ok(());
    }
    println!(
        "remote_content: {}",
        terminal_text(&result.policy.remote_content)
    );
    println!("trusted_senders: {}", result.policy.trusted_senders.len());
    for entry in &result.policy.trusted_senders {
        println!(
            "  {} [{}] added_at={}",
            terminal_text(&entry.value),
            terminal_text(&entry.kind),
            format_unix_with_utc_hint(entry.added_at)
        );
    }
    if !result.persisted {
        println!("persisted: false（策略文件停摆：读到的是缺省值，写会被拒绝）");
    }
    Ok(())
}

fn plugin_id_params(id: &str, kind: Option<&str>) -> Value {
    match kind {
        Some(kind) => json!({ "id": id, "kind": kind }),
        None => json!({ "id": id }),
    }
}

/// 事务类命令的统一输出：`--json` 打一行 JSON，否则走人读分支。
fn print_result(method: &str, result: &Value, json_output: bool) -> Result<(), AppError> {
    if json_output {
        let line = serde_json::to_string(result)
            .map_err(|error| AppError::rpc(format!("JSON 输出失败: {error}")))?;
        println!("{line}");
    } else {
        print_text(method, result);
    }
    Ok(())
}

async fn run_account(client: RpcClient, command: AccountCommand, json_output: bool) -> Result<(), AppError> {
    match command {
        AccountCommand::List { all } => {
            let result = client.list_account_profiles(all).await?;
            if json_output {
                println!(
                    "{}",
                    serde_json::to_string(&result)
                        .map_err(|e| AppError::rpc(format!("JSON 输出失败: {e}")))?
                );
            } else {
                print_profiles(&result.profiles);
            }
        }
        AccountCommand::Test { spec } => {
            let (draft, incoming, outgoing) = collect_draft(&spec)?;
            let report = client
                .probe_account(&draft, &incoming, outgoing.as_deref())
                .await?;
            if json_output {
                println!(
                    "{}",
                    serde_json::to_string(&report)
                        .map_err(|e| AppError::rpc(format!("JSON 输出失败: {e}")))?
                );
            } else {
                print_probe("incoming", &report.incoming);
                if let Some(outgoing_report) = &report.outgoing {
                    print_probe("outgoing", outgoing_report);
                }
                let ok = report.incoming.ok
                    && report.outgoing.as_ref().map(|o| o.ok).unwrap_or(true);
                println!("{}", if ok { "probe ok" } else { "probe failed" });
                if !ok {
                    return Err(AppError::rpc("探测未通过"));
                }
            }
        }
        AccountCommand::Add { spec } => {
            let (draft, incoming, outgoing) = collect_draft(&spec)?;
            let result = client
                .create_account(&draft, &incoming, outgoing.as_deref())
                .await?;
            if json_output {
                println!(
                    "{}",
                    serde_json::to_string(&result)
                        .map_err(|e| AppError::rpc(format!("JSON 输出失败: {e}")))?
                );
            } else {
                println!("added {} ({})", result.profile.id, result.profile.display_name);
                println!("state: {}", result.profile.runtime.state);
            }
        }
        AccountCommand::ProviderTest { spec } => {
            let draft = collect_provider_draft(&spec)?;
            let report = client.probe_account(&draft, "", None).await?;
            if json_output {
                println!(
                    "{}",
                    serde_json::to_string(&report)
                        .map_err(|e| AppError::rpc(format!("JSON 输出失败: {e}")))?
                );
            } else {
                print_probe("incoming", &report.incoming);
                if let Some(outgoing_report) = &report.outgoing {
                    print_probe("outgoing", outgoing_report);
                }
                let ok =
                    report.incoming.ok && report.outgoing.as_ref().map(|o| o.ok).unwrap_or(true);
                println!("{}", if ok { "probe ok" } else { "probe failed" });
                if !ok {
                    return Err(AppError::rpc("探测未通过"));
                }
            }
        }
        AccountCommand::ProviderAdd { spec } => {
            let draft = collect_provider_draft(&spec)?;
            let result = client.create_account(&draft, "", None).await?;
            if json_output {
                println!(
                    "{}",
                    serde_json::to_string(&result)
                        .map_err(|e| AppError::rpc(format!("JSON 输出失败: {e}")))?
                );
            } else {
                println!(
                    "added {} ({})",
                    result.profile.id, result.profile.display_name
                );
                println!("state: {}", result.profile.runtime.state);
            }
        }
        AccountCommand::Edit {
            id,
            display_name,
            address,
            imap_host,
            imap_port,
            imap_security,
            imap_username,
            imap_mailbox,
            update_imap_password,
            remove_smtp,
            smtp_host,
            smtp_port,
            smtp_security,
            smtp_username,
            smtp_credential,
            update_smtp_password,
        } => {
            let profiles = client.list_account_profiles(false).await?.profiles;
            let current = profiles
                .iter()
                .find(|profile| profile.id == id && profile.editable)
                .ok_or_else(|| {
                    AppError::rpc(format!("找不到可编辑的受管账户 {id}（legacy 账户只读）"))
                })?
                .clone();
            let mut patch = UpdatePatchInput {
                display_name,
                address,
                ..Default::default()
            };
            if imap_host.is_some()
                || imap_port.is_some()
                || imap_security.is_some()
                || imap_username.is_some()
                || imap_mailbox.is_some()
            {
                let incoming = current.incoming.clone();
                patch.incoming = Some(IncomingInput::imap(
                    imap_host.unwrap_or(incoming.server),
                    imap_port.unwrap_or(incoming.port),
                    imap_security.unwrap_or(incoming.security),
                    imap_username.unwrap_or(incoming.username),
                    imap_mailbox.unwrap_or(incoming.mailbox),
                    incoming.idle,
                    incoming.poll_secs,
                ));
            }
            if remove_smtp {
                patch.outgoing = Some(None);
            } else if smtp_host.is_some() || smtp_username.is_some() || smtp_credential.is_some() {
                let existing = current.outgoing.clone();
                let host = smtp_host
                    .or(existing.as_ref().map(|o| o.server.clone()))
                    .ok_or_else(|| AppError::rpc("新增发信配置需要 --smtp-host"))?;
                let username = smtp_username
                    .or(existing.as_ref().map(|o| o.username.clone()))
                    .ok_or_else(|| AppError::rpc("新增发信配置需要 --smtp-username"))?;
                patch.outgoing = Some(Some(OutgoingInput::smtp(
                    host,
                    smtp_port.unwrap_or_else(|| existing.as_ref().map(|o| o.port).unwrap_or(587)),
                    smtp_security
                        .or_else(|| existing.as_ref().map(|o| o.security.clone()))
                        .unwrap_or_else(|| "starttls".into()),
                    username,
                    smtp_credential
                        .or_else(|| existing.as_ref().map(|o| o.credential.clone()))
                        .unwrap_or_else(|| "incoming".into()),
                )));
            }

            // 秘密保存在栈上局部变量中，调用结束即释放；绝不进入 argv/env。
            let new_imap_password = if update_imap_password {
                Some(read_secret("新的 IMAP 密码")?)
            } else {
                None
            };
            let new_smtp_password = if update_smtp_password {
                Some(read_secret("新的 SMTP 密码")?)
            } else {
                None
            };
            let secrets = maild_client::AccountSecretUpdate {
                incoming: new_imap_password.as_deref(),
                outgoing: new_smtp_password.as_deref(),
            };
            let updated = client
                .update_account(&id, current.revision, &patch, secrets)
                .await?;
            drop(new_imap_password);
            drop(new_smtp_password);
            if json_output {
                println!(
                    "{}",
                    serde_json::to_string(&updated)
                        .map_err(|e| AppError::rpc(format!("JSON 输出失败: {e}")))?
                );
            } else {
                println!("updated {} revision {}", updated.id, updated.revision);
                println!("state: {}", updated.runtime.state);
            }
        }
        enable_disable @ (AccountCommand::Enable { .. } | AccountCommand::Disable { .. }) => {
            let (id, enabled) = match enable_disable {
                AccountCommand::Enable { id } => (id, true),
                AccountCommand::Disable { id } => (id, false),
                _ => unreachable!(),
            };
            let revision = current_revision(&client, &id).await?;
            let updated = client.set_account_enabled(&id, revision, enabled).await?;
            if json_output {
                println!(
                    "{}",
                    serde_json::to_string(&updated)
                        .map_err(|e| AppError::rpc(format!("JSON 输出失败: {e}")))?
                );
            } else {
                println!("{} revision {}", updated.id, updated.revision);
                println!("enabled: {}", updated.enabled);
                println!("state: {}", updated.runtime.state);
            }
        }
        AccountCommand::Discover { input, network } => {
            let discovery = client.discover_email_provider(&input, network).await?;
            if json_output {
                println!(
                    "{}",
                    serde_json::to_string(&discovery)
                        .map_err(|e| AppError::rpc(format!("JSON 输出失败: {e}")))?
                );
            } else {
                print_discovery(&discovery);
            }
        }
        AccountCommand::Remove { id } => {
            let revision = current_revision(&client, &id).await?;
            let result = client.remove_account(&id, revision).await?;
            if json_output {
                println!(
                    "{}",
                    serde_json::to_string(&result)
                        .map_err(|e| AppError::rpc(format!("JSON 输出失败: {e}")))?
                );
            } else {
                println!("archived: {}", result.archived);
                println!("credentials_deleted: {}", result.credentials_deleted);
                println!("messages_retained: {}", result.messages_retained);
            }
        }
    }
    Ok(())
}

async fn current_revision(client: &RpcClient, id: &str) -> Result<u64, AppError> {
    client
        .list_account_profiles(true)
        .await?
        .profiles
        .into_iter()
        .find(|profile| profile.id == id)
        .map(|profile| profile.revision)
        .ok_or_else(|| AppError::rpc(format!("账户不存在: {id}")))
}

fn collect_draft(spec: &AccountSpec) -> Result<(AccountDraftInput, String, Option<String>), AppError> {
    let incoming_secret = read_secret("IMAP 密码")?;
    let outgoing_secret = if spec.smtp_host.is_some()
        && spec.smtp_credential == "separate"
    {
        Some(read_secret("SMTP 密码")?)
    } else {
        None
    };
    let draft = AccountDraftInput {
        display_name: spec.display_name.clone(),
        address: spec.address.clone(),
        incoming: IncomingInput::imap(
            spec.imap_host.clone(),
            spec.imap_port,
            spec.imap_security.clone(),
            spec.imap_username.clone(),
            spec.imap_mailbox.clone(),
            !spec.no_imap_idle,
            spec.imap_poll_secs,
        ),
        outgoing: spec.smtp_host.as_ref().map(|host| {
            OutgoingInput::smtp(
                host.clone(),
                spec.smtp_port,
                spec.smtp_security.clone(),
                spec.smtp_username
                    .clone()
                    .unwrap_or_else(|| spec.imap_username.clone()),
                spec.smtp_credential.clone(),
            )
        }),
    };
    validate_draft_input(&draft).map_err(|error| AppError::rpc(error.to_string()))?;
    Ok((draft, incoming_secret, outgoing_secret))
}

fn collect_provider_draft(spec: &ProviderAccountSpec) -> Result<AccountDraftInput, AppError> {
    let draft = AccountDraftInput {
        display_name: spec.display_name.clone(),
        address: spec.address.clone(),
        incoming: IncomingInput::plugin(spec.provider.clone(), spec.poll_secs),
        outgoing: (!spec.receive_only).then(|| OutgoingInput::plugin(spec.provider.clone())),
    };
    validate_draft_input(&draft).map_err(|error| AppError::rpc(error.to_string()))?;
    Ok(draft)
}

/// 秘密输入：TTY 上关闭回显逐字符读取；非交互模式只从 stdin 读一行。
/// 绝不接受命令行参数或环境变量。
fn read_secret(prompt: &str) -> Result<String, AppError> {
    #[cfg(unix)]
    {
        let tty = unsafe { libc::isatty(libc::STDIN_FILENO) } == 1;
        if tty {
            return read_secret_no_echo(prompt);
        }
    }
    #[cfg(not(unix))]
    {
        // Windows 写命令在凭据 provider 落地前由服务端拒绝；这里保持 stdin 通道。
        let _ = prompt;
    }
    let mut line = String::new();
    std::io::stdin()
        .read_line(&mut line)
        .map_err(|e| AppError::connection(format!("读取秘密失败: {e}")))?;
    let line = line.trim_end_matches(['\r', '\n']).to_string();
    if line.is_empty() {
        return Err(AppError::connection("秘密不能为空"));
    }
    Ok(line)
}

#[cfg(unix)]
fn read_secret_no_echo(prompt: &str) -> Result<String, AppError> {
    use std::io::{BufRead, Write};

    let termios = unsafe {
        let mut termios: libc::termios = std::mem::zeroed();
        if libc::tcgetattr(libc::STDIN_FILENO, &mut termios) != 0 {
            return Err(AppError::connection("读取终端属性失败"));
        }
        termios
    };
    let mut no_echo = termios;
    no_echo.c_lflag &= !libc::ECHO;
    if unsafe { libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &no_echo) } != 0 {
        return Err(AppError::connection("关闭终端回显失败"));
    }
    let restore = |termios: libc::termios| {
        unsafe { libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &termios) };
    };

    eprint!("{prompt} ");
    let _ = std::io::stderr().flush();
    let mut line = String::new();
    let read_result = std::io::stdin().lock().read_line(&mut line);
    restore(termios);
    // 回显关闭下换行不会自动输出，补一个换行保持终端整洁。
    eprintln!();
    read_result.map_err(|e| AppError::connection(format!("读取秘密失败: {e}")))?;
    let line = line.trim_end_matches(['\r', '\n']).to_string();
    if line.is_empty() {
        return Err(AppError::connection("秘密不能为空"));
    }
    Ok(line)
}

fn print_profiles(profiles: &[maild_client::AccountProfileView]) {
    println!("profiles: {}", profiles.len());
    for profile in profiles {
        println!(
            "  {} [{}] {} <{}> rev{} {} credentials={}/{} ",
            terminal_text(&profile.id),
            if profile.editable { "managed" } else { "legacy" },
            terminal_text(&profile.display_name),
            terminal_text(&profile.address),
            profile.revision,
            if profile.enabled { "enabled" } else { "disabled" },
            terminal_text(&profile.credentials.incoming),
            terminal_text(profile.credentials.outgoing.as_deref().unwrap_or("-")),
        );
        println!(
            "    imap {}:{} ({}) state={} {}",
            terminal_text(&profile.incoming.server),
            profile.incoming.port,
            terminal_text(&profile.incoming.security),
            terminal_text(&profile.runtime.state),
            if profile.runtime.detail.is_empty() {
                String::new()
            } else {
                format!("({})", terminal_text(&profile.runtime.detail))
            },
        );
    }
}

fn print_probe(side: &str, outcome: &maild_client::ProbeOutcomeView) {
    println!(
        "{side}: {} ({}) {}",
        if outcome.ok { "ok" } else { "failed" },
        terminal_text(&outcome.stage),
        terminal_text(&outcome.detail),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_is_32_random_bytes_as_hex() {
        let token = generate_token().unwrap();
        assert_eq!(token.len(), 64);
        assert!(token.bytes().all(|byte| byte.is_ascii_hexdigit()));
    }

    #[test]
    fn terminal_text_strips_escape_and_line_controls() {
        assert_eq!(
            terminal_text("ok\u{1b}]52;bad\u{7}\nnext"),
            "ok ]52;bad  next"
        );
    }

    #[test]
    fn terminal_multiline_text_preserves_lines_and_strips_controls() {
        assert_eq!(
            terminal_multiline_text("one\r\ntwo\rthree\n\u{1b}four"),
            "one\ntwo\nthree\n four"
        );
    }

    #[test]
    fn rules_json_requires_an_object() {
        assert_eq!(
            parse_rules_json(r#"{"show_sensitive":false}"#).unwrap()["show_sensitive"],
            false
        );
        assert!(parse_rules_json("[]").is_err());
        assert!(parse_rules_json("not-json").is_err());
    }

    #[test]
    fn unix_utc_hint_preserves_small_synthetic_values_and_formats_real_timestamps() {
        assert_eq!(format_unix_with_utc_hint(0), "0");
        assert_eq!(format_unix_with_utc_hint(1234), "1234");
        assert_eq!(
            format_unix_with_utc_hint(31_536_000),
            "31536000 (1971-01-01T00:00:00Z)"
        );
        assert_eq!(
            format_unix_with_utc_hint(1_795_001_600),
            "1795001600 (2026-11-18T11:33:20Z)"
        );
        assert_eq!(
            format_unix_with_utc_hint(253_402_300_799),
            "253402300799 (9999-12-31T23:59:59Z)"
        );
        assert_eq!(format_unix_with_utc_hint(253_402_300_800), "253402300800");
    }

    #[test]
    fn endpoint_overrides_are_mutually_exclusive() {
        assert!(Cli::try_parse_from([
            maild_brand::CLI_CTL,
            "--url",
            "http://127.0.0.1:7394",
            "--socket",
            "/tmp/api.sock",
            "status",
        ])
        .is_err());
        assert!(Cli::try_parse_from([
            maild_brand::CLI_CTL,
            "--socket",
            "/tmp/api.sock",
            "--pipe",
            r"\\.\pipe\api",
            "status",
        ])
        .is_err());
    }

    #[test]
    fn notification_commands_are_explicit_subcommands() {
        assert!(matches!(
            Cli::try_parse_from([maild_brand::CLI_CTL, "notification-status"])
                .unwrap()
                .command,
            Command::NotificationStatus
        ));
        assert!(matches!(
            Cli::try_parse_from([maild_brand::CLI_CTL, "request-notification-authorization"])
                .unwrap()
                .command,
            Command::RequestNotificationAuthorization
        ));
    }

    #[test]
    fn plugin_catalog_commands_are_explicit_subcommands() {
        assert!(matches!(
            Cli::try_parse_from([maild_brand::CLI_CTL, "frontends"])
                .unwrap()
                .command,
            Command::Frontends
        ));
        assert!(matches!(
            Cli::try_parse_from([maild_brand::CLI_CTL, "plugins"])
                .unwrap()
                .command,
            Command::Plugins
        ));
    }

    #[test]
    fn account_subcommands_parse_with_defaults() {
        let cli = Cli::try_parse_from([
            maild_brand::CLI_CTL, "account", "list",
        ])
        .unwrap();
        assert!(matches!(
            cli.command,
            Command::Account { command: AccountCommand::List { all: false } }
        ));

        let cli = Cli::try_parse_from([
            maild_brand::CLI_CTL,
            "account",
            "add",
            "--display-name",
            "Main",
            "--address",
            "user@example.test",
            "--imap-host",
            "imap.example.test",
            "--imap-username",
            "user@example.test",
        ])
        .unwrap();
        let Command::Account { command: AccountCommand::Add { spec } } = cli.command else {
            panic!("expected add");
        };
        assert_eq!(spec.imap_port, 993);
        assert_eq!(spec.imap_security, "implicit");
        assert_eq!(spec.smtp_credential, "incoming");
        assert!(spec.smtp_host.is_none());
    }

    #[test]
    fn sync_and_discover_subcommands_parse() {
        let cli = Cli::try_parse_from([maild_brand::CLI_CTL, "sync"]).unwrap();
        assert!(matches!(cli.command, Command::Sync { account: None }));
        let cli = Cli::try_parse_from([maild_brand::CLI_CTL, "sync", "--account", "work"]).unwrap();
        let Command::Sync { account } = cli.command else {
            panic!("expected sync");
        };
        assert_eq!(account.as_deref(), Some("work"));

        let cli = Cli::try_parse_from([maild_brand::CLI_CTL, "account", "discover", "a@qq.com"]).unwrap();
        let Command::Account {
            command: AccountCommand::Discover { input, network },
        } = cli.command
        else {
            panic!("expected discover");
        };
        assert_eq!(input, "a@qq.com");
        assert!(!network);
        let cli = Cli::try_parse_from([
            maild_brand::CLI_CTL, "--json", "account", "discover", "example.org", "--network",
        ])
        .unwrap();
        assert!(cli.json);
        assert!(matches!(
            cli.command,
            Command::Account {
                command: AccountCommand::Discover { network: true, .. }
            }
        ));

        let cli = Cli::try_parse_from([
            maild_brand::CLI_CTL, "set-settings", "--rules-json", "{}", "--persist",
        ])
        .unwrap();
        assert!(matches!(
            cli.command,
            Command::SetSettings { persist: true, .. }
        ));
    }

    #[test]
    fn provider_account_subcommands_build_delegated_drafts() {
        let cli = Cli::try_parse_from([
            maild_brand::CLI_CTL,
            "account",
            "provider-test",
            "--display-name",
            "NodeSeek",
            "--address",
            "user@seek.li",
            "--provider",
            "org.nodeseek.email",
        ])
        .unwrap();
        let Command::Account {
            command: AccountCommand::ProviderTest { spec },
        } = cli.command
        else {
            panic!("expected provider-test");
        };
        assert_eq!(spec.poll_secs, 60);
        assert!(!spec.receive_only);
        let draft = collect_provider_draft(&spec).unwrap();
        assert_eq!(
            serde_json::to_value(draft).unwrap(),
            json!({
                "display_name": "NodeSeek",
                "address": "user@seek.li",
                "incoming": {
                    "kind": "plugin",
                    "provider": "org.nodeseek.email",
                    "poll_secs": 60,
                },
                "outgoing": {
                    "kind": "plugin",
                    "provider": "org.nodeseek.email",
                },
            })
        );

        let cli = Cli::try_parse_from([
            maild_brand::CLI_CTL,
            "account",
            "provider-add",
            "--display-name",
            "NodeSeek",
            "--address",
            "user@nodeseek.org",
            "--provider",
            "org.nodeseek.email",
            "--poll-secs",
            "30",
            "--receive-only",
        ])
        .unwrap();
        let Command::Account {
            command: AccountCommand::ProviderAdd { spec },
        } = cli.command
        else {
            panic!("expected provider-add");
        };
        let draft = collect_provider_draft(&spec).unwrap();
        assert_eq!(
            serde_json::to_value(draft).unwrap(),
            json!({
                "display_name": "NodeSeek",
                "address": "user@nodeseek.org",
                "incoming": {
                    "kind": "plugin",
                    "provider": "org.nodeseek.email",
                    "poll_secs": 30,
                },
            })
        );

        assert!(Cli::try_parse_from([
            maild_brand::CLI_CTL,
            "account",
            "provider-add",
            "--display-name",
            "NodeSeek",
            "--address",
            "user@seek.li",
            "--provider",
            "org.nodeseek.email",
            "--poll-secs",
            "0",
        ])
        .is_err());
    }

    #[test]
    fn edit_rejects_remove_smtp_with_new_host() {
        assert!(Cli::try_parse_from([
            maild_brand::CLI_CTL,
            "account",
            "edit",
            "--id",
            "acct_abcdefghijklmnopqrstuvwxyz23",
            "--remove-smtp",
            "--smtp-host",
            "smtp.example.test",
        ])
        .is_err());
    }

    #[test]
    fn json_error_shape_is_stable() {
        let error = AppError::from(ClientError {
            kind: ErrorKind::Rpc,
            message: "service unavailable".into(),
            rpc_code: Some(-32001),
            rpc_data: None,
        });
        // 没有 data 时输出逐字节不变；有 data 时原样透出，脚本据此重试（ADR-0034）。
        let with_data = AppError::from(ClientError {
            kind: ErrorKind::Rpc,
            message: "draft changed elsewhere".into(),
            rpc_code: Some(-32006),
            rpc_data: Some(json!({ "version": 7 })),
        });
        let parsed: Value = serde_json::from_str(&json_error_line(&with_data)).unwrap();
        assert_eq!(parsed["error"]["data"]["version"], 7);
        assert!(with_data.human_message().contains("version=7"));
        let value: Value = serde_json::from_str(&json_error_line(&error)).unwrap();
        assert_eq!(
            value,
            json!({
                "error": {
                    "kind": "rpc",
                    "message": "service unavailable",
                    "rpc_code": -32001
                }
            })
        );
        assert_eq!(error.code, 4);
    }
}
