use std::fmt::format;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use std::io::{self, Read, Seek, Write};
use std::path::Path;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::thread;
use ratatui::macros::row;
use ratatui::text::Text;
use vt100::Screen;

const SCROLLBACK_LINES: usize = 5000;

#[derive(Default)]
pub struct Replies(Vec<u8>);

impl vt100::Callbacks for Replies {
    fn unhandled_csi(&mut self, screen: &mut vt100::Screen, i1: Option<u8>, _i2: Option<u8>, params: &[&[u16]], c: char) {
        let first = params.first().and_then(|p| p.first()).copied().unwrap_or(0);
        match (i1, c, first) {
            (None, 'n', 6) => {
                let (row, col) = screen.cursor_position();
                self.0.extend(format!("\x1b[{};{}R", row + 1, col + 1).into_bytes());
            }
            (None, 'n', 5) => self.0.extend(b"\x1b[0n"),
            (None, 'c', 0) => self.0.extend(b"x1b[?62;22c"),
            (Some(b'>'), 'c', 0) => self.0.extend(b"\x1b[>0;0;0c"),
            _ => {}
        }
    }
}

pub struct Terminal {
    pub parser: Arc<Mutex<vt100::Parser<Replies>>>,
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    master: Box<dyn MasterPty + Send>,
    child: Box<dyn Child + Send + Sync>,
    size: (u16, u16),
}

impl Terminal {
    pub fn spawn(cwd: &Path, rows: u16, cols: u16, notify: Sender<()>) -> io::Result<Self> {
        let (rows, cols) = (rows.max(1), cols.max(1));
        let pair = native_pty_system()
            .openpty(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 })
            .map_err(io::Error::other)?;

        let shell = std::env::var_os("SHELL").unwrap_or_else(|| "/bin/sh".into());
        let mut cmd = CommandBuilder::new(shell);
        cmd.cwd(cwd);
        cmd.env("TERM", "xterm-256color");
        let child = pair.slave.spawn_command(cmd).map_err(io::Error::other)?;
        drop(pair.slave);

        let mut reader = pair.master.try_clone_reader().map_err(io::Error::other)?;
        let writer: Arc<Mutex<Box<dyn Write + Send>>> =
            Arc::new(Mutex::new(pair.master.take_writer().map_err(io::Error::other)?));
        let parser = Arc::new(Mutex::new(vt100::Parser::new_with_callbacks(rows, cols, SCROLLBACK_LINES, Replies::default())));

