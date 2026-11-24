//! Native management UI for the NodeSeek Email provider.

use std::fs;
use std::sync::mpsc;

use eframe::egui::{
    self, Align, Color32, CornerRadius, FontData, FontFamily, Frame, Layout, Margin, Modal,
    RichText, ScrollArea, Stroke, TextEdit, Vec2,
};
use maild_client::{
    discover_endpoint, AccountDraftInput, ClientError, DiscoveryOptions, ErrorKind, IncomingInput,
    OutgoingInput, RpcClient,
};
use maild_proxy::{CustomProxyConfig, ProxyOverride, ProxyProtocol};
use tokio::runtime::{Builder, Runtime};
use tokio::sync::mpsc as async_mpsc;

use crate::api::{ApiClient, Mailbox, Quota};
use crate::error::{AppError, ErrorCode};
use crate::proxy::{
    describe_global_proxy, fetch_global_proxy_from_daemon, load_effective_proxy_snapshot,
    PluginProxySettings, PluginProxyStore,
};
use crate::secret::{PlatformSecretStore, SecretBuffer, SecretStore as _};
use crate::state::{canonicalize_address, StateStore};
use crate::PLUGIN_ID;

const CANVAS: Color32 = Color32::from_rgb(244, 243, 238);
const SIDEBAR: Color32 = Color32::from_rgb(235, 233, 226);
const SURFACE: Color32 = Color32::from_rgb(250, 249, 246);
const SURFACE_ACTIVE: Color32 = Color32::from_rgb(223, 236, 232);
const TEXT: Color32 = Color32::from_rgb(30, 37, 35);
const MUTED: Color32 = Color32::from_rgb(100, 109, 105);
const BORDER: Color32 = Color32::from_rgb(213, 211, 202);
const ACCENT: Color32 = Color32::from_rgb(22, 112, 103);
const WARNING: Color32 = Color32::from_rgb(157, 96, 35);
const DANGER: Color32 = Color32::from_rgb(174, 65, 53);
const MAX_NOTICE_CHARS: usize = 240;
const DOMAINS: [&str; 2] = ["seek.li", "nodeseek.org"];
const PROXY_PROTOCOLS: [ProxyProtocol; 4] = [
    ProxyProtocol::Socks5h,
    ProxyProtocol::Socks5,
    ProxyProtocol::Http,
    ProxyProtocol::Https,
];

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum ProxyModeChoice {
    #[default]
    Inherit,
    Direct,
    Custom,
}

#[derive(Clone)]
struct ProxyUiSnapshot {
    settings: PluginProxySettings,
    global_summary: String,
}

#[derive(Clone)]
struct Snapshot {
    mailboxes: Vec<Mailbox>,
    quota: Quota,
}

enum Job {
    InspectKey {
        generation: u64,
    },
    SaveKey {
        generation: u64,
        secret: SecretBuffer,
    },
    ClearKey {
        generation: u64,
    },
    SaveProxy {
        generation: u64,
        settings: PluginProxySettings,
    },
    Refresh {
        generation: u64,
    },
    Check {
        generation: u64,
        address: String,
    },
    Create {
        generation: u64,
        address: String,
    },
    Bind {
        generation: u64,
        mailbox: Mailbox,
    },
}

