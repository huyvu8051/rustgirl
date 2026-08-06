use eframe::egui;
use egui::text::LayoutJob;
use egui::{Color32, FontId, TextFormat};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Language {
    Json,
    Markup,
    Plain,
}

/// Picks a highlighter language from a `Content-Type` header value.
pub fn language_for_content_type(content_type: &str) -> Language {
    if content_type.contains("json") {
        Language::Json
    } else if content_type.contains("xml") || content_type.contains("html") {
        Language::Markup
    } else {
        Language::Plain
    }
}

/// Best-effort language guess for text with no (or an unhelpful) Content-Type,
/// e.g. a Raw request body the user is still typing.
pub fn sniff_language(text: &str) -> Language {
    let trimmed = text.trim_start();
    if (trimmed.starts_with('{') || trimmed.starts_with('['))
        && serde_json::from_str::<serde_json::Value>(text).is_ok()
    {
        return Language::Json;
    }
    if trimmed.starts_with('<') {
        return Language::Markup;
    }
    Language::Plain
}

/// Byte ranges in `text` that look like a clickable URL. Not a strict parser —
/// just enough to underline it in the highlighter and let the caller offer
/// Option/Alt+Click-to-open without mistaking JSON/XML punctuation around a
/// URL for part of it.
pub fn detect_links(text: &str) -> Vec<std::ops::Range<usize>> {
    let bytes = text.as_bytes();
    let mut links = Vec::new();
    let mut i = 0usize;
    while i < text.len() {
        let rest = &text[i..];
        if rest.starts_with("http://") || rest.starts_with("https://") {
            let start = i;
            let mut j = i;
            while j < text.len()
                && !bytes[j].is_ascii_whitespace()
                && !matches!(
                    bytes[j],
                    b'"' | b'\'' | b'<' | b'>' | b',' | b')' | b']' | b'}'
                )
            {
                j += 1;
            }
            let mut end = j;
            while end > start && matches!(bytes[end - 1], b'.' | b',' | b';' | b':' | b'!' | b'?') {
                end -= 1;
            }
            if end > start {
                links.push(start..end);
            }
            i = j.max(start + 1);
        } else {
            i += rest.chars().next().map_or(1, char::len_utf8);
        }
    }
    links
}

/// Byte ranges in `text` matching `query` (ASCII case-insensitive). Empty
/// `query` yields no matches. ASCII-only case folding keeps byte offsets
/// aligned between the lowercased haystack and the original text.
pub fn find_matches(text: &str, query: &str) -> Vec<std::ops::Range<usize>> {
    if query.is_empty() {
        return Vec::new();
    }
    let haystack = text.to_ascii_lowercase();
    let needle = query.to_ascii_lowercase();
    let mut matches = Vec::new();
    let mut start = 0usize;
    while start <= haystack.len() {
        let Some(pos) = haystack[start..].find(&needle) else {
            break;
        };
        let match_start = start + pos;
        let match_end = match_start + needle.len();
        matches.push(match_start..match_end);
        start = match_end.max(match_start + 1);
    }
    matches
}

struct Palette {
    key: Color32,
    string: Color32,
    number: Color32,
    keyword: Color32,
    punctuation: Color32,
    plain: Color32,
    tag: Color32,
    attr_name: Color32,
    attr_value: Color32,
    comment: Color32,
    search_highlight: Color32,
}

impl Palette {
    fn for_theme(dark: bool) -> Self {
        if dark {
            Self {
                key: Color32::from_rgb(0x9C, 0xDC, 0xFE),
                string: Color32::from_rgb(0xCE, 0x91, 0x78),
                number: Color32::from_rgb(0xB5, 0xCE, 0xA8),
                keyword: Color32::from_rgb(0x56, 0x9C, 0xD6),
                punctuation: Color32::from_rgb(0xD4, 0xD4, 0xD4),
                plain: Color32::from_rgb(0xD4, 0xD4, 0xD4),
                tag: Color32::from_rgb(0x56, 0x9C, 0xD6),
                attr_name: Color32::from_rgb(0x9C, 0xDC, 0xFE),
                attr_value: Color32::from_rgb(0xCE, 0x91, 0x78),
                comment: Color32::from_rgb(0x6A, 0x99, 0x55),
                search_highlight: Color32::from_rgba_unmultiplied(0xFF, 0xD5, 0x00, 110),
            }
        } else {
            Self {
                key: Color32::from_rgb(0x00, 0x10, 0x80),
                string: Color32::from_rgb(0xA3, 0x15, 0x15),
                number: Color32::from_rgb(0x09, 0x86, 0x58),
                keyword: Color32::from_rgb(0x00, 0x00, 0xFF),
                punctuation: Color32::from_rgb(0x30, 0x30, 0x30),
                plain: Color32::from_rgb(0x30, 0x30, 0x30),
                tag: Color32::from_rgb(0x80, 0x00, 0x00),
                attr_name: Color32::from_rgb(0xE5, 0x00, 0x00),
                attr_value: Color32::from_rgb(0x04, 0x51, 0xA5),
                comment: Color32::from_rgb(0x00, 0x80, 0x00),
                search_highlight: Color32::from_rgba_unmultiplied(0xFF, 0xE0, 0x66, 160),
            }
        }
    }
}

