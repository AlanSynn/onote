//! Markdown adapter — comrak-backed renderer and attachment-link extractor
//! (`CLAUDE.md` §2.3, §3.2). Keeps comrak out of the domain layer; the domain
//! speaks only to [`crate::ports::MarkdownRenderer`] /
//! [`crate::ports::MarkdownLinkExtractor`].
//!
//! Scope note: `CLAUDE.md` §2.3 lists "render preview text for TUI mode" as a
//! comrak responsibility, but the MVP TUI is a transparent *editor* — it shows
//! the raw Markdown so typing maps 1:1 to bytes (image tokens render as an
//! inline glyph overlay, not a reflow). A separate comrak-rendered read-only
//! text preview (a future "read mode") is intentionally deferred; it is not a
//! §10 non-goal, just out of the MVP surface.

use std::collections::HashSet;

use comrak::nodes::NodeValue;
use comrak::{markdown_to_html, parse_document, Arena, Options};

use crate::domain::attachment::{AttachmentReference, LinkStyle};
use crate::domain::note::{split_frontmatter, MarkdownBody};
use crate::domain::vault::RelativeNotePath;
use crate::ports::{MarkdownLinkExtractor, MarkdownRenderer};

/// comrak-backed Markdown adapter (`CLAUDE.md` §2.3, §3.2).
///
/// Implements both [`MarkdownRenderer`] (note body → HTML fragment for share
/// mode) and [`MarkdownLinkExtractor`] (note body → attachment references for
/// the image-token UX). Stateless; cheap to construct.
pub struct ComrakMarkdown;

impl ComrakMarkdown {
    pub fn new() -> Self {
        Self
    }
}

impl Default for ComrakMarkdown {
    fn default() -> Self {
        Self::new()
    }
}

/// Shared GFM-flavored options for both rendering and AST walking
/// (`CLAUDE.md` §2.3). Front-matter delimiter strips a leading `---\n...\n---`
/// block so it never leaks into share HTML or attachment scans.
fn build_options() -> Options<'static> {
    let mut options = Options::default();

    // GFM extension set — task lists, tables, strikethrough, autolinks are the
    // Obsidian-compatible subset. Disable the speculative GFM extras.
    options.extension.strikethrough = true;
    options.extension.table = true;
    options.extension.autolink = true;
    options.extension.tasklist = true;
    options.extension.tagfilter = false;
    options.extension.superscript = false;
    options.extension.footnotes = false;
    options.extension.description_lists = false;
    options.extension.multiline_block_quotes = false;
    options.extension.alerts = false;

    // Obsidian `[[wikilink]]` support (`CLAUDE.md` §1.2). `title_after_pipe`
    // selects `[[Target|Alias]]` (comrak's UrlFirst mode), so a parsed wikilink
    // becomes `NodeValue::WikiLink { url = Target }`. Note the attachment
    // extractor still scans raw text for `![[...]]` embeds, which comrak does
    // not parse natively — enabling wikilinks does not change that path.
    options.extension.wikilinks_title_after_pipe = true;

    // Strip leading front-matter block from rendered output and AST.
    options.extension.front_matter_delimiter = Some("---".to_string());

    // Smart typography.
    options.parse.smart = true;

    // Render raw HTML as escaped text, NOT verbatim. Share HTML is served over
    // HTTP, so a note containing `<script>`/`<img onerror=…>` must not execute
    // in the share page's origin. (CLAUDE.md §3.1 Share: read-only delivery.)
    options.render.unsafe_ = false;

    options
}

/// Compact dedup key for [`LinkStyle`] (the enum is not `Hash`).
fn style_key(style: LinkStyle) -> &'static str {
    match style {
        LinkStyle::Markdown => "md",
        LinkStyle::Obsidian => "obs",
    }
}

/// Strip an Obsidian `|alias`/`|size` suffix from a bracket span's inner text
/// (`[[Target|Alias]]` / `![[Img|200]]` → `Target` / `Img`). Shared by the
/// bracket scanners and the embed→Markdown rewriter so the suffix rule lives in
/// one place (§5 DRY). `|` is ASCII so the byte index lands on a char boundary.
fn strip_alias_suffix(inner: &str) -> &str {
    match inner.find('|') {
        Some(idx) => inner[..idx].trim(),
        None => inner.trim(),
    }
}