enum Outcome {
    Key {
        generation: u64,
        result: Result<bool, AppError>,
        proxy: Option<ProxyUiSnapshot>,
    },
    Saved {
        generation: u64,
        result: Result<(), AppError>,
    },
    Cleared {
        generation: u64,
        result: Result<(), AppError>,
    },
    ProxySaved {
        generation: u64,
        result: Result<ProxyUiSnapshot, AppError>,
    },
    Refreshed {
        generation: u64,
        result: Result<Snapshot, AppError>,
    },
    Checked {
        generation: u64,
        address: String,
        result: Result<bool, AppError>,
    },
    Created {
        generation: u64,
        address: String,
        result: Result<Mailbox, AppError>,
    },
    Bound {
        generation: u64,
        address: String,
        result: Result<String, AppError>,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum Confirmation {
    ClearKey,
    Create(String),
}

#[derive(Default)]
struct UiState {
    generation: u64,
    busy: bool,
    key_configured: Option<bool>,
    api_key: String,
    proxy_mode: ProxyModeChoice,
    proxy_protocol: usize,
    proxy_host: String,
    proxy_port: String,
    proxy_username: String,
    proxy_password: String,
    proxy_has_saved_password: bool,
    proxy_clear_password: bool,
    proxy_bypass: String,
    proxy_saved_custom: Option<CustomProxyConfig>,
    global_proxy_summary: Option<String>,
    local_part: String,
    domain: usize,
    checked: Option<(String, bool)>,
    confirmation: Option<Confirmation>,
    mailboxes: Vec<Mailbox>,
    quota: Option<Quota>,
    notice: Option<(String, Color32)>,
}

impl UiState {
    fn next_generation(&mut self) -> u64 {
        self.generation = self.generation.saturating_add(1);
        self.busy = true;
        self.notice = None;
        self.generation
    }

    fn apply_proxy_snapshot(&mut self, snapshot: ProxyUiSnapshot) {
        self.global_proxy_summary = Some(snapshot.global_summary);
        self.proxy_has_saved_password = snapshot.settings.has_password();
        self.proxy_clear_password = false;
        self.proxy_password.clear();
        self.proxy_saved_custom = snapshot.settings.effective_custom().cloned();
        self.proxy_mode = match &snapshot.settings.proxy_override {
            ProxyOverride::Inherit => ProxyModeChoice::Inherit,
            ProxyOverride::Direct => ProxyModeChoice::Direct,
            ProxyOverride::Custom(_) => ProxyModeChoice::Custom,
        };
        if let Some(custom) = snapshot.settings.effective_custom() {
            self.proxy_protocol = PROXY_PROTOCOLS
                .iter()
                .position(|p| *p == custom.protocol)
                .unwrap_or(0);
            self.proxy_host = custom.host.clone();
            self.proxy_port = custom.port.to_string();
            self.proxy_username = custom.username.clone().unwrap_or_default();
            self.proxy_bypass = custom.bypass.join(", ");
        }
    }

    fn build_proxy_settings(&self) -> Result<PluginProxySettings, AppError> {
        let preserved = self.proxy_saved_custom.clone();
        match self.proxy_mode {
            ProxyModeChoice::Inherit => Ok(PluginProxySettings {
                proxy_override: ProxyOverride::Inherit,
                saved_custom: preserved,
            }),
            ProxyModeChoice::Direct => Ok(PluginProxySettings {
                proxy_override: ProxyOverride::Direct,
                saved_custom: preserved,
            }),
            ProxyModeChoice::Custom => {
                let port: u16 = self.proxy_port.trim().parse().map_err(|_| {
                    AppError::new(ErrorCode::InvalidInput, "代理端口必须是 1..=65535 的整数")
                })?;
                let password = if self.proxy_clear_password {
                    None
                } else if !self.proxy_password.is_empty() {
                    Some(self.proxy_password.clone())
                } else {
                    preserved.as_ref().and_then(|c| c.password.clone())
                };
                let bypass: Vec<String> = self
                    .proxy_bypass
                    .split([',', ';', '\n'])
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(ToOwned::to_owned)
                    .collect();
                let custom = CustomProxyConfig {
                    protocol: PROXY_PROTOCOLS[self.proxy_protocol],
                    host: self.proxy_host.trim().to_string(),
                    port,
                    username: {
                        let u = self.proxy_username.trim();
                        (!u.is_empty()).then(|| u.to_string())
                    },
                    password,
                    bypass,
                };
                PluginProxySettings::from_override(ProxyOverride::Custom(custom)).validate()
            }
        }
    }

    fn address(&self) -> Result<String, AppError> {
        canonicalize_address(&format!(
            "{}@{}",
            self.local_part.trim(),
            DOMAINS[self.domain]
        ))
    }

    fn apply(&mut self, outcome: Outcome) -> Option<Job> {
        let generation = match &outcome {
            Outcome::Key { generation, .. }
            | Outcome::Saved { generation, .. }
            | Outcome::Cleared { generation, .. }
            | Outcome::ProxySaved { generation, .. }
            | Outcome::Refreshed { generation, .. }
            | Outcome::Checked { generation, .. }
            | Outcome::Created { generation, .. }
            | Outcome::Bound { generation, .. } => *generation,
        };
        if generation != self.generation {
            return None;
        }
        self.busy = false;
        match outcome {
            Outcome::Key { result, proxy, .. } => {
                if let Some(snapshot) = proxy {
                    self.apply_proxy_snapshot(snapshot);
                }
                match result {
                    Ok(configured) => {
                        self.key_configured = Some(configured);
                        if configured {
                            return Some(Job::Refresh {
                                generation: self.next_generation(),
                            });
                        }
                    }
                    Err(error) => self.fail(error),
                }
            }
            Outcome::Saved { result, .. } => match result {
                Ok(()) => {
                    self.key_configured = Some(true);
                    self.notice = Some(("API Key 已保存到 macOS Keychain".into(), ACCENT));
                    return Some(Job::Refresh {
                        generation: self.next_generation(),
                    });
                }
                Err(error) => self.fail(error),
            },
            Outcome::Cleared { result, .. } => match result {
                Ok(()) => {
                    self.key_configured = Some(false);
                    self.mailboxes.clear();
                    self.quota = None;
                    self.checked = None;
                    self.notice = Some(("本地 API Key 已清除，远端邮箱未变更".into(), ACCENT));
                }
                Err(error) => self.fail(error),
            },
            Outcome::ProxySaved { result, .. } => match result {
                Ok(snapshot) => {
                    self.apply_proxy_snapshot(snapshot);
                    self.notice = Some(("NodeSeek Email 代理设置已保存并生效".into(), ACCENT));
                }
                Err(error) => self.fail(error),
            },
            Outcome::Refreshed { result, .. } => match result {
                Ok(snapshot) => {
                    self.mailboxes = snapshot.mailboxes;
                    self.quota = Some(snapshot.quota);
                    self.notice = Some(("NodeSeek Email 状态已刷新".into(), ACCENT));
                }
                Err(error) => self.fail(error),
            },
            Outcome::Checked {
                address, result, ..
            } => match result {
                Ok(available) => {
                    self.checked = Some((address, available));
                }
                Err(error) => self.fail(error),
            },
            Outcome::Created {
                address, result, ..
            } => match result {
                Ok(mailbox) => {
                    self.notice = Some((format!("已创建 {address}"), ACCENT));
                    self.checked = None;
                    if !self.mailboxes.iter().any(|item| item.id == mailbox.id) {
                        self.mailboxes.push(mailbox);
                    }
                    return Some(Job::Refresh {
                        generation: self.next_generation(),
                    });
                }
                Err(error) => self.fail(error),
            },
            Outcome::Bound {
                address, result, ..
            } => match result {
                Ok(account) => {
                    self.notice = Some((format!("{address} 已接入 {}，账户 {account}", maild_brand::PRODUCT_NAME), ACCENT));
                }
                Err(error) => self.fail(error),
            },
        }
        None
    }

    fn fail(&mut self, error: AppError) {
        if matches!(
            error.code(),
            ErrorCode::CredentialMissing | ErrorCode::Authentication
        ) {
            self.key_configured = Some(false);
        }
        self.notice = Some((preview(&error.to_string()), DANGER));
    }
}

pub struct NodeSeekEmailUi {
    jobs: Option<async_mpsc::UnboundedSender<Job>>,
    outcomes: mpsc::Receiver<Outcome>,
    runtime: Option<Runtime>,
    state: UiState,
}

impl NodeSeekEmailUi {
    fn new(creation: &eframe::CreationContext<'_>) -> Self {
        install_theme(&creation.egui_ctx);
        install_cjk_font(&creation.egui_ctx);
        let runtime = Builder::new_multi_thread()
            .worker_threads(1)
            .thread_name("nodeseek-email-ui-runtime")
            .enable_all()
            .build()
            .expect("NodeSeek Email UI runtime");
        let (job_tx, mut job_rx) = async_mpsc::unbounded_channel();
        let (outcome_tx, outcome_rx) = mpsc::channel();
        let repaint = creation.egui_ctx.clone();
        runtime.spawn(async move {
            while let Some(job) = job_rx.recv().await {
                let outcome = run_job(job).await;
                if outcome_tx.send(outcome).is_err() {
                    break;
                }
                repaint.request_repaint();
            }
        });
        let mut state = UiState::default();
        let generation = state.next_generation();
        let _ = job_tx.send(Job::InspectKey { generation });
        Self {
            jobs: Some(job_tx),
            outcomes: outcome_rx,
            runtime: Some(runtime),
            state,
        }
    }

    fn submit(&mut self, job: Job) {
        if self
            .jobs
            .as_ref()
            .is_none_or(|jobs| jobs.send(job).is_err())
        {
            self.state.busy = false;
            self.state.notice = Some(("插件后台任务已停止".into(), DANGER));
        }
    }

    fn refresh(&mut self) {
        let generation = self.state.next_generation();
        self.submit(Job::Refresh { generation });
    }

    fn save_key(&mut self) {
        let bytes = std::mem::take(&mut self.state.api_key).into_bytes();
        match SecretBuffer::from_bytes(bytes) {
            Ok(secret) => {
                let generation = self.state.next_generation();
                self.submit(Job::SaveKey { generation, secret });
            }
            Err(error) => self.state.fail(error),
        }
    }

    fn render(&mut self, ui: &mut egui::Ui) {
        while let Ok(outcome) = self.outcomes.try_recv() {
            if let Some(job) = self.state.apply(outcome) {
                self.submit(job);
            }
        }
        Frame::central_panel(ui.style())
            .fill(CANVAS)
            .show(ui, |ui| {
                let width = ui.available_width();
                ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        let content_width = (width - 32.0).clamp(0.0, 900.0);
                        ui.allocate_ui_with_layout(
                            Vec2::new(content_width, ui.available_height()),
                            Layout::top_down(Align::Min),
                            |ui| self.render_content(ui),
                        );
                    });
            });
        self.render_confirmation(ui.ctx());
    }

