//! Terminal backend that adds OSC 8 hyperlinks on top of crossterm drawing.
//!
//! ratatui/crossterm carry no hyperlink channel, so link URLs travel through
//! the frame buffer as underline-color ids (see [`crate::email_body`]) and
//! this backend translates them back into `OSC 8 ; url ST` sequences around
//! the affected cells. Terminals without hyperlink support ignore the
//! sequences; the visible styling is unchanged.

use std::io::{self, Write};
use std::sync::{Arc, Mutex};

use ratatui::{
    backend::{Backend, WindowSize},
    buffer::Cell,
    crossterm::{
        cursor::{Hide, MoveTo, Show},
        execute, queue,
        style::{
            Attribute as CAttribute, Color as CColor, Print, SetAttribute, SetBackgroundColor,
            SetForegroundColor,
        },
        terminal::{self, Clear, ClearType},
    },
    layout::{Position, Size},
    style::{Color, Modifier},
};

/// Per-frame URL table shared between the renderer (publisher) and the
/// backend (consumer). Id N maps to entry N-1.
#[derive(Default)]
pub struct HyperlinkRegistry {
    links: Mutex<Vec<String>>,
}

impl HyperlinkRegistry {
    pub fn publish(&self, links: Vec<String>) {
        *self.links.lock().unwrap() = links;
    }

    fn url(&self, id: u8) -> Option<String> {
        self.links
            .lock()
            .unwrap()
            .get(usize::from(id).saturating_sub(1))
            .cloned()
    }
}

pub struct HyperlinkBackend<W: Write> {
    writer: W,
    registry: Arc<HyperlinkRegistry>,
    open_link: Option<String>,
    cursor: (u16, u16),
}

impl<W: Write> HyperlinkBackend<W> {
    pub fn new(writer: W, registry: Arc<HyperlinkRegistry>) -> Self {
        Self {
            writer,
            registry,
            open_link: None,
            cursor: (0, 0),
        }
    }

    fn set_link(&mut self, url: Option<String>) -> io::Result<()> {
        if url == self.open_link {
            return Ok(());
        }
        match &url {
            Some(url) => write!(self.writer, "\x1b]8;;{url}\x1b\\")?,
            None => write!(self.writer, "\x1b]8;;\x1b\\")?,
        }
        self.open_link = url;
        Ok(())
    }

    fn cell_link(&self, cell: &Cell) -> Option<String> {
        match cell.underline_color {
            Color::Rgb(0, 0, id) if id >= 1 => self.registry.url(id),
            _ => None,
        }
    }
}

impl<W: Write> Write for HyperlinkBackend<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.writer.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.writer.flush()
    }
}

