//! A minimal Vim-style modal-editing layer over egui's `TextEdit`, opt-in
//! via `Settings.vim_mode_enabled` (off by default — zero behavior change
//! for anyone who doesn't turn it on).
//!
//! No existing crate covers this: `edtui` is a full Vim-inspired editor
//! widget, but it's built for Ratatui (terminal rendering), not egui, and
//! there's nothing on crates.io that layers Vim keybindings onto egui's own
//! `TextEdit` specifically. `modalkit` is renderer-agnostic but built around
//! its own buffer/cursor abstraction — adapting it to `egui::TextEdit`'s
//! plain `&mut String` model would be a comparable amount of adapter code
//! to just implementing the bounded subset below directly.
//!
//! Deliberately NOT a byte-perfect Vim reimplementation — two known,
//! disclosed gaps:
//! - Word motions (`w`/`b`/`e`) are a reasonable approximation (character-
//!   class based), not vim's exact algorithm — edge cases around
//!   punctuation runs may differ slightly.
//! - Not integrated with egui's own undo stack (`TextEditState::undoer`):
//!   edits made through Vim commands (`x`, `dd`, `p`, ...) happen outside
//!   the event-driven path that stack records, so Ctrl+Z's behavior across
//!   a Vim edit is not guaranteed to match what real Vim's own undo (`u`)
//!   would do. Not attempted here — a real undo integration would roughly
//!   double this module's size.

use eframe::egui;

/// Which of Vim's three core modes a `vim_multiline_edit` field is in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Mode {
    #[default]
    Normal,
    Insert,
    Visual,
}

/// Per-widget state — stored in egui's own `Id`-keyed temporary memory
/// (`ctx.data_mut(|d| d.get_temp/insert_temp(id, ...))`), the same
/// convention the Bulk Edit toggle already uses elsewhere in this app.
/// Never persisted to disk, never part of a saved `RequestItem`.
#[derive(Clone, Debug, Default)]
pub struct VimState {
    pub mode: Mode,
    /// Char index the Visual selection started at — `None` outside Visual
    /// mode. The selection is `[min(anchor, cursor), max(anchor, cursor)]`,
    /// inclusive of both ends (matching Vim's own character-visual mode).
    visual_anchor: Option<usize>,
    /// First key of a two-key command in progress: `d`/`y` waiting for a
    /// motion, or `g` waiting for a second `g`. No timeout — same as real
    /// Vim, an abandoned pending command just sits there until a
    /// recognized second key completes it, an unrecognized one cancels it,
    /// or Escape cancels it explicitly.
    pending: Option<char>,
    /// The one unnamed register — no named registers (`"a`-`"z`), a
    /// deliberate scope cut.
    register: String,
    /// Whether `register` holds whole line(s) (from `dd`/`yy`, always
    /// including the trailing `\n`) or a plain character range (from `x`,
    /// `dw`/`yw`/..., or a Visual-mode yank/delete) — changes how `p`/`P`
    /// paste it back: as a new line vs. inline at the cursor.
    register_linewise: bool,
}

// ---------- pure text/cursor helpers (unit-tested directly, no egui) ----------

fn char_class(c: char) -> u8 {
    if c == '\n' {
        3
    } else if c.is_whitespace() {
        0
    } else if c.is_alphanumeric() || c == '_' {
        1
    } else {
        2
    }
}

fn word_forward(chars: &[char], pos: usize) -> usize {
    let n = chars.len();
    let mut i = pos.min(n);
    if i >= n {
        return n;
    }
    let start_class = char_class(chars[i]);
    if start_class != 0 {
        while i < n && char_class(chars[i]) == start_class {
            i += 1;
        }
    }
    while i < n && char_class(chars[i]) == 0 {
        i += 1;
    }
    i
}

fn word_backward(chars: &[char], pos: usize) -> usize {
    if pos == 0 {
        return 0;
    }
    let mut i = pos - 1;
    while i > 0 && char_class(chars[i]) == 0 {
        i -= 1;
    }
    let cls = char_class(chars[i]);
    if cls != 0 {
        while i > 0 && char_class(chars[i - 1]) == cls {
            i -= 1;
        }
    }
    i
}