type Ranges<'a> = &'a [std::ops::Range<usize>];

/// Tokenizes and colorizes `text` for the given `language`. Not a real
/// parser: it's a best-effort scanner good enough to make JSON/XML/HTML
/// readable in the editor, not a validator — malformed input just falls back
/// to plain-colored spans instead of panicking. Detected URLs (see
/// [`detect_links`]) get underlined, and any `search_query` match gets a
/// background highlight, both independent of the base token coloring.
pub fn highlight(
    dark: bool,
    font_id: FontId,
    text: &str,
    language: Language,
    search_query: &str,
) -> LayoutJob {
    let palette = Palette::for_theme(dark);
    let links = detect_links(text);
    let matches = find_matches(text, search_query);
    match language {
        Language::Json => highlight_json(&palette, &font_id, text, &links, &matches),
        Language::Markup => highlight_markup(&palette, &font_id, text, &links, &matches),
        Language::Plain => {
            let mut job = LayoutJob::default();
            push(
                &mut job,
                text,
                0..text.len(),
                &font_id,
                palette.plain,
                &links,
                &matches,
                &palette,
            );
            job
        }
    }
}

#[derive(Clone)]
struct CacheEntry {
    text: String,
    dark: bool,
    language: Language,
    search_query: String,
    job: LayoutJob,
}

/// Same as [`highlight`], but memoized in `ctx`'s memory under `cache_id` so
/// re-tokenizing (JSON/markup scanning + link/search-match detection) is
/// skipped whenever `text`/`dark`/`language`/`search_query` are unchanged
/// from the last call with that id — e.g. every frame while the window is
/// being resized, where only the wrap width changes and the text itself
/// doesn't. The caller still needs to set `.wrap.max_width` on the returned
/// job themselves, since that's exactly the part that legitimately varies
/// frame-to-frame.
pub fn highlight_cached(
    ctx: &egui::Context,
    cache_id: egui::Id,
    dark: bool,
    font_id: FontId,
    text: &str,
    language: Language,
    search_query: &str,
) -> LayoutJob {
    let cached = ctx.data_mut(|d| d.get_temp::<CacheEntry>(cache_id));
    if let Some(entry) = &cached
        && entry.dark == dark
        && entry.language == language
        && entry.search_query == search_query
        && entry.text == text
    {
        return entry.job.clone();
    }

    let job = highlight(dark, font_id, text, language, search_query);
    ctx.data_mut(|d| {
        d.insert_temp(
            cache_id,
            CacheEntry {
                text: text.to_string(),
                dark,
                language,
                search_query: search_query.to_string(),
                job: job.clone(),
            },
        );
    });
    job
}

/// Splits `range` at the boundaries of any `markers` it overlaps, invoking
/// `emit(sub_range, inside_marker)` for each contiguous piece in order.
fn split_by_markers(
    range: std::ops::Range<usize>,
    markers: Ranges<'_>,
    mut emit: impl FnMut(std::ops::Range<usize>, bool),
) {
    let mut cursor = range.start;
    for m in markers {
        let start = m.start.max(range.start);
        let end = m.end.min(range.end);
        if start >= end {
            continue;
        }
        if start > cursor {
            emit(cursor..start, false);
        }
        emit(start..end, true);
        cursor = end;
    }
    if cursor < range.end {
        emit(cursor..range.end, false);
    }
}

