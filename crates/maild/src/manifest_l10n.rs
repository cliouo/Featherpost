//! 包 manifest 的可选 `description` 与 `[l10n.<tag>]` 段（ADR-0048 决策 5）。
//!
//! 扩展与前端的名称、描述会出现在任何一个前端的设置页里，只能由包作者提供翻译。daemon 只做
//! 形状校验并原样透传给 `listFrontends` / `listExternalPlugins` / `previewPluginPackage`，
//! **不按语言偏好挑选**——由前端与自己支持的语言协商。
//!
//! `frontend.toml` 与 `plugin.toml` 都是 `deny_unknown_fields`：旧 daemon 见到这两个键会拒绝
//! 整个包，所以它们只在新的 schema 版本里允许（前端 v3、插件 v2），旧版本包照原规则运行。

use std::collections::BTreeMap;

use maild_client::{PackageL10n, PackageL10nEntry};
use serde::Deserialize;

/// `l10n` 下最多几种语言。
pub(crate) const MAX_L10N_LANGUAGES: usize = 16;
/// 兜底 `description` 与各语言 `description` 的字节上限。
pub(crate) const MAX_DESCRIPTION_BYTES: usize = 1024;

/// manifest 里的一项：只允许 `name` 与 `description`。
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ManifestL10nEntry {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    description: Option<String>,
}

/// manifest 里的 `l10n` 表：键是作者写的 BCP 47 标签（未规范化）。
pub(crate) type ManifestL10n = BTreeMap<String, ManifestL10nEntry>;

/// 兜底描述与各语言文字的校验结果，直接进协议 DTO。
#[derive(Clone, Debug, Default)]
pub(crate) struct PackageText {
    pub description: Option<String>,
    pub l10n: PackageL10n,
}

/// 校验并规范化：标签按 [`normalize_tag`](crate::locale_preference::normalize_tag) 规范大小写，
/// 规范化后重名即拒；每项至少给 `name` 或 `description`；长度上限与兜底字段相同
/// （`name` ≤ `max_name_bytes`，`description` ≤ [`MAX_DESCRIPTION_BYTES`]），不得含控制字符。
pub(crate) fn validate(
    description: Option<&str>,
    l10n: &ManifestL10n,
    max_name_bytes: usize,
) -> Result<PackageText, &'static str> {
    if description.is_some_and(|value| !valid_text(value, MAX_DESCRIPTION_BYTES)) {
        return Err("manifest description is invalid");
    }
    if l10n.len() > MAX_L10N_LANGUAGES {
        return Err("manifest l10n declares too many languages");
    }
    let mut out = PackageL10n::new();
    for (tag, entry) in l10n {
        let tag = crate::locale_preference::normalize_tag(tag)
            .map_err(|_| "manifest l10n language tag is invalid")?;
        let valid = (entry.name.is_some() || entry.description.is_some())
            && entry
                .name
                .as_deref()
                .is_none_or(|value| valid_text(value, max_name_bytes))
            && entry
                .description
                .as_deref()
                .is_none_or(|value| valid_text(value, MAX_DESCRIPTION_BYTES));
        if !valid {
            return Err("manifest l10n entry is invalid");
        }
        let localized = PackageL10nEntry {
            name: entry.name.clone(),
            description: entry.description.clone(),
        };
        if out.insert(tag, localized).is_some() {
            return Err("manifest l10n language tag is duplicated");
        }
    }
    Ok(PackageText {
        description: description.map(str::to_owned),
        l10n: out,
    })
}

fn valid_text(value: &str, max_bytes: usize) -> bool {
    !value.is_empty() && value.len() <= max_bytes && !value.chars().any(char::is_control)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Result<PackageText, String> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Fixture {
            #[serde(default)]
            description: Option<String>,
            #[serde(default)]
            l10n: ManifestL10n,
        }
        let fixture: Fixture = toml::from_str(text).map_err(|error| error.to_string())?;
        validate(fixture.description.as_deref(), &fixture.l10n, 160).map_err(str::to_owned)
    }

    #[test]
    fn tags_are_normalized_and_entries_pass_through_unchanged() {
        let text = parse(
            "description = \"Mail client\"\n\
             [l10n.EN]\ndescription = \"A mail client\"\n\
             [l10n.\"zh-hans\"]\nname = \"邮件\"\ndescription = \"邮件客户端\"\n",
        )
        .unwrap();
        assert_eq!(text.description.as_deref(), Some("Mail client"));
        assert_eq!(
            serde_json::to_value(&text.l10n).unwrap(),
            serde_json::json!({
                "en": { "description": "A mail client" },
                "zh-Hans": { "name": "邮件", "description": "邮件客户端" }
            })
        );
        assert!(parse("").unwrap().l10n.is_empty());
    }

    #[test]
    fn malformed_l10n_is_rejected() {
        // `en-000` … `en-016`：合法的「语言 + 3 位数字地区」标签，共 17 种。
        let languages = |count: usize| {
            (0..count)
                .map(|index| format!("[l10n.\"en-{index:03}\"]\nname = \"n\"\n"))
                .collect::<String>()
        };
        let many = languages(MAX_L10N_LANGUAGES + 1);
        for (text, why) in [
            ("[l10n.\"not a tag\"]\nname = \"n\"\n", "非法标签"),
            ("[l10n.en]\nname = \"n\"\n[l10n.EN]\nname = \"m\"\n", "规范化后重名"),
            ("[l10n.en]\n", "空项"),
            ("[l10n.en]\nname = \"\"\n", "空名称"),
            ("[l10n.en]\nname = \"a\\nb\"\n", "控制字符"),
            ("[l10n.en]\ntitle = \"n\"\n", "未知键"),
            ("description = \"\"\n", "空兜底描述"),
            (many.as_str(), "超过 16 种语言"),
        ] {
            assert!(parse(text).is_err(), "{why}");
        }
        let long_name = format!("[l10n.en]\nname = \"{}\"\n", "n".repeat(161));
        assert!(parse(&long_name).is_err(), "名称超长");
        let long_description =
            format!("[l10n.en]\ndescription = \"{}\"\n", "d".repeat(MAX_DESCRIPTION_BYTES + 1));
        assert!(parse(&long_description).is_err(), "描述超长");
        let exactly = languages(MAX_L10N_LANGUAGES);
        assert_eq!(parse(&exactly).unwrap().l10n.len(), MAX_L10N_LANGUAGES);
    }
}
