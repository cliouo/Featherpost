//! Plain-text email block extractor and structured renderer.
//!
//! Splits plain-text email bodies into clean paragraphs, assigns `id = 0` to the
//! subject (when non-empty) so it is always translated in batch 1, and renders
//! structured original/translated blocks for safe React text rendering (zero HTML).

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::error::TranslateError;
use crate::types::{
    validate_batch_response, TextBlock, TranslatedBlock, DEFAULT_MAX_EXPANSION_RATIO,
};

/// Result of extracting translatable blocks from a plain-text email.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtractedPlainText {
    /// Original trimmed subject (if non-empty).
    pub subject: Option<String>,
    /// Ordered non-empty body paragraphs extracted from the plain-text body.
    pub paragraphs: Vec<String>,
    /// Ordered [`TextBlock`]s ready for [`crate::TranslationEngine`].
    /// `id = 0` is reserved for `subject` (when present); body paragraphs start at `id = 1`.
    pub blocks: Vec<TextBlock>,
    /// Mapping from each paragraph index to its block `id`.
    pub paragraph_block_ids: Vec<u32>,
}

impl ExtractedPlainText {
    /// Concatenated translatable body text (excluding empty spacing) suitable for
    /// lightweight language detection.
    pub fn body_text_for_detection(&self) -> String {
        self.paragraphs.join("\n\n")
    }
}

/// A single structured paragraph pair for safe React text rendering.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlainTextBlockView {
    pub id: u32,
    pub original: String,
    pub translated: String,
}

/// Structured rendered result for a translated plain-text email.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RenderedPlainText {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub original_subject: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub translated_subject: Option<String>,
    pub blocks: Vec<PlainTextBlockView>,
}

/// Extract clean paragraphs from a plain-text email body plus optional subject.
///
/// - Normalizes `\r\n` and `\r` to `\n`.
/// - Splits on one or more blank lines, preserving intra-paragraph single newlines.
/// - Includes non-empty `subject` as `id = 0` (always in batch 1).
/// - Assigns sequential IDs `1..` to non-empty body paragraphs.
pub fn extract_plain_text(body: &str, subject: Option<&str>) -> ExtractedPlainText {
    let normalized_subject = subject
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned);

    let normalized_body = body.replace("\r\n", "\n").replace('\r', "\n");

    let mut paragraphs = Vec::new();
    let mut current_lines: Vec<&str> = Vec::new();

    for line in normalized_body.split('\n') {
        if line.trim().is_empty() {
            if !current_lines.is_empty() {
                let para = join_paragraph_lines(&current_lines);
                if !para.is_empty() {
                    paragraphs.push(para);
                }
                current_lines.clear();
            }
        } else {
            current_lines.push(line.trim_end());
        }
    }
    if !current_lines.is_empty() {
        let para = join_paragraph_lines(&current_lines);
        if !para.is_empty() {
            paragraphs.push(para);
        }
    }

    let mut blocks = Vec::with_capacity(paragraphs.len() + usize::from(normalized_subject.is_some()));
    if let Some(ref subj) = normalized_subject {
        blocks.push(TextBlock {
            id: 0,
            text: subj.clone(),
            anchors: Vec::new(),
        });
    }

    let mut paragraph_block_ids = Vec::with_capacity(paragraphs.len());
    for (idx, para) in paragraphs.iter().enumerate() {
        let id = (idx as u32).saturating_add(1);
        paragraph_block_ids.push(id);
        blocks.push(TextBlock {
            id,
            text: para.clone(),
            anchors: Vec::new(),
        });
    }

    ExtractedPlainText {
        subject: normalized_subject,
        paragraphs,
        blocks,
        paragraph_block_ids,
    }
}

fn join_paragraph_lines(lines: &[&str]) -> String {
    let joined = lines.join("\n");
    joined.trim().to_string()
}

/// Validate and assemble translated blocks into a structured [`RenderedPlainText`] view.
///
/// Validates unique & complete IDs via [`validate_batch_response`] before returning.
pub fn render_plain_text(
    extracted: &ExtractedPlainText,
    translated: &[TranslatedBlock],
) -> Result<RenderedPlainText, TranslateError> {
    let validated =
        validate_batch_response(&extracted.blocks, translated, DEFAULT_MAX_EXPANSION_RATIO)?;
    let by_id: HashMap<u32, String> = validated
        .into_iter()
        .map(|item| (item.id, item.text))
        .collect();

    let translated_subject = if extracted.subject.is_some() {
        by_id.get(&0).cloned()
    } else {
        None
    };

    let mut blocks = Vec::with_capacity(extracted.paragraphs.len());
    for (para, &block_id) in extracted
        .paragraphs
        .iter()
        .zip(extracted.paragraph_block_ids.iter())
    {
        let tr = by_id.get(&block_id).cloned().ok_or_else(|| {
            TranslateError::InvalidResponse("missing translated plain-text paragraph".into())
        })?;
        blocks.push(PlainTextBlockView {
            id: block_id,
            original: para.clone(),
            translated: tr,
        });
    }

    Ok(RenderedPlainText {
        original_subject: extracted.subject.clone(),
        translated_subject,
        blocks,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_subject_as_id_zero_and_splits_paragraphs() {
        let extracted = extract_plain_text(
            "Hello team,\r\nPlease review the report.\r\n\r\n   \r\nThanks,\nAlice\n\n",
            Some("  Weekly Sync  "),
        );
        assert_eq!(extracted.subject.as_deref(), Some("Weekly Sync"));
        assert_eq!(
            extracted.paragraphs,
            vec!["Hello team,\nPlease review the report.", "Thanks,\nAlice"]
        );
        assert_eq!(extracted.blocks.len(), 3);
        assert_eq!(extracted.blocks[0].id, 0);
        assert_eq!(extracted.blocks[0].text, "Weekly Sync");
        assert_eq!(extracted.blocks[1].id, 1);
        assert_eq!(extracted.blocks[2].id, 2);
    }

    #[test]
    fn renders_structured_blocks_and_rejects_missing_ids() {
        let extracted = extract_plain_text("First para\n\nSecond para", Some("Subject"));
        let rendered = render_plain_text(
            &extracted,
            &[
                TranslatedBlock {
                    id: 2,
                    text: "第二段".into(),
                },
                TranslatedBlock {
                    id: 0,
                    text: "主题".into(),
                },
                TranslatedBlock {
                    id: 1,
                    text: "第一段".into(),
                },
            ],
        )
        .unwrap();
        assert_eq!(rendered.original_subject.as_deref(), Some("Subject"));
        assert_eq!(rendered.translated_subject.as_deref(), Some("主题"));
        assert_eq!(rendered.blocks.len(), 2);
        assert_eq!(rendered.blocks[0].original, "First para");
        assert_eq!(rendered.blocks[0].translated, "第一段");
        assert_eq!(rendered.blocks[1].translated, "第二段");

        // Missing block id 2 must fail closed.
        let err = render_plain_text(
            &extracted,
            &[
                TranslatedBlock {
                    id: 0,
                    text: "主题".into(),
                },
                TranslatedBlock {
                    id: 1,
                    text: "第一段".into(),
                },
            ],
        );
        assert!(matches!(err, Err(TranslateError::InvalidResponse(_))));
    }
}