    fn render_content(&mut self, ui: &mut egui::Ui) {
        ui.add_space(16.0);
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.label(
                    RichText::new("NodeSeek Email")
                        .size(24.0)
                        .strong()
                        .color(TEXT),
                );
                ui.label(RichText::new(format!("{} 社区适配器", maild_brand::PRODUCT_NAME)).size(12.5).color(MUTED));
            });
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui
                    .add_enabled(
                        !self.state.busy && self.state.key_configured == Some(true),
                        egui::Button::new(if self.state.busy {
                            "处理中"
                        } else {
                            "刷新"
                        }),
                    )
                    .on_hover_text("刷新配额与邮箱列表")
                    .clicked()
                {
                    self.refresh();
                }
                status_dot(ui, self.state.key_configured);
            });
        });
        ui.add_space(12.0);

        if let Some((notice, color)) = &self.state.notice {
            Frame::new()
                .fill(color.gamma_multiply(0.10))
                .corner_radius(CornerRadius::same(8))
                .inner_margin(Margin::symmetric(12, 8))
                .show(ui, |ui| {
                    ui.label(RichText::new(notice).color(*color).size(13.0));
                });
            ui.add_space(10.0);
        }

        self.render_credentials(ui);
        ui.add_space(16.0);
        self.render_proxy_settings(ui);
        if self.state.key_configured != Some(true) {
            return;
        }
        ui.add_space(16.0);
        self.render_quota(ui);
        ui.add_space(16.0);
        self.render_creator(ui);
        ui.add_space(16.0);
        self.render_mailboxes(ui);
        ui.add_space(20.0);
    }

    fn save_proxy(&mut self) {
        match self.state.build_proxy_settings() {
            Ok(settings) => {
                self.state.proxy_password.clear();
                let generation = self.state.next_generation();
                self.submit(Job::SaveProxy {
                    generation,
                    settings,
                });
            }
            Err(error) => self.state.fail(error),
        }
    }

    fn render_proxy_settings(&mut self, ui: &mut egui::Ui) {
        section_header(
            ui,
            "网络代理",
            &format!(
                "默认跟随 {} 全局代理；也可仅为 NodeSeek Email 单独覆盖",
                maild_brand::PRODUCT_NAME
            ),
        );
        ui.horizontal_wrapped(|ui| {
            ui.selectable_value(
                &mut self.state.proxy_mode,
                ProxyModeChoice::Inherit,
                "跟随全局代理",
            );
            ui.selectable_value(
                &mut self.state.proxy_mode,
                ProxyModeChoice::Direct,
                "直连（不使用代理）",
            );
            ui.selectable_value(
                &mut self.state.proxy_mode,
                ProxyModeChoice::Custom,
                "独立自定义代理",
            );
        });
        if self.state.proxy_mode == ProxyModeChoice::Inherit {
            let summary = self
                .state
                .global_proxy_summary
                .as_deref()
                .unwrap_or("将自动继承全局代理配置与实时变更");
            ui.label(RichText::new(summary).size(12.5).color(MUTED));
        }
        if self.state.proxy_mode == ProxyModeChoice::Custom {
            ui.add_space(6.0);
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new("协议").color(MUTED));
                for (index, protocol) in PROXY_PROTOCOLS.iter().enumerate() {
                    ui.selectable_value(&mut self.state.proxy_protocol, index, protocol.as_str());
                }
            });
            ui.horizontal_wrapped(|ui| {
                let host_width = (ui.available_width() - 120.0).clamp(160.0, 380.0);
                ui.add_sized(
                    [host_width, 36.0],
                    TextEdit::singleline(&mut self.state.proxy_host)
                        .hint_text("代理主机（如 127.0.0.1）"),
                );
                ui.add_sized(
                    [100.0, 36.0],
                    TextEdit::singleline(&mut self.state.proxy_port).hint_text("端口（7890）"),
                );
            });
            ui.horizontal_wrapped(|ui| {
                let field_width = ((ui.available_width() - 16.0) / 2.0).clamp(140.0, 240.0);
                ui.add_sized(
                    [field_width, 36.0],
                    TextEdit::singleline(&mut self.state.proxy_username)
                        .hint_text("用户名（可选）"),
                );
                let pw_hint = if self.state.proxy_has_saved_password {
                    "密码（留空保留已存密码）"
                } else {
                    "密码（可选）"
                };
                ui.add_sized(
                    [field_width, 36.0],
                    TextEdit::singleline(&mut self.state.proxy_password)
                        .password(true)
                        .hint_text(pw_hint),
                );
            });
            if self.state.proxy_has_saved_password {
                ui.checkbox(&mut self.state.proxy_clear_password, "清除已保存的代理密码");
            }
            ui.add_sized(
                [ui.available_width().min(500.0), 36.0],
                TextEdit::singleline(&mut self.state.proxy_bypass)
                    .hint_text("旁路规则（可选，逗号分隔，如 *.internal, 10.0.0.0/8）"),
            );
        }
        ui.add_space(6.0);
        if ui
            .add_enabled(!self.state.busy, egui::Button::new("保存代理设置"))
            .clicked()
        {
            self.save_proxy();
        }
    }

    fn render_credentials(&mut self, ui: &mut egui::Ui) {
        section_header(ui, "访问凭据", "API Key 只保存在当前用户的 macOS Keychain");
        if self.state.key_configured == Some(true) {
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new("已配置").strong().color(ACCENT));
                ui.label(RichText::new(format!(
                    "密钥不会进入 {} RPC、配置文件或日志",
                    maild_brand::PRODUCT_NAME
                )).color(MUTED));
                if ui
                    .add_enabled(!self.state.busy, egui::Button::new("清除"))
                    .on_hover_text("仅移除本地 Keychain 项，不删除远端邮箱")
                    .clicked()
                {
                    self.state.confirmation = Some(Confirmation::ClearKey);
                }
            });
        } else {
            ui.horizontal(|ui| {
                let response = ui.add_sized(
                    [ui.available_width().min(560.0) - 88.0, 40.0],
                    TextEdit::singleline(&mut self.state.api_key)
                        .password(true)
                        .hint_text(format!("{}...", crate::secret::api_key_prefix()))
                        .desired_width(f32::INFINITY),
                );
                if response.changed() && self.state.api_key.len() > 256 {
                    self.state.api_key.truncate(256);
                }
                if ui
                    .add_enabled(
                        !self.state.busy && !self.state.api_key.is_empty(),
                        egui::Button::new("保存"),
                    )
                    .clicked()
                {
                    self.save_key();
                }
            });
        }
    }

    fn render_quota(&self, ui: &mut egui::Ui) {
        section_header(
            ui,
            "账户用量",
            "创建次数由 NodeSeek 等级与 7 天窗口控制，接口仅返回邮箱数和存储额度",
        );
        let Some(quota) = &self.state.quota else {
            ui.label(RichText::new("读取中").color(MUTED));
            return;
        };
        ui.horizontal_wrapped(|ui| {
            metric(
                ui,
                "邮箱",
                quota
                    .mailbox_count
                    .map(|v| v.to_string())
                    .as_deref()
                    .unwrap_or("-"),
            );
            metric(ui, "存储", storage_label(quota).as_str());
            metric(
                ui,
                "使用率",
                quota
                    .storage_usage_percent
                    .map(|v| format!("{v}%"))
                    .as_deref()
                    .unwrap_or("-"),
            );
        });
    }

    fn render_creator(&mut self, ui: &mut egui::Ui) {
        section_header(ui, "创建邮箱", "先检查可用性，创建前会再次确认完整地址");
        ui.horizontal_wrapped(|ui| {
            let response = ui.add_sized(
                [ui.available_width().min(360.0), 40.0],
                TextEdit::singleline(&mut self.state.local_part)
                    .hint_text("邮箱前缀")
                    .desired_width(f32::INFINITY),
            );
            if response.changed() {
                self.state.checked = None;
                if self.state.local_part.len() > 64 {
                    self.state.local_part.truncate(64);
                }
            }
            for (index, domain) in DOMAINS.iter().enumerate() {
                if ui
                    .selectable_value(&mut self.state.domain, index, format!("@{domain}"))
                    .changed()
                {
                    self.state.checked = None;
                }
            }
        });
        ui.horizontal_wrapped(|ui| {
            let address = self.state.address();
            if ui
                .add_enabled(
                    !self.state.busy && address.is_ok(),
                    egui::Button::new("检查可用性"),
                )
                .clicked()
            {
                let address = address.expect("button requires valid address");
                let generation = self.state.next_generation();
                self.submit(Job::Check {
                    generation,
                    address,
                });
            }
            match &self.state.checked {
                Some((address, true)) => {
                    ui.label(RichText::new(format!("{address} 可用")).color(ACCENT));
                    if ui
                        .add_enabled(!self.state.busy, egui::Button::new("创建"))
                        .clicked()
                    {
                        self.state.confirmation = Some(Confirmation::Create(address.clone()));
                    }
                }
                Some((address, false)) => {
                    ui.label(RichText::new(format!("{address} 已被占用")).color(WARNING));
                }
                None => {}
            }
        });
    }

    fn render_mailboxes(&mut self, ui: &mut egui::Ui) {
        section_header(
            ui,
            "我的邮箱",
            &format!("{} 个远端邮箱", self.state.mailboxes.len()),
        );
        if self.state.mailboxes.is_empty() {
            ui.label(RichText::new("还没有邮箱").color(MUTED));
            return;
        }
        let mailboxes = self.state.mailboxes.clone();
        for mailbox in mailboxes {
            Frame::new()
                .fill(SURFACE)
                .stroke(Stroke::new(1.0, BORDER))
                .corner_radius(CornerRadius::same(8))
                .inner_margin(Margin::same(12))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.vertical(|ui| {
                            ui.label(RichText::new(&mailbox.address).strong().color(TEXT));
                            let send = if mailbox.send_disabled == Some(true) {
                                "仅收信"
                            } else {
                                "可收发"
                            };
                            ui.label(
                                RichText::new(format!("ID {}  ·  {send}", mailbox.id))
                                    .monospace()
                                    .size(12.0)
                                    .color(MUTED),
                            );
                        });
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if ui
                                .add_enabled(!self.state.busy, egui::Button::new(format!("接入 {}", maild_brand::PRODUCT_NAME)))
                                .on_hover_text("创建同地址的 NodeSeek provider 账户")
                                .clicked()
                            {
                                let generation = self.state.next_generation();
                                self.submit(Job::Bind {
                                    generation,
                                    mailbox: mailbox.clone(),
                                });
                            }
                        });
                    });
                });
            ui.add_space(8.0);
        }
    }

    fn render_confirmation(&mut self, ctx: &egui::Context) {
        let Some(confirmation) = self.state.confirmation.clone() else {
            return;
        };
        let modal = Modal::new(egui::Id::new("nodeseek-confirmation")).show(ctx, |ui| {
            match &confirmation {
                Confirmation::ClearKey => {
                    ui.heading("清除 API Key？");
                    ui.label("只会移除本地 Keychain 项，NodeSeek 上的邮箱不会被删除。");
                }
                Confirmation::Create(address) => {
                    ui.heading("创建这个邮箱？");
                    ui.label(RichText::new(address).monospace().strong());
                    ui.label("创建会占用本周额度，插件不会自动重试。");
                }
            }
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.button("取消").clicked() {
                    ui.close();
                }
                let label = match confirmation {
                    Confirmation::ClearKey => "确认清除",
                    Confirmation::Create(_) => "确认创建",
                };
                if ui.button(label).clicked() {
                    match confirmation.clone() {
                        Confirmation::ClearKey => {
                            let generation = self.state.next_generation();
                            self.submit(Job::ClearKey { generation });
                        }
                        Confirmation::Create(address) => {
                            let generation = self.state.next_generation();
                            self.submit(Job::Create {
                                generation,
                                address,
                            });
                        }
                    }
                    ui.close();
                }
            });
        });
        if modal.should_close() {
            self.state.confirmation = None;
        }
    }
}