/// Appends `text[range]` to `job` in `color`, first splitting it at any
/// `links` it overlaps (adds an underline) and then, within each of those
/// pieces, at any `search_matches` (adds a background highlight) — so a
/// search hit inside a link, or inside a JSON string's quotes, still gets
/// exactly the right styling for just that sub-span.
#[allow(clippy::too_many_arguments)]
fn push(
    job: &mut LayoutJob,
    text: &str,
    range: std::ops::Range<usize>,
    font_id: &FontId,
    color: Color32,
    links: Ranges<'_>,
    search_matches: Ranges<'_>,
    palette: &Palette,
) {
    if range.start >= range.end {
        return;
    }
    split_by_markers(range, links, |sub_range, is_link| {
        split_by_markers(sub_range, search_matches, |final_range, is_match| {
            append_span(
                job,
                text,
                final_range,
                font_id,
                color,
                is_link,
                is_match,
                palette,
            );
        });
    });
}

#[allow(clippy::too_many_arguments)]
fn append_span(
    job: &mut LayoutJob,
    text: &str,
    range: std::ops::Range<usize>,
    font_id: &FontId,
    color: Color32,
    underline: bool,
    highlighted: bool,
    palette: &Palette,
) {
    if range.start >= range.end {
        return;
    }
    let mut format = TextFormat {
        font_id: font_id.clone(),
        color,
        ..Default::default()
    };
    if underline {
        format.underline = egui::Stroke::new(1.0, color);
    }
    if highlighted {
        format.background = palette.search_highlight;
    }
    job.append(&text[range], 0.0, format);
}

fn highlight_json(
    palette: &Palette,
    font_id: &FontId,
    text: &str,
    links: Ranges<'_>,
    matches: Ranges<'_>,
) -> LayoutJob {
    let mut job = LayoutJob::default();
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let len = text.len();
    let byte_at = |idx: usize| chars.get(idx).map(|(p, _)| *p).unwrap_or(len);

    let mut i = 0usize;
    while i < chars.len() {
        let (start, c) = chars[i];
        if c.is_whitespace() {
            let mut j = i + 1;
            while j < chars.len() && chars[j].1.is_whitespace() {
                j += 1;
            }
            push(
                &mut job,
                text,
                start..byte_at(j),
                font_id,
                palette.plain,
                links,
                matches,
                palette,
            );
            i = j;
        } else if c == '"' {
            let mut j = i + 1;
            while j < chars.len() {
                let cc = chars[j].1;
                if cc == '\\' {
                    j += 2;
                    continue;
                }
                if cc == '"' {
                    j += 1;
                    break;
                }
                j += 1;
            }
            let end = byte_at(j);
            let mut k = j;
            while k < chars.len() && chars[k].1.is_whitespace() {
                k += 1;
            }
            let is_key = chars.get(k).is_some_and(|(_, cc)| *cc == ':');
            push(
                &mut job,
                text,
                start..end,
                font_id,
                if is_key { palette.key } else { palette.string },
                links,
                matches,
                palette,
            );
            i = j;
        } else if c == '-' || c.is_ascii_digit() {
            let mut j = i + 1;
            while j < chars.len() && matches!(chars[j].1, '0'..='9' | '.' | 'e' | 'E' | '+' | '-') {
                j += 1;
            }
            push(
                &mut job,
                text,
                start..byte_at(j),
                font_id,
                palette.number,
                links,
                matches,
                palette,
            );
            i = j;
        } else if c.is_alphabetic() {
            let mut j = i + 1;
            while j < chars.len() && chars[j].1.is_alphanumeric() {
                j += 1;
            }
            let end = byte_at(j);
            let word = &text[start..end];
            let color = if matches!(word, "true" | "false" | "null") {
                palette.keyword
            } else {
                palette.plain
            };
            push(
                &mut job,
                text,
                start..end,
                font_id,
                color,
                links,
                matches,
                palette,
            );
            i = j;
        } else if "{}[]:,".contains(c) {
            push(
                &mut job,
                text,
                start..start + c.len_utf8(),
                font_id,
                palette.punctuation,
                links,
                matches,
                palette,
            );
            i += 1;
        } else {
            push(
                &mut job,
                text,
                start..start + c.len_utf8(),
                font_id,
                palette.plain,
                links,
                matches,
                palette,
            );
            i += 1;
        }
    }
    job
}