        let (thread_parser, thread_writer) = (parser.clone(), writer.clone());
        thread::spawn(move || {
            let mut buf = [0u8; 8192];
            loop {
                let n = match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => n,
                };
                let replies = {
                    let Ok(mut p) = thread_parser.lock() else { break };
                    p.process(&buf[..n]);
                    std::mem::take(&mut p.callbacks_mut().0)
                };
                if !replies.is_empty() && let Ok(mut w) = thread_writer.lock()
                {
                    let _ = w.write_all(&replies).and_then(|_| w.flush());
                }
                if notify.send(()).is_err() { break; }
            }
            let _ = notify.send(());
        });

        Ok(Self { parser, writer, master: pair.master, child, size: (rows, cols) })
    }

    pub fn write(&self, bytes: &[u8]) {
        if let Ok(mut w) = self.writer.lock() {
            let _ = w.write_all(bytes).and_then(|_| w.flush());
        }
    }

    pub fn paste(&self, text: &str) {
        let bracketed = self.parser.lock().is_ok_and(|p| p.screen().bracketed_paste());
        if bracketed {
            self.write(format!("\x1b[200~{}\x1b[201~", text).as_bytes());
        } else {
            self.write(text.as_bytes());
        }
    }

    pub fn resize(&mut self, rows: u16, cols: u16) {
        let (rows, cols) = (rows.max(1), cols.max(1));
        if self.size == (rows, cols) { return; }
        self.size = (rows, cols);
        let _ = self.master.resize(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 });
        if let Ok(mut p) = self.parser.lock() {
            p.screen_mut().set_size(rows, cols);
        }
    }

    pub fn scroll(&self, delta: isize) {
        if let Ok(mut p) = self.parser.lock() {
            let cur = p.screen().scrollback() as isize;
            p.screen_mut().set_scrollback((cur + delta).max(0) as usize);
        }
    }

    pub fn has_exited(&mut self) -> bool {
        !matches!(self.child.try_wait(), Ok(None))
    }

    pub fn send_key(&self, key: KeyEvent) {
        let app_cursor = self.parser.lock().is_ok_and(|p| p.screen().application_cursor());
        if let Some(bytes) = key_to_bytes(key, app_cursor) {
            if let Ok(mut p) = self.parser.lock() {
                p.screen_mut().set_scrollback(0);
            }
            self.write(&bytes);
        }
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

pub fn key_to_bytes(key: KeyEvent, app_cursor: bool) -> Option<Vec<u8>> {
    let m = key.modifiers;
    let ctrl = m.contains(KeyModifiers::CONTROL);
    let alt = m.contains(KeyModifiers::ALT);
    let modp = 1 + m.contains(KeyModifiers::SHIFT) as u8 + 2 * alt as u8 + 4 * ctrl as u8;

    let csi_final = |c: char| -> Vec<u8> {
        if modp > 1 { format!("\x1b[1;{}{}", modp, c).into_bytes() }
        else if app_cursor && "ABCD".contains(c) { format!("\x1bO{}", c).into_bytes() }
        else { format!("\x1b[{}", c).into_bytes() }
    };
    let tilde = |n: u8| -> Vec<u8> {
        if modp > 1 { format!("\x1b[{};{}~", n, modp).into_bytes() } else { format!("\x1b[{}~", n).into_bytes() }
    };

    let bytes = match key.code {
        KeyCode::Char(c) if ctrl => {
            let b = match c.to_ascii_lowercase() {
                c @ 'a'..='z' => c as u8 - b'a' + 1,
                ' ' | '@' | '2' => 0,
                '[' | '3' => 27,
                '\\' | '4' => 28,
                ']' | '5' => 29,
                '^' | '6' => 30,
                '_' | '-' | '7' => 31,
                _ => return None,
            };
            if alt { vec![27, b] } else { vec![b] }
        }
        KeyCode::Char(c) => {
            let mut v = if alt { vec![27] } else { Vec::new() };
            v.extend(c.to_string().into_bytes());
            v
        }
        KeyCode::Enter => if alt { b"\x1b\r".to_vec() } else { b"\r".to_vec() },
        KeyCode::Backspace => if alt { vec![27, 0x7f] } else if ctrl { vec![0x08] } else { vec![0x7f] },
        KeyCode::Tab => b"\t".to_vec(),
        KeyCode::BackTab => b"\x1b[Z".to_vec(),
        KeyCode::Esc => vec![27],
        KeyCode::Up => csi_final('A'),
        KeyCode::Down => csi_final('B'),
        KeyCode::Right => csi_final('C'),
        KeyCode::Left => csi_final('D'),
        KeyCode::Home => csi_final('H'),
        KeyCode::End => csi_final('F'),
        KeyCode::Insert => tilde(2),
        KeyCode::Delete => tilde(3),
        KeyCode::PageUp => tilde(5),
        KeyCode::PageDown => tilde(6),
        KeyCode::F(n @ 1..=4) => {
            let c = (b'P' + n - 1) as char;
            if modp > 1 { format!("\x1b[1;{}{}", modp, c).into_bytes() } else { format!("\x1bO{}", c).into_bytes() }
        }
        KeyCode::F(n @ 5..=12) => tilde([15, 17, 18, 19, 20, 21, 23, 24][(n - 5) as usize]),
        _ => return None,
    };

    Some(bytes)
}

pub fn render(term: &Terminal, buf: &mut ratatui::buffer::Buffer, area: ratatui::layout::Rect) -> Option<(u16, u16)> {
    use ratatui::style::{Color, Modifier, Style};
    let conv = |c: vt100::Color| match c {
        vt100::Color::Default => Color::Reset,
        vt100::Color::Idx(i) => Color::Indexed(i),
        vt100::Color::Rgb(r, g, b) => Color::Rgb(r, g, b),
    };

    let parser = term.parser.lock().ok()?;
    let screen = parser.screen();
    for row in 0..area.height {
        for col in 0..area.width {
            let Some(cell) = screen.cell(row, col) else { continue };
            if cell.is_wide_continuation() { continue; }
            let (mut fg, mut bg) = (conv(cell.fgcolor()), conv(cell.bgcolor()));
            if cell.inverse() {
                std::mem::swap(&mut fg, &mut bg);
                // Invertir el color por defecto no tiene efecto; se fuerza contraste.
                if fg == Color::Reset { fg = Color::Black; }
                if bg == Color::Reset { bg = Color::White; }
            }
            let mut style = Style::default().fg(fg).bg(bg);
            if cell.bold() { style = style.add_modifier(Modifier::BOLD); }
            if cell.dim() { style = style.add_modifier(Modifier::DIM); }
            if cell.italic() { style = style.add_modifier(Modifier::ITALIC); }
            if cell.underline() { style = style.add_modifier(Modifier::UNDERLINED); }
            let symbol = if cell.has_contents() { cell.contents() } else { " " };
            buf[(area.x + col, area.y + row)].set_symbol(symbol).set_style(style);
        }
    }

    // En el historial el cursor no corresponde a la vista.
    if screen.hide_cursor() || screen.scrollback() > 0 { return None; }
    let (row, col) = screen.cursor_position();
    (row < area.height && col < area.width).then_some((area.x + col, area.y + row))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    fn k(code: KeyCode, m: KeyModifiers) -> Vec<u8> { key_to_bytes(KeyEvent::new(code, m), false).unwrap() }

    #[test]
    fn keys_encode_as_xterm() {
        assert_eq!(k(KeyCode::Char('c'), KeyModifiers::CONTROL), vec![3]);
        assert_eq!(k(KeyCode::Char('b'), KeyModifiers::ALT), b"\x1bb");
        assert_eq!(k(KeyCode::Left, KeyModifiers::CONTROL), b"\x1b[1;5D");
        assert_eq!(k(KeyCode::Up, KeyModifiers::empty()), b"\x1b[A");
        assert_eq!(key_to_bytes(KeyEvent::new(KeyCode::Up, KeyModifiers::empty()), true).unwrap(), b"\x1bOA");
        assert_eq!(k(KeyCode::Delete, KeyModifiers::empty()), b"\x1b[3~");
        assert_eq!(k(KeyCode::F(5), KeyModifiers::empty()), b"\x1b[15~");
        assert_eq!(k(KeyCode::Char('ñ'), KeyModifiers::empty()), "ñ".as_bytes());
    }

    #[test]
    fn answers_cursor_position_query() {
        let mut p = vt100::Parser::new_with_callbacks(5, 10, 0, Replies::default());
        p.process(b"ab\x1b[6n");
        assert_eq!(p.callbacks().0, b"\x1b[1;3R");
    }

    #[test]
    fn runs_shell_and_exits() {
        let (tx, rx) = mpsc::channel();
        let mut t = Terminal::spawn(Path::new("/"), 10, 40, tx).unwrap();
        t.write(b"echo wyv$((40+2)); exit\r");
        // El canal se cierra cuando el lector ve EOF: toda la salida ya está procesada.
        let deadline = Instant::now() + Duration::from_secs(10);
        while rx.recv_timeout(Duration::from_secs(10)).is_ok() && Instant::now() < deadline {}
        assert!(t.has_exited());
        assert!(t.parser.lock().unwrap().screen().contents().contains("wyv42"));
    }
}