impl eframe::App for NodeSeekEmailUi {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.render(ui);
    }
}

impl Drop for NodeSeekEmailUi {
    fn drop(&mut self) {
        self.jobs.take();
        if let Some(runtime) = self.runtime.take() {
            runtime.shutdown_background();
        }
    }
}

async fn run_job(job: Job) -> Outcome {
    match job {
        Job::InspectKey { generation } => {
            let result = PlatformSecretStore.exists();
            let proxy = load_proxy_ui_snapshot().await.ok();
            Outcome::Key {
                generation,
                result,
                proxy,
            }
        }
        Job::SaveKey { generation, secret } => Outcome::Saved {
            generation,
            result: PlatformSecretStore.save(secret),
        },
        Job::ClearKey { generation } => Outcome::Cleared {
            generation,
            result: PlatformSecretStore.delete(),
        },
        Job::SaveProxy {
            generation,
            settings,
        } => {
            let result = async {
                let store = PluginProxyStore::discover()?;
                let saved = store.save(&settings)?;
                let (global_cfg, global_sys, _) = fetch_global_proxy_from_daemon().await;
                Ok(ProxyUiSnapshot {
                    settings: saved,
                    global_summary: describe_global_proxy(&global_cfg, &global_sys),
                })
            }
            .await;
            Outcome::ProxySaved { generation, result }
        }
        Job::Refresh { generation } => Outcome::Refreshed {
            generation,
            result: refresh_snapshot().await,
        },
        Job::Check {
            generation,
            address,
        } => {
            let result = match api_client().await {
                Ok(api) => api.check_mailbox(&address).await,
                Err(error) => Err(error),
            };
            Outcome::Checked {
                generation,
                address,
                result,
            }
        }
        Job::Create {
            generation,
            address,
        } => {
            let result = match api_client().await {
                Ok(api) => api
                    .create_mailbox(&address)
                    .await
                    .map(|value| value.mailbox),
                Err(error) => Err(error),
            };
            Outcome::Created {
                generation,
                address,
                result,
            }
        }
        Job::Bind {
            generation,
            mailbox,
        } => {
            let address = mailbox.address.clone();
            Outcome::Bound {
                generation,
                address,
                result: bind_mailbox(&mailbox).await,
            }
        }
    }
}

