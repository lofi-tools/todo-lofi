//! VT parsing and the frame the canvas paints.
//!
//! `libghostty-vt` needs a Zig toolchain that this machine does not have, so the
//! documented fallback engine is used: `alacritty_terminal` behind the core's
//! terminal boundary.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use alacritty_terminal::event::{Event, EventListener};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Boundary, Column, Direction, Line, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionRange, SelectionType};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::color::Colors;
use alacritty_terminal::term::search::{BuildError, RegexSearch};
use alacritty_terminal::term::{Config, Term, TermMode};
use alacritty_terminal::vte::ansi::{
    Color, CursorShape, NamedColor, NamedPrivateMode, Processor, Rgb,
};
use alacritty_terminal::vte::{Params, Parser, Perform};
use compact_str::CompactString;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TerminalAppearance {
    light: bool,
    more_contrast: bool,
}

impl TerminalAppearance {
    fn foreground(self) -> u32 {
        match (self.light, self.more_contrast) {
            (true, true) => 0x000000,
            (true, false) => 0x242633,
            (false, true) => 0xffffff,
            (false, false) => 0xe6e6ea,
        }
    }

    fn background(self) -> u32 {
        match (self.light, self.more_contrast) {
            (true, _) => 0xffffff,
            (false, true) => 0x08080a,
            (false, false) => 0x131316,
        }
    }

    fn ansi(self) -> &'static [u32; 16] {
        if self.light {
            &[
                0x21232b, 0x963539, 0x35602f, 0x76500f, 0x304d9c, 0x653c9b, 0x206367, 0x525565,
                0x53596a, 0xa32c45, 0x286541, 0x805309, 0x344eaf, 0x7345a2, 0x236675, 0x333747,
            ]
        } else {
            &[
                0x1d1d22, 0xe06a6a, 0x7fb37f, 0xd9b06a, 0x6f8fdc, 0xb48cff, 0x6fbfc0, 0xd6d6de,
                0x888897, 0xf08a8a, 0x9fd39f, 0xf0cc8a, 0x8faff0, 0xccb3ff, 0x8fd6d6, 0xffffff,
            ]
        }
    }
}