/// Scan raw text for `open … close` bracketed spans, returning each inner
/// string (optionally with an Obsidian `|alias`/`|size` suffix stripped) in
/// first-seen order. Empty results are dropped.
///
/// Shared bracket-matching core for `![[…]]` embeds and `[[…]]` wikilinks —
/// keeps the two scanners DRY. `open`/`close` must be ASCII-only so byte-level
/// `find` stays UTF-8 safe (ASCII marker bytes never appear in continuation
/// bytes, so the byte indices returned by `find` always land on char
/// boundaries).
///
/// - `strip_alias`: when true, the part of the inner span before the first `|`
///   wins (`[[Target|Alias]]` / `![[Img|200]]` → `Target` / `Img`). Both
///   scanners pass `true` because Obsidian embeds routinely carry a size or
///   alias suffix (`![[a.png|200]]`, `![[a.png|200x300]]`, `![[a.png|cap]]`)
///   that would otherwise leak into the attachment ref and never resolve on
///   disk. Sibling suffix-stripping is symmetric for wikilinks.
/// - `skip_if_bang_prefix`: when true, a match whose byte immediately before
///   `open` is `!` is rejected — used so `[[…]]` does NOT swallow `![[…]]`
///   embeds (those are attachment refs, not note links).
fn scan_bracket_spans(
    text: &str,
    open: &str,
    close: &str,
    strip_alias: bool,
    skip_if_bang_prefix: bool,
) -> Vec<String> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut search_from = 0;
    while let Some(rel) = text[search_from..].find(open) {
        let open_start = search_from + rel;
        if skip_if_bang_prefix && open_start > 0 && bytes[open_start - 1] == b'!' {
            // `[[` directly preceded by `!` is an embed, not a wikilink.
            search_from = open_start + open.len();
            continue;
        }
        let inner_start = open_start + open.len();
        if inner_start > text.len() {
            break;
        }
        match text[inner_start..].find(close) {
            Some(rel_close) => {
                let inner = &text[inner_start..inner_start + rel_close];
                let target = if strip_alias {
                    strip_alias_suffix(inner)
                } else {
                    inner.trim()
                };
                if !target.is_empty() {
                    out.push(target.to_string());
                }
                search_from = inner_start + rel_close + close.len();
            }
            None => break,
        }
    }
    out
}

/// Scan raw text for Obsidian `![[...]]` embeds (`CLAUDE.md` §1.2).
///
/// comrak does not parse wikilink-style embeds in this configuration, so we
/// locate the literal `![[` opener and the next `]]` closer via the shared
/// [`scan_bracket_spans`] helper. The inner span has its `|size`/`|alias`
/// suffix stripped (`![[Attachments/b.png|200]]` → `Attachments/b.png`) so the
/// resulting attachment ref resolves on disk; validation/dedup happens
/// upstream.
fn scan_obsidian_embeds(text: &str) -> Vec<String> {
    scan_bracket_spans(text, "![[", "]]", true, false)
}