async fn load_proxy_ui_snapshot() -> Result<ProxyUiSnapshot, AppError> {
    let settings = PluginProxyStore::discover()?.load()?;
    let (global_cfg, global_sys, _) = fetch_global_proxy_from_daemon().await;
    Ok(ProxyUiSnapshot {
        settings,
        global_summary: describe_global_proxy(&global_cfg, &global_sys),
    })
}

async fn api_client() -> Result<ApiClient, AppError> {
    let proxy = load_effective_proxy_snapshot().await;
    ApiClient::production_with_proxy(PlatformSecretStore.load()?, proxy)
}

async fn refresh_snapshot() -> Result<Snapshot, AppError> {
    let api = api_client().await?;
    let (mailboxes, quota) = tokio::join!(api.list_mailboxes(), api.quota());
    Ok(Snapshot {
        mailboxes: mailboxes?,
        quota: quota?,
    })
}

async fn bind_mailbox(mailbox: &Mailbox) -> Result<String, AppError> {
    let address = canonicalize_address(&mailbox.address)?;
    let config = maild_paths::resolve_config(None).map_err(|_| maild_unavailable())?;
    let endpoint = discover_endpoint(&DiscoveryOptions {
        config_path: Some(config.path),
        ..DiscoveryOptions::default()
    })
    .map_err(|_| maild_unavailable())?;
    let client = RpcClient::new(endpoint).map_err(maild_error)?;
    let profiles = client
        .list_account_profiles(true)
        .await
        .map_err(maild_error)?;
    if let Some(profile) = profiles
        .profiles
        .iter()
        .find(|profile| profile.address == address)
    {
        if profile.archived_at.is_some() {
            // Archived host profiles are retained for history, but their provider-side binding
            // must be released before a fresh account can claim the same address.
            let state = StateStore::discover()?;
            if let Err(error) = state.unimport_mailbox(&profile.id, &address) {
                if error.code() != ErrorCode::NotFound {
                    return Err(error);
                }
            }
        } else {
            let compatible = profile.incoming.kind == "plugin"
                && profile.incoming.provider.as_deref() == Some(PLUGIN_ID)
                && profile.outgoing.as_ref().is_some_and(|outgoing| {
                    outgoing.kind == "plugin" && outgoing.provider.as_deref() == Some(PLUGIN_ID)
                });
            return if compatible {
                Ok(profile.id.clone())
            } else {
                Err(AppError::new(
                    ErrorCode::Conflict,
                    "本地邮件后台中已有同地址的其他类型账户",
                ))
            };
        }
    }
    let display_name = address.split('@').next().unwrap_or("NodeSeek");
    let draft = AccountDraftInput {
        display_name: format!("NodeSeek · {display_name}"),
        address,
        incoming: IncomingInput::plugin(PLUGIN_ID, 60),
        outgoing: Some(OutgoingInput::plugin(PLUGIN_ID)),
    };
    client
        .create_account(&draft, "", None)
        .await
        .map(|result| result.profile.id)
        .map_err(maild_error)
}

