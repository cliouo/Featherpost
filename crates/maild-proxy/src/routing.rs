//! Cordis-style extensible and conditional proxy routing primitives.
//!
//! Zero-config by default ([`ProxyOverride::Inherit`] / [`ProxyDecision::Inherit`]): components
//! and plugins inherit the global proxy configuration unless they opt into a simple
//! [`ProxyOverride`], a declarative [`ConditionalProxyConfig`] / [`ProxyRule`] ruleset, or a
//! programmatic [`ProxyResolver`].
//!
//! Security invariant: passwords are NEVER printed in `Debug` output, and mandatory loopback
//! bypass (`localhost`, `*.localhost`, `127.0.0.0/8`, `::1`) is always enforced at the top of
//! [`ChainedResolver`] (with a test/debug-only opt-out via [`ResolveOptions`]).

use std::fmt;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::bypass::{is_loopback_host, BypassList};
use crate::model::{
    validate_custom_config, validate_endpoint, CustomProxyConfig, ProxyConfig, ProxyEndpoint,
    ProxyTarget, ResolvedRoute, SystemProxySnapshot, TargetScheme, MAX_BYPASS_ENTRIES,
};
use crate::resolve::{resolve_route_with_options, ResolveOptions};

/// Maximum number of declarative rules permitted in a [`ConditionalProxyConfig`].
pub const MAX_CONDITIONAL_RULES: usize = 256;
/// Maximum number of matcher items per vector field (`hosts`, `ports`, `tags`, etc.).
pub const MAX_MATCHER_ITEMS: usize = 256;
/// Maximum byte length for lightweight context identifiers (`tag`, `plugin_id`, `account_id`, metadata key/value).
pub const MAX_CONTEXT_TOKEN_BYTES: usize = 255;

/// High-level purpose of an outbound connection, used for conditional proxy routing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProxyPurpose {
    /// Core mail protocols (IMAP, SMTP, account probing, autodiscover).
    Mail,
    /// Remote images fetched on behalf of sandboxed email HTML documents (`Route::Remote`).
    RemoteImage,
    /// Outbound requests initiated by an external provider or extension plugin.
    Plugin,
    /// Unspecified / generic outbound network request.
    #[default]
    Generic,
}

impl ProxyPurpose {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Mail => "mail",
            Self::RemoteImage => "remote_image",
            Self::Plugin => "plugin",
            Self::Generic => "generic",
        }
    }
}

/// Per-request routing context supplied to [`ProxyResolver::resolve`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProxyContext {
    pub target: ProxyTarget,
    pub purpose: ProxyPurpose,
    pub plugin_id: Option<String>,
    pub account_id: Option<String>,
    pub tags: Vec<String>,
    pub metadata: Vec<(String, String)>,
}

impl ProxyContext {
    pub fn new(target: ProxyTarget) -> Self {
        Self {
            target,
            purpose: ProxyPurpose::Generic,
            plugin_id: None,
            account_id: None,
            tags: Vec::new(),
            metadata: Vec::new(),
        }
    }

    pub fn with_purpose(mut self, purpose: ProxyPurpose) -> Self {
        self.purpose = purpose;
        self
    }

    pub fn with_plugin_id(mut self, plugin_id: impl Into<String>) -> Self {
        self.plugin_id = Some(plugin_id.into());
        self
    }

    pub fn with_account_id(mut self, account_id: impl Into<String>) -> Self {
        self.account_id = Some(account_id.into());
        self
    }

    pub fn with_tag(mut self, tag: impl Into<String>) -> Self {
        self.tags.push(tag.into());
        self
    }

    pub fn with_tags(mut self, tags: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.tags.extend(tags.into_iter().map(Into::into));
        self
    }

    pub fn with_meta(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.metadata.push((key.into(), value.into()));
        self
    }

    pub fn has_tag(&self, tag: &str) -> bool {
        self.tags.iter().any(|existing| existing == tag)
    }

    pub fn meta(&self, key: &str) -> Option<&str> {
        self.metadata
            .iter()
            .rev()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }
}

impl From<ProxyTarget> for ProxyContext {
    fn from(target: ProxyTarget) -> Self {
        Self::new(target)
    }
}

impl From<&ProxyTarget> for ProxyContext {
    fn from(target: &ProxyTarget) -> Self {
        Self::new(target.clone())
    }
}

/// Reusable context defaults attached to a scoped connector or `reqwest` adapter.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProxyContextDefaults {
    pub purpose: ProxyPurpose,
    pub plugin_id: Option<String>,
    pub account_id: Option<String>,
    pub tags: Vec<String>,
    pub metadata: Vec<(String, String)>,
}

impl ProxyContextDefaults {
    pub fn mail() -> Self {
        Self {
            purpose: ProxyPurpose::Mail,
            ..Self::default()
        }
    }

    pub fn remote_image() -> Self {
        Self {
            purpose: ProxyPurpose::RemoteImage,
            ..Self::default()
        }
    }

    pub fn plugin(plugin_id: impl Into<String>) -> Self {
        Self {
            purpose: ProxyPurpose::Plugin,
            plugin_id: Some(plugin_id.into()),
            ..Self::default()
        }
    }

    pub fn with_purpose(mut self, purpose: ProxyPurpose) -> Self {
        self.purpose = purpose;
        self
    }

    pub fn with_plugin_id(mut self, plugin_id: impl Into<String>) -> Self {
        self.plugin_id = Some(plugin_id.into());
        self
    }

    pub fn with_account_id(mut self, account_id: impl Into<String>) -> Self {
        self.account_id = Some(account_id.into());
        self
    }

    pub fn with_tag(mut self, tag: impl Into<String>) -> Self {
        self.tags.push(tag.into());
        self
    }

