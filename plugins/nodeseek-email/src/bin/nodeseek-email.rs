use std::io::BufRead as _;

use clap::{Parser, Subcommand};
use maild_client::{discover_endpoint, AccountProfileView, DiscoveryOptions, RpcClient};
use maild_proxy::{CustomProxyConfig, ProxyOverride, ProxyProtocol};
use nodeseek_email::api::ApiClient;
use nodeseek_email::error::{AppError, AppResult, ErrorCode};
use nodeseek_email::proxy::{
    describe_global_proxy, fetch_global_proxy_from_daemon, load_effective_proxy_snapshot,
    PluginProxySettings, PluginProxyStore,
};
use nodeseek_email::secret::{read_api_key, PlatformSecretStore, SecretStore as _};
use nodeseek_email::state::{canonicalize_address, MailboxBinding, StateStore};
use nodeseek_email::PLUGIN_ID;

#[derive(Parser)]
#[command(
    name = "nodeseek-email",
    about = format!("{} community adapter for NodeSeek Email", maild_brand::PRODUCT_NAME)
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Store an API key in macOS Keychain.
    Configure {
        /// Read one API key line from stdin instead of a no-echo TTY prompt.
        #[arg(long)]
        api_key_stdin: bool,
    },
    /// Report whether a valid Keychain item exists (never prints the key).
    KeyStatus,
    /// Remove only the local Keychain item. This never deletes remote mailboxes.
    ClearKey {
        #[arg(long)]
        confirm: bool,
    },
    #[command(subcommand)]
    Mailbox(MailboxCommand),
    /// Show the provider quota fields returned by NodeSeek Email.
    Quota,
    /// View or override proxy settings specifically for NodeSeek Email (default: inherit global proxy).
    #[command(subcommand)]
    Proxy(ProxyCommand),
}

#[derive(Subcommand)]
enum ProxyCommand {
    /// Show this plugin's proxy override and the inherited global Featherpost proxy summary.
    Status,
    /// Configure plugin-level proxy override (`inherit`, `direct`, or `custom`).
    Set {
        /// Override mode: `inherit` (follow global), `direct`, or `custom`.
        #[arg(long)]
        mode: String,
        /// Custom proxy protocol (`socks5h`, `socks5`, `http`, `https`).
        #[arg(long)]
        protocol: Option<String>,
        /// Custom proxy host.
        #[arg(long)]
        host: Option<String>,
        /// Custom proxy port.
        #[arg(long)]
        port: Option<u16>,
        /// Optional custom proxy username.
        #[arg(long)]
        username: Option<String>,
        /// Read one proxy password line from stdin (never passed on argv).
        #[arg(long)]
        password_stdin: bool,
        /// Clear any saved custom proxy password.
        #[arg(long)]
        clear_password: bool,
        /// Comma-separated bypass entries (optional).
        #[arg(long)]
        bypass: Option<String>,
    },
    /// Reset plugin proxy override back to `inherit` (follow global proxy).
    Reset,
}

#[derive(Subcommand)]
enum MailboxCommand {
    List,
    Check {
        address: String,
    },
    /// Create exactly one remote mailbox. No automatic retry is performed.
    Create {
        address: String,
        #[arg(long)]
        confirm: bool,
    },
    /// Persist an existing remote mailbox -> local account mapping.
    Import {
        account: String,
        address: String,
    },
    /// Remove only a local binding after the local account has been archived.
    Unimport {
        account: String,
        address: String,
        #[arg(long)]
        confirm: bool,
    },
}

#[tokio::main]
async fn main() -> std::process::ExitCode {
    match execute(Cli::parse()).await {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("nodeseek-email: {error}");
            std::process::ExitCode::FAILURE
        }
    }
}