fn maild_error(error: ClientError) -> AppError {
    let code = if error.kind == ErrorKind::Connection {
        ErrorCode::Network
    } else {
        ErrorCode::Upstream
    };
    AppError::with_detail(code, "本地邮件后台请求失败", error.to_string())
}

fn maild_unavailable() -> AppError {
    AppError::new(
        ErrorCode::Network,
        "无法连接本地邮件后台或 NodeSeek provider 尚未就绪，请先启用插件",
    )
}

fn install_theme(ctx: &egui::Context) {
    ctx.set_theme(egui::Theme::Light);
    let mut style = (*ctx.style_of(egui::Theme::Light)).clone();
    style.visuals = egui::Visuals::light();
    style.visuals.panel_fill = CANVAS;
    style.visuals.window_fill = SURFACE;
    style.visuals.extreme_bg_color = SIDEBAR;
    style.visuals.widgets.inactive.bg_fill = SURFACE;
    style.visuals.widgets.inactive.bg_stroke = Stroke::new(1.0, BORDER);
    style.visuals.widgets.inactive.corner_radius = CornerRadius::same(8);
    style.visuals.widgets.hovered.bg_fill = SURFACE_ACTIVE;
    style.visuals.widgets.hovered.bg_stroke = Stroke::new(1.0, ACCENT);
    style.visuals.widgets.hovered.corner_radius = CornerRadius::same(8);
    style.spacing.item_spacing = Vec2::splat(8.0);
    style.spacing.button_padding = Vec2::new(12.0, 8.0);
    style.spacing.interact_size.y = 40.0;
    ctx.set_style_of(egui::Theme::Light, std::sync::Arc::new(style));
}