    pub fn with_meta(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.metadata.push((key.into(), value.into()));
        self
    }

    pub fn build_context(&self, target: ProxyTarget) -> ProxyContext {
        ProxyContext {
            target,
            purpose: self.purpose,
            plugin_id: self.plugin_id.clone(),
            account_id: self.account_id.clone(),
            tags: self.tags.clone(),
            metadata: self.metadata.clone(),
        }
    }

    /// Fill unset fields in `ctx` with defaults without overwriting explicit per-request values.
    pub fn apply_to(&self, ctx: &ProxyContext) -> ProxyContext {
        let mut merged = ctx.clone();
        if merged.purpose == ProxyPurpose::Generic && self.purpose != ProxyPurpose::Generic {
            merged.purpose = self.purpose;
        }
        if merged.plugin_id.is_none() {
            merged.plugin_id = self.plugin_id.clone();
        }
        if merged.account_id.is_none() {
            merged.account_id = self.account_id.clone();
        }
        for tag in &self.tags {
            if !merged.tags.iter().any(|existing| existing == tag) {
                merged.tags.push(tag.clone());
            }
        }
        for (key, value) in &self.metadata {
            if !merged.metadata.iter().any(|(k, _)| k == key) {
                merged.metadata.push((key.clone(), value.clone()));
            }
        }
        merged
    }
}

/// Output of a [`ProxyResolver`].
#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", content = "endpoint", rename_all = "snake_case")]
pub enum ProxyDecision {
    /// Force a direct connection for this request.
    Direct,
    /// Route this request through a specific proxy endpoint.
    Use(ProxyEndpoint),
    /// Fall through to the next resolver in the chain (plugin default -> global config -> system).
    #[default]
    Inherit,
}

impl fmt::Debug for ProxyDecision {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Direct => f.write_str("ProxyDecision::Direct"),
            Self::Use(ep) => f.debug_tuple("ProxyDecision::Use").field(ep).finish(),
            Self::Inherit => f.write_str("ProxyDecision::Inherit"),
        }
    }
}

impl ProxyDecision {
    pub fn has_password(&self) -> bool {
        match self {
            Self::Use(ep) => ep.password.as_deref().is_some_and(|p| !p.is_empty()),
            Self::Direct | Self::Inherit => false,
        }
    }

    pub fn without_secret(&self) -> Self {
        match self {
            Self::Direct => Self::Direct,
            Self::Use(ep) => Self::Use(ep.without_secret()),
            Self::Inherit => Self::Inherit,
        }
    }

    pub fn validate(&self) -> Result<Self, String> {
        match self {
            Self::Direct => Ok(Self::Direct),
            Self::Inherit => Ok(Self::Inherit),
            Self::Use(ep) => validate_endpoint(ep).map(Self::Use),
        }
    }
}

/// Simple plugin- or component-level proxy override.
///
/// Defaults to [`ProxyOverride::Inherit`], keeping plugins zero-config until a user explicitly
/// overrides proxying for that plugin.
#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", content = "custom", rename_all = "snake_case")]
pub enum ProxyOverride {
    /// Follow the global Featherpost proxy policy.
    #[default]
    Inherit,
    /// Always connect directly for this plugin/component.
    Direct,
    /// Use a plugin-specific custom proxy configuration (with its own bypass list).
    Custom(CustomProxyConfig),
}

impl fmt::Debug for ProxyOverride {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Inherit => f.write_str("ProxyOverride::Inherit"),
            Self::Direct => f.write_str("ProxyOverride::Direct"),
            Self::Custom(cfg) => f.debug_tuple("ProxyOverride::Custom").field(cfg).finish(),
        }
    }
}

impl ProxyOverride {
    pub const fn mode_str(&self) -> &'static str {
        match self {
            Self::Inherit => "inherit",
            Self::Direct => "direct",
            Self::Custom(_) => "custom",
        }
    }

    pub fn has_password(&self) -> bool {
        match self {
            Self::Custom(cfg) => cfg.has_password(),
            Self::Inherit | Self::Direct => false,
        }
    }

    pub fn without_secret(&self) -> Self {
        match self {
            Self::Inherit => Self::Inherit,
            Self::Direct => Self::Direct,
            Self::Custom(cfg) => Self::Custom(cfg.without_secret()),
        }
    }

    pub fn validate(&self) -> Result<Self, String> {
        match self {
            Self::Inherit => Ok(Self::Inherit),
            Self::Direct => Ok(Self::Direct),
            Self::Custom(cfg) => validate_custom_config(cfg).map(Self::Custom),
        }
    }

    /// Evaluate this override against a target (honoring `CustomProxyConfig::bypass`).
    pub fn resolve_decision(&self, target: &ProxyTarget) -> ProxyDecision {
        match self {
            Self::Inherit => ProxyDecision::Inherit,
            Self::Direct => ProxyDecision::Direct,
            Self::Custom(cfg) => {
                let bypass = BypassList::parse_entries(&cfg.bypass);
                if bypass.matches(&target.host, target.port) {
                    ProxyDecision::Direct
                } else {
                    ProxyDecision::Use(cfg.to_endpoint())
                }
            }
        }
    }
}

/// Composable proxy routing strategy.
pub trait ProxyResolver: Send + Sync {
    fn resolve(&self, ctx: &ProxyContext) -> ProxyDecision;
}

impl<T: ProxyResolver + ?Sized> ProxyResolver for Arc<T> {
    fn resolve(&self, ctx: &ProxyContext) -> ProxyDecision {
        (**self).resolve(ctx)
    }
}