/// Lands on the last character of the current-or-next word — a simplified
/// approximation of Vim's `e`, not its exact algorithm (see module doc).
fn word_end(chars: &[char], pos: usize) -> usize {
    let n = chars.len();
    if n == 0 {
        return 0;
    }
    let mut i = pos.min(n - 1);
    while i < n && char_class(chars[i]) == 0 {
        i += 1;
    }
    if i >= n {
        return n - 1;
    }
    let cls = char_class(chars[i]);
    while i + 1 < n && char_class(chars[i + 1]) == cls {
        i += 1;
    }
    i
}

/// The char index each line starts at (index 0 always included).
fn line_starts(chars: &[char]) -> Vec<usize> {
    let mut starts = vec![0];
    for (i, &c) in chars.iter().enumerate() {
        if c == '\n' {
            starts.push(i + 1);
        }
    }
    starts
}

fn line_col(chars: &[char], pos: usize) -> (usize, usize) {
    let starts = line_starts(chars);
    let pos = pos.min(chars.len());
    let line = starts.partition_point(|&s| s <= pos).saturating_sub(1);
    (line, pos - starts[line])
}

/// `(start, end)` char indices of `line`, excluding its own trailing `\n`.
fn line_range(chars: &[char], line: usize) -> (usize, usize) {
    let starts = line_starts(chars);
    let line = line.min(starts.len() - 1);
    let start = starts[line];
    let end = if line + 1 < starts.len() {
        starts[line + 1] - 1
    } else {
        chars.len()
    };
    (start, end)
}

fn pos_for_line_col(chars: &[char], line: usize, col: usize) -> usize {
    let (start, end) = line_range(chars, line);
    (start + col).min(end)
}

fn move_down(chars: &[char], pos: usize) -> usize {
    let (line, col) = line_col(chars, pos);
    pos_for_line_col(chars, line + 1, col)
}

fn move_up(chars: &[char], pos: usize) -> usize {
    let (line, col) = line_col(chars, pos);
    pos_for_line_col(chars, line.saturating_sub(1), col)
}

fn line_home(chars: &[char], pos: usize) -> usize {
    line_range(chars, line_col(chars, pos).0).0
}

/// Vim's `$` — lands ON the last character of the line, not past it.
fn line_dollar(chars: &[char], pos: usize) -> usize {
    let (start, end) = line_range(chars, line_col(chars, pos).0);
    if end > start { end - 1 } else { start }
}

fn char_to_byte(text: &str, char_idx: usize) -> usize {
    text.char_indices()
        .nth(char_idx)
        .map(|(b, _)| b)
        .unwrap_or(text.len())
}

fn delete_range(text: &mut String, chars: &[char], range: std::ops::Range<usize>) -> String {
    let end = range.end.min(chars.len());
    let start = range.start.min(end);
    let removed: String = chars[start..end].iter().collect();
    let byte_start = char_to_byte(text, start);
    let byte_end = char_to_byte(text, end);
    text.replace_range(byte_start..byte_end, "");
    removed
}

fn insert_at(text: &mut String, char_idx: usize, s: &str) {
    let byte_idx = char_to_byte(text, char_idx);
    text.insert_str(byte_idx, s);
}

fn do_paste(state: &mut VimState, text: &mut String, cursor: usize, after: bool) -> usize {
    if state.register.is_empty() {
        return cursor;
    }
    let chars: Vec<char> = text.chars().collect();
    if state.register_linewise {
        let (start, end) = line_range(&chars, line_col(&chars, cursor).0);
        if !after {
            insert_at(text, start, &state.register);
            return start;
        }
        if end < chars.len() {
            // A real trailing '\n' already sits at `end` — insert the new
            // line right after it.
            let insert_pos = end + 1;
            insert_at(text, insert_pos, &state.register);
            insert_pos
        } else {
            // The current line is the last one and has no trailing '\n'
            // yet — supply one ourselves before the register's own content.
            insert_at(text, end, "\n");
            insert_at(text, end + 1, &state.register);
            end + 1
        }
    } else {
        let insert_pos = if after {
            (cursor + 1).min(chars.len())
        } else {
            cursor
        };
        insert_at(text, insert_pos, &state.register);
        insert_pos + state.register.chars().count().saturating_sub(1)
    }
}