fn install_cjk_font(ctx: &egui::Context) {
    const CANDIDATES: [&str; 3] = [
        "/System/Library/Fonts/PingFang.ttc",
        "/System/Library/Fonts/Hiragino Sans GB.ttc",
        "/System/Library/Fonts/STHeiti Light.ttc",
    ];
    for path in CANDIDATES {
        let Ok(metadata) = fs::metadata(path) else {
            continue;
        };
        if metadata.len() > 32 * 1024 * 1024 {
            continue;
        }
        let Ok(bytes) = fs::read(path) else { continue };
        let faces = ttf_parser::fonts_in_collection(&bytes).unwrap_or(1).min(64);
        for index in 0..faces {
            let Ok(face) = ttf_parser::Face::parse(&bytes, index) else {
                continue;
            };
            if ['邮', '箱', '配', '额']
                .iter()
                .all(|ch| face.glyph_index(*ch).is_some())
            {
                let mut definitions = egui::FontDefinitions::default();
                let mut data = FontData::from_owned(bytes);
                data.index = index;
                definitions
                    .font_data
                    .insert("nodeseek-cjk".into(), std::sync::Arc::new(data));
                definitions
                    .families
                    .get_mut(&FontFamily::Proportional)
                    .unwrap()
                    .push("nodeseek-cjk".into());
                definitions
                    .families
                    .get_mut(&FontFamily::Monospace)
                    .unwrap()
                    .push("nodeseek-cjk".into());
                ctx.set_fonts(definitions);
                return;
            }
        }
    }
}

fn section_header(ui: &mut egui::Ui, title: &str, detail: &str) {
    ui.label(RichText::new(title).size(16.0).strong().color(TEXT));
    ui.label(RichText::new(detail).size(12.5).color(MUTED));
    ui.add_space(6.0);
}

fn status_dot(ui: &mut egui::Ui, configured: Option<bool>) {
    let (label, color) = match configured {
        Some(true) => ("凭据已就绪", ACCENT),
        Some(false) => ("需要 API Key", WARNING),
        None => ("检查中", MUTED),
    };
    ui.horizontal(|ui| {
        let (rect, _) = ui.allocate_exact_size(Vec2::splat(8.0), egui::Sense::hover());
        ui.painter().circle_filled(rect.center(), 4.0, color);
        ui.label(RichText::new(label).size(12.5).color(color));
    });
}

fn metric(ui: &mut egui::Ui, label: &str, value: &str) {
    Frame::new()
        .fill(SIDEBAR)
        .corner_radius(CornerRadius::same(8))
        .inner_margin(Margin::symmetric(12, 8))
        .show(ui, |ui| {
            ui.label(RichText::new(label).size(11.5).color(MUTED));
            ui.label(
                RichText::new(value)
                    .size(16.0)
                    .strong()
                    .color(TEXT)
                    .monospace(),
            );
        });
}

fn storage_label(quota: &Quota) -> String {
    match (quota.storage_used, quota.storage_quota) {
        (Some(used), Some(total)) => format!("{} / {}", human_bytes(used), human_bytes(total)),
        _ => "-".into(),
    }
}

fn human_bytes(value: i64) -> String {
    if value >= 1024 * 1024 * 1024 {
        format!("{:.1} GiB", value as f64 / (1024.0 * 1024.0 * 1024.0))
    } else if value >= 1024 * 1024 {
        format!("{:.1} MiB", value as f64 / (1024.0 * 1024.0))
    } else if value >= 1024 {
        format!("{:.1} KiB", value as f64 / 1024.0)
    } else {
        format!("{value} B")
    }
}