impl<T: ProxyResolver + ?Sized> ProxyResolver for Box<T> {
    fn resolve(&self, ctx: &ProxyContext) -> ProxyDecision {
        (**self).resolve(ctx)
    }
}

impl ProxyResolver for ProxyOverride {
    fn resolve(&self, ctx: &ProxyContext) -> ProxyDecision {
        self.resolve_decision(&ctx.target)
    }
}

/// Wrapper turning any `Fn(&ProxyContext) -> ProxyDecision + Send + Sync` closure into a
/// [`ProxyResolver`].
pub struct FnResolver<F>(pub F);

impl<F> fmt::Debug for FnResolver<F> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("FnResolver(..)")
    }
}

impl<F> ProxyResolver for FnResolver<F>
where
    F: Fn(&ProxyContext) -> ProxyDecision + Send + Sync,
{
    fn resolve(&self, ctx: &ProxyContext) -> ProxyDecision {
        (self.0)(ctx)
    }
}

/// Convenience constructor for a programmatic closure-based [`ProxyResolver`].
pub fn fn_resolver<F>(f: F) -> FnResolver<F>
where
    F: Fn(&ProxyContext) -> ProxyDecision + Send + Sync,
{
    FnResolver(f)
}

/// Terminal [`ProxyResolver`] backed by global [`ProxyConfig`] + [`SystemProxySnapshot`].
#[derive(Clone, Debug)]
pub struct GlobalConfigResolver {
    config: ProxyConfig,
    system: SystemProxySnapshot,
    options: ResolveOptions,
}

impl GlobalConfigResolver {
    pub fn new(config: ProxyConfig, system: SystemProxySnapshot) -> Self {
        Self {
            config,
            system,
            options: ResolveOptions::default(),
        }
    }

    pub fn with_options(mut self, options: ResolveOptions) -> Self {
        self.options = options;
        self
    }
}

impl ProxyResolver for GlobalConfigResolver {
    fn resolve(&self, ctx: &ProxyContext) -> ProxyDecision {
        match resolve_route_with_options(&self.config, &self.system, &ctx.target, self.options) {
            ResolvedRoute::Direct => ProxyDecision::Direct,
            ResolvedRoute::Proxy(ep) => ProxyDecision::Use(ep),
        }
    }
}

#[derive(Clone)]
struct ChainEntry {
    priority: i32,
    order: u64,
    resolver: Arc<dyn ProxyResolver>,
}

/// Priority-ordered chain of [`ProxyResolver`]s.
///
/// Precedence:
/// 1. Mandatory loopback bypass (`localhost`, `*.localhost`, `127.0.0.0/8`, `::1`) ->
///    [`ProxyDecision::Direct`], unless explicitly opted out in test/debug builds.
/// 2. Registered resolvers sorted by `priority` descending (higher runs first), with FIFO
///    insertion order breaking ties.
/// 3. First non-[`ProxyDecision::Inherit`] decision wins.
#[derive(Clone, Default)]
pub struct ChainedResolver {
    entries: Vec<ChainEntry>,
    next_order: u64,
    options: ResolveOptions,
}

impl fmt::Debug for ChainedResolver {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ChainedResolver")
            .field("len", &self.entries.len())
            .field("options", &self.options)
            .finish()
    }
}

impl ChainedResolver {
    pub fn new() -> Self {
        Self::default()
    }

    /// Build a chain from resolvers evaluated in iterator order (all at default priority `0`).
    pub fn from_resolvers(resolvers: impl IntoIterator<Item = Arc<dyn ProxyResolver>>) -> Self {
        let mut chain = Self::new();
        for resolver in resolvers {
            chain.push_arc(0, resolver);
        }
        chain
    }

    /// Append a resolver at default priority `0` (FIFO among equal priorities).
    pub fn push(&mut self, resolver: impl ProxyResolver + 'static) {
        self.push_with_priority(0, resolver);
    }

    /// Insert a resolver with an explicit numeric priority (higher priority evaluates first).
    pub fn push_with_priority(&mut self, priority: i32, resolver: impl ProxyResolver + 'static) {
        self.push_arc(priority, Arc::new(resolver));
    }

    /// Insert an `Arc<dyn ProxyResolver>` with an explicit numeric priority.
    pub fn push_arc(&mut self, priority: i32, resolver: Arc<dyn ProxyResolver>) {
        let order = self.next_order;
        self.next_order = self.next_order.wrapping_add(1);
        self.entries.push(ChainEntry {
            priority,
            order,
            resolver,
        });
        self.entries
            .sort_by(|a, b| b.priority.cmp(&a.priority).then_with(|| a.order.cmp(&b.order)));
    }

    pub fn with_resolver(mut self, resolver: impl ProxyResolver + 'static) -> Self {
        self.push(resolver);
        self
    }

    pub fn with_priority_resolver(
        mut self,
        priority: i32,
        resolver: impl ProxyResolver + 'static,
    ) -> Self {
        self.push_with_priority(priority, resolver);
        self
    }

    pub fn with_options(mut self, options: ResolveOptions) -> Self {
        self.options = options;
        self
    }

