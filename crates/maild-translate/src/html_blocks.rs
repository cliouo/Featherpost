//! Implicit-close-safe two-pass HTML block extractor and rewriter built on `lol_html` 2.9.0.
//!
//! ## Safety & Correctness Invariants
//! 1. **HTML5 Implicit-Closure Safety**: `lol_html` 2.9.0 does NOT invoke `Element::on_end_tag`
//!    for elements whose closing tags are legally omitted (`<p>`, `<li>`, `<td>`, `<tr>`, `<dt>`,
//!    `<dd>`, `<head>`, etc.). Rather than tracking a naive `skip_depth` counter or relying on
//!    `end.before`, Pass 1 maintains an explicit HTML5-aware open-element stack where each frame
//!    stores its own `skipped: bool` and applies HTML5 implicit-closure rules on start tags.
//!    Pass 1 records exact ground truth of every `elem_id`, `text_node_id`, explicit end tag, and
//!    provably safe segment boundary before Pass 2 begins.
//! 2. **Deterministic Lockstep Two-Pass Execution**: Both passes operate on the exact same
//!    immutable, already-sanitized HTML string. Pass 2 never calls `el.remove()` on elements
//!    containing inner text (using `el.remove_and_keep_content()` + text node removal instead),
//!    guaranteeing identical `elem_id` and `text_node_id` numbering from byte 0 to EOF.
//! 3. **Prompt-Injection & XSS Defense**:
//!    - Skips non-translatable subtrees (`head, title, style, script, noscript, code, pre, kbd,
//!      samp, var, svg, math, textarea, select, option, template`, `hidden`, `aria-hidden="true"`,
//!      and CSS `display:none`, `visibility:hidden`, `font-size:0`).
//!    - Strips any spoofed sender `data-fp-tr` attributes in Pass 2 so ONLY Rust-generated
//!      translation containers carry `data-fp-tr="1"`.
//!    - Extracts inline `<a href=".../link/<token>/<n>">` anchors into per-block placeholders
//!      `[[k]]...[[/k]]` whose slot IDs are chosen strictly disjoint from any literal `[[N]]`
//!      already present in the original email text. Real `/link/<token>/<n>` URLs stay strictly
//!      in Rust and never leave the process.
//!    - Validates returned IDs (unique, complete, expansion-bounded) and validates that registered
//!      placeholder pairs are balanced, unique, non-nested, and in-bounds. Malformed placeholder
//!      output safely degrades to plain HTML-escaped text (zero raw LLM HTML ever enters the DOM).
//! 4. **Provable Attribution & Safe Fallback**:
//!    - Preserves `<img>` elements intact in both bilingual and translated-only modes.
//!    - Preserves single-CTA `<a>` elements (including button styling and `href`) in translated-only
//!      mode while replacing only their inner text via `ContentType::Text`.
//!    - Whenever safe in-place attribution cannot be proven, preserves the original block intact
//!      and renders the escaped translation in a safe dedicated fallback container.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use lol_html::html_content::{ContentType, Element, EndTag};
use lol_html::{doc_text, element, RewriteStrSettings};

use crate::error::TranslateError;
use crate::types::{
    validate_batch_response, AnchorSlot, TextBlock, TranslatedBlock, DEFAULT_MAX_EXPANSION_RATIO,
};

/// Trusted HTML attribute stamped ONLY on Rust-generated translation containers.
/// Sender HTML attributes with this name are stripped by `maild-ui::html::prepare` AND Pass 2.
pub const TRUSTED_TR_ATTR: &str = "data-fp-tr";
pub const TRUSTED_TR_VALUE: &str = "1";

/// Inline style applied to bilingual / fallback translation containers in light mode.
/// `maild-ui::html::dark_reading_html` recognizes `data-fp-tr="1"` and applies high-priority
/// dark-mode inline `!important` styles over this.
const BILINGUAL_LIGHT_STYLE: &str = "display:block;margin-top:4px;padding-left:8px;\
border-left:2px solid #3b82f6;color:#374151;font-size:0.95em;line-height:1.5;\
font-weight:normal;font-style:normal;text-decoration:none;";

/// Extracted plan from Pass 1 over an immutable already-sanitized HTML document.
#[derive(Clone, Debug)]
pub struct ExtractedHtmlBlocks {
    /// Original trimmed subject (if non-empty).
    pub subject: Option<String>,
    /// Ordered blocks ready for [`crate::TranslationEngine`] (`id = 0` is subject if present).
    pub blocks: Vec<TextBlock>,
    /// Internal per-segment rewrite metadata (keeps `/link/<token>/<n>` strictly in Rust).
    segments: Vec<SegmentPlan>,
}

impl ExtractedHtmlBlocks {
    /// Concatenated translatable body text (excluding skipped tags/hidden subtrees) suitable for
    /// lightweight language detection.
    pub fn body_text_for_detection(&self) -> String {
        let mut out = String::new();
        for block in &self.blocks {
            if block.id == 0 && self.subject.is_some() {
                continue;
            }
            if !out.is_empty() {
                out.push('\n');
            }
            // Strip placeholder wrappers when sampling for language detection so markers don't
            // skew token/character ratios.
            out.push_str(&strip_registered_placeholders(&block.text, &block.anchors));
        }
        out
    }

    /// Number of translatable HTML body segments extracted (excluding subject).
    pub fn body_segment_count(&self) -> usize {
        self.segments.len()
    }
}

/// Rendered HTML output for both bilingual and translated-only views.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RenderedHtmlTranslation {
    pub translated_subject: Option<String>,
    pub bilingual_html: String,
    pub translated_html: String,
}