#[derive(Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalFrame {
    pub revision: u32,
    pub columns: usize,
    pub rows: usize,
    pub cursor_column: usize,
    pub cursor_row: usize,
    pub cursor_visible: bool,
    pub cursor_style: u8,
    pub display_offset: usize,
    pub modes: u32,
    pub cells: Vec<FrameCell>,
    pub search_highlight: Option<SearchHighlight>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct SearchHighlight {
    id: u32,
    start: u32,
    end: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FrameCell {
    pub text: CompactString,
    pub width: u8,
    pub foreground: u32,
    pub background: u32,
    pub bold: bool,
    pub italic: bool,
    pub linked: bool,
    pub wraps: bool,
    pub underline: bool,
    pub strikeout: bool,
}

impl TerminalFrame {
    pub fn encode(&self, previous: Option<&Self>) -> Vec<u8> {
        let previous = previous.filter(|frame| {
            frame.columns == self.columns
                && frame.rows == self.rows
                && frame.display_offset == self.display_offset
        });
        let full = previous.is_none();
        let mut bytes = Vec::new();
        bytes.extend([
            1,
            u8::from(self.cursor_visible)
                | (u8::from(full) << 1)
                | (self.cursor_style << 2)
                | (u8::from(self.search_highlight.is_some()) << 4),
        ]);
        bytes.extend((self.columns as u16).to_le_bytes());
        bytes.extend((self.rows as u16).to_le_bytes());
        bytes.extend((self.cursor_column as u16).to_le_bytes());
        bytes.extend((self.cursor_row as u16).to_le_bytes());
        bytes.extend((self.display_offset as u32).to_le_bytes());
        bytes.extend(self.modes.to_le_bytes());
        let row_count_offset = bytes.len();
        bytes.extend([0, 0]);

        let mut row_count = 0u16;
        for (index, row) in self.cells.chunks_exact(self.columns).enumerate() {
            let start = index * self.columns;
            if previous.and_then(|frame| frame.cells.get(start..start + self.columns)) == Some(row)
            {
                continue;
            }
            row_count += 1;
            bytes.extend((index as u16).to_le_bytes());
            let count_offset = bytes.len();
            bytes.extend([0, 0]);
            let mut run_count = 0u16;
            let mut remaining = row;
            while let Some((cell, rest)) = remaining.split_first() {
                let run_length = 1 + rest.iter().take_while(|next| *next == cell).count();
                bytes.extend((run_length as u16).to_le_bytes());
                let flags = cell.width
                    | (u8::from(cell.bold) << 2)
                    | (u8::from(cell.italic) << 3)
                    | (u8::from(cell.linked) << 4)
                    | (u8::from(cell.wraps) << 5)
                    | (u8::from(cell.underline) << 6)
                    | (u8::from(cell.strikeout) << 7);
                bytes.push(flags);
                bytes.extend(cell.foreground.to_le_bytes());
                bytes.extend(cell.background.to_le_bytes());
                let text = cell.text.as_bytes();
                bytes.extend((text.len() as u32).to_le_bytes());
                bytes.extend(text);
                remaining = &remaining[run_length..];
                run_count += 1;
            }
            bytes[count_offset..count_offset + 2].copy_from_slice(&run_count.to_le_bytes());
        }
        bytes[row_count_offset..row_count_offset + 2].copy_from_slice(&row_count.to_le_bytes());
        if let Some(highlight) = self.search_highlight {
            bytes.extend(highlight.id.to_le_bytes());
            bytes.extend(highlight.start.to_le_bytes());
            bytes.extend(highlight.end.to_le_bytes());
        }
        bytes
    }
}

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SearchDirection {
    Previous,
    Next,
}

struct ScreenSize {
    columns: usize,
    rows: usize,
}

impl Dimensions for ScreenSize {
    fn total_lines(&self) -> usize {
        self.rows
    }

    fn screen_lines(&self) -> usize {
        self.rows
    }

    fn columns(&self) -> usize {
        self.columns
    }
}

enum PtyReply {
    Text(String),
    Color(usize, Arc<dyn Fn(Rgb) -> String + Sync + Send>),
}

#[derive(Clone, Default)]
struct PtyReplies {
    pending: Arc<Mutex<Vec<PtyReply>>>,
    bell: Arc<AtomicBool>,
}

impl EventListener for PtyReplies {
    fn send_event(&self, event: Event) {
        let reply = match event {
            Event::PtyWrite(text) => PtyReply::Text(text),
            Event::ColorRequest(index, format) => PtyReply::Color(index, format),
            Event::Bell => {
                self.bell.store(true, Ordering::Relaxed);
                return;
            }
            _ => return,
        };
        self.pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(reply);
    }
}

const KITTY_STACK_LIMIT: usize = 64;
const KITTY_FLAGS: u16 = 0b1_1111;

#[derive(Default)]
struct KeyboardModes {
    flags: u16,
    stack: VecDeque<u16>,
}

enum KeyboardCommand {
    Query,
    Set { flags: u16, operation: u16 },
    Push(u16),
    Pop(u16),
    AlternateScreen(bool),
    Reset,
    ColorQuery,
}

#[derive(Default)]
struct KeyboardSequence(Option<KeyboardCommand>);

impl Perform for KeyboardSequence {
    fn csi_dispatch(&mut self, params: &Params, intermediates: &[u8], ignore: bool, action: char) {
        if ignore {
            return;
        }
        let mut params = params.iter().filter_map(|param| param.first().copied());
        if matches!(action, 'h' | 'l')
            && intermediates == b"?"
            && params.any(|mode| mode == NamedPrivateMode::SwapScreenAndSetRestoreCursor as u16)
        {
            self.0 = Some(KeyboardCommand::AlternateScreen(action == 'h'));
        }
        if action != 'u' {
            return;
        }
        let first = params.next().unwrap_or(0);
        self.0 = match intermediates {
            b"?" => Some(KeyboardCommand::Query),
            b"=" => Some(KeyboardCommand::Set {
                flags: first & KITTY_FLAGS,
                operation: params.next().unwrap_or(1),
            }),
            b">" => Some(KeyboardCommand::Push(first & KITTY_FLAGS)),
            b"<" => Some(KeyboardCommand::Pop(first.max(1))),
            _ => None,
        };
    }

    fn osc_dispatch(&mut self, params: &[&[u8]], _: bool) {
        if let [command, rest @ ..] = params
            && matches!(*command, b"4" | b"10" | b"11" | b"12")
            && rest.contains(&b"?".as_slice())
        {
            self.0 = Some(KeyboardCommand::ColorQuery);
        }
    }

    fn esc_dispatch(&mut self, intermediates: &[u8], ignore: bool, byte: u8) {
        if !ignore && intermediates.is_empty() && byte == b'c' {
            self.0 = Some(KeyboardCommand::Reset);
        }
    }

    fn terminated(&self) -> bool {
        self.0.is_some()
    }
}

pub struct Screen {
    appearance: TerminalAppearance,
    term: Term<PtyReplies>,
    replies: PtyReplies,
    ready_replies: Vec<u8>,
    parser: Processor,
    keyboard_parser: Parser,
    keyboards: [KeyboardModes; 2],
    keyboard_alternate_screen: bool,
    search_id: u32,
    search_text: String,
    search_span: i32,
    columns: usize,
    rows: usize,
}

impl Screen {
    #[cfg(test)]
    pub fn new(columns: usize, rows: usize) -> Self {
        Self::with_scrollback(columns, rows, crate::settings::DEFAULT_SCROLLBACK)
    }

    pub fn with_scrollback(columns: usize, rows: usize, scrolling_history: usize) -> Self {
        let size = ScreenSize { columns, rows };
        let replies = PtyReplies::default();
        Self {
            appearance: TerminalAppearance::default(),
            term: Term::new(
                Config {
                    scrolling_history,
                    ..Config::default()
                },
                &size,
                replies.clone(),
            ),
            replies,
            ready_replies: Vec::new(),
            parser: Processor::new(),
            keyboard_parser: Parser::new(),
            keyboards: Default::default(),
            keyboard_alternate_screen: false,
            search_id: 0,
            search_text: String::new(),
            search_span: 0,
            columns,
            rows,
        }
    }

    pub fn set_appearance(&mut self, appearance: TerminalAppearance) -> bool {
        if self.appearance == appearance {
            return false;
        }
        self.appearance = appearance;
        true
    }

    pub fn process(&mut self, mut bytes: &[u8]) {
        // Alacritty 0.26 can panic when its keyboard stack fills. Parse just
        // these commands separately until a release includes the upstream fix.
        while !bytes.is_empty() {
            let mut sequence = KeyboardSequence::default();
            let consumed = self
                .keyboard_parser
                .advance_until_terminated(&mut sequence, bytes);
            let (parsed, remaining) = bytes.split_at(consumed);
            self.parser.advance(&mut self.term, parsed);
            if let Some(command) = sequence.0 {
                self.apply_keyboard_command(command);
            }
            bytes = remaining;
        }
        self.discard_replaced_search();
    }

    pub fn sync_deadline(&self) -> Option<Instant> {
        self.parser.sync_timeout().sync_timeout()
    }

    pub fn finish_sync(&mut self) {
        self.parser.stop_sync(&mut self.term);
        self.discard_replaced_search();
    }

    fn apply_keyboard_command(&mut self, command: KeyboardCommand) {
        // VTE can defer applying a screen switch during synchronized output.
        // Keyboard commands still need the screen selected by the byte stream.
        let keyboard = &mut self.keyboards[usize::from(self.keyboard_alternate_screen)];
        match command {
            KeyboardCommand::Query => {
                self.replies
                    .send_event(Event::PtyWrite(format!("\x1b[?{}u", keyboard.flags)));
            }
            KeyboardCommand::Set { flags, operation } => {
                keyboard.flags = match operation {
                    2 => keyboard.flags | flags,
                    3 => keyboard.flags & !flags,
                    _ => flags,
                };
            }
            KeyboardCommand::Push(flags) => {
                if keyboard.stack.len() == KITTY_STACK_LIMIT {
                    keyboard.stack.pop_front();
                }
                keyboard.stack.push_back(keyboard.flags);
                keyboard.flags = flags;
            }
            KeyboardCommand::Pop(count) => {
                for _ in 0..count {
                    keyboard.flags = keyboard.stack.pop_back().unwrap_or(0);
                    if keyboard.stack.is_empty() {
                        keyboard.flags = 0;
                        break;
                    }
                }
            }
            KeyboardCommand::AlternateScreen(alternate) => {
                self.keyboard_alternate_screen = alternate
            }
            KeyboardCommand::Reset => {
                self.keyboard_alternate_screen = false;
                self.keyboards = Default::default();
            }
            KeyboardCommand::ColorQuery => self.resolve_replies(),
        }
    }

    fn resolve_replies(&mut self) {
        let pending = std::mem::take(
            &mut *self
                .replies
                .pending
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        );
        for reply in pending {
            let text = match reply {
                PtyReply::Text(text) => text,
                PtyReply::Color(index, format) => {
                    let color = match index {
                        0..=255 => Color::Indexed(index as u8),
                        256 => Color::Named(NamedColor::Foreground),
                        257 => Color::Named(NamedColor::Background),
                        258 => Color::Named(NamedColor::Cursor),
                        _ => continue,
                    };
                    let value = color_to_rgb(self.term.colors(), color, self.appearance);
                    format(Rgb {
                        r: (value >> 16) as u8,
                        g: (value >> 8) as u8,
                        b: value as u8,
                    })
                }
            };
            self.ready_replies.extend_from_slice(text.as_bytes());
        }
    }

    pub fn take_replies(&mut self) -> Vec<u8> {
        self.resolve_replies();
        std::mem::take(&mut self.ready_replies)
    }

    pub fn take_bell(&self) -> bool {
        self.replies.bell.swap(false, Ordering::Relaxed)
    }

    pub fn resize(&mut self, columns: usize, rows: usize) {
        self.columns = columns;
        self.rows = rows;
        self.term.resize(ScreenSize { columns, rows });
    }

    pub fn scroll(&mut self, lines: i32) {
        self.term
            .scroll_display(Scroll::Delta(lines.clamp(-256, 256)));
    }

    pub fn search(
        &mut self,
        query: &str,
        anchor: Option<u32>,
        direction: SearchDirection,
    ) -> Result<Option<u32>, Box<BuildError>> {
        if query.is_empty() {
            return Ok(None);
        }
        let mut pattern = String::new();
        for character in query.chars() {
            if r"\.^$|?*+()[]{}".contains(character) {
                pattern.push('\\');
            }
            pattern.push(character);
        }
        let mut regex = RegexSearch::new(&pattern)?;
        let grid = self.term.grid();
        let selection = if anchor == Some(self.search_id) {
            self.search_selection()
        } else {
            None
        };
        let (origin, direction, side) = match direction {
            SearchDirection::Previous => (
                selection.map_or_else(
                    || Point::new(grid.bottommost_line(), grid.last_column()),
                    |selection| selection.start.sub(grid, Boundary::None, 1),
                ),
                Direction::Left,
                Side::Left,
            ),
            SearchDirection::Next => (
                selection.map_or_else(
                    || Point::new(grid.topmost_line(), Column(0)),
                    |selection| selection.end.add(grid, Boundary::None, 1),
                ),
                Direction::Right,
                Side::Right,
            ),
        };
        let Some(found) = self
            .term
            .search_next(&mut regex, origin, direction, side, None)
        else {
            self.term.selection = None;
            return Ok(None);
        };
        let offset = grid.display_offset() as i32;
        let desired_offset = (-found.start().line.0).max(0);
        self.term
            .scroll_display(Scroll::Delta(desired_offset - offset));
        let mut selection = Selection::new(SelectionType::Simple, *found.start(), Side::Left);
        selection.update(*found.end(), Side::Right);
        self.term.selection = Some(selection);
        self.search_text = self.term.bounds_to_string(*found.start(), *found.end());
        self.search_span = self.cell_index(*found.end()) - self.cell_index(*found.start());
        self.search_id = self.search_id.wrapping_add(1);
        Ok(Some(self.search_id))
    }

    fn search_selection(&self) -> Option<SelectionRange> {
        let range = self.term.selection.as_ref()?.to_range(&self.term)?;
        (self.cell_index(range.end) - self.cell_index(range.start) == self.search_span
            && self.term.bounds_to_string(range.start, range.end) == self.search_text)
            .then_some(range)
    }

    fn discard_replaced_search(&mut self) {
        if self.term.selection.is_some() && self.search_selection().is_none() {
            self.term.selection = None;
        }
    }

    fn cell_index(&self, point: Point) -> i32 {
        point.line.0 * self.columns as i32 + point.column.0 as i32
    }

    fn search_highlight(&self, display_offset: usize) -> Option<SearchHighlight> {
        let range = self.search_selection()?;
        let offset = (display_offset * self.columns) as i32;
        let start = (self.cell_index(range.start) + offset).max(0);
        let end = (self.cell_index(range.end) + offset).min((self.columns * self.rows) as i32 - 1);
        (start <= end).then(|| SearchHighlight {
            id: self.search_id,
            start: start as u32,
            end: end as u32,
        })
    }

    pub fn follow_output(&mut self) -> bool {
        if self.term.grid().display_offset() == 0 {
            return false;
        }
        self.term.scroll_display(Scroll::Bottom);
        true
    }

    pub fn link(&self, row: usize, column: usize) -> Option<String> {
        if row >= self.rows || column >= self.columns {
            return None;
        }
        let grid = self.term.grid();
        let line = Line(row as i32 - grid.display_offset() as i32);
        let cell = &grid[line][Column(column)];
        cell.hyperlink().map(|link| link.uri().to_owned())
    }

    pub fn frame(&self) -> TerminalFrame {
        let grid = self.term.grid();
        let cursor = grid.cursor.point;
        let display_offset = grid.display_offset();
        let viewport_row = usize::try_from(cursor.line.0)
            .unwrap_or(0)
            .saturating_add(display_offset);
        let shape = self.term.cursor_style().shape;
        let cursor_style = match shape {
            CursorShape::Block | CursorShape::Hidden => 0,
            CursorShape::Underline => 1,
            CursorShape::Beam => 2,
            CursorShape::HollowBlock => 3,
        };
        let cursor_visible = viewport_row < self.rows
            && self.term.mode().contains(TermMode::SHOW_CURSOR)
            && shape != CursorShape::Hidden;
        let mut cells = Vec::with_capacity(self.columns * self.rows);

        for indexed in grid.display_iter() {
            let cell = indexed.cell;
            let mut foreground = color_to_rgb(self.term.colors(), cell.fg, self.appearance);
            let mut background = color_to_rgb(self.term.colors(), cell.bg, self.appearance);
            if cell.flags.contains(Flags::INVERSE) {
                std::mem::swap(&mut foreground, &mut background);
            }
            let hidden = cell.flags.contains(Flags::HIDDEN);
            cells.push(FrameCell {
                text: if cell
                    .flags
                    .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
                {
                    CompactString::default()
                } else if hidden {
                    CompactString::const_new(" ")
                } else {
                    let mut text = CompactString::new(cell.c.encode_utf8(&mut [0; 4]));
                    if let Some(combining) = cell.zerowidth() {
                        text.extend(combining.iter().copied());
                    }
                    text
                },
                width: if cell.flags.contains(Flags::WIDE_CHAR) {
                    2
                } else if cell
                    .flags
                    .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
                {
                    0
                } else {
                    1
                },
                foreground,
                background,
                bold: cell.flags.contains(Flags::BOLD),
                italic: cell.flags.contains(Flags::ITALIC),
                linked: !hidden && cell.hyperlink().is_some(),
                wraps: cell.flags.contains(Flags::WRAPLINE),
                underline: !hidden && cell.flags.intersects(Flags::ALL_UNDERLINES),
                strikeout: !hidden && cell.flags.contains(Flags::STRIKEOUT),
            });
        }

        let keyboard =
            &self.keyboards[usize::from(self.term.mode().contains(TermMode::ALT_SCREEN))];
        TerminalFrame {
            revision: 0,
            columns: self.columns,
            rows: self.rows,
            cursor_column: cursor.column.0.min(self.columns.saturating_sub(1)),
            cursor_row: viewport_row.min(self.rows.saturating_sub(1)),
            cursor_visible,
            cursor_style,
            display_offset,
            modes: self.term.mode().bits() | (u32::from(keyboard.flags) << 18),
            cells,
            search_highlight: self.search_highlight(display_offset),
        }
    }
}

fn color_to_rgb(colors: &Colors, color: Color, appearance: TerminalAppearance) -> u32 {
    let value = match color {
        Color::Spec(value) => value,
        Color::Indexed(index) => match colors[usize::from(index)] {
            Some(value) => value,
            None => return indexed(index, appearance),
        },
        Color::Named(named) => match colors[named] {
            Some(value) => value,
            None => return named_color(named, appearance),
        },
    };
    rgb(value.r, value.g, value.b)
}

fn rgb(r: u8, g: u8, b: u8) -> u32 {
    (u32::from(r) << 16) | (u32::from(g) << 8) | u32::from(b)
}

fn indexed(index: u8, appearance: TerminalAppearance) -> u32 {
    if let Some(color) = appearance.ansi().get(usize::from(index)) {
        return *color;
    }
    if index < 232 {
        let cube = index - 16;
        let steps = [0u8, 95, 135, 175, 215, 255];
        let r = steps[(cube / 36) as usize % 6];
        let g = steps[(cube / 6) as usize % 6];
        let b = steps[(cube % 6) as usize];
        return rgb(r, g, b);
    }
    let level = 8 + (index - 232) * 10;
    rgb(level, level, level)
}

fn named_color(color: NamedColor, appearance: TerminalAppearance) -> u32 {
    if let Some(color) = appearance.ansi().get(color as usize) {
        return *color;
    }
    match color {
        NamedColor::Background => appearance.background(),
        NamedColor::Foreground | NamedColor::BrightForeground | NamedColor::Cursor => {
            appearance.foreground()
        }
        NamedColor::DimForeground | NamedColor::DimWhite => appearance.ansi()[7],
        NamedColor::DimBlack if appearance.light => appearance.ansi()[8],
        NamedColor::DimRed if appearance.light => appearance.ansi()[1],
        NamedColor::DimGreen if appearance.light => appearance.ansi()[2],
        NamedColor::DimYellow if appearance.light => appearance.ansi()[3],
        NamedColor::DimBlue if appearance.light => appearance.ansi()[4],
        NamedColor::DimMagenta if appearance.light => appearance.ansi()[5],
        NamedColor::DimCyan if appearance.light => appearance.ansi()[6],
        NamedColor::DimBlack => 0x17171b,
        NamedColor::DimRed => 0xa85555,
        NamedColor::DimGreen => 0x628a62,
        NamedColor::DimYellow => 0xa8864f,
        NamedColor::DimBlue => 0x5570b0,
        NamedColor::DimMagenta => 0x8a6bc4,
        NamedColor::DimCyan => 0x559394,
        _ => appearance.foreground(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(frame: &TerminalFrame, row: usize) -> String {
        let start = row * frame.columns;
        frame.cells[start..start + frame.columns]
            .iter()
            .map(|cell| cell.text.as_str())
            .collect()
    }

    #[test]
    fn encodes_a_complete_binary_frame() {
        let mut screen = Screen::new(2, 1);
        screen.process(b"AA");
        let mut frame = screen.frame();
        frame.modes = 1025;
        assert_eq!(
            frame.encode(None),
            [
                1, 3, 2, 0, 1, 0, 1, 0, 0, 0, 0, 0, 0, 0, 1, 4, 0, 0, 1, 0, 0, 0, 1, 0, 2, 0, 1,
                234, 230, 230, 0, 22, 19, 19, 0, 1, 0, 0, 0, 65,
            ],
        );
    }

    #[test]
    fn sends_only_rows_changed_since_the_subscriber_frame() {
        let mut screen = Screen::new(10, 3);
        let previous = screen.frame();
        screen.process(b"\r\nX");
        let current = screen.frame();
        let delta = current.encode(Some(&previous));
        assert_eq!(delta.get(1), Some(&1));
        assert_eq!(delta.get(18..22), Some([1, 0, 1, 0].as_slice()));
        assert!(delta.len() < current.encode(None).len());

        screen.resize(12, 3);
        let resized = screen.frame().encode(Some(&current));
        assert_eq!(resized.get(1), Some(&3));
        assert_eq!(resized.get(18..20), Some([3, 0].as_slice()));
    }

    #[test]
    fn search_highlights_and_navigation_follow_scrolling_text()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut screen = Screen::new(16, 3);
        screen.process(b"hit first\r\nnoise\r\nhit second\r\nmore\r\nlive\r\nbottom");
        let second = screen
            .search("hit", None, SearchDirection::Previous)?
            .ok_or("missing second hit")?;
        let matched = screen.frame();
        assert!(row(&matched, 0).starts_with("hit second"));
        let highlight = SearchHighlight {
            id: second,
            start: 0,
            end: 2,
        };
        assert_eq!(matched.search_highlight, Some(highlight));

        screen.process(b"\r\nnew output\r\nnewer output");
        let scrolled = screen.frame();
        assert!(scrolled.display_offset > matched.display_offset);
        assert!(row(&scrolled, 0).starts_with("hit second"));
        assert_eq!(scrolled.search_highlight, Some(highlight));

        screen.scroll(-10);
        assert!(screen.frame().search_highlight.is_none());
        let first = screen
            .search("hit", Some(second), SearchDirection::Previous)?
            .ok_or("missing first hit")?;
        assert_ne!(first, second);
        assert!(row(&screen.frame(), 0).starts_with("hit first"));
        screen.search("hit", Some(first), SearchDirection::Next)?;
        assert!(row(&screen.frame(), 0).starts_with("hit second"));
        Ok(())
    }

    #[test]
    fn overwritten_search_text_loses_its_highlight() -> Result<(), Box<dyn std::error::Error>> {
        let mut screen = Screen::new(10, 3);
        screen.process(b"hit");
        assert!(
            screen
                .search("hit", None, SearchDirection::Previous)?
                .is_some()
        );
        let matched = screen.frame();
        let encoded = matched.encode(None);
        assert!(encoded.get(1).is_some_and(|flags| flags & 16 != 0));
        assert!(encoded.ends_with(&[1, 0, 0, 0, 0, 0, 0, 0, 2, 0, 0, 0]));

        screen.process(b"\x1b[Hnew");
        assert!(screen.frame().search_highlight.is_none());

        screen.process(b"\x1b[Hhit");
        let redrawn = screen.frame();
        assert!(row(&redrawn, 0).starts_with("hit"));
        assert!(redrawn.search_highlight.is_none());

        assert!(
            screen
                .search("hit", None, SearchDirection::Previous)?
                .is_some()
        );
        screen.process(b"\x1b[?2026h\x1b[Hnew");
        screen.finish_sync();
        screen.process(b"\x1b[Hhit");
        assert!(screen.frame().search_highlight.is_none());
        Ok(())
    }

    #[test]
    fn retains_the_tail_of_a_megabyte_of_output() {
        let mut screen = Screen::new(80, 24);
        let output = b"1234567890\r\n".repeat(87_382);
        for chunk in output.chunks(8192) {
            screen.process(chunk);
        }
        screen.process(b"END");
        assert!(row(&screen.frame(), 23).starts_with("END"));
    }

    #[test]
    #[ignore = "reports local parser and encoder throughput"]
    fn terminal_frame_burst() {
        use std::fmt::Write as _;
        use std::time::Instant;

        const BURST_BYTES: usize = 1024 * 1024;
        const BATCH_BYTES: usize = 32 * 8192;
        let mut output = String::with_capacity(BURST_BYTES);
        for line in 0.. {
            assert!(writeln!(output, "\x1b[32m{line:06}\x1b[0m Shepherd terminal burst\r").is_ok());
            if output.len() >= BURST_BYTES {
                break;
            }
        }
        output.truncate(BURST_BYTES);
        let mut screen = Screen::new(80, 24);
        let start = Instant::now();
        let mut previous = None;
        let mut frames = 0;
        let mut encoded_bytes = 0;
        for batch in output.as_bytes().chunks(BATCH_BYTES) {
            screen.process(batch);
            let frame = screen.frame();
            encoded_bytes += frame.encode(previous.as_ref()).len();
            previous = Some(frame);
            frames += 1;
        }
        eprintln!(
            "{BURST_BYTES} PTY bytes in {:.1?}; {frames} frames, {encoded_bytes} encoded bytes",
            start.elapsed(),
        );
        assert!(encoded_bytes < BURST_BYTES);

        const SNAPSHOTS: usize = 1_000;
        let snapshot_start = Instant::now();
        for _ in 0..SNAPSHOTS {
            std::hint::black_box(screen.frame());
        }
        eprintln!("{SNAPSHOTS} snapshots in {:.1?}", snapshot_start.elapsed());
    }

    #[test]
    fn text_lands_in_the_grid_and_moves_the_cursor() {
        let mut screen = Screen::new(10, 3);
        screen.process(b"hello\r\nworld");

        let frame = screen.frame();
        assert_eq!(row(&frame, 0), "hello     ");
        assert_eq!(row(&frame, 1), "world     ");
        assert_eq!((frame.cursor_row, frame.cursor_column), (1, 5));
    }

    #[test]
    fn cursor_position_queries_are_answered() {
        let mut screen = Screen::new(10, 3);
        screen.process(b"abc\x1b[6n");
        assert_eq!(screen.take_replies(), b"\x1b[1;4R");
        assert!(screen.take_replies().is_empty());
    }

    #[test]
    fn a_synchronized_update_can_flush_without_an_end_marker() {
        let mut screen = Screen::new(10, 3);
        screen.process(b"\x1b[?2026hhello");
        assert!(screen.sync_deadline().is_some());
        assert_eq!(row(&screen.frame(), 0), "          ");
        screen.finish_sync();
        assert!(screen.sync_deadline().is_none());
        assert!(row(&screen.frame(), 0).starts_with("hello"));
    }

    #[test]
    fn negotiates_kitty_modes_in_pty_reply_order() {
        let mut screen = Screen::new(10, 3);
        screen.process(b"\x1b[>1");
        screen.process(b"u\x1b[?u\x1b[c");
        assert!(screen.frame().modes & (1 << 18) != 0);
        assert_eq!(screen.take_replies(), b"\x1b[?1u\x1b[?6c");

        screen.process(b"\x1b[?1049h\x1b[?u\x1b[>31u\x1b[?u\x1b[?1049l\x1b[?u");
        assert_eq!(screen.take_replies(), b"\x1b[?0u\x1b[?31u\x1b[?1u");

        screen.process(b"\x1b[=2;2u\x1b[?u\x1b[=1;3u\x1b[?u\x1b[<u\x1b[?u");
        assert_eq!(screen.take_replies(), b"\x1b[?3u\x1b[?2u\x1b[?0u");
    }

    #[test]
    fn synchronized_screen_switches_keep_kitty_modes_separate() {
        let mut screen = Screen::new(10, 3);
        screen.process(b"\x1b[>1umain\x1b[?2026h\x1b[?25;1049");
        screen.process(b"h\x1b[Halt\x1b[>31u\x1b[?u");
        let buffered = screen.frame();
        assert!(row(&buffered, 0).starts_with("main"));
        assert_eq!(buffered.modes >> 18, 1);
        assert_eq!(screen.take_replies(), b"\x1b[?31u");

        screen.process(b"\x1b[?2026l");
        let alternate = screen.frame();
        assert!(row(&alternate, 0).starts_with("alt"));
        assert_eq!(alternate.modes >> 18, 31);

        screen.process(b"\x1b[?2026h\x1b[?1049l\x1b[?u");
        assert_eq!(screen.take_replies(), b"\x1b[?1u");
        screen.finish_sync();
        let main = screen.frame();
        assert!(row(&main, 0).starts_with("main"));
        assert_eq!(main.modes >> 18, 1);
    }

    #[test]
    fn bounds_the_kitty_mode_stack() {
        let mut screen = Screen::new(10, 3);
        screen.process(&b"\x1b[>1u".repeat(4100));
        assert_eq!(screen.keyboards[0].stack.len(), KITTY_STACK_LIMIT);
        screen.process(b"\x1b[<65535u\x1b[?u");
        assert_eq!(screen.take_replies(), b"\x1b[?0u");
    }

    #[test]
    fn reports_a_bell_once_per_batch() {
        let mut screen = Screen::new(10, 3);
        assert!(!screen.take_bell());
        screen.process(b"\x07\x07");
        assert!(screen.take_bell());
        assert!(!screen.take_bell());
    }

    #[test]
    fn cursor_style_survives_binary_encoding() {
        let mut screen = Screen::new(10, 3);
        screen.process(b"\x1b[6 q");
        assert_eq!(screen.frame().encode(None).get(1), Some(&11));
        screen.process(b"\x1b[4 q");
        assert_eq!(screen.frame().encode(None).get(1), Some(&7));
    }

    #[test]
    fn osc_color_queries_observe_the_palette_at_the_query() {
        let mut screen = Screen::new(10, 3);
        screen.process(b"\x1b]10;#123456\x07\x1b]10;?\x07\x1b]10;#abcdef\x07");
        let replies = screen.take_replies();
        assert!(String::from_utf8_lossy(&replies).contains("rgb:1212/3434/5656"));
        assert!(screen.take_replies().is_empty());
    }

    #[test]
    fn switching_appearance_recolors_the_viewport_and_keeps_explicit_colors() {
        let mut screen = Screen::new(10, 3);
        screen.process(b"D\x1b[31mR\x1b[38;2;18;52;86mT\x1b[0m");
        assert_eq!(
            screen.frame().cells.first().map(|cell| cell.background),
            Some(0x131316)
        );
        let appearance = TerminalAppearance {
            light: true,
            more_contrast: false,
        };
        assert!(screen.set_appearance(appearance));
        assert!(!screen.set_appearance(appearance));
        let frame = screen.frame();
        assert_eq!(
            frame
                .cells
                .first()
                .map(|cell| (cell.foreground, cell.background)),
            Some((0x242633, 0xffffff))
        );
        assert_eq!(
            frame.cells.get(1).map(|cell| cell.foreground),
            Some(0x963539)
        );
        assert_eq!(
            frame.cells.get(2).map(|cell| cell.foreground),
            Some(0x123456)
        );
        screen.process(b"\x1b]11;?\x07\x1b]4;1;#2a4c6e\x07");
        assert!(String::from_utf8_lossy(&screen.take_replies()).contains("rgb:ffff/ffff/ffff"));
        screen.set_appearance(TerminalAppearance {
            light: false,
            more_contrast: true,
        });
        let frame = screen.frame();
        assert_eq!(
            frame.cells.first().map(|cell| cell.background),
            Some(0x08080a)
        );
        assert_eq!(
            frame.cells.get(1).map(|cell| cell.foreground),
            Some(0x2a4c6e)
        );
    }

    #[test]
    fn osc_palette_overrides_change_existing_cells() {
        let mut screen = Screen::new(10, 3);
        screen.process(b"\x1b[31mR\x1b[0mD\x1b]4;1;#123456\x07\x1b]10;#abcdef\x07");
        let frame = screen.frame();
        assert_eq!(
            frame.cells.first().map(|cell| cell.foreground),
            Some(0x123456)
        );
        assert_eq!(
            frame.cells.get(1).map(|cell| cell.foreground),
            Some(0xabcdef)
        );
    }

    #[test]
    fn concealed_text_does_not_leave_the_screen() {
        let mut screen = Screen::new(16, 2);
        screen.process(b"\x1b[8msecret\x1b[0mvisible");
        assert!(row(&screen.frame(), 0).starts_with("      visible"));
    }

    #[test]
    fn sgr_attributes_reach_the_frame() {
        let mut screen = Screen::new(20, 2);
        screen.process(b"\x1b[1;31mred");

        let frame = screen.frame();
        assert_eq!(frame.cells[0].text, "r");
        assert!(frame.cells[0].bold);
        assert_eq!(frame.cells[0].foreground, 0xe06a6a);
        assert!(!frame.cells[0].italic);
    }

    #[test]
    fn custom_scrollback_bounds_the_retained_history() {
        let mut screen = Screen::with_scrollback(12, 3, 2);
        for _ in 0..10 {
            screen.process(b"line\r\n");
        }
        screen.scroll(100);
        assert_eq!(screen.term.grid().display_offset(), 2);
    }

    #[test]
    fn scrollback_and_alternate_screen_keep_separate_contents() {
        let mut screen = Screen::new(12, 3);
        screen.process(b"first\r\nsecond\r\nthird\r\nfourth");
        screen.scroll(1);
        let history = screen.frame();
        assert_eq!(history.display_offset, 1);
        assert!(!history.cursor_visible);
        assert!(row(&history, 0).starts_with("first"));

        screen.follow_output();
        screen.process(b"\x1b[?1049h\x1b[Halternate");
        assert!(row(&screen.frame(), 0).starts_with("alternate"));
        screen.process(b"\x1b[?1049l");
        assert!(row(&screen.frame(), 2).starts_with("fourth"));
    }

    #[test]
    fn wide_and_combining_glyphs_keep_their_cell_widths() {
        let mut screen = Screen::new(12, 3);
        screen.process("界é".as_bytes());
        let frame = screen.frame();
        let [wide, spacer, combining, ..] = frame.cells.as_slice() else {
            panic!("expected a complete grid");
        };
        assert_eq!((&*wide.text, wide.width), ("界", 2));
        assert_eq!((&*spacer.text, spacer.width), ("", 0));
        assert_eq!((&*combining.text, combining.width), ("é", 1));
    }

    #[test]
    fn bounds_combining_marks_in_grid_cells_and_frames() {
        let mut screen = Screen::new(10, 3);
        screen.process(b"e");
        let marks = "\u{301}".repeat(4096);
        for _ in 0..4 {
            screen.process(marks.as_bytes());
        }
        screen.process("界".as_bytes());

        let cell = &screen.term.grid()[Line(0)][Column(0)];
        assert_eq!(cell.zerowidth().map(<[char]>::len), Some(9));
        let frame = screen.frame();
        let expected = format!("e{}", "\u{301}".repeat(9));
        assert_eq!(
            frame.cells.first().map(|cell| cell.text.as_str()),
            Some(expected.as_str())
        );
        assert_eq!(frame.cells.get(1).map(|cell| cell.width), Some(2));
    }

    #[test]
    fn resizing_reflows_a_wrapped_line_with_a_wide_glyph() {
        let mut screen = Screen::new(12, 4);
        screen.process("abc界defghij".as_bytes());
        screen.resize(6, 4);
        screen.scroll(1);
        let narrow = screen.frame();
        assert_eq!(row(&narrow, 0), "abc界d");
        assert!(row(&narrow, 1).starts_with("efghij"));
        assert_eq!(narrow.cells.get(3).map(|cell| cell.width), Some(2));
        assert_eq!(narrow.cells.get(4).map(|cell| cell.width), Some(0));
        assert!(narrow.cells.get(5).is_some_and(|cell| cell.wraps));

        screen.follow_output();
        screen.resize(12, 4);
        screen.scroll(1);
        let wide = screen.frame();
        assert!((0..wide.rows).any(|index| row(&wide, index) == "abc界defghij"));
    }

    #[test]
    fn resizing_rebuilds_the_frame_at_the_new_size() {
        let mut screen = Screen::new(4, 2);
        screen.process(b"abcdef");

        screen.resize(8, 3);
        let frame = screen.frame();
        assert_eq!((frame.columns, frame.rows), (8, 3));
        assert_eq!(frame.cells.len(), 24);
        assert!(frame.cursor_column < 8);
        assert!(frame.cursor_row < 3);
    }
}