    #[cfg(any(test, debug_assertions))]
    pub fn with_allow_loopback_proxy(mut self, allow: bool) -> Self {
        self.options.allow_loopback_proxy = allow;
        self
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    fn allow_loopback(&self) -> bool {
        #[cfg(any(test, debug_assertions))]
        {
            self.options.allow_loopback_proxy
        }
        #[cfg(not(any(test, debug_assertions)))]
        {
            false
        }
    }

    /// Resolve through the chain and fall back to global `config` + `system` when the chain
    /// returns [`ProxyDecision::Inherit`].
    pub fn resolve_route(
        &self,
        ctx: &ProxyContext,
        global_config: &ProxyConfig,
        system: &SystemProxySnapshot,
    ) -> ResolvedRoute {
        match self.resolve(ctx) {
            ProxyDecision::Direct => ResolvedRoute::Direct,
            ProxyDecision::Use(ep) => ResolvedRoute::Proxy(ep),
            ProxyDecision::Inherit => {
                resolve_route_with_options(global_config, system, &ctx.target, self.options)
            }
        }
    }

    /// Resolve through the chain treating any remaining [`ProxyDecision::Inherit`] as direct.
    pub fn resolve_final(&self, ctx: &ProxyContext) -> ResolvedRoute {
        match self.resolve(ctx) {
            ProxyDecision::Direct | ProxyDecision::Inherit => ResolvedRoute::Direct,
            ProxyDecision::Use(ep) => ResolvedRoute::Proxy(ep),
        }
    }
}

impl ProxyResolver for ChainedResolver {
    fn resolve(&self, ctx: &ProxyContext) -> ProxyDecision {
        // Mandatory loopback bypass is always enforced at the top unless opted out in test/debug.
        if !self.allow_loopback() && is_loopback_host(&ctx.target.host) {
            return ProxyDecision::Direct;
        }
        for entry in &self.entries {
            match entry.resolver.resolve(ctx) {
                ProxyDecision::Inherit => continue,
                decided => return decided,
            }
        }
        ProxyDecision::Inherit
    }
}

/// Declarative match criteria for a [`ProxyRule`].
///
/// All non-empty fields are ANDed together; values within a list field (`hosts`, `ports`,
/// `schemes`, `purposes`, `plugin_ids`, `account_ids`, `tags`) are ORed. `metadata` entries are
/// ANDed (all specified key-value pairs must match). An empty matcher matches every request.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProxyMatcher {
    /// Hostnames, domain suffixes (`.example.com`), wildcards (`*.example.com`), IPs, CIDRs,
    /// `host:port`, `<local>`, or `*` (same syntax as [`BypassList`]).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hosts: Vec<String>,
    /// Target TCP ports.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ports: Vec<u16>,
    /// Target connection schemes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub schemes: Vec<TargetScheme>,
    /// Request purposes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub purposes: Vec<ProxyPurpose>,
    /// Source plugin IDs.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub plugin_ids: Vec<String>,
    /// Source account IDs.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub account_ids: Vec<String>,
    /// Context tags (matches if `ctx` contains any listed tag).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    /// Context metadata key-value requirements (all pairs must match).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub metadata: Vec<(String, String)>,
}

impl ProxyMatcher {
    pub fn with_host(mut self, host: impl Into<String>) -> Self {
        self.hosts.push(host.into());
        self
    }

    pub fn with_port(mut self, port: u16) -> Self {
        self.ports.push(port);
        self
    }

    pub fn with_scheme(mut self, scheme: TargetScheme) -> Self {
        self.schemes.push(scheme);
        self
    }

    pub fn with_purpose(mut self, purpose: ProxyPurpose) -> Self {
        self.purposes.push(purpose);
        self
    }

    pub fn with_plugin_id(mut self, plugin_id: impl Into<String>) -> Self {
        self.plugin_ids.push(plugin_id.into());
        self
    }

    pub fn with_account_id(mut self, account_id: impl Into<String>) -> Self {
        self.account_ids.push(account_id.into());
        self
    }

    pub fn with_tag(mut self, tag: impl Into<String>) -> Self {
        self.tags.push(tag.into());
        self
    }

    pub fn with_meta(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.metadata.push((key.into(), value.into()));
        self
    }

    pub fn validate(&self) -> Result<Self, String> {
        if self.hosts.len() > MAX_BYPASS_ENTRIES {
            return Err(format!("规则主机匹配项超过 {MAX_BYPASS_ENTRIES} 条上限"));
        }
        let mut hosts = Vec::with_capacity(self.hosts.len());
        for raw in &self.hosts {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                continue;
            }
            hosts.push(trimmed.to_string());
        }
        BypassList::parse_strict(&hosts)?;

        if self.ports.len() > MAX_MATCHER_ITEMS {
            return Err(format!("规则端口匹配项超过 {MAX_MATCHER_ITEMS} 条上限"));
        }
        if self.ports.contains(&0) {
            return Err("规则端口不能为 0".into());
        }
        if self.schemes.len() > MAX_MATCHER_ITEMS || self.purposes.len() > MAX_MATCHER_ITEMS {
            return Err(format!("规则协议或用途匹配项超过 {MAX_MATCHER_ITEMS} 条上限"));
        }

        let plugin_ids = validate_token_list(&self.plugin_ids, "plugin_ids")?;
        let account_ids = validate_token_list(&self.account_ids, "account_ids")?;
        let tags = validate_token_list(&self.tags, "tags")?;

        if self.metadata.len() > MAX_MATCHER_ITEMS {
            return Err(format!("规则元数据匹配项超过 {MAX_MATCHER_ITEMS} 条上限"));
        }
        let mut metadata = Vec::with_capacity(self.metadata.len());
        for (k, v) in &self.metadata {
            let k = k.trim();
            if k.is_empty() || k.len() > MAX_CONTEXT_TOKEN_BYTES || k.chars().any(char::is_control)
            {
                return Err("规则元数据键无效".into());
            }
            if v.len() > MAX_CONTEXT_TOKEN_BYTES || v.chars().any(char::is_control) {
                return Err("规则元数据值无效".into());
            }
            metadata.push((k.to_string(), v.clone()));
        }