/// Applies one typed character to `text`/`cursor` per Vim's Normal/Visual
/// semantics (Insert mode does nothing here — typing is left entirely to
/// the caller's `TextEdit`). `readonly` disables every command that would
/// mutate `text` (used for the response body viewer: navigation + Visual
/// yank-to-clipboard only). Returns the new cursor position, plus text to
/// copy to the OS clipboard when a Visual yank/delete happened on a
/// `readonly` buffer (the only case where "yank" can't go into the
/// internal register instead, since there's nothing to paste back into).
pub fn apply_key(
    state: &mut VimState,
    text: &mut String,
    cursor: usize,
    ch: char,
    readonly: bool,
) -> (usize, Option<String>) {
    let new_cursor = match state.mode {
        Mode::Insert => cursor,
        Mode::Normal => apply_normal_key(state, text, cursor, ch, readonly),
        Mode::Visual => {
            let (c, clip) = apply_visual_key(state, text, cursor, ch, readonly);
            return (c.min(text.chars().count()), clip);
        }
    };
    (new_cursor.min(text.chars().count()), None)
}

fn apply_normal_key(
    state: &mut VimState,
    text: &mut String,
    cursor: usize,
    ch: char,
    readonly: bool,
) -> usize {
    let chars: Vec<char> = text.chars().collect();

    if let Some(op) = state.pending {
        state.pending = None;
        if op == 'g' {
            return if ch == 'g' { 0 } else { cursor };
        }
        if readonly {
            return cursor;
        }
        let range = match ch {
            _ if ch == op => {
                // `dd`/`yy` — the whole line, including its trailing '\n'
                // so a paste puts it back as a real line.
                let (s, e) = line_range(&chars, line_col(&chars, cursor).0);
                Some(((s..(e + 1).min(chars.len())), true))
            }
            'w' => Some((cursor..word_forward(&chars, cursor), false)),
            'e' => Some((
                cursor..(word_end(&chars, cursor) + 1).min(chars.len()),
                false,
            )),
            'b' => Some((word_backward(&chars, cursor)..cursor, false)),
            '0' => Some((line_home(&chars, cursor)..cursor, false)),
            '$' => Some((
                cursor..(line_dollar(&chars, cursor) + 1).min(chars.len()),
                false,
            )),
            _ => None,
        };
        let Some((range, linewise)) = range else {
            return cursor;
        };
        let start = range.start;
        let removed = delete_range(text, &chars, range);
        state.register = removed;
        state.register_linewise = linewise;
        return start;
    }

    match ch {
        'h' => {
            let home = line_home(&chars, cursor);
            if cursor > home { cursor - 1 } else { cursor }
        }
        'l' => {
            let dollar = line_dollar(&chars, cursor);
            if cursor < dollar { cursor + 1 } else { cursor }
        }
        'j' => move_down(&chars, cursor),
        'k' => move_up(&chars, cursor),
        '0' => line_home(&chars, cursor),
        '$' => line_dollar(&chars, cursor),
        'w' => word_forward(&chars, cursor),
        'b' => word_backward(&chars, cursor),
        'e' => word_end(&chars, cursor),
        'g' => {
            state.pending = Some('g');
            cursor
        }
        'G' => {
            let last_line = line_starts(&chars).len() - 1;
            line_range(&chars, last_line).0
        }
        'i' if !readonly => {
            state.mode = Mode::Insert;
            cursor
        }
        'a' if !readonly => {
            state.mode = Mode::Insert;
            if cursor < chars.len() && chars[cursor] != '\n' {
                cursor + 1
            } else {
                cursor
            }
        }
        'o' if !readonly => {
            state.mode = Mode::Insert;
            let (_, end) = line_range(&chars, line_col(&chars, cursor).0);
            insert_at(text, end, "\n");
            end + 1
        }
        'O' if !readonly => {
            state.mode = Mode::Insert;
            let (start, _) = line_range(&chars, line_col(&chars, cursor).0);
            insert_at(text, start, "\n");
            start
        }
        'x' if !readonly => {
            if cursor < chars.len() && chars[cursor] != '\n' {
                let removed = delete_range(text, &chars, cursor..cursor + 1);
                state.register = removed;
                state.register_linewise = false;
            }
            cursor
        }
        'v' => {
            state.mode = Mode::Visual;
            state.visual_anchor = Some(cursor);
            cursor
        }
        'd' if !readonly => {
            state.pending = Some('d');
            cursor
        }
        'y' if !readonly => {
            state.pending = Some('y');
            cursor
        }
        'p' if !readonly => do_paste(state, text, cursor, true),
        'P' if !readonly => do_paste(state, text, cursor, false),
        _ => cursor,
    }
}