/// Rewrite Obsidian `![[...]]` image embeds to standard Markdown `![](...)` so
/// comrak — which does NOT parse `![[...]]` natively — renders them as `<img>`
/// in share HTML instead of literal text (`CLAUDE.md` §1.2, §2.8, §9 Spike-4).
/// This is the path a vault with `image_link_style = "obsidian"` exercises on
/// `onote share`: every pasted image produces a `![[...]]` token, and without
/// this rewrite the share page shows raw `![[Attachments/…]]` text, never an
/// image. The `|size`/`|alias` suffix is dropped via [`strip_alias_suffix`]
/// (Markdown image syntax has none). Non-embed text — including `[[…]]`
/// wikilinks, which comrak parses via the wikilinks extension — is copied
/// verbatim. A truncated opener (`![[` with no closer) is left as-is.
///
/// Marker bytes (`![[` / `]]`) are ASCII, so byte-level `find` indices always
/// land on UTF-8 char boundaries and the `&str` slices are safe. A `![[…]]`
/// that appears literally inside a fenced code block becomes `![](…)` literal
/// text inside `<code>` (cosmetic; comrak does not re-parse fenced content) —
/// the attachment extractor has the same minor property.
fn obsidian_embeds_to_markdown(body: &str) -> String {
    const OPEN: &str = "![[";
    const CLOSE: &str = "]]";
    let mut out = String::with_capacity(body.len());
    let mut cursor = 0;
    loop {
        match body[cursor..].find(OPEN) {
            None => {
                out.push_str(&body[cursor..]);
                break;
            }
            Some(rel) => {
                let bang = cursor + rel;
                // Copy everything before the `![[` verbatim.
                out.push_str(&body[cursor..bang]);
                let inner_start = bang + OPEN.len();
                if inner_start > body.len() {
                    out.push_str(&body[bang..]);
                    break;
                }
                match body[inner_start..].find(CLOSE) {
                    None => {
                        out.push_str(&body[bang..]);
                        break;
                    }
                    Some(rel_close) => {
                        let inner = &body[inner_start..inner_start + rel_close];
                        out.push_str("![](");
                        out.push_str(strip_alias_suffix(inner));
                        out.push(')');
                        cursor = inner_start + rel_close + CLOSE.len();
                    }
                }
            }
        }
    }
    out
}

/// Scan raw text for Obsidian `[[...]]` wikilinks (`CLAUDE.md` §1.2).
///
/// Returns the link target — the part before the first `|` for
/// `[[Target|Alias]]` — in first-seen order. Embeds (`![[…]]`) are skipped by
/// [`scan_bracket_spans`] (`skip_if_bang_prefix = true`) so they stay
/// classified as attachment refs rather than note links.
fn scan_wikilinks(text: &str) -> Vec<String> {
    scan_bracket_spans(text, "[[", "]]", true, true)
}

/// Scan raw text for Obsidian-style `#tags` (`CLAUDE.md` §1.2).
///
/// A tag is `#` immediately followed by one or more Unicode-alphanumeric chars
/// (plus `_`, `-`, `/`), where the `#` is NOT preceded by a word char — so
/// `foo#bar` is rejected but start-of-line `#bar` and ` #bar` are accepted.
/// CJK and accented tags (`#标签`, `#café`) are supported because Obsidian
/// permits Unicode tags. Markdown headings (`# `) are naturally excluded
/// because the space after `#` is not a tag char.
///
/// Fenced code blocks (a line starting with three backticks or three tildes)
/// are stripped first so a `#word` inside a code block is NOT reported as a
/// tag — Obsidian treats code blocks as literal text. Indented (4-space) code
/// blocks are a lesser concern and are intentionally NOT stripped.
fn scan_tags(text: &str) -> Vec<String> {
    /// Tag-body char: Unicode alphanumeric, `_`, `-`, or `/`.
    fn is_tag_char(c: char) -> bool {
        c.is_alphanumeric() || c == '_' || c == '-' || c == '/'
    }
    /// Word char for the "preceded by a word char" guard: Unicode alphanumeric
    /// or `_`. Extends to non-ASCII so `café#x` doesn't falsely trigger.
    fn is_word_char(c: char) -> bool {
        c.is_alphanumeric() || c == '_'
    }
    /// Detect a CommonMark fenced-code-block marker: a line whose first
    /// non-space characters are 3+ backticks or 3+ tildes. Returns the fence
    /// character so callers can require a matching open/close kind. Only
    /// space-indentation is trimmed (CommonMark fence rule); tabs do not count.
    fn fence_marker(line: &str) -> Option<char> {
        let trimmed = line.trim_start_matches(' ');
        let bytes = trimmed.as_bytes();
        if bytes.len() >= 3 && bytes[0] == b'`' && bytes[1] == b'`' && bytes[2] == b'`' {
            Some('`')
        } else if bytes.len() >= 3 && bytes[0] == b'~' && bytes[1] == b'~' && bytes[2] == b'~' {
            Some('~')
        } else {
            None
        }
    }

    // Strip fenced-code-block lines so a `#word` inside doesn't masquerade as
    // a tag. Replace each stripped line with a bare newline; positions aren't
    // used downstream, this just keeps the line structure tidy. Track the
    // opening fence char so a `~~~` line inside a ` ``` ` block does NOT close
    // it (matching CommonMark).
    let mut filtered = String::with_capacity(text.len());
    let mut in_fence = false;
    let mut fence_kind: Option<char> = None;
    for line in text.split_inclusive('\n') {
        let marker = fence_marker(line);
        match (in_fence, marker, fence_kind) {
            (false, Some(fc), _) => {
                // Opening fence (info string after the fence is irrelevant —
                // the whole line is consumed).
                in_fence = true;
                fence_kind = Some(fc);
                filtered.push('\n');
            }
            (true, Some(fc), Some(open)) if fc == open => {
                // Matching closing fence.
                in_fence = false;
                fence_kind = None;
                filtered.push('\n');
            }
            (true, _, _) => filtered.push('\n'),
            (false, None, _) => filtered.push_str(line),
        }
    }

    let chars: Vec<char> = filtered.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] != '#' {
            i += 1;
            continue;
        }
        // `#` preceded by a word char is not a tag (e.g. `foo#bar`, `café#x`).
        if i > 0 && is_word_char(chars[i - 1]) {
            i += 1;
            continue;
        }
        // Collect the run of tag chars after `#`.
        let mut j = i + 1;
        while j < chars.len() && is_tag_char(chars[j]) {
            j += 1;
        }
        if j > i + 1 {
            let tag: String = chars[i + 1..j].iter().collect();
            out.push(tag);
            i = j;
        } else {
            // `#` with no tag char after it (e.g. a heading) — step past `#`.
            i += 1;
        }
    }
    out
}