async fn execute(cli: Cli) -> AppResult<()> {
    match cli.command {
        Command::Configure { api_key_stdin } => {
            let secret = read_api_key(api_key_stdin)?;
            PlatformSecretStore.save(secret)?;
            println!("NodeSeek Email API key 已保存到 macOS Keychain");
        }
        Command::KeyStatus => {
            println!(
                "{}",
                if PlatformSecretStore.exists()? {
                    "configured"
                } else {
                    "missing"
                }
            );
        }
        Command::ClearKey { confirm } => {
            require_confirmation(confirm)?;
            PlatformSecretStore.delete()?;
            println!("NodeSeek Email API key 已从 macOS Keychain 删除；远端邮箱未变更");
        }
        Command::Mailbox(MailboxCommand::Unimport {
            account,
            address,
            confirm,
        }) => {
            require_unimport_confirmation(confirm)?;
            let paths = managed_unimport_paths()?;
            verify_archived_account(&paths, &account).await?;
            let state = StateStore::at_root(paths.plugin_data_dir.join(PLUGIN_ID))?;
            let binding = state.binding(&account)?;
            validate_unimport_binding(&binding, &address)?;
            let binding = state.unimport_mailbox(&account, &address)?;
            println!(
                "unimported\taccount={}\tmaild_validation=archived_account_verified\tremote_mailbox_unchanged=true\tsynthetic_uid_state_deleted=true",
                binding.account
            );
        }
        Command::Mailbox(command) => {
            let proxy = load_effective_proxy_snapshot().await;
            let api = ApiClient::production_with_proxy(PlatformSecretStore.load()?, proxy)?;
            match command {
                MailboxCommand::List => {
                    for mailbox in api.list_mailboxes().await? {
                        println!(
                            "{}\t{}\tstatus={}\tsend_disabled={}",
                            mailbox.id,
                            mailbox.address,
                            optional_i64(mailbox.status),
                            optional_bool(mailbox.send_disabled)
                        );
                    }
                }
                MailboxCommand::Check { address } => {
                    println!("available={}", api.check_mailbox(&address).await?);
                }
                MailboxCommand::Create { address, confirm } => {
                    require_confirmation(confirm)?;
                    let outcome = api.create_mailbox(&address).await?;
                    println!(
                        "created={}\tid={}\taddress={}",
                        !outcome.reconciled, outcome.mailbox.id, outcome.mailbox.address
                    );
                }
                MailboxCommand::Import { account, address } => {
                    let mailbox = api.resolve_mailbox(&address).await?;
                    let paths = managed_cli_paths(false)?;
                    verify_import_account(&paths, &account, &mailbox.address).await?;
                    let state = StateStore::at_root(paths.plugin_data_dir.join(PLUGIN_ID))?;
                    let binding = state.import_mailbox(&account, &mailbox.address, mailbox.id)?;
                    println!(
                        "imported\taccount={}\tid={}\taddress={}",
                        binding.account, binding.mailbox_id, binding.address
                    );
                }
                MailboxCommand::Unimport { .. } => unreachable!("unimport handled without API key"),
            }
        }
        Command::Quota => {
            let proxy = load_effective_proxy_snapshot().await;
            let api = ApiClient::production_with_proxy(PlatformSecretStore.load()?, proxy)?;
            let quota = api.quota().await?;
            let current = quota
                .current
                .as_ref()
                .map(serde_json::to_string)
                .transpose()
                .map_err(|_| AppError::new(ErrorCode::InvalidResponse, "配额字段无法编码"))?
                .unwrap_or_else(|| "-".into());
            println!(
                "current={}\tmailboxes={}\tstorage_used={}\tstorage_quota={}\tusage_percent={}\tquota_source={}",
                current,
                optional_i64(quota.mailbox_count),
                optional_i64(quota.storage_used),
                optional_i64(quota.storage_quota),
                optional_i64(quota.storage_usage_percent),
                quota.storage_quota_source.as_deref().unwrap_or("-")
            );
        }
        Command::Proxy(command) => execute_proxy_command(command).await?,
    }
    Ok(())
}