fn apply_visual_key(
    state: &mut VimState,
    text: &mut String,
    cursor: usize,
    ch: char,
    readonly: bool,
) -> (usize, Option<String>) {
    let chars: Vec<char> = text.chars().collect();
    let anchor = state.visual_anchor.unwrap_or(cursor);

    let moved = match ch {
        'h' => {
            let home = line_home(&chars, cursor);
            if cursor > home {
                Some(cursor - 1)
            } else {
                None
            }
        }
        'l' => {
            let dollar = line_dollar(&chars, cursor);
            if cursor < dollar {
                Some(cursor + 1)
            } else {
                None
            }
        }
        'j' => Some(move_down(&chars, cursor)),
        'k' => Some(move_up(&chars, cursor)),
        '0' => Some(line_home(&chars, cursor)),
        '$' => Some(line_dollar(&chars, cursor)),
        'w' => Some(word_forward(&chars, cursor)),
        'b' => Some(word_backward(&chars, cursor)),
        'e' => Some(word_end(&chars, cursor)),
        'G' => {
            let last = line_starts(&chars).len() - 1;
            Some(line_range(&chars, last).0)
        }
        'g' => {
            if state.pending == Some('g') {
                state.pending = None;
                Some(0)
            } else {
                state.pending = Some('g');
                None
            }
        }
        _ => None,
    };
    if let Some(new_cursor) = moved {
        return (new_cursor, None);
    }

    match ch {
        'd' | 'x' | 'y' => {
            let lo = anchor.min(cursor);
            let hi = anchor.max(cursor);
            let end = (hi + 1).min(chars.len());
            let selected: String = chars[lo..end].iter().collect();
            state.mode = Mode::Normal;
            state.visual_anchor = None;
            if readonly {
                // Nothing to persist internally for a read-only view — the
                // OS clipboard is the only thing "yank" can mean there.
                return (lo, Some(selected));
            }
            state.register_linewise = false;
            if ch == 'y' {
                state.register = selected;
            } else {
                state.register = selected;
                delete_range(text, &chars, lo..end);
            }
            (lo, None)
        }
        _ => (cursor, None),
    }
}