/// True if `url` carries a URI scheme (`scheme:...`) and therefore points
/// outside the vault. Used to filter note-link extraction to in-vault
/// navigation targets (`CLAUDE.md` §1.2).
///
/// A "scheme" is the substring before the first `:`, provided it is non-empty,
/// not absurdly long (≤ 32 chars), and consists entirely of ASCII
/// alphanumeric characters. This admits `http`, `https`, `mailto`, `file`,
/// `ftp`, `obsidian`, `data`, etc. (case-insensitively — `is_ascii_alphanumeric`
/// matches upper- and lowercase), while leaving schemeless relative paths
/// (`Notes/x.md`) and Obsidian wikilinks (`[[Note]]`) as in-vault links.
/// Replaces an earlier 3-scheme allowlist that leaked `obsidian://`,
/// `file://`, `ftp://`, and `data:` as note links.
fn is_external_url(url: &str) -> bool {
    let u = url.trim();
    let Some(colon) = u.find(':') else {
        return false;
    };
    let scheme = &u[..colon];
    if scheme.is_empty() || scheme.len() > 32 {
        return false;
    }
    scheme.bytes().all(|b| b.is_ascii_alphanumeric())
}

/// Strip the `#fragment` and `?query` portions of a schemeless (in-vault)
/// Markdown link target so it resolves as a real file path. Example:
/// `[a](Notes/x.md#top)` → `Notes/x.md`; `[a](Notes/x.md?v=1)` → `Notes/x.md`.
/// Returns the original target (minus trailing whitespace) when no `#` or `?`
/// is present. Both `#` and `?` are ASCII so the byte index returned by `find`
/// lands on a char boundary, making the slice UTF-8 safe.
fn strip_fragment_query(target: &str) -> &str {
    let end = target.find(['#', '?']).unwrap_or(target.len());
    target[..end].trim_end()
}

impl MarkdownRenderer for ComrakMarkdown {
    fn render_html(&self, body: &MarkdownBody) -> String {
        let options = build_options();
        // Rewrite Obsidian `![[...]]` embeds → standard `![](...)` FIRST: comrak
        // does not parse `![[...]]` natively, so without this an embed-bearing
        // note renders as literal `![[…]]` text in share HTML instead of an
        // image (§1.2, §2.8, §9 Spike-4). comrak still strips frontmatter and
        // escapes raw HTML on the rewritten body (`unsafe_ = false`).
        let rewritten = obsidian_embeds_to_markdown(body.as_str());
        markdown_to_html(&rewritten, &options)
    }
}