async fn execute_proxy_command(command: ProxyCommand) -> AppResult<()> {
    let store = PluginProxyStore::discover()?;
    match command {
        ProxyCommand::Status => {
            let settings = store.load()?;
            let (global_cfg, global_sys, daemon_online) = fetch_global_proxy_from_daemon().await;
            let plugin_desc = match &settings.proxy_override {
                ProxyOverride::Inherit => "inherit".to_string(),
                ProxyOverride::Direct => "direct".to_string(),
                ProxyOverride::Custom(cfg) => format!("custom ({})", cfg.to_endpoint().redacted_url()),
            };
            println!(
                "plugin_override={}\thas_password={}\tglobal={}\tdaemon_online={}",
                plugin_desc,
                settings.has_password(),
                describe_global_proxy(&global_cfg, &global_sys),
                daemon_online
            );
        }
        ProxyCommand::Reset => {
            let existing = store.load()?;
            let next = PluginProxySettings {
                proxy_override: ProxyOverride::Inherit,
                saved_custom: existing.effective_custom().cloned(),
            };
            store.save(&next)?;
            println!("override=inherit");
        }
        ProxyCommand::Set {
            mode,
            protocol,
            host,
            port,
            username,
            password_stdin,
            clear_password,
            bypass,
        } => {
            let existing = store.load()?;
            if password_stdin && clear_password {
                return Err(AppError::new(
                    ErrorCode::InvalidInput,
                    "--password-stdin 与 --clear-password 不能同时使用",
                ));
            }
            let normalized = mode.trim().to_ascii_lowercase();
            let next = match normalized.as_str() {
                "inherit" => PluginProxySettings {
                    proxy_override: ProxyOverride::Inherit,
                    saved_custom: existing.effective_custom().cloned(),
                },
                "direct" => PluginProxySettings {
                    proxy_override: ProxyOverride::Direct,
                    saved_custom: existing.effective_custom().cloned(),
                },
                "custom" => {
                    let prev = existing.effective_custom();
                    let proto = match protocol.as_deref() {
                        Some(raw) => ProxyProtocol::parse_scheme(raw).map_err(|detail| {
                            AppError::with_detail(ErrorCode::InvalidInput, "代理协议无效", detail)
                        })?,
                        None => prev.map(|c| c.protocol).unwrap_or(ProxyProtocol::Socks5h),
                    };
                    let host = host
                        .or_else(|| prev.map(|c| c.host.clone()))
                        .ok_or_else(|| {
                            AppError::new(ErrorCode::InvalidInput, "custom 模式必须指定 --host")
                        })?;
                    let port = port
                        .or_else(|| prev.map(|c| c.port))
                        .ok_or_else(|| {
                            AppError::new(ErrorCode::InvalidInput, "custom 模式必须指定 --port")
                        })?;
                    let username = username.or_else(|| prev.and_then(|c| c.username.clone()));
                    let password = if clear_password {
                        None
                    } else if password_stdin {
                        let mut line = String::new();
                        std::io::stdin()
                            .lock()
                            .read_line(&mut line)
                            .map_err(|_| AppError::invalid_input())?;
                        let trimmed = line.trim_end_matches(['\r', '\n']);
                        if trimmed.is_empty() {
                            None
                        } else {
                            Some(trimmed.to_string())
                        }
                    } else {
                        prev.and_then(|c| c.password.clone())
                    };
                    let bypass_list = match bypass {
                        Some(raw) => raw
                            .split([',', ';'])
                            .map(str::trim)
                            .filter(|s| !s.is_empty())
                            .map(ToOwned::to_owned)
                            .collect(),
                        None => prev.map(|c| c.bypass.clone()).unwrap_or_default(),
                    };
                    let custom = CustomProxyConfig {
                        protocol: proto,
                        host,
                        port,
                        username,
                        password,
                        bypass: bypass_list,
                    };
                    PluginProxySettings::from_override(ProxyOverride::Custom(custom))
                }
                _ => {
                    return Err(AppError::new(
                        ErrorCode::InvalidInput,
                        "mode 仅支持 inherit / direct / custom",
                    ));
                }
            };
            let saved = store.save(&next)?;
            match &saved.proxy_override {
                ProxyOverride::Inherit => println!("override=inherit"),
                ProxyOverride::Direct => println!("override=direct"),
                ProxyOverride::Custom(cfg) => {
                    println!(
                        "override=custom\tendpoint={}\thas_password={}",
                        cfg.to_endpoint().redacted_url(),
                        cfg.has_password()
                    );
                }
            }
        }
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct UnimportAccountFacts<'a> {
    id: &'a str,
    archived: bool,
}

impl<'a> From<&'a AccountProfileView> for UnimportAccountFacts<'a> {
    fn from(profile: &'a AccountProfileView) -> Self {
        Self {
            id: &profile.id,
            archived: profile.archived_at.is_some(),
        }
    }
}