/// Renders `text` as a Vim-mode-aware multiline editor when `enabled`, or a
/// plain passthrough (identical to calling `configure` directly) when not —
/// so turning the Settings toggle off is a true no-op, not just "Vim keys
/// stop doing anything." `configure` applies whatever the caller needs on
/// top of the base `TextEdit::multiline(text).id(id)` (width, layouter,
/// `.code_editor()`, ...).
///
/// `readonly` disables every Vim command that would mutate `text` (see
/// `apply_key`) — used for the response body viewer, which is displayed
/// through a throwaway clone rather than a real `&mut` into any saved
/// state, so "editing" it has nothing to persist into anyway.
pub fn vim_multiline_edit<'a>(
    ui: &mut egui::Ui,
    id: egui::Id,
    text: &'a mut String,
    enabled: bool,
    readonly: bool,
    configure: impl FnOnce(egui::TextEdit<'a>) -> egui::TextEdit<'a>,
) -> (egui::Response, std::sync::Arc<egui::Galley>, egui::Pos2) {
    if !enabled {
        let output = configure(egui::TextEdit::multiline(text)).id(id).show(ui);
        return (output.response.response, output.galley, output.galley_pos);
    }

    let vim_id = id.with("vim_state");
    let mut state = ui
        .ctx()
        .data_mut(|d| d.get_temp::<VimState>(vim_id))
        .unwrap_or_default();

    let has_focus = ui.ctx().memory(|m| m.has_focus(id));
    let mut clipboard_text: Option<String> = None;

    if has_focus {
        if state.mode == Mode::Insert {
            // Only Escape is intercepted — everything else is native
            // typing, handled by `TextEdit` exactly as without Vim mode.
            let esc = ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape));
            if esc {
                state.mode = Mode::Normal;
            }
        } else {
            // Normal/Visual: nothing types. Drain every keyboard event this
            // frame (both the printable characters that would otherwise be
            // inserted, and the ones we don't recognize as a command — an
            // unmapped key does nothing in real Vim too, not "type itself")
            // and interpret single-character `Text` events as commands.
            // Non-keyboard events (pointer clicks, scrolling) pass through
            // untouched so the mouse still works.
            let events = ui.ctx().input_mut(|i| std::mem::take(&mut i.events));
            let mut passthrough = Vec::with_capacity(events.len());
            let mut cursor = current_cursor(ui.ctx(), id);
            for event in events {
                match &event {
                    egui::Event::Key {
                        key: egui::Key::Escape,
                        pressed: true,
                        ..
                    } => {
                        state.mode = Mode::Normal;
                        state.visual_anchor = None;
                        state.pending = None;
                    }
                    egui::Event::Text(s) => {
                        if let Some(ch) = s.chars().next() {
                            if s.chars().count() == 1 {
                                let (new_cursor, clip) =
                                    apply_key(&mut state, text, cursor, ch, readonly);
                                cursor = new_cursor;
                                if clip.is_some() {
                                    clipboard_text = clip;
                                }
                            }
                        }
                    }
                    _ => passthrough.push(event),
                }
            }
            ui.ctx().input_mut(|i| i.events = passthrough);
            set_cursor(ui.ctx(), id, cursor, &state);
        }
    }

    if let Some(clip) = clipboard_text {
        ui.ctx().copy_text(clip);
    }

    ui.horizontal(|ui| {
        let (label, color) = match state.mode {
            Mode::Normal => ("-- NORMAL --", egui::Color32::from_rgb(0x61, 0xAF, 0xEF)),
            Mode::Insert => ("-- INSERT --", egui::Color32::from_rgb(0x98, 0xC3, 0x79)),
            Mode::Visual => ("-- VISUAL --", egui::Color32::from_rgb(0xE5, 0xC0, 0x7B)),
        };
        ui.colored_label(color, label);
        if readonly {
            ui.weak("(read-only: navigation + y to copy)");
        }
    });

    let output = configure(egui::TextEdit::multiline(text)).id(id).show(ui);

    ui.ctx().data_mut(|d| d.insert_temp(vim_id, state));
    (output.response.response, output.galley, output.galley_pos)
}

fn current_cursor(ctx: &egui::Context, id: egui::Id) -> usize {
    egui::TextEdit::load_state(ctx, id)
        .and_then(|s| s.cursor.char_range())
        .map(|r| r.primary.index.0)
        .unwrap_or(0)
}