        Ok(Self {
            hosts,
            ports: self.ports.clone(),
            schemes: self.schemes.clone(),
            purposes: self.purposes.clone(),
            plugin_ids,
            account_ids,
            tags,
            metadata,
        })
    }

    pub fn compile(&self) -> Result<CompiledMatcher, String> {
        let valid = self.validate()?;
        let hosts = if valid.hosts.is_empty() {
            None
        } else {
            Some(BypassList::parse_strict(&valid.hosts)?)
        };
        Ok(CompiledMatcher {
            hosts,
            ports: valid.ports,
            schemes: valid.schemes,
            purposes: valid.purposes,
            plugin_ids: valid.plugin_ids,
            account_ids: valid.account_ids,
            tags: valid.tags,
            metadata: valid.metadata,
        })
    }

    pub fn matches(&self, ctx: &ProxyContext) -> bool {
        let hosts = (!self.hosts.is_empty()).then(|| BypassList::parse_entries(&self.hosts));
        matches_fields(
            hosts.as_ref(),
            &self.ports,
            &self.schemes,
            &self.purposes,
            &self.plugin_ids,
            &self.account_ids,
            &self.tags,
            &self.metadata,
            ctx,
        )
    }
}

fn validate_token_list(items: &[String], field: &str) -> Result<Vec<String>, String> {
    if items.len() > MAX_MATCHER_ITEMS {
        return Err(format!("规则 {field} 超过 {MAX_MATCHER_ITEMS} 条上限"));
    }
    let mut out = Vec::with_capacity(items.len());
    for raw in items {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            continue;
        }
        if trimmed.len() > MAX_CONTEXT_TOKEN_BYTES || trimmed.chars().any(char::is_control) {
            return Err(format!("规则 {field} 包含无效条目"));
        }
        out.push(trimmed.to_string());
    }
    Ok(out)
}

#[allow(clippy::too_many_arguments)]
fn matches_fields(
    hosts: Option<&BypassList>,
    ports: &[u16],
    schemes: &[TargetScheme],
    purposes: &[ProxyPurpose],
    plugin_ids: &[String],
    account_ids: &[String],
    tags: &[String],
    metadata: &[(String, String)],
    ctx: &ProxyContext,
) -> bool {
    if let Some(list) = hosts {
        if !list.matches(&ctx.target.host, ctx.target.port) {
            return false;
        }
    }
    if !ports.is_empty() && !ports.contains(&ctx.target.port) {
        return false;
    }
    if !schemes.is_empty() && !schemes.contains(&ctx.target.scheme) {
        return false;
    }
    if !purposes.is_empty() && !purposes.contains(&ctx.purpose) {
        return false;
    }
    if !plugin_ids.is_empty()
        && !ctx
            .plugin_id
            .as_deref()
            .is_some_and(|id| plugin_ids.iter().any(|candidate| candidate == id))
    {
        return false;
    }
    if !account_ids.is_empty()
        && !ctx
            .account_id
            .as_deref()
            .is_some_and(|id| account_ids.iter().any(|candidate| candidate == id))
    {
        return false;
    }
    if !tags.is_empty() && !tags.iter().any(|tag| ctx.has_tag(tag)) {
        return false;
    }
    if !metadata.is_empty()
        && !metadata
            .iter()
            .all(|(k, v)| ctx.meta(k).is_some_and(|actual| actual == v))
    {
        return false;
    }
    true
}

/// Pre-compiled form of [`ProxyMatcher`] for zero-parse per-request evaluation.
#[derive(Clone, Debug)]
pub struct CompiledMatcher {
    hosts: Option<BypassList>,
    ports: Vec<u16>,
    schemes: Vec<TargetScheme>,
    purposes: Vec<ProxyPurpose>,
    plugin_ids: Vec<String>,
    account_ids: Vec<String>,
    tags: Vec<String>,
    metadata: Vec<(String, String)>,
}

impl CompiledMatcher {
    pub fn matches(&self, ctx: &ProxyContext) -> bool {
        matches_fields(
            self.hosts.as_ref(),
            &self.ports,
            &self.schemes,
            &self.purposes,
            &self.plugin_ids,
            &self.account_ids,
            &self.tags,
            &self.metadata,
            ctx,
        )
    }
}

/// A single declarative rule mapping matching requests to a [`ProxyDecision`].
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProxyRule {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default)]
    pub matcher: ProxyMatcher,
    pub decision: ProxyDecision,
}

impl fmt::Debug for ProxyRule {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProxyRule")
            .field("name", &self.name)
            .field("matcher", &self.matcher)
            .field("decision", &self.decision)
            .finish()
    }
}

impl ProxyRule {
    pub fn new(matcher: ProxyMatcher, decision: ProxyDecision) -> Self {
        Self {
            name: None,
            matcher,
            decision,
        }
    }

    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    pub fn has_password(&self) -> bool {
        self.decision.has_password()
    }

    pub fn without_secret(&self) -> Self {
        Self {
            name: self.name.clone(),
            matcher: self.matcher.clone(),
            decision: self.decision.without_secret(),
        }
    }

    pub fn validate(&self) -> Result<Self, String> {
        let name = match self.name.as_deref() {
            None => None,
            Some(raw) => {
                let trimmed = raw.trim();
                if trimmed.is_empty() {
                    None
                } else {
                    if trimmed.len() > MAX_CONTEXT_TOKEN_BYTES
                        || trimmed.chars().any(char::is_control)
                    {
                        return Err("规则名称无效".into());
                    }
                    Some(trimmed.to_string())
                }
            }
        };
        Ok(Self {
            name,
            matcher: self.matcher.validate()?,
            decision: self.decision.validate()?,
        })
    }
}

/// Declarative conditional proxy routing configuration: ordered rules + fallback [`ProxyOverride`].
#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConditionalProxyConfig {
    #[serde(default)]
    pub rules: Vec<ProxyRule>,
    #[serde(default)]
    pub fallback: ProxyOverride,
}