#[derive(Clone, Debug)]
struct RegisteredLink {
    slot_id: u32,
    safe_href: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BilingualAnchor {
    AfterTextNode(usize),
    AfterEndTag(usize),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TranslatedInsertPoint {
    ReplaceTextNode(usize),
    BeforeElement(usize),
}

#[derive(Clone, Debug)]
enum RewriteStrategy {
    /// Simple contiguous text (plus optional `<br>` / closed inline formatting), no `<a>` tags.
    InlineText {
        first_tid: usize,
        remove_tids: Vec<usize>,
        remove_br_elem_ids: Vec<usize>,
        bilingual_anchor: BilingualAnchor,
    },
    /// All text in the segment belongs to a single `<a>` (e.g. CTA button). Keeps the `<a>`
    /// element and all its attributes/styling intact and replaces only inner text.
    SingleCtaLink {
        first_tid: usize,
        remove_tids: Vec<usize>,
        remove_br_elem_ids: Vec<usize>,
        bilingual_anchor: BilingualAnchor,
    },
    /// Mixed prose + one or more verified inline `/link/<token>/<n>` `<a>` elements that opened
    /// and closed cleanly inside this segment without non-text children.
    MixedInlineLinks {
        insert_point: TranslatedInsertPoint,
        remove_tids: Vec<usize>,
        unwrap_elem_ids: Vec<usize>,
        remove_br_elem_ids: Vec<usize>,
        links: Vec<RegisteredLink>,
        bilingual_anchor: BilingualAnchor,
    },
    /// Attribution or in-place replacement could not be proven safe (e.g. unclosed or
    /// cross-boundary `<a>`). Preserves the original HTML intact in BOTH modes and renders
    /// the escaped translation in a dedicated fallback container.
    FallbackContainer {
        bilingual_anchor: BilingualAnchor,
    },
}

#[derive(Clone, Debug)]
struct SegmentPlan {
    block_id: u32,
    strategy: RewriteStrategy,
}

#[derive(Clone, Debug)]
struct OpenFrame {
    elem_id: usize,
    tag: String,
    skipped: bool,
    is_block: bool,
    is_link: bool,
    contains_non_text_child: bool,
    /// Segment generation active when this element opened (used to check whether an inline
    /// element opened inside the current segment vs enclosing it).
    opened_in_segment: u64,
}

#[derive(Clone, Debug)]
enum RawToken {
    Text {
        tid: usize,
        decoded: String,
        active_link: Option<ActiveLinkRef>,
    },
    Br {
        elem_id: usize,
        active_link: Option<ActiveLinkRef>,
    },
    LinkOpen {
        elem_id: usize,
        safe_href: Option<String>,
        link_index: Option<u32>,
    },
    LinkClose {
        elem_id: usize,
    },
    InlineClose {
        elem_id: usize,
        opened_in_segment: u64,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ActiveLinkRef {
    elem_id: usize,
    opened_in_segment: u64,
}

struct Pass1State {
    link_prefix: String,
    open_stack: Vec<OpenFrame>,
    explicit_end_tags: HashSet<usize>,
    link_had_non_text_child: HashSet<usize>,
    next_elem_id: usize,
    next_text_node_id: usize,
    text_buf: String,
    current_segment_gen: u64,
    current_tokens: Vec<RawToken>,
    finished_raw_segments: Vec<(u64, Vec<RawToken>)>,
}

impl Pass1State {
    fn new(link_prefix: &str) -> Self {
        Self {
            link_prefix: link_prefix.to_string(),
            open_stack: Vec::new(),
            explicit_end_tags: HashSet::new(),
            link_had_non_text_child: HashSet::new(),
            next_elem_id: 0,
            next_text_node_id: 0,
            text_buf: String::new(),
            current_segment_gen: 1,
            current_tokens: Vec::new(),
            finished_raw_segments: Vec::new(),
        }
    }

    fn is_skipped(&self) -> bool {
        self.open_stack.iter().any(|f| f.skipped)
    }

    fn current_active_link(&self) -> Option<ActiveLinkRef> {
        self.open_stack.iter().rev().find(|f| f.is_link).map(|f| ActiveLinkRef {
            elem_id: f.elem_id,
            opened_in_segment: f.opened_in_segment,
        })
    }

    fn mark_enclosing_links_non_text(&mut self) {
        for frame in &mut self.open_stack {
            if frame.is_link {
                frame.contains_non_text_child = true;
                self.link_had_non_text_child.insert(frame.elem_id);
            }
        }
    }

    fn flush_segment(&mut self) {
        if !self.current_tokens.is_empty() {
            let tokens = std::mem::take(&mut self.current_tokens);
            self.finished_raw_segments
                .push((self.current_segment_gen, tokens));
        }
        self.current_segment_gen += 1;
    }

    fn apply_implicit_closures(&mut self, incoming_tag: &str) {
        // Any non-head tag implicitly closes an unclosed <head>.
        if !is_head_allowed_tag(incoming_tag) && self.open_stack.iter().any(|f| f.tag == "head") {
            self.pop_down_to_tag("head", &[]);
        }

        match incoming_tag {
            "li" => {
                self.pop_down_to_any(&["li"], &["ul", "ol", "table", "td", "th", "html", "body"]);
            }
            "dt" | "dd" => {
                self.pop_down_to_any(&["dt", "dd"], &["dl", "table", "td", "th", "html", "body"]);
            }
            "td" | "th" => {
                self.pop_down_to_any(&["td", "th"], &["tr", "table", "html", "body"]);
            }
            "tr" => {
                self.pop_down_to_any(
                    &["td", "th", "tr"],
                    &["table", "thead", "tbody", "tfoot", "html", "body"],
                );
            }
            "thead" | "tbody" | "tfoot" => {
                self.pop_down_to_any(
                    &["td", "th", "tr", "thead", "tbody", "tfoot"],
                    &["table", "html", "body"],
                );
            }
            "colgroup" => {
                self.pop_down_to_any(&["colgroup"], &["table", "html", "body"]);
            }
            "option" => {
                self.pop_down_to_any(&["option"], &["select", "optgroup", "html", "body"]);
            }
            "optgroup" => {
                self.pop_down_to_any(&["option", "optgroup"], &["select", "html", "body"]);
            }
            "a" => {
                // HTML5 forbids nested <a>.
                self.pop_down_to_any(&["a"], &["table", "td", "th", "html", "body"]);
            }
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                self.pop_down_to_any(
                    &["p", "h1", "h2", "h3", "h4", "h5", "h6"],
                    &["table", "td", "th", "caption", "html", "body"],
                );
            }
            _ => {}
        }

        // Opening any block element that implicitly closes <p>.
        if closes_open_p(incoming_tag) {
            self.pop_down_to_any(&["p"], &["table", "td", "th", "caption", "html", "body"]);
        }
    }

    fn pop_down_to_tag(&mut self, target: &str, scope_stop: &[&str]) {
        self.pop_down_to_any(&[target], scope_stop);
    }

    fn pop_down_to_any(&mut self, targets: &[&str], scope_stop: &[&str]) {
        let mut found_idx = None;
        for (idx, frame) in self.open_stack.iter().enumerate().rev() {
            if targets.contains(&frame.tag.as_str()) {
                found_idx = Some(idx);
                break;
            }
            if scope_stop.contains(&frame.tag.as_str()) {
                break;
            }
        }
        if let Some(idx) = found_idx {
            let mut popped_block_or_skip = false;
            while self.open_stack.len() > idx {
                if let Some(popped) = self.open_stack.pop() {
                    if popped.is_block || popped.skipped {
                        popped_block_or_skip = true;
                    }
                    if popped.contains_non_text_child {
                        self.link_had_non_text_child.insert(popped.elem_id);
                    }
                }
            }
            if popped_block_or_skip {
                self.flush_segment();
            }
        }
    }

    fn on_explicit_end(&mut self, elem_id: usize) {
        self.explicit_end_tags.insert(elem_id);
        let Some(pos) = self.open_stack.iter().rposition(|f| f.elem_id == elem_id) else {
            // Already implicitly closed earlier — safe no-op!
            return;
        };
        let mut flushed = false;
        while self.open_stack.len() > pos {
            if let Some(popped) = self.open_stack.pop() {
                if popped.contains_non_text_child {
                    self.link_had_non_text_child.insert(popped.elem_id);
                }
                if popped.is_block || popped.skipped {
                    self.flush_segment();
                    flushed = true;
                } else if !self.is_skipped() && !flushed {
                    if popped.is_link {
                        self.current_tokens.push(RawToken::LinkClose {
                            elem_id: popped.elem_id,
                        });
                    } else {
                        self.current_tokens.push(RawToken::InlineClose {
                            elem_id: popped.elem_id,
                            opened_in_segment: popped.opened_in_segment,
                        });
                    }
                }
            }
        }
    }
}

/// Pass 1: Extract translatable blocks and provably safe rewrite plans from an already-sanitized
/// HTML email document.
///
/// - `html`: Immutable already-sanitized HTML string (output of `maild-ui::html::prepare`).
/// - `subject`: Optional email subject (included as `id = 0` when non-empty).
/// - `link_prefix`: Active document's safe link prefix (e.g. `featherpost-mail://localhost/link/<token>/`).
pub fn extract_html_blocks(
    html: &str,
    subject: Option<&str>,
    link_prefix: &str,
) -> Result<ExtractedHtmlBlocks, TranslateError> {
    let state = Rc::new(RefCell::new(Pass1State::new(link_prefix)));

    let el_state = Rc::clone(&state);
    let txt_state = Rc::clone(&state);

    lol_html::rewrite_str(
        html,
        RewriteStrSettings {
            element_content_handlers: vec![element!("*", move |el| {
                let mut st = el_state.borrow_mut();
                let elem_id = st.next_elem_id;
                st.next_elem_id += 1;

                let tag = el.tag_name().to_ascii_lowercase();
                st.apply_implicit_closures(&tag);

                let skipped = is_skip_element(&tag, el);
                let is_block = is_block_element(&tag);
                let is_replaced = is_replaced_or_media_element(&tag);

                if skipped || is_block || is_replaced {
                    if is_replaced || skipped {
                        st.mark_enclosing_links_non_text();
                    }
                    st.flush_segment();
                }

                if tag == "br" {
                    if !st.is_skipped() && !skipped {
                        let active_link = st.current_active_link();
                        st.current_tokens.push(RawToken::Br {
                            elem_id,
                            active_link,
                        });
                    }
                    return Ok(());
                }

                let is_link = tag == "a";
                let (safe_href, link_index) = if is_link {
                    parse_registered_link(el, &st.link_prefix)
                } else {
                    (None, None)
                };

                if is_link && !st.is_skipped() && !skipped {
                    st.current_tokens.push(RawToken::LinkOpen {
                        elem_id,
                        safe_href: safe_href.clone(),
                        link_index,
                    });
                }

                if !is_void_element(&tag) && !el.is_self_closing() {
                    let opened_in_segment = st.current_segment_gen;
                    st.open_stack.push(OpenFrame {
                        elem_id,
                        tag,
                        skipped,
                        is_block,
                        is_link,
                        contains_non_text_child: false,
                        opened_in_segment,
                    });

                    let end_state = Rc::clone(&el_state);
                    el.on_end_tag(Box::new(move |_| {
                        end_state.borrow_mut().on_explicit_end(elem_id);
                        Ok(())
                    }))?;
                }

                Ok(())
            })],
            document_content_handlers: vec![doc_text!(move |chunk| {
                let mut st = txt_state.borrow_mut();
                st.text_buf.push_str(chunk.as_str());
                if chunk.last_in_text_node() {
                    let tid = st.next_text_node_id;
                    st.next_text_node_id += 1;
                    let raw = std::mem::take(&mut st.text_buf);
                    if !st.is_skipped() && !raw.is_empty() {
                        let decoded = decode_html_entities(&raw);
                        let active_link = st.current_active_link();
                        st.current_tokens.push(RawToken::Text {
                            tid,
                            decoded,
                            active_link,
                        });
                    }
                }
                Ok(())
            })],
            ..RewriteStrSettings::new()
        },
    )
    .map_err(|_| TranslateError::InvalidConfig("failed to parse sanitized HTML".into()))?;

    let mut state = state.borrow_mut();
    state.flush_segment();

    let normalized_subject = subject
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned);

    let mut blocks = Vec::new();
    if let Some(ref subj) = normalized_subject {
        blocks.push(TextBlock {
            id: 0,
            text: subj.clone(),
            anchors: Vec::new(),
        });
    }

    let mut segments = Vec::new();
    let mut next_block_id: u32 = 1;

    let raw_segments = std::mem::take(&mut state.finished_raw_segments);
    for (seg_gen, tokens) in raw_segments {
        if let Some((block, plan)) = build_segment_plan(
            next_block_id,
            seg_gen,
            &tokens,
            &state.explicit_end_tags,
            &state.link_had_non_text_child,
        ) {
            blocks.push(block);
            segments.push(plan);
            next_block_id = next_block_id.saturating_add(1);
        }
    }

    Ok(ExtractedHtmlBlocks {
        subject: normalized_subject,
        blocks,
        segments,
    })
}

fn build_segment_plan(
    block_id: u32,
    seg_gen: u64,
    tokens: &[RawToken],
    explicit_end_tags: &HashSet<usize>,
    link_had_non_text_child: &HashSet<usize>,
) -> Option<(TextBlock, SegmentPlan)> {
    // Find first and last non-whitespace text tokens.
    let first_meaningful_idx = tokens.iter().position(|t| match t {
        RawToken::Text { decoded, .. } => !decoded.trim().is_empty(),
        _ => false,
    })?;
    let last_meaningful_idx = tokens.iter().rposition(|t| match t {
        RawToken::Text { decoded, .. } => !decoded.trim().is_empty(),
        _ => false,
    })?;

    // Check if there is at least one alphabetic character so decorative punctuation/separators
    // like "|", "•", or "—" are left untouched.
    let has_translatable_chars = tokens[first_meaningful_idx..=last_meaningful_idx]
        .iter()
        .any(|t| match t {
            RawToken::Text { decoded, .. } => decoded.chars().any(char::is_alphabetic),
            _ => false,
        });
    if !has_translatable_chars {
        return None;
    }

    // Collect meaningful text nodes and trailing close tags right after last_meaningful_idx.
    let mut text_tids = Vec::new();
    let mut br_elem_ids = Vec::new();
    let mut link_ids_in_text: Vec<Option<ActiveLinkRef>> = Vec::new();

    for token in &tokens[first_meaningful_idx..=last_meaningful_idx] {
        match token {
            RawToken::Text {
                tid, active_link, ..
            } => {
                text_tids.push(*tid);
                link_ids_in_text.push(active_link.clone());
            }
            RawToken::Br { elem_id, .. } => {
                br_elem_ids.push(*elem_id);
            }
            _ => {}
        }
    }

    let first_tid = *text_tids.first()?;
    let last_tid = *text_tids.last()?;

    // Determine the best safe insertion point for bilingual mode:
    // If an inline element or <a> that opened inside this segment explicitly closes right after
    // the last meaningful text node, insert AFTER that closing tag so we don't nest the
    // translation inside <b> or <a>. Otherwise insert right AFTER the last text node —
    // completely independent of whether the enclosing <p>/<li>/<td> ever has an explicit end tag!
    let mut bilingual_anchor = BilingualAnchor::AfterTextNode(last_tid);
    for token in &tokens[last_meaningful_idx + 1..] {
        match token {
            RawToken::LinkClose { elem_id } if explicit_end_tags.contains(elem_id) => {
                bilingual_anchor = BilingualAnchor::AfterEndTag(*elem_id);
            }
            RawToken::InlineClose {
                elem_id,
                opened_in_segment,
            } if *opened_in_segment == seg_gen && explicit_end_tags.contains(elem_id) => {
                bilingual_anchor = BilingualAnchor::AfterEndTag(*elem_id);
            }
            _ => {}
        }
    }

    let any_link_present = link_ids_in_text.iter().any(Option::is_some);

    // Case 1: Pure text segment (no <a> links at all).
    if !any_link_present {
        let plain = assemble_plain_segment_text(&tokens[first_meaningful_idx..=last_meaningful_idx]);
        if plain.is_empty() {
            return None;
        }
        let remove_tids = text_tids[1..].to_vec();
        return Some((
            TextBlock {
                id: block_id,
                text: plain,
                anchors: Vec::new(),
            },
            SegmentPlan {
                block_id,
                strategy: RewriteStrategy::InlineText {
                    first_tid,
                    remove_tids,
                    remove_br_elem_ids: br_elem_ids,
                    bilingual_anchor,
                },
            },
        ));
    }

    // Case 2: Single-CTA link segment (every meaningful text node is inside the SAME <a>).
    let first_link = link_ids_in_text.first().and_then(|o| o.as_ref());
    let all_in_same_link = first_link.is_some()
        && link_ids_in_text
            .iter()
            .all(|l| l.as_ref().map(|r| r.elem_id) == first_link.map(|r| r.elem_id));

    if all_in_same_link {
        let plain = assemble_plain_segment_text(&tokens[first_meaningful_idx..=last_meaningful_idx]);
        if plain.is_empty() {
            return None;
        }
        let link_ref = first_link.unwrap();
        // If this <a> opened before the segment (e.g., <a><img/> Caption</a>), keep bilingual
        // anchor after the text node or after </a> if explicitly closed.
        if link_ref.opened_in_segment == seg_gen && explicit_end_tags.contains(&link_ref.elem_id) {
            if let Some(RawToken::LinkClose { elem_id }) = tokens[last_meaningful_idx + 1..]
                .iter()
                .find(|t| matches!(t, RawToken::LinkClose { elem_id } if *elem_id == link_ref.elem_id))
            {
                bilingual_anchor = BilingualAnchor::AfterEndTag(*elem_id);
            }
        }
        let remove_tids = text_tids[1..].to_vec();
        return Some((
            TextBlock {
                id: block_id,
                text: plain,
                anchors: Vec::new(),
            },
            SegmentPlan {
                block_id,
                strategy: RewriteStrategy::SingleCtaLink {
                    first_tid,
                    remove_tids,
                    remove_br_elem_ids: br_elem_ids,
                    bilingual_anchor,
                },
            },
        ));
    }

    // Case 3: Mixed prose + inline <a> link(s).
    // Verify provable safety:
    // - No text node belongs to an <a> that opened BEFORE this segment.
    // - Every <a> opened in this segment has a valid registered /link/<token>/<n> href,
    //   closed explicitly inside this segment, and contained no non-text children (like <img>).
    let mut link_opens: HashMap<usize, (String, u32)> = HashMap::new();
    let mut link_closes: HashSet<usize> = HashSet::new();
    let mut open_order: Vec<usize> = Vec::new();
    let mut provably_safe = true;

    for token in tokens {
        match token {
            RawToken::LinkOpen {
                elem_id,
                safe_href,
                link_index,
            } => match (safe_href, link_index) {
                (Some(href), Some(idx))
                    if explicit_end_tags.contains(elem_id)
                        && !link_had_non_text_child.contains(elem_id) =>
                {
                    link_opens.insert(*elem_id, (href.clone(), *idx));
                    open_order.push(*elem_id);
                }
                _ => {
                    provably_safe = false;
                }
            },
            RawToken::LinkClose { elem_id } => {
                link_closes.insert(*elem_id);
            }
            RawToken::Text {
                active_link: Some(link_ref),
                ..
            } if link_ref.opened_in_segment != seg_gen
                || !link_opens.contains_key(&link_ref.elem_id) =>
            {
                provably_safe = false;
            }
            _ => {}
        }
    }

    for elem_id in &open_order {
        if !link_closes.contains(elem_id) {
            provably_safe = false;
        }
    }

    if !provably_safe {
        // Case 4: Unprovable attribution — preserve original HTML completely intact and attach
        // escaped translation in a safe fallback container.
        let plain = assemble_plain_segment_text(&tokens[first_meaningful_idx..=last_meaningful_idx]);
        if plain.is_empty() {
            return None;
        }
        return Some((
            TextBlock {
                id: block_id,
                text: plain,
                anchors: Vec::new(),
            },
            SegmentPlan {
                block_id,
                strategy: RewriteStrategy::FallbackContainer { bilingual_anchor },
            },
        ));
    }

    // Collect all literal `[[N]]` / `[[/N]]` numbers already present in the original email text
    // so our real link placeholder slot IDs are guaranteed disjoint from literal email text!
    let mut used_slot_nums: HashSet<u32> = HashSet::new();
    for token in tokens {
        if let RawToken::Text { decoded, .. } = token {
            collect_literal_marker_numbers(decoded, &mut used_slot_nums);
        }
    }

    let mut next_slot_id: u32 = 1;
    let mut elem_to_slot: HashMap<usize, u32> = HashMap::new();
    let mut anchors: Vec<AnchorSlot> = Vec::new();
    let mut registered_links: Vec<RegisteredLink> = Vec::new();

    // Assign slot IDs only to links that actually have non-empty text within the meaningful span.
    let mut links_with_text: HashSet<usize> = HashSet::new();
    for token in &tokens[first_meaningful_idx..=last_meaningful_idx] {
        if let RawToken::Text {
            decoded,
            active_link: Some(link_ref),
            ..
        } = token
        {
            if !decoded.trim().is_empty() {
                links_with_text.insert(link_ref.elem_id);
            }
        }
    }

    for &elem_id in &open_order {
        if !links_with_text.contains(&elem_id) {
            continue;
        }
        while used_slot_nums.contains(&next_slot_id) {
            next_slot_id = next_slot_id.saturating_add(1);
        }
        let slot_id = next_slot_id;
        used_slot_nums.insert(slot_id);
        next_slot_id = next_slot_id.saturating_add(1);

        let (safe_href, link_index) = link_opens.get(&elem_id)?.clone();
        elem_to_slot.insert(elem_id, slot_id);
        anchors.push(AnchorSlot {
            slot_id,
            link_index,
        });
        registered_links.push(RegisteredLink {
            slot_id,
            safe_href,
        });
    }

    // Build placeholder-annotated text (`Hello [[1]]click here[[/1]] today.`).
    let mut annotated = String::new();
    let mut open_slot: Option<u32> = None;

    for token in &tokens[first_meaningful_idx..=last_meaningful_idx] {
        match token {
            RawToken::Text {
                decoded,
                active_link,
                ..
            } => {
                let target_slot = active_link
                    .as_ref()
                    .and_then(|r| elem_to_slot.get(&r.elem_id).copied());
                if open_slot != target_slot {
                    if let Some(prev) = open_slot.take() {
                        annotated.push_str(&format!("[[/{prev}]]"));
                    }
                    if let Some(next) = target_slot {
                        annotated.push_str(&format!("[[{next}]]"));
                        open_slot = Some(next);
                    }
                }
                annotated.push_str(decoded);
            }
            RawToken::Br { active_link, .. } => {
                let target_slot = active_link
                    .as_ref()
                    .and_then(|r| elem_to_slot.get(&r.elem_id).copied());
                if open_slot != target_slot {
                    if let Some(prev) = open_slot.take() {
                        annotated.push_str(&format!("[[/{prev}]]"));
                    }
                    if let Some(next) = target_slot {
                        annotated.push_str(&format!("[[{next}]]"));
                        open_slot = Some(next);
                    }
                }
                annotated.push('\n');
            }
            _ => {}
        }
    }
    if let Some(prev) = open_slot.take() {
        annotated.push_str(&format!("[[/{prev}]]"));
    }

    let trimmed = collapse_segment_whitespace(&annotated);
    if trimmed.is_empty() {
        return None;
    }

    // Determine exact translated-only insertion point:
    // If the first meaningful text node is inside an <a> that will be unwrapped, insert BEFORE
    // that <a> element and remove ALL text nodes in the segment. Otherwise replace `first_tid`
    // and remove remaining text nodes.
    let first_is_in_link = link_ids_in_text.first().and_then(|o| o.as_ref());
    let (insert_point, remove_tids) = if let Some(link_ref) = first_is_in_link {
        (
            TranslatedInsertPoint::BeforeElement(link_ref.elem_id),
            text_tids.clone(),
        )
    } else {
        (
            TranslatedInsertPoint::ReplaceTextNode(first_tid),
            text_tids[1..].to_vec(),
        )
    };

    // Unwrap only the <a> elements whose text belonged to this segment.
    let unwrap_elem_ids: Vec<usize> = open_order
        .into_iter()
        .filter(|id| elem_to_slot.contains_key(id))
        .collect();

    Some((
        TextBlock {
            id: block_id,
            text: trimmed,
            anchors,
        },
        SegmentPlan {
            block_id,
            strategy: RewriteStrategy::MixedInlineLinks {
                insert_point,
                remove_tids,
                unwrap_elem_ids,
                remove_br_elem_ids: br_elem_ids,
                links: registered_links,
                bilingual_anchor,
            },
        },
    ))
}

fn assemble_plain_segment_text(tokens: &[RawToken]) -> String {
    let mut raw = String::new();
    for token in tokens {
        match token {
            RawToken::Text { decoded, .. } => raw.push_str(decoded),
            RawToken::Br { .. } => raw.push('\n'),
            _ => {}
        }
    }
    collapse_segment_whitespace(&raw)
}

fn collapse_segment_whitespace(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut pending_space = false;
    let mut last_was_newline = false;

    for ch in input.chars() {
        if ch == '\n' {
            pending_space = false;
            if !out.is_empty() && !last_was_newline {
                out.push('\n');
                last_was_newline = true;
            }
        } else if ch.is_whitespace() {
            if !out.is_empty() && !last_was_newline {
                pending_space = true;
            }
        } else {
            if pending_space {
                out.push(' ');
                pending_space = false;
            }
            out.push(ch);
            last_was_newline = false;
        }
    }
    out.trim().to_string()
}

/// Pass 2: Validate translated outputs and render BOTH bilingual and translated-only HTML views
/// from the exact same immutable sanitized HTML string used in Pass 1.
pub fn render_html_translation(
    sanitized_html: &str,
    extracted: &ExtractedHtmlBlocks,
    translated: &[TranslatedBlock],
) -> Result<RenderedHtmlTranslation, TranslateError> {
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

    let bilingual_html = apply_pass2(sanitized_html, &extracted.segments, &by_id, true)?;
    let translated_html = apply_pass2(sanitized_html, &extracted.segments, &by_id, false)?;

    Ok(RenderedHtmlTranslation {
        translated_subject,
        bilingual_html,
        translated_html,
    })
}

enum TextNodeAction {
    /// Bilingual insertion right after the text node (preserves original text).
    InsertAfterHtml(String),
    /// Translated-only replacement using pure `ContentType::Text`.
    ReplaceText(String),
    /// Translated-only replacement using strictly-escaped Rust-constructed HTML (for `<br>` or
    /// reconstructed registered `/link/<token>/<n>` anchors).
    ReplaceHtml(String),
    /// Remove this text node (its content was folded into the segment's primary replacement).
    Remove,
}

#[derive(Default)]
struct Pass2Plan {
    text_actions: HashMap<usize, TextNodeAction>,
    after_end_tag_html: HashMap<usize, String>,
    before_elem_html: HashMap<usize, String>,
    unwrap_elem_ids: HashSet<usize>,
    remove_br_elem_ids: HashSet<usize>,
}

fn apply_pass2(
    sanitized_html: &str,
    segments: &[SegmentPlan],
    by_id: &HashMap<u32, String>,
    bilingual: bool,
) -> Result<String, TranslateError> {
    let mut plan = Pass2Plan::default();

    for seg in segments {
        let Some(tr_text) = by_id.get(&seg.block_id) else {
            return Err(TranslateError::InvalidResponse(
                "missing translated HTML block".into(),
            ));
        };

        if bilingual {
            let inner_html = match &seg.strategy {
                RewriteStrategy::MixedInlineLinks { links, .. } => {
                    reconstruct_inline_links_html(tr_text, links)
                }
                _ => escape_html_text_with_br(tr_text),
            };
            let is_fallback = matches!(seg.strategy, RewriteStrategy::FallbackContainer { .. });
            let container = build_trusted_translation_container(&inner_html, is_fallback);
            let anchor = match &seg.strategy {
                RewriteStrategy::InlineText {
                    bilingual_anchor, ..
                }
                | RewriteStrategy::SingleCtaLink {
                    bilingual_anchor, ..
                }
                | RewriteStrategy::MixedInlineLinks {
                    bilingual_anchor, ..
                }
                | RewriteStrategy::FallbackContainer { bilingual_anchor } => *bilingual_anchor,
            };
            match anchor {
                BilingualAnchor::AfterTextNode(tid) => {
                    plan.text_actions
                        .insert(tid, TextNodeAction::InsertAfterHtml(container));
                }
                BilingualAnchor::AfterEndTag(eid) => {
                    plan.after_end_tag_html
                        .entry(eid)
                        .and_modify(|s| s.push_str(&container))
                        .or_insert(container);
                }
            }
        } else {
            // Translated-only mode
            match &seg.strategy {
                RewriteStrategy::InlineText {
                    first_tid,
                    remove_tids,
                    remove_br_elem_ids,
                    ..
                }
                | RewriteStrategy::SingleCtaLink {
                    first_tid,
                    remove_tids,
                    remove_br_elem_ids,
                    ..
                } => {
                    if tr_text.contains('\n') {
                        plan.text_actions.insert(
                            *first_tid,
                            TextNodeAction::ReplaceHtml(escape_html_text_with_br(tr_text)),
                        );
                    } else {
                        plan.text_actions
                            .insert(*first_tid, TextNodeAction::ReplaceText(tr_text.clone()));
                    }
                    for &tid in remove_tids {
                        plan.text_actions.insert(tid, TextNodeAction::Remove);
                    }
                    for &eid in remove_br_elem_ids {
                        plan.remove_br_elem_ids.insert(eid);
                    }
                }
                RewriteStrategy::MixedInlineLinks {
                    insert_point,
                    remove_tids,
                    unwrap_elem_ids,
                    remove_br_elem_ids,
                    links,
                    ..
                } => {
                    let reconstructed = reconstruct_inline_links_html(tr_text, links);
                    match *insert_point {
                        TranslatedInsertPoint::ReplaceTextNode(tid) => {
                            plan.text_actions
                                .insert(tid, TextNodeAction::ReplaceHtml(reconstructed));
                        }
                        TranslatedInsertPoint::BeforeElement(eid) => {
                            plan.before_elem_html.insert(eid, reconstructed);
                        }
                    }
                    for &tid in remove_tids {
                        plan.text_actions.insert(tid, TextNodeAction::Remove);
                    }
                    for &eid in unwrap_elem_ids {
                        plan.unwrap_elem_ids.insert(eid);
                    }
                    for &eid in remove_br_elem_ids {
                        plan.remove_br_elem_ids.insert(eid);
                    }
                }
                RewriteStrategy::FallbackContainer { bilingual_anchor } => {
                    // Safe fallback: preserve original HTML intact and attach escaped translation.
                    let inner_html = escape_html_text_with_br(tr_text);
                    let container = build_trusted_translation_container(&inner_html, true);
                    match *bilingual_anchor {
                        BilingualAnchor::AfterTextNode(tid) => {
                            plan.text_actions
                                .insert(tid, TextNodeAction::InsertAfterHtml(container));
                        }
                        BilingualAnchor::AfterEndTag(eid) => {
                            plan.after_end_tag_html
                                .entry(eid)
                                .and_modify(|s| s.push_str(&container))
                                .or_insert(container);
                        }
                    }
                }
            }
        }
    }

    let plan = Rc::new(plan);
    let next_elem_id = Rc::new(RefCell::new(0usize));
    let next_text_node_id = Rc::new(RefCell::new(0usize));

    let el_plan = Rc::clone(&plan);
    let el_counter = Rc::clone(&next_elem_id);
    let txt_plan = Rc::clone(&plan);
    let txt_counter = Rc::clone(&next_text_node_id);

    lol_html::rewrite_str(
        sanitized_html,
        RewriteStrSettings {
            element_content_handlers: vec![element!("*", move |el| {
                let elem_id = {
                    let mut c = el_counter.borrow_mut();
                    let id = *c;
                    *c += 1;
                    id
                };

                // Defense-in-depth: strip any spoofed sender `data-fp-tr*` attributes so ONLY
                // freshly emitted Rust containers can ever carry `data-fp-tr="1"`.
                let spoofed: Vec<String> = el
                    .attributes()
                    .iter()
                    .map(|a| a.name())
                    .filter(|name| {
                        let lower = name.to_ascii_lowercase();
                        lower == TRUSTED_TR_ATTR || lower.starts_with("data-fp-tr-")
                    })
                    .collect();
                for attr in spoofed {
                    el.remove_attribute(&attr);
                }

                if let Some(html_before) = el_plan.before_elem_html.get(&elem_id) {
                    el.before(html_before, ContentType::Html);
                }

                if el_plan.remove_br_elem_ids.contains(&elem_id) {
                    // <br> is a void element; removing it never affects inner text_node_id.
                    el.remove();
                    return Ok(());
                }

                if el_plan.unwrap_elem_ids.contains(&elem_id) {
                    // CRITICAL: Use remove_and_keep_content() rather than remove() so inner
                    // doc_text!() callbacks still fire in exact lockstep with Pass 1!
                    el.remove_and_keep_content();
                    return Ok(());
                }

                if let Some(after_html) = el_plan.after_end_tag_html.get(&elem_id).cloned() {
                    type EndTagCallback = Box<
                        dyn FnOnce(&mut EndTag<'_>) -> Result<(), Box<dyn std::error::Error + Send + Sync>>,
                    >;
                    let handler: EndTagCallback = Box::new(move |end| {
                        end.after(&after_html, ContentType::Html);
                        Ok(())
                    });
                    el.on_end_tag(handler)?;
                }

                Ok(())
            })],
            document_content_handlers: vec![doc_text!(move |chunk| {
                let tid = *txt_counter.borrow();
                let is_last = chunk.last_in_text_node();
                if let Some(action) = txt_plan.text_actions.get(&tid) {
                    match action {
                        TextNodeAction::InsertAfterHtml(html) => {
                            if is_last {
                                chunk.after(html, ContentType::Html);
                            }
                        }
                        TextNodeAction::ReplaceText(text) => {
                            if is_last {
                                chunk.replace(text, ContentType::Text);
                            } else {
                                chunk.remove();
                            }
                        }
                        TextNodeAction::ReplaceHtml(html) => {
                            if is_last {
                                chunk.replace(html, ContentType::Html);
                            } else {
                                chunk.remove();
                            }
                        }
                        TextNodeAction::Remove => {
                            chunk.remove();
                        }
                    }
                }
                if is_last {
                    *txt_counter.borrow_mut() += 1;
                }
                Ok(())
            })],
            ..RewriteStrSettings::new()
        },
    )
    .map_err(|_| TranslateError::InvalidConfig("failed to rewrite translated HTML".into()))
}

fn build_trusted_translation_container(inner_escaped_html: &str, is_fallback: bool) -> String {
    let class_name = if is_fallback {
        "fp-tr-block fp-tr-fallback"
    } else {
        "fp-tr-block"
    };
    format!(
        "<span class=\"{class_name}\" {TRUSTED_TR_ATTR}=\"{TRUSTED_TR_VALUE}\" dir=\"auto\" \
         style=\"{BILINGUAL_LIGHT_STYLE}\">{inner_escaped_html}</span>"
    )
}

/// Reconstruct inline `<a href="...">` links strictly from `registered_links` for a single block.
///
/// Validation rules:
/// - Every `slot_id` in `registered_links` MUST appear EXACTLY ONCE as a balanced, non-nested
///   `[[slot_id]]...[[/slot_id]]` pair.
/// - Any literal `[[N]]` whose `N` is NOT in `registered_links` is treated as literal plain text
///   and strictly HTML-escaped (NEVER turned into an `<a>` tag).
/// - Every character of model-generated text (both outside and inside placeholders) is strictly
///   HTML-escaped — ZERO raw HTML from the LLM ever reaches the output.
/// - If any registered placeholder is missing, duplicated, unclosed, nested, or malformed,
///   safely degrades to plain HTML-escaped text (stripping broken registered placeholder markers).
fn reconstruct_inline_links_html(translated: &str, registered_links: &[RegisteredLink]) -> String {
    if registered_links.is_empty() {
        return escape_html_text_with_br(translated);
    }

    let link_map: HashMap<u32, &str> = registered_links
        .iter()
        .map(|l| (l.slot_id, l.safe_href.as_str()))
        .collect();

    if let Some(html) = try_reconstruct_strict(translated, &link_map) {
        html
    } else {
        // Malformed placeholder markers from provider: degrade safely to plain escaped text.
        let cleaned = strip_registered_markers_from_text(translated, &link_map);
        escape_html_text_with_br(&cleaned)
    }
}

#[derive(Debug)]
enum MarkerKind {
    Open(u32),
    Close(u32),
}

fn parse_marker_at(s: &str) -> Option<(MarkerKind, usize)> {
    let rest = s.strip_prefix("[[")?;
    let (is_close, digits_part) = match rest.strip_prefix('/') {
        Some(after_slash) => (true, after_slash),
        None => (false, rest),
    };
    let close_idx = digits_part.find("]]")?;
    let num_str = &digits_part[..close_idx];
    if num_str.is_empty() || num_str.len() > 10 || !num_str.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let id = num_str.parse::<u32>().ok()?;
    let total_len = 2 + usize::from(is_close) + close_idx + 2;
    let kind = if is_close {
        MarkerKind::Close(id)
    } else {
        MarkerKind::Open(id)
    };
    Some((kind, total_len))
}

fn try_reconstruct_strict(text: &str, link_map: &HashMap<u32, &str>) -> Option<String> {
    let mut out = String::with_capacity(text.len() + link_map.len() * 48);
    let mut active_slot: Option<u32> = None;
    let mut seen_slots: HashSet<u32> = HashSet::with_capacity(link_map.len());
    let mut i = 0;

    while i < text.len() {
        if let Some(rel) = text[i..].find("[[") {
            let chunk = &text[i..i + rel];
            out.push_str(&escape_html_text_with_br(chunk));
            i += rel;

            if let Some((kind, len)) = parse_marker_at(&text[i..]) {
                match kind {
                    MarkerKind::Open(id) if link_map.contains_key(&id) => {
                        // Reject nested or duplicate registered slot opens.
                        if active_slot.is_some() || !seen_slots.insert(id) {
                            return None;
                        }
                        let href = link_map[&id];
                        out.push_str("<a href=\"");
                        out.push_str(&escape_html_attr(href));
                        out.push_str("\">");
                        active_slot = Some(id);
                        i += len;
                        continue;
                    }
                    MarkerKind::Close(id) if link_map.contains_key(&id) => {
                        if active_slot != Some(id) {
                            return None;
                        }
                        out.push_str("</a>");
                        active_slot = None;
                        i += len;
                        continue;
                    }
                    _ => {
                        // Unregistered literal marker (e.g. literal `[[1]]` in email prose):
                        // emit as plain escaped text, NEVER as a link!
                        out.push_str(&escape_html_text_with_br(&text[i..i + len]));
                        i += len;
                        continue;
                    }
                }
            } else {
                out.push_str("[[");
                i += 2;
            }
        } else {
            out.push_str(&escape_html_text_with_br(&text[i..]));
            break;
        }
    }

    // Must close all opened links and include every registered slot exactly once.
    if active_slot.is_some() || seen_slots.len() != link_map.len() {
        return None;
    }

    Some(out)
}

fn strip_registered_markers_from_text(text: &str, link_map: &HashMap<u32, &str>) -> String {
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < text.len() {
        if let Some(rel) = text[i..].find("[[") {
            out.push_str(&text[i..i + rel]);
            i += rel;
            if let Some((kind, len)) = parse_marker_at(&text[i..]) {
                let id = match kind {
                    MarkerKind::Open(n) | MarkerKind::Close(n) => n,
                };
                if link_map.contains_key(&id) {
                    i += len;
                    continue;
                }
            }
            out.push_str("[[");
            i += 2;
        } else {
            out.push_str(&text[i..]);
            break;
        }
    }
    out
}

fn strip_registered_placeholders(text: &str, anchors: &[AnchorSlot]) -> String {
    if anchors.is_empty() {
        return text.to_string();
    }
    let map: HashMap<u32, &str> = anchors.iter().map(|a| (a.slot_id, "")).collect();
    strip_registered_markers_from_text(text, &map)
}

fn collect_literal_marker_numbers(text: &str, out: &mut HashSet<u32>) {
    let mut i = 0;
    while i < text.len() {
        if let Some(rel) = text[i..].find("[[") {
            i += rel;
            if let Some((kind, len)) = parse_marker_at(&text[i..]) {
                let id = match kind {
                    MarkerKind::Open(n) | MarkerKind::Close(n) => n,
                };
                out.insert(id);
                i += len;
            } else {
                i += 2;
            }
        } else {
            break;
        }
    }
}

fn parse_registered_link(el: &Element, link_prefix: &str) -> (Option<String>, Option<u32>) {
    if link_prefix.is_empty() {
        return (None, None);
    }
    let Some(raw_href) = el.get_attribute("href") else {
        return (None, None);
    };
    let trimmed = raw_href.trim();
    let Some(suffix) = trimmed.strip_prefix(link_prefix) else {
        return (None, None);
    };
    if suffix.is_empty() || !suffix.bytes().all(|b| b.is_ascii_digit()) {
        return (None, None);
    }
    let Ok(index) = suffix.parse::<u32>() else {
        return (None, None);
    };
    (Some(trimmed.to_string()), Some(index))
}

fn is_skip_element(tag: &str, el: &Element) -> bool {
    if matches!(
        tag,
        "head"
            | "title"
            | "style"
            | "script"
            | "noscript"
            | "code"
            | "pre"
            | "kbd"
            | "samp"
            | "var"
            | "svg"
            | "math"
            | "textarea"
            | "select"
            | "option"
            | "template"
    ) {
        return true;
    }
    if el.get_attribute("hidden").is_some() {
        return true;
    }
    if el
        .get_attribute("aria-hidden")
        .is_some_and(|v| v.trim().eq_ignore_ascii_case("true"))
    {
        return true;
    }
    if let Some(style) = el.get_attribute("style") {
        if style_hides_element(&style) {
            return true;
        }
    }
    false
}

/// Inspect inline CSS declarations safely for `display: none`, `visibility: hidden`, or zero
/// `font-size` without false-positiving on `font-size: 0.85em`.
fn style_hides_element(style: &str) -> bool {
    for decl in style.split(';') {
        let Some((raw_prop, raw_val)) = decl.split_once(':') else {
            continue;
        };
        let prop = raw_prop.trim().to_ascii_lowercase();
        let val = raw_val
            .split('!')
            .next()
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase();
        match prop.as_str() {
            "display" if val == "none" => return true,
            "visibility" if val == "hidden" || val == "collapse" => return true,
            "font-size" if is_zero_css_length(&val) => return true,
            _ => {}
        }
    }
    false
}

fn is_zero_css_length(val: &str) -> bool {
    let trimmed = val.trim();
    if trimmed.is_empty() {
        return false;
    }
    // Strip known CSS length / percentage unit suffix.
    let num_part = trimmed
        .strip_suffix("px")
        .or_else(|| trimmed.strip_suffix("pt"))
        .or_else(|| trimmed.strip_suffix("rem"))
        .or_else(|| trimmed.strip_suffix("em"))
        .or_else(|| trimmed.strip_suffix('%'))
        .or_else(|| trimmed.strip_suffix("ex"))
        .or_else(|| trimmed.strip_suffix("ch"))
        .unwrap_or(trimmed)
        .trim();
    if num_part.is_empty() {
        return false;
    }
    num_part
        .parse::<f64>()
        .is_ok_and(|n| n.is_finite() && n == 0.0)
}

fn is_void_element(tag: &str) -> bool {
    matches!(
        tag,
        "area"
            | "base"
            | "br"
            | "col"
            | "embed"
            | "hr"
            | "img"
            | "input"
            | "link"
            | "meta"
            | "param"
            | "source"
            | "track"
            | "wbr"
    )
}

fn is_head_allowed_tag(tag: &str) -> bool {
    matches!(
        tag,
        "head" | "title" | "base" | "link" | "meta" | "style" | "script" | "noscript" | "template"
    )
}

fn is_block_element(tag: &str) -> bool {
    matches!(
        tag,
        "html"
            | "body"
            | "header"
            | "footer"
            | "main"
            | "nav"
            | "section"
            | "article"
            | "aside"
            | "div"
            | "p"
            | "h1"
            | "h2"
            | "h3"
            | "h4"
            | "h5"
            | "h6"
            | "ul"
            | "ol"
            | "li"
            | "dl"
            | "dt"
            | "dd"
            | "table"
            | "thead"
            | "tbody"
            | "tfoot"
            | "tr"
            | "td"
            | "th"
            | "caption"
            | "colgroup"
            | "blockquote"
            | "figure"
            | "figcaption"
            | "address"
            | "hr"
            | "details"
            | "summary"
            | "center"
            | "form"
            | "fieldset"
            | "legend"
            | "pre"
            | "dialog"
    )
}

fn closes_open_p(tag: &str) -> bool {
    matches!(
        tag,
        "address"
            | "article"
            | "aside"
            | "blockquote"
            | "details"
            | "dialog"
            | "dd"
            | "div"
            | "dl"
            | "dt"
            | "fieldset"
            | "figcaption"
            | "figure"
            | "footer"
            | "form"
            | "h1"
            | "h2"
            | "h3"
            | "h4"
            | "h5"
            | "h6"
            | "header"
            | "hgroup"
            | "hr"
            | "li"
            | "main"
            | "nav"
            | "ol"
            | "p"
            | "pre"
            | "section"
            | "table"
            | "ul"
    )
}

fn is_replaced_or_media_element(tag: &str) -> bool {
    matches!(
        tag,
        "img" | "hr" | "picture" | "video" | "audio" | "canvas" | "iframe" | "object" | "embed"
    )
}

/// Escape plain text for HTML text nodes, turning `\n` into `<br>`.
pub fn escape_html_text_with_br(input: &str) -> String {
    let mut out = String::with_capacity(input.len() + 16);
    for ch in input.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            '\n' => out.push_str("<br>"),
            _ => out.push(ch),
        }
    }
    out
}

fn escape_html_attr(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for ch in input.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(ch),
        }
    }
    out
}

