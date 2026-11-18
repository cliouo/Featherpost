//! filters：`mail/received` waterfall 过滤链的编排者（spine 服务之一）。
//!
//! ARCHITECTURE §5：`mail/received` 是 waterfall——监听器 `next()` 委托下游，
//! 直接返回 = 短路（丢弃/放行）。filters 是这条链上的编排者：
//!
//! 规则按配置顺序执行：
//! - `drop` 规则命中 → 短路 `Reject`（邮件被丢弃，store/otp 看不到）；
//! - `accept` 规则命中（白名单）→ 短路 `Accept`（跳过后续 drop 规则）；
//! - 全部未命中 → `next()` 委托（R4 终端默认 Accept）。
//!
//! 规则可排序、可停用（`enabled = false`），per-account 作用域天然由
//! `scoped(realm)` 事件语义提供（filters 挂在账户 scope 上即只滤该账户）。

use std::any::TypeId;

use cordis::{Context, Plugin, PluginError, Service};
use regex::Regex;
use serde::Deserialize;

use crate::events::{FilterDecision, MailMessage, MailReceived};

#[derive(Clone, Debug, Deserialize, Default)]
pub struct FiltersConfig {
    /// 规则列表（顺序执行，首条命中短路）。
    #[serde(default)]
    pub rules: Vec<Rule>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Rule {
    /// "subject" | "sender" | "body"
    pub field: String,
    /// 子串（默认）或正则（`is_regex = true`）
    pub pattern: String,
    #[serde(default)]
    pub is_regex: bool,
    /// "drop"（默认）| "accept"
    #[serde(default = "default_action")]
    pub action: String,
}

fn default_action() -> String {
    "drop".into()
}

impl Service for FiltersConfig {
    fn name(&self) -> &'static str {
        "filters.config"
    }
}

/// 编译好的规则。
pub(crate) struct CompiledRule {
    field: String,
    pattern: String,
    re: Option<Regex>,
    accept: bool,
}

pub(crate) fn compile(rules: &[Rule]) -> Result<Vec<CompiledRule>, PluginError> {
    rules
        .iter()
        .map(|r| {
            let re = if r.is_regex {
                Some(Regex::new(&r.pattern).map_err(|e| format!("filters 正则无效: {e}"))?)
            } else {
                None
            };
            Ok(CompiledRule {
                field: r.field.clone(),
                pattern: r.pattern.clone(),
                re,
                accept: r.action == "accept",
            })
        })
        .collect()
}

/// 纯判定：返回首条命中规则的决定（无命中 → None，走 next()）。
pub(crate) fn evaluate(msg: &MailMessage, rules: &[CompiledRule]) -> Option<FilterDecision> {
    for r in rules {
        let hay = match r.field.as_str() {
            "subject" => &msg.subject,
            "sender" => &msg.sender,
            _ => &msg.body,
        };
        let hit = match &r.re {
            Some(re) => re.is_match(hay),
            None => hay.contains(&r.pattern),
        };
        if hit {
            return Some(if r.accept {
                FilterDecision::Accept
            } else {
                FilterDecision::Reject
            });
        }
    }
    None
}

/// filters 插件：inject 配置 → 挂到 `mail/received` waterfall 链。
pub struct FiltersPlugin;

impl Plugin for FiltersPlugin {
    fn inject(&self) -> Vec<TypeId> {
        vec![TypeId::of::<FiltersConfig>()]
    }

    fn apply(&self, ctx: &Context) -> Result<(), PluginError> {
        let cfg = ctx.get::<FiltersConfig>().expect("inject 就绪");
        let rules = compile(&cfg.rules)?;

        ctx.on_waterfall_scoped::<MailReceived>(move |msg, next| match evaluate(&msg, &rules) {
            Some(d) => Box::pin(async move { d }),
            None => next(),
        });
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(field: &str, pattern: &str, accept: bool, is_regex: bool) -> Rule {
        Rule {
            field: field.into(),
            pattern: pattern.into(),
            is_regex,
            action: if accept {
                "accept".into()
            } else {
                "drop".into()
            },
        }
    }

    fn msg(sender: &str, subject: &str, body: &str) -> MailMessage {
        MailMessage {
            account: "a".into(),
            uid: "1".into(),
            sender: sender.into(),
            recipient: String::new(),
            subject: subject.into(),
            body: body.into(),
            received_at: 0,
            ..Default::default()
        }
    }

    #[test]
    fn drop_rule_matches_substring() {
        let rules = compile(&[rule("subject", "广告", false, false)]).unwrap();
        assert_eq!(
            evaluate(&msg("s@t", "双十一广告", "b"), &rules),
            Some(FilterDecision::Reject)
        );
        assert_eq!(evaluate(&msg("s@t", "正常邮件", "b"), &rules), None);
    }

    #[test]
    fn accept_whitelist_short_circuits_before_drop() {
        let rules = compile(&[
            rule("sender", "boss@corp", true, false),
            rule("subject", "广告", false, false),
        ])
        .unwrap();
        // 白名单命中 → Accept，跳过后续 drop 规则
        assert_eq!(
            evaluate(&msg("boss@corp", "广告", "b"), &rules),
            Some(FilterDecision::Accept)
        );
        // 非白名单 → drop 命中
        assert_eq!(
            evaluate(&msg("other@corp", "广告", "b"), &rules),
            Some(FilterDecision::Reject)
        );
    }

    #[test]
    fn regex_rule_matches() {
        let rules = compile(&[rule("body", r"验证码[：:]?\s*\d{6}", false, true)]).unwrap();
        assert_eq!(
            evaluate(&msg("s@t", "s", "验证码：123456"), &rules),
            Some(FilterDecision::Reject)
        );
        assert_eq!(evaluate(&msg("s@t", "s", "没码"), &rules), None);
    }

    #[test]
    fn no_hit_delegates_to_next() {
        let rules = compile(&[rule("subject", "广告", false, false)]).unwrap();
        assert_eq!(evaluate(&msg("s@t", "正常", "b"), &rules), None);
    }

    #[test]
    fn invalid_regex_is_compile_error() {
        assert!(compile(&[rule("body", "(unclosed", false, true)]).is_err());
    }
}