fn preview(value: &str) -> String {
    let mut out: String = value.chars().take(MAX_NOTICE_CHARS).collect();
    if value.chars().count() > MAX_NOTICE_CHARS {
        out.push('…');
    }
    out
}

pub fn run() -> eframe::Result {
    eframe::run_native(
        "NodeSeek Email",
        eframe::NativeOptions {
            viewport: egui::ViewportBuilder::default()
                .with_title("NodeSeek Email")
                .with_inner_size([820.0, 720.0])
                .with_min_inner_size([440.0, 560.0]),
            centered: true,
            renderer: eframe::Renderer::Glow,
            ..Default::default()
        },
        Box::new(|creation| Ok(Box::new(NodeSeekEmailUi::new(creation)))),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn populated_ui() -> NodeSeekEmailUi {
        let (_outcome_tx, outcomes) = mpsc::channel();
        NodeSeekEmailUi {
            jobs: None,
            outcomes,
            runtime: None,
            state: UiState {
                key_configured: Some(true),
                local_part: "weekly-notes".into(),
                checked: Some(("weekly-notes@seek.li".into(), true)),
                mailboxes: vec![
                    Mailbox {
                        id: 101,
                        address: "hello@seek.li".into(),
                        status: Some(1),
                        send_disabled: Some(false),
                    },
                    Mailbox {
                        id: 102,
                        address: "updates@nodeseek.org".into(),
                        status: Some(1),
                        send_disabled: Some(true),
                    },
                ],
                quota: Some(Quota {
                    current: None,
                    storage_used: Some(32 * 1024 * 1024),
                    storage_quota: Some(1024 * 1024 * 1024),
                    storage_usage_percent: Some(3),
                    storage_quota_source: Some("level".into()),
                    mailbox_count: Some(2),
                }),
                notice: Some(("NodeSeek Email 状态已刷新".into(), ACCENT)),
                ..UiState::default()
            },
        }
    }

    #[test]
    fn populated_management_ui_renders_at_compact_and_wide_widths() {
        for width in [440.0, 820.0] {
            let context = egui::Context::default();
            install_theme(&context);
            let mut app = populated_ui();
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    Vec2::new(width, 720.0),
                )),
                ..Default::default()
            };
            let mut output = context.run_ui(input, |ui| app.render(ui));
            assert!(!output.shapes.is_empty());
            output.textures_delta.clear();
        }
    }

    #[test]
    fn address_is_revalidated_after_domain_or_local_change() {
        let mut state = UiState {
            local_part: "demo".into(),
            ..Default::default()
        };
        assert_eq!(state.address().unwrap(), "demo@seek.li");
        state.domain = 1;
        assert_eq!(state.address().unwrap(), "demo@nodeseek.org");
        state.local_part = "".into();
        assert!(state.address().is_err());
    }

    #[test]
    fn stale_outcome_cannot_replace_current_mailboxes() {
        let mut state = UiState {
            generation: 4,
            busy: true,
            ..Default::default()
        };
        let next = state.apply(Outcome::Refreshed {
            generation: 3,
            result: Ok(Snapshot {
                mailboxes: vec![Mailbox {
                    id: 1,
                    address: "old@seek.li".into(),
                    status: None,
                    send_disabled: None,
                }],
                quota: Quota {
                    current: None,
                    storage_used: None,
                    storage_quota: None,
                    storage_usage_percent: None,
                    storage_quota_source: None,
                    mailbox_count: Some(1),
                },
            }),
        });
        assert!(next.is_none());
        assert!(state.mailboxes.is_empty());
        assert!(state.busy);
    }

    #[test]
    fn create_requires_exact_checked_address() {
        let state = UiState {
            local_part: "demo".into(),
            checked: Some(("demo@seek.li".into(), true)),
            domain: 1,
            ..Default::default()
        };
        assert_ne!(state.checked.as_ref().unwrap().0, state.address().unwrap());
    }

    #[test]
    fn maild_rpc_error_keeps_provider_detail() {
        let error = maild_error(ClientError {
            kind: ErrorKind::Rpc,
            message: "provider 未注册".into(),
            rpc_code: Some(-32001),
            rpc_data: None,
        });
        assert_eq!(error.code(), ErrorCode::Upstream);
        assert!(error.to_string().contains("provider 未注册"));
        assert!(error.to_string().contains("-32001"));
    }

    #[test]
    fn proxy_ui_state_builds_inherit_direct_and_custom_overrides() {
        let mut state = UiState::default();
        assert_eq!(
            state.build_proxy_settings().unwrap().proxy_override,
            ProxyOverride::Inherit
        );
        state.proxy_mode = ProxyModeChoice::Direct;
        assert_eq!(
            state.build_proxy_settings().unwrap().proxy_override,
            ProxyOverride::Direct
        );
        state.proxy_mode = ProxyModeChoice::Custom;
        state.proxy_host = "127.0.0.1".into();
        state.proxy_port = "7890".into();
        state.proxy_username = "alice".into();
        state.proxy_password = "pw".into();
        let saved = state.build_proxy_settings().unwrap();
        assert!(matches!(saved.proxy_override, ProxyOverride::Custom(_)));
        assert!(saved.has_password());
    }
}