impl MarkdownLinkExtractor for ComrakMarkdown {
    fn extract_attachment_links(&self, body: &MarkdownBody) -> Vec<AttachmentReference> {
        let options = build_options();
        let arena = Arena::new();
        let root = parse_document(&arena, body.as_str(), &options);

        // Collect (raw, style) pairs first; build references in one pass so
        // escape attempts are dropped before they can poison the dedup set.
        let mut raw: Vec<(String, LinkStyle)> = Vec::new();

        // (1) Standard Markdown image tokens via AST walk.
        for node in root.descendants() {
            let data = node.data.borrow();
            if let NodeValue::Image(link) = &data.value {
                raw.push((link.url.clone(), LinkStyle::Markdown));
            }
        }

        // (2) Obsidian `![[...]]` embeds via raw-text scan. The comrak AST pass
        // above strips frontmatter via `front_matter_delimiter`, but this raw-
        // text scan does not — so without stripping it would pick up a metadata-
        // only embed like `banner: ![[Attachments/banner.png]]`. Strip the
        // leading `---\n...\n---` block first (build_options doc, `CLAUDE.md`
        // §1.2). `split_frontmatter` returns `(frontmatter, body)` and leaves
        // the body unchanged when no frontmatter is present.
        let body_no_fm = split_frontmatter(body.as_str()).1;
        for inner in scan_obsidian_embeds(&body_no_fm) {
            raw.push((inner, LinkStyle::Obsidian));
        }

        // Build references: trim, skip empty, reject traversal (`from_user`
        // returns Err on escape attempts), dedup on (target, style) preserving
        // first-seen order.
        let mut seen: HashSet<(String, &'static str)> = HashSet::new();
        let mut out: Vec<AttachmentReference> = Vec::new();
        for (s, style) in raw {
            let s = s.trim();
            if s.is_empty() {
                continue;
            }
            let target = match RelativeNotePath::from_user(s) {
                Ok(t) => t,
                Err(_) => continue,
            };
            let key = (target.as_str(), style_key(style));
            if seen.insert(key) {
                out.push(AttachmentReference { target, style });
            }
        }
        out
    }

    fn extract_note_links(&self, body: &MarkdownBody) -> Vec<String> {
        let options = build_options();
        let arena = Arena::new();
        let root = parse_document(&arena, body.as_str(), &options);

        let mut targets: Vec<String> = Vec::new();

        // (1) Standard Markdown `[text](url)` links via AST walk. WikiLink
        // nodes are deliberately NOT collected here — comrak runs the url
        // through `clean_url`, which can percent-encode the target. Wikilinks
        // are recovered faithfully from raw text in step (2) instead.
        for node in root.descendants() {
            let data = node.data.borrow();
            if let NodeValue::Link(link) = &data.value {
                let url = link.url.trim();
                if !url.is_empty() && !is_external_url(url) {
                    // Strip `#fragment`/`?query` so the target resolves on disk
                    // (`[a](Notes/x.md#top)` → `Notes/x.md`). A pure-fragment
                    // link (`[a](#top)`) collapses to empty and is dropped.
                    let stripped = strip_fragment_query(url);
                    if !stripped.is_empty() {
                        targets.push(stripped.to_string());
                    }
                }
            }
        }

        // (2) Obsidian `[[...]]` wikilinks via raw-text scan (target before `|`).
        targets.extend(scan_wikilinks(body.as_str()));

        // Dedup preserving first-seen order.
        let mut seen: HashSet<String> = HashSet::new();
        let mut out: Vec<String> = Vec::new();
        for t in targets {
            if seen.insert(t.clone()) {
                out.push(t);
            }
        }
        out
    }

    fn extract_tags(&self, body: &MarkdownBody) -> Vec<String> {
        let mut seen: HashSet<String> = HashSet::new();
        let mut out: Vec<String> = Vec::new();
        for tag in scan_tags(body.as_str()) {
            if seen.insert(tag.clone()) {
                out.push(tag);
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::note::MarkdownBody;

    #[test]
    fn render_html_produces_tags() {
        let m = ComrakMarkdown::new();
        let html = m.render_html(&MarkdownBody("# Hello\n\nworld".to_string()));
        assert!(html.contains('<'), "expected HTML tags, got: {html}");
        assert!(html.contains("Hello"), "missing content, got: {html}");
    }

    #[test]
    fn render_html_strips_frontmatter() {
        let m = ComrakMarkdown::new();
        let body = MarkdownBody("---\ntitle: x\n---\n# Body".to_string());
        let html = m.render_html(&body);
        assert!(!html.contains("title: x"), "frontmatter leaked: {html}");
        assert!(html.contains("Body"), "body dropped: {html}");
    }

    #[test]
    fn render_html_turns_obsidian_embed_into_img() {
        // CLAUDE.md §1.2 + §2.8: an Obsidian `![[...]]` embed must render as an
        // <img> in share HTML, not literal text. comrak doesn't parse `![[...]]`
        // natively, so render_html rewrites embeds to `![](...)` first. This is
        // the path a vault with `image_link_style = "obsidian"` exercises on
        // share (the user's actual config).
        let m = ComrakMarkdown::new();
        let html = m.render_html(&MarkdownBody("![[Attachments/x.png]]".to_string()));
        assert!(
            html.contains("<img"),
            "obsidian embed must render as <img>, got: {html}"
        );
        assert!(
            html.contains("Attachments/x.png"),
            "img src must carry the target path, got: {html}"
        );
        assert!(
            !html.contains("![["),
            "raw embed syntax leaked into HTML, got: {html}"
        );
    }

    #[test]
    fn render_html_obsidian_embed_strips_size_suffix_and_keeps_md_image() {
        // `![[x.png|200]]` → <img src="x.png"> (suffix dropped), and a normal
        // Markdown image `![](y.png)` is left untouched by the rewrite.
        let m = ComrakMarkdown::new();
        let html = m.render_html(&MarkdownBody(
            "![[Attachments/x.png|200]] and ![](Attachments/y.png)".to_string(),
        ));
        assert!(html.contains("<img"), "got: {html}");
        assert!(
            html.contains("Attachments/x.png"),
            "embed target lost, got: {html}"
        );
        assert!(
            html.contains("Attachments/y.png"),
            "normal markdown image dropped, got: {html}"
        );
        assert!(!html.contains("|200"), "size suffix leaked, got: {html}");
    }

    #[test]
    fn extract_picks_up_markdown_and_obsidian() {
        let m = ComrakMarkdown::new();
        let body = MarkdownBody("![](Attachments/a.png)\n\n![[Attachments/b.png]]".to_string());
        let refs = m.extract_attachment_links(&body);
        let has_md = refs
            .iter()
            .any(|r| r.target.as_str() == "Attachments/a.png" && r.style == LinkStyle::Markdown);
        let has_obs = refs
            .iter()
            .any(|r| r.target.as_str() == "Attachments/b.png" && r.style == LinkStyle::Obsidian);
        assert!(has_md, "missing Markdown image, got: {refs:?}");
        assert!(has_obs, "missing Obsidian embed, got: {refs:?}");
    }

    #[test]
    fn extract_rejects_escape_attempts() {
        let m = ComrakMarkdown::new();
        let body = MarkdownBody("![[../escape.png]]".to_string());
        let refs = m.extract_attachment_links(&body);
        assert!(refs.is_empty(), "expected no refs, got: {refs:?}");
    }

    #[test]
    fn extract_attachment_links_ignores_frontmatter_embeds() {
        // Defect fix: a `![[...]]` embed that appears ONLY inside the frontmatter
        // block (e.g. a `banner:` metadata field) must NOT yield an attachment
        // reference — frontmatter is metadata, not note content, and the file's
        // `build_options` doc promises it "never leaks into share HTML or
        // attachment scans". The comrak AST pass already strips frontmatter; the
        // raw-text Obsidian-embed scan now strips the leading `---\n...\n---`
        // block first too.
        let m = ComrakMarkdown::new();
        let body = MarkdownBody(
            "---\nbanner: ![[Attachments/banner.png]]\n---\n\nNo images in the body.".to_string(),
        );
        let refs = m.extract_attachment_links(&body);
        assert!(
            refs.is_empty(),
            "frontmatter embed leaked into attachment refs, got: {refs:?}"
        );
    }

    #[test]
    fn extract_note_links_finds_wikilink_and_md_link() {
        let m = ComrakMarkdown::new();
        let body = MarkdownBody("See [[Ideas/Robot]] and [notes](Notes/x.md)".to_string());
        let links = m.extract_note_links(&body);
        assert!(
            links.iter().any(|s| s == "Ideas/Robot"),
            "missing wikilink target, got: {links:?}"
        );
        assert!(
            links.iter().any(|s| s == "Notes/x.md"),
            "missing markdown link target, got: {links:?}"
        );
    }

    #[test]
    fn extract_note_links_strips_alias() {
        let m = ComrakMarkdown::new();
        let body = MarkdownBody("[[Robot|my bot]]".to_string());
        let links = m.extract_note_links(&body);
        assert!(
            links.iter().any(|s| s == "Robot"),
            "expected alias-stripped target, got: {links:?}"
        );
        assert!(
            !links.iter().any(|s| s.contains("my bot")),
            "alias leaked into target, got: {links:?}"
        );
    }

    #[test]
    fn extract_note_links_skips_external() {
        let m = ComrakMarkdown::new();
        let body = MarkdownBody("[g](https://x.com)".to_string());
        let links = m.extract_note_links(&body);
        assert!(links.is_empty(), "expected no local links, got: {links:?}");
    }

    #[test]
    fn extract_note_links_ignores_obsidian_embeds() {
        // `![[x]]` is an attachment ref, NOT a note link. Enabling the wikilinks
        // extension must not reclassify embeds as note links.
        let m = ComrakMarkdown::new();
        let body = MarkdownBody("![[Attachments/b.png]]".to_string());
        let links = m.extract_note_links(&body);
        assert!(
            links.is_empty(),
            "image embed must not be a note link, got: {links:?}"
        );
        let refs = m.extract_attachment_links(&body);
        assert!(
            refs.iter().any(|r| r.target.as_str() == "Attachments/b.png"
                && r.style == LinkStyle::Obsidian),
            "embed stopped extracting as Obsidian attachment, got: {refs:?}"
        );
    }

    #[test]
    fn extract_tags_finds_tags() {
        let m = ComrakMarkdown::new();
        let body = MarkdownBody("#hello world #a-b #todo/done".to_string());
        let tags = m.extract_tags(&body);
        assert_eq!(tags, vec!["hello", "a-b", "todo/done"], "got: {tags:?}");
    }

    #[test]
    fn extract_tags_not_in_word() {
        let m = ComrakMarkdown::new();
        let body = MarkdownBody("foo#bar".to_string());
        let tags = m.extract_tags(&body);
        assert!(tags.is_empty(), "expected no tags, got: {tags:?}");
    }

    #[test]
    fn extract_strips_obsidian_embed_size_suffix() {
        // FIX 1: `![[x|200]]`, `![[x|200x300]]`, `![[x|caption]]` must all
        // yield `x` as the attachment ref target — not `x|200` etc.
        let m = ComrakMarkdown::new();
        let body = MarkdownBody(
            "![[Attachments/b.png|200]]\n![[Attachments/c.png|200x300]]\n![[Attachments/d.png|caption]]".to_string(),
        );
        let refs = m.extract_attachment_links(&body);
        let targets: Vec<String> = refs.iter().map(|r| r.target.as_str()).collect();
        assert!(
            targets.iter().any(|t| *t == "Attachments/b.png"),
            "missing size-only strip, got: {targets:?}"
        );
        assert!(
            targets.iter().any(|t| *t == "Attachments/c.png"),
            "missing WxH strip, got: {targets:?}"
        );
        assert!(
            targets.iter().any(|t| *t == "Attachments/d.png"),
            "missing caption strip, got: {targets:?}"
        );
        assert!(
            !targets
                .iter()
                .any(|t| t.contains('|') || t.contains("200") || t.contains("caption")),
            "size/alias suffix leaked into target, got: {targets:?}"
        );
    }

    #[test]
    fn extract_tags_skips_fenced_code_blocks() {
        // FIX 2: a `#word` inside a fenced block is literal code, not a tag.
        let m = ComrakMarkdown::new();
        let body = MarkdownBody(
            "#realtag\n\n```rust\nconst x = 1; #immediatetag\n```\n\n#aftertag".to_string(),
        );
        let tags = m.extract_tags(&body);
        assert!(
            tags.contains(&"realtag".to_string()),
            "missing realtag outside fence, got: {tags:?}"
        );
        assert!(
            tags.contains(&"aftertag".to_string()),
            "missing aftertag after fence, got: {tags:?}"
        );
        assert!(
            !tags.contains(&"immediatetag".to_string()),
            "fence-leaked tag extracted, got: {tags:?}"
        );
    }

    #[test]
    fn extract_tags_supports_unicode() {
        // FIX 3: Obsidian supports Unicode tags — CJK, accented Latin.
        let m = ComrakMarkdown::new();
        let body = MarkdownBody("#标签 see #café here".to_string());
        let tags = m.extract_tags(&body);
        assert!(
            tags.contains(&"标签".to_string()),
            "missing CJK tag, got: {tags:?}"
        );
        assert!(
            tags.contains(&"café".to_string()),
            "missing accented-Latin tag, got: {tags:?}"
        );
    }

    #[test]
    fn extract_tags_unicode_word_boundary() {
        // FIX 3: a `#` preceded by a non-ASCII word char (`é`) must NOT start
        // a tag — extends the ASCII word-boundary rule to Unicode alphanumeric.
        let m = ComrakMarkdown::new();
        let body = MarkdownBody("café#x".to_string());
        let tags = m.extract_tags(&body);
        assert!(
            !tags.iter().any(|t| t == "x"),
            "Unicode word-boundary guard failed, got: {tags:?}"
        );
    }

    #[test]
    fn extract_note_links_filters_schemed_urls() {
        // FIX 4: any URL with a scheme is external — `file://`, `obsidian://`,
        // `ftp://`, `data:`, … must NOT be returned as in-vault note links.
        let m = ComrakMarkdown::new();
        let body = MarkdownBody(
            "[x](file:///etc/passwd) [a](obsidian://open?vault=V&file=Y) [n](Notes/x.md) [[WikilinkNote]]".to_string(),
        );
        let links = m.extract_note_links(&body);
        assert!(
            links.iter().any(|s| s == "Notes/x.md"),
            "missing in-vault markdown link, got: {links:?}"
        );
        assert!(
            links.iter().any(|s| s == "WikilinkNote"),
            "missing wikilink, got: {links:?}"
        );
        assert!(
            !links.iter().any(|s| s.starts_with("file:")),
            "file:// leaked as note link, got: {links:?}"
        );
        assert!(
            !links.iter().any(|s| s.starts_with("obsidian:")),
            "obsidian:// leaked as note link, got: {links:?}"
        );
    }

    #[test]
    fn extract_note_links_strips_fragment_and_query() {
        // FIX 5: schemeless link targets lose `#fragment` / `?query` so they
        // resolve as real file paths.
        let m = ComrakMarkdown::new();
        let body = MarkdownBody("[a](Notes/x.md#top) [b](Notes/y.md?v=1)".to_string());
        let links = m.extract_note_links(&body);
        assert!(
            links.iter().any(|s| s == "Notes/x.md"),
            "expected #fragment stripped, got: {links:?}"
        );
        assert!(
            links.iter().any(|s| s == "Notes/y.md"),
            "expected ?query stripped, got: {links:?}"
        );
        assert!(
            !links.iter().any(|s| s.contains('#') || s.contains('?')),
            "fragment/query leaked into target, got: {links:?}"
        );
    }
}