/// Decode common named and numeric HTML entities inside extracted text nodes so providers receive
/// clean UTF-8 text and Pass 2 re-escapes once without double-encoding.
pub fn decode_html_entities(value: &str) -> String {
    if !value.contains('&') {
        return value.to_string();
    }
    let mut out = String::with_capacity(value.len());
    let mut rest = value;
    while let Some(pos) = rest.find('&') {
        out.push_str(&rest[..pos]);
        let tail = &rest[pos..];
        let Some(end) = tail[..tail.len().min(14)].find(';') else {
            out.push('&');
            rest = &tail[1..];
            continue;
        };
        let entity = &tail[1..end];
        let decoded = match entity {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            "nbsp" => Some('\u{00A0}'),
            "mdash" => Some('\u{2014}'),
            "ndash" => Some('\u{2013}'),
            "hellip" => Some('\u{2026}'),
            "copy" => Some('\u{00A9}'),
            "reg" => Some('\u{00AE}'),
            "trade" => Some('\u{2122}'),
            "lsquo" => Some('\u{2018}'),
            "rsquo" => Some('\u{2019}'),
            "ldquo" => Some('\u{201C}'),
            "rdquo" => Some('\u{201D}'),
            "bull" => Some('\u{2022}'),
            "middot" => Some('\u{00B7}'),
            "euro" => Some('\u{20AC}'),
            numeric if numeric.starts_with('#') => {
                let digits = &numeric[1..];
                let code = match digits.strip_prefix(['x', 'X']) {
                    Some(hex) => u32::from_str_radix(hex, 16).ok(),
                    None => digits.parse::<u32>().ok(),
                };
                code.and_then(char::from_u32)
                    .filter(|ch| !ch.is_control() || matches!(ch, '\t' | '\n' | '\r'))
            }
            _ => None,
        };
        match decoded {
            Some(ch) => {
                out.push(ch);
                rest = &tail[end + 1..];
            }
            None => {
                out.push('&');
                rest = &tail[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const PREFIX: &str = "featherpost-mail://localhost/link/0123456789abcdef0123456789abcdef/";

    #[test]
    fn handles_implicit_closures_p_li_td_and_hidden_skip_recovery() {
        let html = r#"<head><title>Ignored Title</title>
            <body>
                <p style="display:none">Hidden secret<p>First visible paragraph
                <p>Second visible paragraph
                <ul><li>Item one<li>Item two</ul>
                <table><tr><td>Cell A<td>Cell B<tr><td>Cell C</table>
            </body>"#;
        let extracted = extract_html_blocks(html, Some("Subj"), PREFIX).unwrap();
        let texts: Vec<&str> = extracted.blocks.iter().map(|b| b.text.as_str()).collect();
        assert_eq!(
            texts,
            vec![
                "Subj",
                "First visible paragraph",
                "Second visible paragraph",
                "Item one",
                "Item two",
                "Cell A",
                "Cell B",
                "Cell C",
            ]
        );

        let translated: Vec<TranslatedBlock> = extracted
            .blocks
            .iter()
            .map(|b| TranslatedBlock {
                id: b.id,
                text: format!("TR:{}", b.text),
            })
            .collect();
        let rendered = render_html_translation(html, &extracted, &translated).unwrap();
        assert_eq!(rendered.translated_subject.as_deref(), Some("TR:Subj"));
        for expected in [
            "TR:First visible paragraph",
            "TR:Second visible paragraph",
            "TR:Item one",
            "TR:Item two",
            "TR:Cell A",
            "TR:Cell B",
            "TR:Cell C",
        ] {
            assert!(
                rendered.bilingual_html.contains(expected),
                "bilingual missing {expected}: {}",
                rendered.bilingual_html
            );
            assert!(
                rendered.translated_html.contains(expected),
                "translated missing {expected}: {}",
                rendered.translated_html
            );
        }
        assert!(rendered.bilingual_html.contains("First visible paragraph"));
        assert!(!rendered.translated_html.contains(">First visible paragraph<"));
        assert!(rendered.bilingual_html.contains("Hidden secret"));
        assert!(!rendered.bilingual_html.contains("TR:Hidden secret"));
    }

    #[test]
    fn handles_bare_top_level_text_sibling_prose_br_and_entities() {
        let html = "Bare intro text<div>Prose before<p>Line 1<br>AT&amp;T &lt;news&gt;</p>Prose after</div>Bare trailing";
        let extracted = extract_html_blocks(html, None, PREFIX).unwrap();
        let texts: Vec<&str> = extracted.blocks.iter().map(|b| b.text.as_str()).collect();
        assert_eq!(
            texts,
            vec![
                "Bare intro text",
                "Prose before",
                "Line 1\nAT&T <news>",
                "Prose after",
                "Bare trailing",
            ]
        );
        let translated: Vec<TranslatedBlock> = extracted
            .blocks
            .iter()
            .map(|b| TranslatedBlock {
                id: b.id,
                text: format!("[{}]", b.text),
            })
            .collect();
        let rendered = render_html_translation(html, &extracted, &translated).unwrap();
        assert!(rendered.translated_html.contains("[Bare intro text]"));
        assert!(rendered.translated_html.contains("[Prose before]"));
        assert!(rendered
            .translated_html
            .contains("[Line 1<br>AT&amp;T &lt;news&gt;]"));
        assert!(rendered.translated_html.contains("[Prose after]"));
        assert!(rendered.translated_html.contains("[Bare trailing]"));
    }

    #[test]
    fn distinguishes_literal_brackets_from_real_link_placeholders() {
        let html = format!(
            r#"<p>See footnote [[1]] and <a href="{PREFIX}0">click here</a> for details.</p>"#
        );
        let extracted = extract_html_blocks(&html, None, PREFIX).unwrap();
        assert_eq!(extracted.blocks.len(), 1);
        // Because literal [[1]] exists in source text, real link MUST get slot 2!
        assert_eq!(
            extracted.blocks[0].text,
            "See footnote [[1]] and [[2]]click here[[/2]] for details."
        );
        assert_eq!(
            extracted.blocks[0].anchors,
            vec![AnchorSlot {
                slot_id: 2,
                link_index: 0,
            }]
        );

        let rendered = render_html_translation(
            &html,
            &extracted,
            &[TranslatedBlock {
                id: 1,
                text: "参见脚注 [[1]] 并[[2]]点击这里[[/2]]查看详情。".into(),
            }],
        )
        .unwrap();
        assert!(rendered
            .translated_html
            .contains(&format!("参见脚注 [[1]] 并<a href=\"{PREFIX}0\">点击这里</a>查看详情。")));
        assert!(rendered.bilingual_html.contains("See footnote [[1]]"));
        assert!(rendered.bilingual_html.contains("data-fp-tr=\"1\""));
    }

    #[test]
    fn blocks_xss_prompt_injection_and_forged_links_and_degrades_malformed_placeholders() {
        let html = format!(
            r#"<p>Read <a href="{PREFIX}0">the guide</a> now.</p><p>Plain paragraph</p>"#
        );
        let extracted = extract_html_blocks(&html, None, PREFIX).unwrap();
        let rendered = render_html_translation(
            &html,
            &extracted,
            &[
                // Malformed placeholder (unclosed [[1]]) + XSS + forged link!
                TranslatedBlock {
                    id: 1,
                    text: r#"<script>alert(1)</script><img src=x onerror=alert(2)><a href="https://evil.test">steal</a> [[1]]broken"#.into(),
                },
                // Fake placeholder on block with no anchors + XSS!
                TranslatedBlock {
                    id: 2,
                    text: r#"[[1]]Fake Link[[/1]] <svg onload=alert(3)>"#.into(),
                },
            ],
        )
        .unwrap();

        for out in [&rendered.bilingual_html, &rendered.translated_html] {
            assert!(!out.contains("<script>"), "raw script leaked: {out}");
            assert!(!out.contains("<img src=x"), "raw img onerror leaked: {out}");
            assert!(
                !out.contains("<a href=\"https://evil.test"),
                "forged link leaked: {out}"
            );
            assert!(!out.contains("<svg onload="), "raw svg onload leaked: {out}");
            assert!(out.contains("&lt;script&gt;alert(1)&lt;/script&gt;"));
            assert!(out.contains("&lt;a href=&quot;https://evil.test&quot;&gt;steal&lt;/a&gt;"));
        }
        // Block 1 had malformed placeholder -> degraded cleanly without broken <a> or raw [[1]]
        assert!(!rendered.translated_html.contains("[[1]]broken"));
        // Block 2 had no anchors -> literal [[1]] stays escaped text, never becomes <a>
        assert!(rendered.translated_html.contains("[[1]]Fake Link[[/1]]"));
    }

    #[test]
    fn preserves_cta_button_styling_and_cid_images_in_translated_only_mode() {
        let html = format!(
            r#"<div><img src="featherpost-mail://localhost/part/0123456789abcdef0123456789abcdef/2" alt="logo"><p>Welcome aboard</p><a href="{PREFIX}0" style="background:#0066ff;color:#fff;padding:12px">Verify Account</a></div>"#
        );
        let extracted = extract_html_blocks(&html, None, PREFIX).unwrap();
        assert_eq!(extracted.blocks.len(), 2);
        let rendered = render_html_translation(
            &html,
            &extracted,
            &[
                TranslatedBlock {
                    id: 1,
                    text: "欢迎加入".into(),
                },
                TranslatedBlock {
                    id: 2,
                    text: "验证账户".into(),
                },
            ],
        )
        .unwrap();
        assert!(rendered
            .translated_html
            .contains("src=\"featherpost-mail://localhost/part/0123456789abcdef0123456789abcdef/2\""));
        assert!(rendered.translated_html.contains(&format!(
            "<a href=\"{PREFIX}0\" style=\"background:#0066ff;color:#fff;padding:12px\">验证账户</a>"
        )));
    }

    #[test]
    fn strips_spoofed_sender_data_fp_tr_attribute() {
        let html = r#"<p class="fp-tr-block" data-fp-tr="1" data-fp-tr-fake="x">Hello world</p>"#;
        let extracted = extract_html_blocks(html, None, PREFIX).unwrap();
        let rendered = render_html_translation(
            html,
            &extracted,
            &[TranslatedBlock {
                id: 1,
                text: "你好世界".into(),
            }],
        )
        .unwrap();
        // Original <p> must have its spoofed data-fp-tr stripped!
        assert!(rendered.translated_html.contains("<p class=\"fp-tr-block\">你好世界</p>"));
        assert!(!rendered.translated_html.contains("data-fp-tr"));
        // Bilingual has data-fp-tr ONLY on our generated <span class="fp-tr-block" data-fp-tr="1">
        assert_eq!(rendered.bilingual_html.matches("data-fp-tr=\"1\"").count(), 1);
    }

    #[test]
    fn skips_code_pre_style_hidden_and_font_size_zero_but_keeps_fractional_font_size() {
        let html = r#"<div>
            <code>const x = 1;</code>
            <pre>fn main() {}</pre>
            <span hidden>hidden attr</span>
            <span aria-hidden="true">aria hidden</span>
            <span style="visibility: hidden">vis hidden</span>
            <span style="font-size: 0px">zero font</span>
            <span style="font-size: 0.85em">small visible text</span>
        </div>"#;
        let extracted = extract_html_blocks(html, None, PREFIX).unwrap();
        let texts: Vec<&str> = extracted.blocks.iter().map(|b| b.text.as_str()).collect();
        assert_eq!(texts, vec!["small visible text"]);
    }
}