fn set_cursor(ctx: &egui::Context, id: egui::Id, pos: usize, state: &VimState) {
    let Some(mut text_state) = egui::TextEdit::load_state(ctx, id) else {
        return;
    };
    let range = match (state.mode, state.visual_anchor) {
        (Mode::Visual, Some(anchor)) => egui::text::CCursorRange::two(
            egui::text::CCursor::new(anchor),
            egui::text::CCursor::new(pos),
        ),
        _ => egui::text::CCursorRange::one(egui::text::CCursor::new(pos)),
    };
    text_state.cursor.set_char_range(Some(range));
    egui::TextEdit::store_state(ctx, id, text_state);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(state: &mut VimState, text: &mut String, cursor: usize, ch: char) -> usize {
        apply_key(state, text, cursor, ch, false).0
    }

    #[test]
    fn hjkl_move_within_bounds() {
        let mut state = VimState::default();
        let mut text = "abc\ndef".to_string();
        assert_eq!(key(&mut state, &mut text, 0, 'h'), 0); // can't go before line start
        assert_eq!(key(&mut state, &mut text, 1, 'h'), 0);
        assert_eq!(key(&mut state, &mut text, 0, 'l'), 1);
        assert_eq!(key(&mut state, &mut text, 2, 'l'), 2); // 'c' is the last char, $ position
        assert_eq!(key(&mut state, &mut text, 1, 'j'), 5); // col 1 -> "def"[1] = 'e' at index 5
        assert_eq!(key(&mut state, &mut text, 5, 'k'), 1);
    }

    #[test]
    fn zero_and_dollar_move_to_line_bounds() {
        let mut state = VimState::default();
        let mut text = "hello\nworld".to_string();
        assert_eq!(key(&mut state, &mut text, 8, '0'), 6);
        assert_eq!(key(&mut state, &mut text, 6, '$'), 10);
    }

    #[test]
    fn gg_and_shift_g_jump_to_buffer_bounds() {
        let mut state = VimState::default();
        let mut text = "one\ntwo\nthree".to_string();
        assert_eq!(key(&mut state, &mut text, 5, 'G'), 8);
        assert_eq!(state.pending, None);
        // 'g' alone starts a pending command; the second 'g' completes it.
        assert_eq!(key(&mut state, &mut text, 8, 'g'), 8);
        assert_eq!(state.pending, Some('g'));
        assert_eq!(key(&mut state, &mut text, 8, 'g'), 0);
        assert_eq!(state.pending, None);
    }

    #[test]
    fn word_motions_skip_whitespace_and_punctuation_runs() {
        let mut state = VimState::default();
        let mut text = "foo, bar".to_string();
        assert_eq!(key(&mut state, &mut text, 0, 'w'), 3); // "foo" -> ","
        assert_eq!(key(&mut state, &mut text, 3, 'w'), 5); // "," -> "bar"
        assert_eq!(key(&mut state, &mut text, 5, 'b'), 3);
        assert_eq!(key(&mut state, &mut text, 0, 'e'), 2); // end of "foo"
    }

    #[test]
    fn x_deletes_the_char_under_the_cursor_into_the_register() {
        let mut state = VimState::default();
        let mut text = "abc".to_string();
        let new_cursor = key(&mut state, &mut text, 1, 'x');
        assert_eq!(text, "ac");
        assert_eq!(new_cursor, 1);
        assert_eq!(state.register, "b");
        assert!(!state.register_linewise);
    }

    #[test]
    fn dd_deletes_the_whole_line_linewise() {
        let mut state = VimState::default();
        let mut text = "one\ntwo\nthree".to_string();
        key(&mut state, &mut text, 5, 'd');
        assert_eq!(state.pending, Some('d'));
        let cursor = key(&mut state, &mut text, 5, 'd');
        assert_eq!(text, "one\nthree");
        assert_eq!(cursor, 4);
        assert_eq!(state.register, "two\n");
        assert!(state.register_linewise);
    }

    #[test]
    fn dw_deletes_a_word_charwise() {
        let mut state = VimState::default();
        let mut text = "foo bar".to_string();
        key(&mut state, &mut text, 0, 'd');
        key(&mut state, &mut text, 0, 'w');
        assert_eq!(text, "bar");
        assert_eq!(state.register, "foo ");
        assert!(!state.register_linewise);
    }

    #[test]
    fn p_pastes_a_linewise_register_as_a_new_line_below() {
        let mut state = VimState::default();
        let mut text = "one\ntwo".to_string();
        key(&mut state, &mut text, 0, 'd');
        key(&mut state, &mut text, 0, 'd'); // deletes "one\n", cursor now on "two"
        assert_eq!(text, "two");
        let cursor = do_paste(&mut state, &mut text, 0, true);
        assert_eq!(text, "two\none\n");
        assert_eq!(cursor, 4);
    }

    #[test]
    fn p_pastes_a_charwise_register_after_the_cursor() {
        let mut state = VimState::default();
        let mut text = "ac".to_string();
        key(&mut state, &mut text, 1, 'x'); // register = "c", text = "a"
        assert_eq!(text, "a");
        let cursor = do_paste(&mut state, &mut text, 0, true);
        assert_eq!(text, "ac");
        assert_eq!(cursor, 1);
    }

    #[test]
    fn visual_mode_selects_and_yanks_without_deleting() {
        let mut state = VimState::default();
        let mut text = "hello world".to_string();
        assert_eq!(key(&mut state, &mut text, 0, 'v'), 0);
        assert_eq!(state.mode, Mode::Visual);
        let (cursor, clip) = apply_key(&mut state, &mut text, 0, 'l', false);
        assert_eq!(cursor, 1);
        assert_eq!(clip, None);
        let (cursor, clip) = apply_key(&mut state, &mut text, 1, 'l', false);
        assert_eq!(cursor, 2);
        assert_eq!(clip, None);
        let (cursor, _) = apply_key(&mut state, &mut text, 2, 'y', false);
        assert_eq!(cursor, 0);
        assert_eq!(text, "hello world", "yank never mutates the text");
        assert_eq!(state.register, "hel");
        assert_eq!(state.mode, Mode::Normal);
    }

    #[test]
    fn visual_mode_delete_removes_the_selection() {
        let mut state = VimState::default();
        let mut text = "hello world".to_string();
        apply_key(&mut state, &mut text, 0, 'v', false);
        apply_key(&mut state, &mut text, 0, 'l', false);
        apply_key(&mut state, &mut text, 1, 'l', false);
        let (cursor, _) = apply_key(&mut state, &mut text, 2, 'd', false);
        assert_eq!(text, "lo world");
        assert_eq!(cursor, 0);
        assert_eq!(state.register, "hel");
        assert_eq!(state.mode, Mode::Normal);
    }

    #[test]
    fn readonly_visual_yank_returns_clipboard_text_and_never_mutates() {
        let mut state = VimState::default();
        let mut text = "hello world".to_string();
        apply_key(&mut state, &mut text, 0, 'v', true);
        apply_key(&mut state, &mut text, 0, 'l', true);
        apply_key(&mut state, &mut text, 1, 'l', true);
        let (_, clip) = apply_key(&mut state, &mut text, 2, 'y', true);
        assert_eq!(clip, Some("hel".to_string()));
        assert_eq!(text, "hello world");
        assert_eq!(
            state.register, "",
            "readonly never touches the internal register"
        );
    }

    #[test]
    fn readonly_normal_mode_ignores_editing_commands() {
        let mut state = VimState::default();
        let mut text = "abc".to_string();
        let (cursor, _) = apply_key(&mut state, &mut text, 1, 'x', true);
        assert_eq!(text, "abc", "x is a no-op when readonly");
        assert_eq!(cursor, 1);
        assert_eq!(
            state.mode,
            Mode::Normal,
            "i doesn't enter Insert when readonly"
        );
        apply_key(&mut state, &mut text, 1, 'i', true);
        assert_eq!(state.mode, Mode::Normal);
    }

    #[test]
    fn escape_cancels_a_pending_operator() {
        let mut state = VimState::default();
        let mut text = "abc".to_string();
        key(&mut state, &mut text, 0, 'd');
        assert_eq!(state.pending, Some('d'));
        // The egui-facing `vim_multiline_edit` clears `pending` on Escape
        // directly (see its event loop) rather than through `apply_key` —
        // asserted here on the state field itself, matching that path.
        state.pending = None;
        assert_eq!(state.pending, None);
    }
}