impl fmt::Debug for ConditionalProxyConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ConditionalProxyConfig")
            .field("rules", &self.rules)
            .field("fallback", &self.fallback)
            .finish()
    }
}

impl ConditionalProxyConfig {
    pub fn new(rules: Vec<ProxyRule>, fallback: ProxyOverride) -> Self {
        Self { rules, fallback }
    }

    pub fn has_password(&self) -> bool {
        self.fallback.has_password() || self.rules.iter().any(ProxyRule::has_password)
    }

    pub fn without_secret(&self) -> Self {
        Self {
            rules: self.rules.iter().map(ProxyRule::without_secret).collect(),
            fallback: self.fallback.without_secret(),
        }
    }

    pub fn validate(&self) -> Result<Self, String> {
        if self.rules.len() > MAX_CONDITIONAL_RULES {
            return Err(format!("条件代理规则超过 {MAX_CONDITIONAL_RULES} 条上限"));
        }
        let mut rules = Vec::with_capacity(self.rules.len());
        for rule in &self.rules {
            rules.push(rule.validate()?);
        }
        let fallback = self.fallback.validate()?;
        Ok(Self { rules, fallback })
    }

    pub fn compile(&self) -> Result<ConditionalResolver, String> {
        ConditionalResolver::new(self.clone())
    }
}

#[derive(Clone, Debug)]
struct CompiledRule {
    matcher: CompiledMatcher,
    decision: ProxyDecision,
}

#[derive(Clone, Debug)]
enum CompiledFallback {
    Inherit,
    Direct,
    Custom {
        endpoint: ProxyEndpoint,
        bypass: BypassList,
    },
}

/// Pre-compiled [`ProxyResolver`] driven by a [`ConditionalProxyConfig`].
///
/// Rules are evaluated in order; the first matching rule returns its [`ProxyDecision`]
/// (including an explicit [`ProxyDecision::Inherit`] to short-circuit later rules and fall back
/// to the parent/global resolver). If no rule matches, `fallback` ([`ProxyOverride`]) decides.
#[derive(Clone, Debug)]
pub struct ConditionalResolver {
    rules: Vec<CompiledRule>,
    fallback: CompiledFallback,
}

impl ConditionalResolver {
    pub fn new(config: ConditionalProxyConfig) -> Result<Self, String> {
        let valid = config.validate()?;
        let mut rules = Vec::with_capacity(valid.rules.len());
        for rule in valid.rules {
            rules.push(CompiledRule {
                matcher: rule.matcher.compile()?,
                decision: rule.decision,
            });
        }
        let fallback = match valid.fallback {
            ProxyOverride::Inherit => CompiledFallback::Inherit,
            ProxyOverride::Direct => CompiledFallback::Direct,
            ProxyOverride::Custom(cfg) => CompiledFallback::Custom {
                bypass: BypassList::parse_strict(&cfg.bypass)?,
                endpoint: cfg.to_endpoint(),
            },
        };
        Ok(Self { rules, fallback })
    }

    pub fn from_override(override_cfg: ProxyOverride) -> Result<Self, String> {
        Self::new(ConditionalProxyConfig {
            rules: Vec::new(),
            fallback: override_cfg,
        })
    }
}