const UNIMPORT_FORBIDDEN_ENV: [&str; 6] = [
    maild_paths::CONFIG_ENV,
    maild_paths::PLUGIN_DATA_ENV,
    maild_client::API_URL_ENV,
    maild_client::API_SOCKET_ENV,
    maild_client::API_PIPE_ENV,
    maild_client::API_TOKEN_ENV,
];

fn managed_unimport_paths() -> AppResult<maild_paths::AppPaths> {
    managed_cli_paths(true)
}

fn managed_cli_paths(unimport: bool) -> AppResult<maild_paths::AppPaths> {
    if first_forbidden_unimport_override(|name| std::env::var_os(name).is_some()).is_some() {
        return Err(AppError::new(
            ErrorCode::InvalidInput,
            if unimport {
                "unimport 不接受配置、API endpoint 或插件数据目录环境覆盖；未修改任何状态"
            } else {
                "import 不接受配置、API endpoint 或插件数据目录环境覆盖；未修改任何状态"
            },
        ));
    }
    maild_paths::AppPaths::discover().map_err(|_| {
        AppError::new(
            ErrorCode::State,
            "无法解析平台受管路径；未清理任何状态",
        )
    })
}

async fn verify_import_account(
    paths: &maild_paths::AppPaths,
    account: &str,
    address: &str,
) -> AppResult<()> {
    let profiles = managed_client(paths)?
        .list_account_profiles(true)
        .await
        .map_err(|_| managed_daemon_error())?;
    validate_import_account(&profiles.profiles, account, address)
}

fn validate_import_account(
    profiles: &[AccountProfileView],
    account: &str,
    address: &str,
) -> AppResult<()> {
    let mut matches = profiles.iter().filter(|profile| profile.id == account);
    let profile = matches.next().ok_or_else(|| {
        AppError::new(ErrorCode::NotFound, "本地邮件后台中不存在指定的精确账户 ID")
    })?;
    if matches.next().is_some() {
        return Err(AppError::new(ErrorCode::Conflict, "本地邮件后台返回了重复账户 ID"));
    }
    if profile.archived_at.is_some()
        || profile.address != canonicalize_address(address)?
        || profile.incoming.kind != "plugin"
        || profile.incoming.provider.as_deref() != Some(PLUGIN_ID)
    {
        return Err(AppError::new(
            ErrorCode::Conflict,
            "本地账户必须未归档、地址完全匹配且收信 provider 为 NodeSeek Email",
        ));
    }
    Ok(())
}

fn managed_client(paths: &maild_paths::AppPaths) -> AppResult<RpcClient> {
    let endpoint = discover_endpoint(&DiscoveryOptions {
        config_path: Some(paths.config_file.clone()),
        ..DiscoveryOptions::default()
    })
    .map_err(|_| managed_daemon_error())?;
    RpcClient::new(endpoint).map_err(|_| managed_daemon_error())
}

