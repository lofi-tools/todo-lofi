//! VT parsing and the frame the canvas paints.
//!
//! `libghostty-vt` needs a Zig toolchain that this machine does not have, so the
//! documented fallback engine is used: `alacritty_terminal` behind the core's
//! terminal boundary. Frames are the whole visible grid; dirty-row runs are a
//! later optimisation, not a correctness requirement.

use alacritty_terminal::event::VoidListener;
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::{Config, Term, TermMode};
use alacritty_terminal::vte::ansi::{Color, NamedColor, Processor, Rgb};
use serde::Serialize;

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalFrame {
    pub columns: usize,
    pub rows: usize,
    pub cursor_column: usize,
    pub cursor_row: usize,
    pub cursor_visible: bool,
    pub display_offset: usize,
    pub modes: u32,
    pub cells: Vec<FrameCell>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FrameCell {
    pub text: String,
    pub foreground: u32,
    pub background: u32,
    pub bold: bool,
    pub italic: bool,
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

pub struct Screen {
    term: Term<VoidListener>,
    parser: Processor,
    columns: usize,
    rows: usize,
}

impl Screen {
    pub fn new(columns: usize, rows: usize) -> Self {
        let size = ScreenSize { columns, rows };
        Self {
            term: Term::new(Config::default(), &size, VoidListener),
            parser: Processor::new(),
            columns,
            rows,
        }
    }

    pub fn process(&mut self, bytes: &[u8]) {
        self.parser.advance(&mut self.term, bytes);
    }

    pub fn resize(&mut self, columns: usize, rows: usize) {
        self.columns = columns;
        self.rows = rows;
        self.term.resize(ScreenSize { columns, rows });
    }

    pub fn scroll(&mut self, lines: i32) {
        self.term
            .scroll_display(Scroll::Delta(lines.clamp(-120, 120)));
    }

    pub fn follow_output(&mut self) -> bool {
        if self.term.grid().display_offset() == 0 {
            return false;
        }
        self.term.scroll_display(Scroll::Bottom);
        true
    }

    pub fn frame(&self) -> TerminalFrame {
        let grid = self.term.grid();
        let cursor = grid.cursor.point;
        let display_offset = grid.display_offset();
        let viewport_row = usize::try_from(cursor.line.0)
            .unwrap_or(0)
            .saturating_add(display_offset);
        let cursor_visible =
            viewport_row < self.rows && self.term.mode().contains(TermMode::SHOW_CURSOR);
        let mut cells = Vec::with_capacity(self.columns * self.rows);

        for indexed in grid.display_iter() {
            let cell = indexed.cell;
            let mut foreground = color_to_rgb(cell.fg);
            let mut background = color_to_rgb(cell.bg);
            if cell.flags.contains(Flags::INVERSE) {
                std::mem::swap(&mut foreground, &mut background);
            }
            cells.push(FrameCell {
                text: cell.c.to_string(),
                foreground,
                background,
                bold: cell.flags.contains(Flags::BOLD),
                italic: cell.flags.contains(Flags::ITALIC),
            });
        }

        TerminalFrame {
            columns: self.columns,
            rows: self.rows,
            cursor_column: cursor.column.0.min(self.columns.saturating_sub(1)),
            cursor_row: viewport_row.min(self.rows.saturating_sub(1)),
            cursor_visible,
            display_offset,
            modes: self.term.mode().bits(),
            cells,
        }
    }
}

fn color_to_rgb(color: Color) -> u32 {
    match color {
        Color::Spec(Rgb { r, g, b }) => rgb(r, g, b),
        Color::Indexed(index) => indexed(index),
        Color::Named(named) => named_color(named),
    }
}

fn rgb(r: u8, g: u8, b: u8) -> u32 {
    (u32::from(r) << 16) | (u32::from(g) << 8) | u32::from(b)
}

fn indexed(index: u8) -> u32 {
    const BASE: [u32; 16] = [
        0x1d1d22, 0xe06a6a, 0x7fb37f, 0xd9b06a, 0x6f8fdc, 0xb48cff, 0x6fbfc0, 0xd6d6de, 0x5f5f6b,
        0xf08a8a, 0x9fd39f, 0xf0cc8a, 0x8faff0, 0xccb3ff, 0x8fd6d6, 0xffffff,
    ];
    if index < 16 {
        return BASE[index as usize];
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

fn named_color(color: NamedColor) -> u32 {
    match color {
        NamedColor::Foreground | NamedColor::BrightWhite => 0xe6e6ea,
        NamedColor::Background => 0x131316,
        NamedColor::Cursor => 0xe6e6ea,
        NamedColor::Black => 0x1d1d22,
        NamedColor::Red => 0xe06a6a,
        NamedColor::Green => 0x7fb37f,
        NamedColor::Yellow => 0xd9b06a,
        NamedColor::Blue => 0x6f8fdc,
        NamedColor::Magenta => 0xb48cff,
        NamedColor::Cyan => 0x6fbfc0,
        NamedColor::White => 0xd6d6de,
        NamedColor::BrightBlack => 0x5f5f6b,
        NamedColor::BrightRed => 0xf08a8a,
        NamedColor::BrightGreen => 0x9fd39f,
        NamedColor::BrightYellow => 0xf0cc8a,
        NamedColor::BrightBlue => 0x8faff0,
        NamedColor::BrightMagenta => 0xccb3ff,
        NamedColor::BrightCyan => 0x8fd6d6,
        NamedColor::DimBlack => 0x17171b,
        NamedColor::DimRed => 0xa85555,
        NamedColor::DimGreen => 0x628a62,
        NamedColor::DimYellow => 0xa8864f,
        NamedColor::DimBlue => 0x5570b0,
        NamedColor::DimMagenta => 0x8a6bc4,
        NamedColor::DimCyan => 0x559394,
        NamedColor::DimWhite => 0xa5a5ad,
        NamedColor::BrightForeground => 0xffffff,
        NamedColor::DimForeground => 0xa5a5ad,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(frame: &TerminalFrame, row: usize) -> String {
        let start = row * frame.columns;
        frame.cells[start..start + frame.columns]
            .iter()
            .map(|cell| cell.text.clone())
            .collect()
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
    fn scrollback_and_alternate_screen_keep_separate_contents() {
        let mut screen = Screen::new(12, 3);
        screen.process(b"first\r\nsecond\r\nthird\r\nfourth");
        screen.scroll(1);
        let history = screen.frame();
        assert_eq!(history.display_offset, 1);
        assert!(!history.cursor_visible);
        assert!(row(&history, 0).starts_with("first"));

        screen.follow_output();
        screen.process(b"\x1b[?1049halternate");
        assert!(row(&screen.frame(), 0).starts_with("alternate"));
        screen.process(b"\x1b[?1049l");
        assert!(row(&screen.frame(), 2).starts_with("fourth"));
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