impl ProxyResolver for ConditionalResolver {
    fn resolve(&self, ctx: &ProxyContext) -> ProxyDecision {
        for rule in &self.rules {
            if rule.matcher.matches(ctx) {
                return rule.decision.clone();
            }
        }
        match &self.fallback {
            CompiledFallback::Inherit => ProxyDecision::Inherit,
            CompiledFallback::Direct => ProxyDecision::Direct,
            CompiledFallback::Custom { endpoint, bypass } => {
                if bypass.matches(&ctx.target.host, ctx.target.port) {
                    ProxyDecision::Direct
                } else {
                    ProxyDecision::Use(endpoint.clone())
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ProxyMode, ProxyProtocol};

    fn ep(protocol: ProxyProtocol, host: &str, port: u16) -> ProxyEndpoint {
        ProxyEndpoint {
            protocol,
            host: host.into(),
            port,
            username: None,
            password: None,
        }
    }

    fn custom_cfg(
        protocol: ProxyProtocol,
        host: &str,
        port: u16,
        password: Option<&str>,
        bypass: Vec<&str>,
    ) -> CustomProxyConfig {
        CustomProxyConfig {
            protocol,
            host: host.into(),
            port,
            username: password.map(|_| "user".into()),
            password: password.map(Into::into),
            bypass: bypass.into_iter().map(Into::into).collect(),
        }
    }

    #[test]
    fn proxy_context_builder_and_defaults_merge_cleanly() {
        let target = ProxyTarget::new("api.seek.li", 443, TargetScheme::Https);
        let ctx = ProxyContext::new(target.clone())
            .with_purpose(ProxyPurpose::Plugin)
            .with_plugin_id("app.maild.provider.nodeseek-email")
            .with_account_id("acct-1")
            .with_tag("fast")
            .with_tags(["edge", "ipv4"])
            .with_meta("region", "us")
            .with_meta("region", "jp");

        assert_eq!(ctx.target, target);
        assert_eq!(ctx.purpose, ProxyPurpose::Plugin);
        assert_eq!(
            ctx.plugin_id.as_deref(),
            Some("app.maild.provider.nodeseek-email")
        );
        assert_eq!(ctx.account_id.as_deref(), Some("acct-1"));
        assert!(ctx.has_tag("fast"));
        assert!(ctx.has_tag("edge"));
        assert!(!ctx.has_tag("slow"));
        assert_eq!(ctx.meta("region"), Some("jp"));
        assert_eq!(ctx.meta("missing"), None);

        let defaults = ProxyContextDefaults::mail()
            .with_account_id("default-acct")
            .with_tag("mail")
            .with_meta("env", "prod");
        let built = defaults.build_context(target.clone());
        assert_eq!(built.purpose, ProxyPurpose::Mail);
        assert_eq!(built.account_id.as_deref(), Some("default-acct"));
        assert!(built.has_tag("mail"));
        assert_eq!(built.meta("env"), Some("prod"));

        // Explicit per-request values win over defaults.
        let explicit = ProxyContext::new(target)
            .with_account_id("override-acct")
            .with_meta("env", "staging");
        let merged = defaults.apply_to(&explicit);
        assert_eq!(merged.purpose, ProxyPurpose::Mail);
        assert_eq!(merged.account_id.as_deref(), Some("override-acct"));
        assert_eq!(merged.meta("env"), Some("staging"));
        assert!(merged.has_tag("mail"));
    }

    #[test]
    fn proxy_override_resolves_inherit_direct_and_custom_with_bypass() {
        let target = ProxyTarget::new("seek.li", 443, TargetScheme::Https);
        let internal = ProxyTarget::new("intranet.corp.test", 443, TargetScheme::Https);

        assert_eq!(
            ProxyOverride::Inherit.resolve(&ProxyContext::from(&target)),
            ProxyDecision::Inherit
        );
        assert_eq!(
            ProxyOverride::Direct.resolve(&ProxyContext::from(&target)),
            ProxyDecision::Direct
        );

        let ov = ProxyOverride::Custom(custom_cfg(
            ProxyProtocol::Socks5h,
            "10.0.0.5",
            7890,
            Some("secret-pw"),
            vec![".corp.test"],
        ));
        assert_eq!(
            ov.resolve(&ProxyContext::from(&target)),
            ProxyDecision::Use(ProxyEndpoint {
                protocol: ProxyProtocol::Socks5h,
                host: "10.0.0.5".into(),
                port: 7890,
                username: Some("user".into()),
                password: Some("secret-pw".into()),
            })
        );
        assert_eq!(
            ov.resolve(&ProxyContext::from(&internal)),
            ProxyDecision::Direct
        );
    }

    #[test]
    fn conditional_resolver_routes_multiple_rules_and_falls_back_to_inherit() {
        let proxy1 = ep(ProxyProtocol::Socks5h, "10.0.0.1", 1080);
        let proxy2 = ep(ProxyProtocol::Http, "10.0.0.2", 8080);

        let cfg = ConditionalProxyConfig::new(
            vec![
                ProxyRule::new(
                    ProxyMatcher::default()
                        .with_host("*.alpha.example")
                        .with_port(443),
                    ProxyDecision::Use(proxy1.clone()),
                )
                .with_name("alpha-https"),
                ProxyRule::new(
                    ProxyMatcher::default()
                        .with_tag("route-b")
                        .with_scheme(TargetScheme::Https),
                    ProxyDecision::Use(proxy2.clone()),
                )
                .with_name("tag-b"),
                ProxyRule::new(
                    ProxyMatcher::default().with_host("10.20.0.0/16"),
                    ProxyDecision::Direct,
                )
                .with_name("corp-cidr"),
                ProxyRule::new(
                    ProxyMatcher::default()
                        .with_purpose(ProxyPurpose::RemoteImage)
                        .with_meta("tier", "fallback"),
                    ProxyDecision::Inherit,
                ),
            ],
            ProxyOverride::Inherit,
        );
        let resolver = cfg.compile().unwrap();

        let global = ProxyConfig {
            mode: ProxyMode::Custom,
            custom: Some(custom_cfg(ProxyProtocol::Http, "10.0.0.9", 3128, None, vec![])),
        };
        let system = SystemProxySnapshot::default();
        let chain = ChainedResolver::new().with_resolver(resolver);

        // Rule 1 matches host + port -> proxy 1
        let ctx_a = ProxyContext::new(ProxyTarget::new(
            "cdn.alpha.example",
            443,
            TargetScheme::Https,
        ));
        assert_eq!(
            chain.resolve_route(&ctx_a, &global, &system),
            ResolvedRoute::Proxy(proxy1.clone())
        );

        // Wrong port does not match Rule 1 -> falls through to global proxy
        let ctx_a_80 = ProxyContext::new(ProxyTarget::new(
            "cdn.alpha.example",
            80,
            TargetScheme::Http,
        ));
        assert_eq!(
            chain.resolve_route(&ctx_a_80, &global, &system),
            ResolvedRoute::Proxy(ep(ProxyProtocol::Http, "10.0.0.9", 3128))
        );

        // Rule 2 matches tag + scheme -> proxy 2
        let ctx_b = ProxyContext::new(ProxyTarget::new("other.example", 443, TargetScheme::Https))
            .with_tag("route-b");
        assert_eq!(
            chain.resolve_route(&ctx_b, &global, &system),
            ResolvedRoute::Proxy(proxy2)
        );

        // Rule 3 matches CIDR -> Direct (overrides global)
        let ctx_cidr = ProxyContext::new(ProxyTarget::new("10.20.5.9", 8443, TargetScheme::Https));
        assert_eq!(
            chain.resolve_route(&ctx_cidr, &global, &system),
            ResolvedRoute::Direct
        );

        // Rule 4 explicit Inherit -> falls through to global proxy
        let ctx_inherit =
            ProxyContext::new(ProxyTarget::new("img.example", 443, TargetScheme::Https))
                .with_purpose(ProxyPurpose::RemoteImage)
                .with_meta("tier", "fallback");
        assert_eq!(
            chain.resolve_route(&ctx_inherit, &global, &system),
            ResolvedRoute::Proxy(ep(ProxyProtocol::Http, "10.0.0.9", 3128))
        );

        // Loopback is ALWAYS bypassed even if a rule or global proxy would match it!
        let ctx_loop = ProxyContext::new(ProxyTarget::new("127.0.0.1", 443, TargetScheme::Https))
            .with_tag("route-b");
        assert_eq!(chain.resolve(&ctx_loop), ProxyDecision::Direct);
        assert_eq!(
            chain.resolve_route(&ctx_loop, &global, &system),
            ResolvedRoute::Direct
        );

        // Test opt-out allows loopback proxying for hermetic tests.
        let chain_allow = chain.with_allow_loopback_proxy(true);
        assert_eq!(
            chain_allow.resolve(&ctx_loop),
            ProxyDecision::Use(ep(ProxyProtocol::Http, "10.0.0.2", 8080))
        );
    }

    #[test]
    fn chained_resolver_respects_priority_and_programmatic_closures() {
        let proxy_hi = ep(ProxyProtocol::Socks5h, "10.0.0.10", 1080);
        let proxy_lo = ep(ProxyProtocol::Http, "10.0.0.20", 8080);

        let hi_clone = proxy_hi.clone();
        let lo_clone = proxy_lo.clone();

        let chain = ChainedResolver::new()
            .with_priority_resolver(
                -10,
                fn_resolver(move |_ctx| ProxyDecision::Use(lo_clone.clone())),
            )
            .with_priority_resolver(
                50,
                fn_resolver(move |ctx| {
                    if ctx.plugin_id.as_deref() == Some("plugin.a") {
                        ProxyDecision::Use(hi_clone.clone())
                    } else {
                        ProxyDecision::Inherit
                    }
                }),
            );

        let target = ProxyTarget::new("example.com", 443, TargetScheme::Https);
        assert_eq!(
            chain.resolve(&ProxyContext::new(target.clone()).with_plugin_id("plugin.a")),
            ProxyDecision::Use(proxy_hi)
        );
        assert_eq!(
            chain.resolve(&ProxyContext::new(target).with_plugin_id("plugin.b")),
            ProxyDecision::Use(proxy_lo)
        );
    }

    #[test]
    fn debug_and_without_secret_redact_passwords_everywhere() {
        let secret = "ultra-secret-routing-pw";
        let ep_secret = ProxyEndpoint {
            protocol: ProxyProtocol::Socks5h,
            host: "10.0.0.1".into(),
            port: 1080,
            username: Some("alice".into()),
            password: Some(secret.into()),
        };
        let decision = ProxyDecision::Use(ep_secret);
        let ov = ProxyOverride::Custom(custom_cfg(
            ProxyProtocol::Http,
            "10.0.0.2",
            8080,
            Some(secret),
            vec![],
        ));
        let rule = ProxyRule::new(ProxyMatcher::default().with_host("example.com"), decision.clone());
        let cfg = ConditionalProxyConfig::new(vec![rule.clone()], ov.clone());

        assert!(decision.has_password());
        assert!(ov.has_password());
        assert!(rule.has_password());
        assert!(cfg.has_password());

        for rendered in [
            format!("{decision:?}"),
            format!("{ov:?}"),
            format!("{rule:?}"),
            format!("{cfg:?}"),
        ] {
            assert!(!rendered.contains(secret), "leaked password: {rendered}");
            assert!(rendered.contains("[REDACTED]"));
        }

        assert!(!decision.without_secret().has_password());
        assert!(!ov.without_secret().has_password());
        assert!(!rule.without_secret().has_password());
        assert!(!cfg.without_secret().has_password());
    }

    #[test]
    fn serde_roundtrip_and_strict_validation() {
        let cfg = ConditionalProxyConfig::new(
            vec![
                ProxyRule::new(
                    ProxyMatcher::default()
                        .with_host("*.seek.li")
                        .with_port(443)
                        .with_scheme(TargetScheme::Https)
                        .with_purpose(ProxyPurpose::Plugin)
                        .with_plugin_id("app.maild.provider.nodeseek-email")
                        .with_tag("api"),
                    ProxyDecision::Use(ep(ProxyProtocol::Socks5h, "127.0.0.1", 7890)),
                )
                .with_name("nodeseek"),
                ProxyRule::new(
                    ProxyMatcher::default().with_host("10.0.0.0/8"),
                    ProxyDecision::Direct,
                ),
            ],
            ProxyOverride::Inherit,
        );
        let json = serde_json::to_string(&cfg).unwrap();
        let parsed: ConditionalProxyConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, cfg);
        assert!(parsed.validate().is_ok());

        // Invalid matcher entries fail validation strictly.
        let bad_port = ConditionalProxyConfig::new(
            vec![ProxyRule::new(
                ProxyMatcher::default().with_port(0),
                ProxyDecision::Direct,
            )],
            ProxyOverride::Inherit,
        );
        assert!(bad_port.validate().is_err());

        let bad_host = ConditionalProxyConfig::new(
            vec![ProxyRule::new(
                ProxyMatcher::default().with_host("10.0.0.0/99"),
                ProxyDecision::Direct,
            )],
            ProxyOverride::Inherit,
        );
        assert!(bad_host.validate().is_err());

        let bad_endpoint = ConditionalProxyConfig::new(
            vec![ProxyRule::new(
                ProxyMatcher::default(),
                ProxyDecision::Use(ep(ProxyProtocol::Http, "127.0.0.1", 0)),
            )],
            ProxyOverride::Inherit,
        );
        assert!(bad_endpoint.validate().is_err());
    }
}