fn managed_daemon_error() -> AppError {
    AppError::new(
        ErrorCode::Network,
        "无法通过平台受管配置连接并验证本地邮件后台；未修改任何状态",
    )
}

fn first_forbidden_unimport_override(mut is_set: impl FnMut(&str) -> bool) -> Option<&'static str> {
    UNIMPORT_FORBIDDEN_ENV
        .iter()
        .copied()
        .find(|name| is_set(name))
}

async fn verify_archived_account(paths: &maild_paths::AppPaths, account: &str) -> AppResult<()> {
    let result = managed_client(paths)?
        .list_account_profiles(true)
        .await
        .map_err(|_| unimport_daemon_error())?;
    let accounts: Vec<_> = result
        .profiles
        .iter()
        .map(UnimportAccountFacts::from)
        .collect();
    validate_unimport_account(&accounts, account)
}

fn validate_unimport_account(
    accounts: &[UnimportAccountFacts<'_>],
    account: &str,
) -> AppResult<()> {
    let mut matches = accounts.iter().filter(|profile| profile.id == account);
    let profile = matches.next().ok_or_else(|| {
        AppError::new(
            ErrorCode::NotFound,
            "本地邮件后台中不存在指定的精确账户 ID；未清理任何状态",
        )
    })?;
    if matches.next().is_some() {
        return Err(AppError::new(
            ErrorCode::Conflict,
            "本地邮件后台返回了重复账户 ID；未清理任何状态",
        ));
    }
    if !profile.archived {
        return Err(AppError::new(
            ErrorCode::Conflict,
            "对应本地账户尚未归档；未清理任何状态",
        ));
    }
    Ok(())
}

fn validate_unimport_binding(binding: &MailboxBinding, address: &str) -> AppResult<()> {
    if binding.address == canonicalize_address(address)? {
        Ok(())
    } else {
        Err(AppError::new(
            ErrorCode::Conflict,
            "持久 NodeSeek binding 与待解除地址不一致；未清理任何状态",
        ))
    }
}

fn unimport_daemon_error() -> AppError {
    AppError::new(
        ErrorCode::Network,
        "无法通过平台受管配置连接并验证本地邮件后台；未清理任何状态",
    )
}

fn require_confirmation(confirm: bool) -> AppResult<()> {
    if confirm {
        Ok(())
    } else {
        Err(AppError::new(
            ErrorCode::InvalidInput,
            "远端或凭据变更需要显式 --confirm",
        ))
    }
}

fn require_unimport_confirmation(confirm: bool) -> AppResult<()> {
    if confirm {
        Ok(())
    } else {
        Err(AppError::new(
            ErrorCode::InvalidInput,
            "必须先归档对应本地账户，再用 --confirm 清理本地绑定和游标状态",
        ))
    }
}

fn optional_i64(value: Option<i64>) -> String {
    value
        .map(|value| value.to_string())
        .unwrap_or_else(|| "-".into())
}

fn optional_bool(value: Option<bool>) -> &'static str {
    match value {
        Some(true) => "true",
        Some(false) => "false",
        None => "-",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory as _;

    #[test]
    fn cli_has_no_argv_api_key_option() {
        let command = Cli::command();
        command.clone().debug_assert();
        let configure = command.find_subcommand("configure").unwrap();
        let long_options: Vec<_> = configure
            .get_arguments()
            .filter_map(|arg| arg.get_long())
            .collect();
        assert!(long_options.contains(&"api-key-stdin"));
        assert!(!long_options.contains(&"api-key"));

        let proxy = command.find_subcommand("proxy").unwrap();
        let proxy_set = proxy.find_subcommand("set").unwrap();
        let proxy_opts: Vec<_> = proxy_set
            .get_arguments()
            .filter_map(|arg| arg.get_long())
            .collect();
        assert!(proxy_opts.contains(&"password-stdin"));
        assert!(!proxy_opts.contains(&"password"));
    }

    #[test]
    fn destructive_local_and_remote_actions_require_confirmation() {
        assert!(require_confirmation(false).is_err());
        assert!(require_confirmation(true).is_ok());
        assert!(require_unimport_confirmation(false).is_err());
        assert!(require_unimport_confirmation(true).is_ok());
    }

    #[test]
    fn unimport_cli_is_explicit_and_has_no_api_key_argument() {
        let cli = Cli::try_parse_from([
            "nodeseek-email",
            "mailbox",
            "unimport",
            "account-old",
            "box@seek.li",
            "--confirm",
        ])
        .unwrap();
        assert!(matches!(
            cli.command,
            Command::Mailbox(MailboxCommand::Unimport {
                account,
                address,
                confirm: true,
            }) if account == "account-old" && address == "box@seek.li"
        ));
        // The previous offline bypass was intentionally removed: destructive validation always
        // requires the live managed daemon, so this must stay an unknown option.
        assert!(Cli::try_parse_from([
            "nodeseek-email",
            "mailbox",
            "unimport",
            "account-old",
            "box@seek.li",
            "--offline-recovery",
            "--confirm",
        ])
        .is_err());
    }

    fn archived_account() -> UnimportAccountFacts<'static> {
        UnimportAccountFacts {
            id: "account-old",
            archived: true,
        }
    }

    #[test]
    fn unimport_guard_accepts_only_the_exact_archived_account() {
        let profile = archived_account();
        assert!(validate_unimport_account(&[profile], "account-old").is_ok());
        let error = validate_unimport_account(&[profile], "ACCOUNT-OLD").unwrap_err();
        assert_eq!(error.code(), ErrorCode::NotFound);

        let mut profile = archived_account();
        profile.archived = false;
        assert_eq!(
            validate_unimport_account(&[profile], "account-old")
                .unwrap_err()
                .code(),
            ErrorCode::Conflict
        );
    }

    #[test]
    fn unimport_binding_not_current_profile_address_is_authoritative() {
        let binding = MailboxBinding {
            account: "account-old".into(),
            address: "CaseSensitive@seek.li".into(),
            mailbox_id: 41,
        };
        assert!(validate_unimport_binding(&binding, "CaseSensitive@SEEK.LI").is_ok());
        assert_eq!(
            validate_unimport_binding(&binding, "casesensitive@seek.li")
                .unwrap_err()
                .code(),
            ErrorCode::Conflict
        );
    }

    #[test]
    fn unimport_guard_rejects_duplicate_ids_and_environment_overrides() {
        let profile = archived_account();
        assert_eq!(
            validate_unimport_account(&[profile, profile], "account-old")
                .unwrap_err()
                .code(),
            ErrorCode::Conflict
        );
        assert_eq!(
            first_forbidden_unimport_override(|name| name == maild_client::API_URL_ENV),
            Some(maild_client::API_URL_ENV)
        );
        assert_eq!(first_forbidden_unimport_override(|_| false), None);
    }

    #[test]
    fn import_guard_requires_live_exact_nodeseek_account() {
        let profile: AccountProfileView = serde_json::from_value(serde_json::json!({
            "id": "acct-node",
            "revision": 1,
            "display_name": "NodeSeek",
            "address": "CaseSensitive@seek.li",
            "enabled": true,
            "source": "managed",
            "editable": true,
            "incoming": { "kind": "plugin", "provider": PLUGIN_ID, "poll_secs": 60 },
            "outgoing": { "kind": "plugin", "provider": PLUGIN_ID },
            "credentials": { "incoming": "shared" },
            "runtime": { "state": "online", "detail": "" }
        }))
        .unwrap();
        assert!(validate_import_account(
            std::slice::from_ref(&profile),
            "acct-node",
            "CaseSensitive@SEEK.LI"
        )
        .is_ok());
        assert_eq!(
            validate_import_account(&[profile], "acct-node", "other@seek.li")
                .unwrap_err()
                .code(),
            ErrorCode::Conflict
        );
    }
}
