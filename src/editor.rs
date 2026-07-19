use ropey::Rope;
use std::fs::{self, File};
use std::io::{self, BufReader, BufWriter};
use std::path::Path;

pub struct EditorBuffer {
    pub text: Rope,
    pub cursor_char_idx: usize,
    pub scroll_x: usize,
    pub scroll_y: usize,
}

impl EditorBuffer {
    pub fn new() -> Self {
        Self { 
            text: Rope::new(),
            cursor_char_idx: 0,
            scroll_x: 0,
            scroll_y: 0,
        }
    }

    pub fn load_from_file<P: AsRef<Path>>(path: P) -> io::Result<Self> {
        let bytes = fs::read(path)?;
        let text_str = String::from_utf8_lossy(&bytes);
        Ok(Self { 
            text: Rope::from_str(&text_str),
            cursor_char_idx: 0,
            scroll_x: 0,
            scroll_y: 0
        })
    }

    pub fn save_to_file<P: AsRef<Path>>(&self, path: P) -> io::Result<()> {
        let file = File::create(path)?;
        self.text.write_to(BufWriter::new(file))?;
        Ok(())
    }

    pub fn insert_char(&mut self, ch: char) {
        self.text.insert_char(self.cursor_char_idx, ch);
        self.cursor_char_idx += 1;
    }

    pub fn delete_backwards(&mut self) {
        if self.cursor_char_idx == 0 { return; }

        let current_line_idx = self.text.char_to_line(self.cursor_char_idx);
        let line_start_char_idx = self.text.line_to_char(current_line_idx);
        let col = self.cursor_char_idx - line_start_char_idx;

        if col > 0 {
            // Backspace inteligente: retrocede hasta el múltiplo de 4 más cercano,
            // emulando el borrado de un tabstop en vez de un solo espacio.
            let rem = col % 4;
            let step = if rem == 0 { 4 } else { rem };

            if col >= step {
                let start_idx = self.cursor_char_idx - step;
                let mut all_spaces = true;
                
                // Solo se aplica el borrado alineado si el bloque contiene únicamente espacios.
                for ch in self.text.slice(start_idx..self.cursor_char_idx).chars() {
                    if ch != ' ' {
                        all_spaces = false;
                        break;
                    }
                }

                if all_spaces {
                    self.cursor_char_idx -= step;
                    self.text.remove(self.cursor_char_idx..(self.cursor_char_idx + step));
                    return;
                }
            }
        }

        // Si no aplica el borrado alineado, se elimina un único carácter.
        self.cursor_char_idx -= 1;
        self.text.remove(self.cursor_char_idx..self.cursor_char_idx + 1);
    }
    
    // --- Movimiento y posicionamiento del cursor ---

    fn line_len_without_nl(&self, line_idx: usize) -> usize {
        let line = self.text.line(line_idx);
        let mut len = line.len_chars();
        if len > 0 && line.char(len - 1) == '\n' {
            len -= 1;
            if len > 0 && line.char(len - 1) == '\r' {
                len -= 1;
            }
        }
        len
    }

    pub fn move_cursor_left(&mut self) {
        if self.cursor_char_idx > 0 {
            self.cursor_char_idx -= 1;
        }
    }

    pub fn move_cursor_right(&mut self) {
        if self.cursor_char_idx < self.text.len_chars() {
            self.cursor_char_idx += 1;
        }
    }

    pub fn move_cursor_up(&mut self) {
        let current_line = self.text.char_to_line(self.cursor_char_idx);
        if current_line > 0 {
            let current_col = self.cursor_char_idx - self.text.line_to_char(current_line);
            let target_line = current_line - 1;
            let target_col = current_col.min(self.line_len_without_nl(target_line));
            self.cursor_char_idx = self.text.line_to_char(target_line) + target_col;
        }
    }

    pub fn move_cursor_down(&mut self) {
        let current_line = self.text.char_to_line(self.cursor_char_idx);
        if current_line + 1 < self.text.len_lines() {
            let current_col = self.cursor_char_idx - self.text.line_to_char(current_line);
            let target_line = current_line + 1;
            let target_col = current_col.min(self.line_len_without_nl(target_line));
            self.cursor_char_idx = self.text.line_to_char(target_line) + target_col;
        }
    }

    pub fn ensure_cursor_visible(&mut self, view_width: usize, view_height: usize) {
        let cursor_y = self.text.char_to_line(self.cursor_char_idx);
        let cursor_x = self.cursor_char_idx - self.text.line_to_char(cursor_y);

        if cursor_y < self.scroll_y {
            self.scroll_y = cursor_y;
        } else if cursor_y >= self.scroll_y + view_height {
            self.scroll_y = cursor_y.saturating_sub(view_height - 1);
        }

        if cursor_x < self.scroll_x {
            self.scroll_x = cursor_x;
        } else if cursor_x >= self.scroll_x + view_width {
            self.scroll_x = cursor_x.saturating_sub(view_width - 1);
        }
    }

    pub fn get_lsp_position(&self) -> (u32, u32) {
        let line = self.text.char_to_line(self.cursor_char_idx);
        let col = self.cursor_char_idx - self.text.line_to_char(line);
        // Ropey trabaja con índices de caracteres, mientras que el protocolo LSP
        // espera unidades de código UTF-16. Ambos coinciden para ASCII, pero para
        // caracteres fuera del plano básico (p. ej. emojis) esta conversión
        // no es exacta y queda pendiente de una implementación más precisa.
        (line as u32, col as u32)
    }

    pub fn get_full_text(&self) -> String {
        self.text.to_string()
    }

    pub fn get_current_word_prefix(&self) -> String {
        let mut prefix = String::new();
        let mut idx = self.cursor_char_idx;
        while idx > 0 {
            idx -= 1;
            let ch = self.text.char(idx);
            // El recorrido se detiene al encontrar un delimitador de palabra
            // (espacios, puntos, dos puntos, etc.).
            if ch.is_alphanumeric() || ch == '_' {
                prefix.insert(0, ch);
            } else {
                break;
            }
        }
        prefix
    }
}