impl<W: Write> Backend for HyperlinkBackend<W> {
    fn draw<'a, I>(&mut self, content: I) -> io::Result<()>
    where
        I: Iterator<Item = (u16, u16, &'a Cell)>,
    {
        for (x, y, cell) in content {
            self.set_link(self.cell_link(cell))?;
            queue!(self.writer, MoveTo(x, y))?;
            queue!(self.writer, SetAttribute(CAttribute::Reset))?;
            queue!(self.writer, SetForegroundColor(to_crossterm(cell.fg)))?;
            queue!(self.writer, SetBackgroundColor(to_crossterm(cell.bg)))?;
            if cell.modifier.contains(Modifier::BOLD) {
                queue!(self.writer, SetAttribute(CAttribute::Bold))?;
            }
            if cell.modifier.contains(Modifier::DIM) {
                queue!(self.writer, SetAttribute(CAttribute::Dim))?;
            }
            if cell.modifier.contains(Modifier::ITALIC) {
                queue!(self.writer, SetAttribute(CAttribute::Italic))?;
            }
            if cell.modifier.contains(Modifier::UNDERLINED) {
                queue!(self.writer, SetAttribute(CAttribute::Underlined))?;
            }
            if cell.modifier.contains(Modifier::SLOW_BLINK) {
                queue!(self.writer, SetAttribute(CAttribute::SlowBlink))?;
            }
            if cell.modifier.contains(Modifier::RAPID_BLINK) {
                queue!(self.writer, SetAttribute(CAttribute::RapidBlink))?;
            }
            if cell.modifier.contains(Modifier::REVERSED) {
                queue!(self.writer, SetAttribute(CAttribute::Reverse))?;
            }
            if cell.modifier.contains(Modifier::HIDDEN) {
                queue!(self.writer, SetAttribute(CAttribute::Hidden))?;
            }
            if cell.modifier.contains(Modifier::CROSSED_OUT) {
                queue!(self.writer, SetAttribute(CAttribute::CrossedOut))?;
            }
            queue!(self.writer, Print(cell.symbol()))?;
        }
        self.set_link(None)
    }

    fn hide_cursor(&mut self) -> io::Result<()> {
        execute!(self.writer, Hide).map(|_| ())
    }

    fn show_cursor(&mut self) -> io::Result<()> {
        execute!(self.writer, Show).map(|_| ())
    }

    fn get_cursor_position(&mut self) -> io::Result<Position> {
        Ok(self.cursor.into())
    }

    fn set_cursor_position<P: Into<Position>>(&mut self, position: P) -> io::Result<()> {
        let Position { x, y } = position.into();
        self.cursor = (x, y);
        execute!(self.writer, MoveTo(x, y)).map(|_| ())
    }

    fn clear(&mut self) -> io::Result<()> {
        self.set_link(None)?;
        execute!(self.writer, Clear(ClearType::All)).map(|_| ())
    }

    fn size(&self) -> io::Result<Size> {
        let (columns, rows) = terminal::size()?;
        Ok(Size::new(columns, rows))
    }

    fn window_size(&mut self) -> io::Result<WindowSize> {
        let size = terminal::window_size()?;
        Ok(WindowSize {
            columns_rows: Size::new(size.columns, size.rows),
            pixels: Size::new(size.width, size.height),
        })
    }

    fn flush(&mut self) -> io::Result<()> {
        self.writer.flush()
    }
}

fn to_crossterm(color: Color) -> CColor {
    match color {
        Color::Reset => CColor::Reset,
        Color::Black => CColor::Black,
        Color::Red => CColor::DarkRed,
        Color::Green => CColor::DarkGreen,
        Color::Yellow => CColor::DarkYellow,
        Color::Blue => CColor::DarkBlue,
        Color::Magenta => CColor::DarkMagenta,
        Color::Cyan => CColor::DarkCyan,
        Color::Gray => CColor::Grey,
        Color::DarkGray => CColor::DarkGrey,
        Color::LightRed => CColor::Red,
        Color::LightGreen => CColor::Green,
        Color::LightYellow => CColor::Yellow,
        Color::LightBlue => CColor::Blue,
        Color::LightMagenta => CColor::Magenta,
        Color::LightCyan => CColor::Cyan,
        Color::White => CColor::White,
        Color::Rgb(r, g, b) => CColor::Rgb { r, g, b },
        Color::Indexed(i) => CColor::AnsiValue(i),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::Style;

    #[test]
    fn wraps_link_cells_in_osc8_sequences() {
        let registry = Arc::new(HyperlinkRegistry::default());
        registry.publish(vec!["https://example.com".to_string()]);
        let mut backend = HyperlinkBackend::new(Vec::new(), registry);

        let mut link_cell = Cell::default();
        link_cell.set_symbol("x");
        link_cell.set_style(Style::default().underline_color(Color::Rgb(0, 0, 1)));
        let mut plain_cell = Cell::default();
        plain_cell.set_symbol("y");

        backend
            .draw(vec![(0, 0, &link_cell), (1, 0, &plain_cell)].into_iter())
            .unwrap();

        let out = String::from_utf8_lossy(&backend.writer);
        let open = out.find("\x1b]8;;https://example.com\x1b\\").unwrap();
        let close = out.rfind("\x1b]8;;\x1b\\").unwrap();
        assert!(open < close);
    }
}