fn highlight_markup(
    palette: &Palette,
    font_id: &FontId,
    text: &str,
    links: Ranges<'_>,
    matches: Ranges<'_>,
) -> LayoutJob {
    let mut job = LayoutJob::default();
    let len = text.len();
    let mut i = 0usize;
    while i < len {
        if text[i..].starts_with("<!--") {
            let end = text[i..].find("-->").map_or(len, |p| i + p + 3);
            push(
                &mut job,
                text,
                i..end,
                font_id,
                palette.comment,
                links,
                matches,
                palette,
            );
            i = end;
        } else if text.as_bytes()[i] == b'<' {
            let tag_end = text[i..].find('>').map_or(len, |p| i + p + 1);
            highlight_tag(&mut job, palette, font_id, text, i, tag_end, links, matches);
            i = tag_end;
        } else {
            let next_lt = text[i..].find('<').map_or(len, |p| i + p);
            push(
                &mut job,
                text,
                i..next_lt,
                font_id,
                palette.plain,
                links,
                matches,
                palette,
            );
            i = next_lt;
        }
    }
    job
}

/// Colorizes a single `<...>` tag (including a closing `</...>` or
/// self-closing `.../>`  one), assuming ASCII tag/attribute syntax — the only
/// bytes inspected directly are `< > / = "` and whitespace, so any UTF-8
/// content in attribute values or text is passed through untouched.
#[allow(clippy::too_many_arguments)]
fn highlight_tag(
    job: &mut LayoutJob,
    palette: &Palette,
    font_id: &FontId,
    text: &str,
    start: usize,
    end: usize,
    links: Ranges<'_>,
    matches: Ranges<'_>,
) {
    let bytes = text.as_bytes();
    let mut i = start;

    let mut j = i + 1;
    if bytes.get(j) == Some(&b'/') {
        j += 1;
    }
    push(
        job,
        text,
        i..j,
        font_id,
        palette.punctuation,
        links,
        matches,
        palette,
    );
    i = j;

    let name_start = i;
    while i < end && !bytes[i].is_ascii_whitespace() && bytes[i] != b'>' && bytes[i] != b'/' {
        i += 1;
    }
    push(
        job,
        text,
        name_start..i,
        font_id,
        palette.tag,
        links,
        matches,
        palette,
    );

    while i < end {
        let ws_start = i;
        while i < end && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        push(
            job,
            text,
            ws_start..i,
            font_id,
            palette.plain,
            links,
            matches,
            palette,
        );
        if i >= end {
            break;
        }

        match bytes[i] {
            b'>' => {
                push(
                    job,
                    text,
                    i..i + 1,
                    font_id,
                    palette.punctuation,
                    links,
                    matches,
                    palette,
                );
                break;
            }
            b'/' => {
                push(
                    job,
                    text,
                    i..i + 1,
                    font_id,
                    palette.punctuation,
                    links,
                    matches,
                    palette,
                );
                i += 1;
                continue;
            }
            _ => {}
        }

        let name_start = i;
        while i < end && !matches!(bytes[i], b'=' | b'>' | b'/') && !bytes[i].is_ascii_whitespace()
        {
            i += 1;
        }
        push(
            job,
            text,
            name_start..i,
            font_id,
            palette.attr_name,
            links,
            matches,
            palette,
        );

        let ws_start = i;
        while i < end && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        push(
            job,
            text,
            ws_start..i,
            font_id,
            palette.plain,
            links,
            matches,
            palette,
        );

        if i < end && bytes[i] == b'=' {
            push(
                job,
                text,
                i..i + 1,
                font_id,
                palette.punctuation,
                links,
                matches,
                palette,
            );
            i += 1;
            let ws_start = i;
            while i < end && bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            push(
                job,
                text,
                ws_start..i,
                font_id,
                palette.plain,
                links,
                matches,
                palette,
            );

            if i < end && matches!(bytes[i], b'"' | b'\'') {
                let quote = bytes[i];
                let value_start = i;
                i += 1;
                while i < end && bytes[i] != quote {
                    i += 1;
                }
                if i < end {
                    i += 1;
                }
                push(
                    job,
                    text,
                    value_start..i,
                    font_id,
                    palette.attr_value,
                    links,
                    matches,
                    palette,
                );
            }
        }
    }
}
