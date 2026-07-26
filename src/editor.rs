use ropey::Rope;
use std::fs::{self, File};
use std::io::{self, BufWriter};
use std::path::Path;
use std::ops::Range;
use syntect::parsing::ParseState;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

/// Primitiva atómica que describe un cambio textual en el buffer.
#[derive(Clone, Debug)]
pub enum Mutation {
    Insert { idx: usize, text: String },
    Delete { idx: usize, text: String },
}

/// Agrupación de mutaciones lógicas que representan un estado deshechable (ej. tipear una palabra).
#[derive(Clone, Debug)]
pub struct Transaction {
    pub mutations: Vec<Mutation>,
    pub cursor_before: usize,
    pub cursor_after: usize,
}

/// Buffer de texto del editor. Usa una `Rope` (árbol de cuerdas) en vez de
/// un `String` plano para que insertar/borrar caracteres en cualquier punto
/// del documento sea eficiente incluso en archivos grandes.
pub struct EditorBuffer {
    pub text: Rope,
    pub cursor_char_idx: usize,
    pub selection_anchor: Option<usize>,
    pub scroll_x: usize,
    pub scroll_y: usize,
    pub syntax_cache: Vec<ParseState>,
    pub min_dirty_line: usize,

    // Historial de mutaciones
    pub undo_stack: Vec<Transaction>,
    pub redo_stack: Vec<Transaction>,
    pub current_tx: Option<Transaction>,
    pub target_visual_col: usize,
}

impl EditorBuffer {
    pub fn new() -> Self {
        Self {
            text: Rope::new(),
            cursor_char_idx: 0,
            selection_anchor: None,
            scroll_x: 0,
            scroll_y: 0,
            syntax_cache: Vec::with_capacity(1024),
            min_dirty_line: 0,
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            current_tx: None,
            target_visual_col: 0,
        }
    }

    pub fn load_from_file<P: AsRef<Path>>(path: P) -> io::Result<Self> {
        let bytes = fs::read(path)?;
        let text_str = String::from_utf8_lossy(&bytes);
        let text = Rope::from_str(&text_str);
        let lines_count = text.len_lines();
        Ok(Self {
            text,
            cursor_char_idx: 0,
            selection_anchor: None,
            scroll_x: 0,
            scroll_y: 0,
            syntax_cache: Vec::with_capacity(lines_count),
            min_dirty_line: 0,
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            current_tx: None,
            target_visual_col: 0,
        })
    }

    pub fn save_to_file<P: AsRef<Path>>(&self, path: P) -> io::Result<()> {
        let file = File::create(path)?;
        self.text.write_to(BufWriter::new(file))?;
        Ok(())
    }

    /// Obliga a sellar la transacción activa para que las siguientes ediciones no se agrupen con las pasadas.
    pub fn commit_tx(&mut self) {
        if let Some(tx) = self.current_tx.take() {
            if !tx.mutations.is_empty() {
                self.undo_stack.push(tx);
            }
        }
    }

    /// Registra una nueva mutación, fusionándola con la transacción activa si es secuencial,
    /// o generando una nueva si el usuario saltó de posición o cambió de acción (Insert -> Delete).
    fn record_edit(&mut self, muta: Mutation, cursor_before: usize, cursor_after: usize) {
        self.redo_stack.clear();

        let mut should_commit = false;

        if let Some(tx) = &self.current_tx {
            if cursor_before != tx.cursor_after {
                should_commit = true;
            } else {
                let last_is_insert = matches!(tx.mutations.last(), Some(Mutation::Insert{..}));
                let is_insert = matches!(muta, Mutation::Insert{..});
                if last_is_insert != is_insert {
                    should_commit = true;
                } else if tx.mutations.len() >= 100 {
                    should_commit = true;
                }
            }
        }

        if should_commit {
            self.commit_tx();
        }

        if self.current_tx.is_none() {
            self.current_tx = Some(Transaction {
                mutations: Vec::new(),
                cursor_before,
                cursor_after: 0,
            });
        }

        if let Some(tx) = &mut self.current_tx {
            let mut merged = false;
            if let Some(last) = tx.mutations.last_mut() {
                match (last, &muta) {
                    (Mutation::Insert { idx: l_idx, text: l_text }, Mutation::Insert { idx, text }) => {
                        // Inserciones contiguas (escribir natural)
                        if *l_idx + l_text.chars().count() == *idx {
                            l_text.push_str(text);
                            merged = true;
                        }
                    },
                    (Mutation::Delete { idx: l_idx, text: l_text }, Mutation::Delete { idx, text }) => {
                        if *idx + text.chars().count() == *l_idx {
                            // Borrado contiguo hacia atrás (Backspace)
                            l_text.insert_str(0, text);
                            *l_idx = *idx;
                            merged = true;
                        } else if *idx == *l_idx {
                            // Borrado contiguo estático (Suprimir frontal)
                            l_text.push_str(text);
                            merged = true;
                        }
                    },
                    _ => {}
                }
            }

            if !merged {
                tx.mutations.push(muta.clone());
            }

            tx.cursor_after = cursor_after;

            // Romper el grupo automáticamente si insertamos un separador de palabra o salto de línea
            if let Mutation::Insert { text, .. } = &muta {
                if text.contains(|c: char| c.is_whitespace() || c.is_ascii_punctuation()) {
                    self.commit_tx();
                }
            }
        }
    }

    /// Retrocede al estado guardado anterior. Devuelve los deltas procesados listos
    /// para enviar de regreso al servidor LSP.
    pub fn undo(&mut self) -> Option<Vec<(usize, usize, String)>> {
        self.commit_tx();
        let tx = self.undo_stack.pop()?;

        let mut lsp_deltas = Vec::new();
        let mut min_affected = usize::MAX;

        for muta in tx.mutations.iter().rev() {
            match muta {
                Mutation::Insert { idx, text } => {
                    let len = text.chars().count();
                    self.text.remove(*idx..(*idx + len));
                    lsp_deltas.push((*idx, *idx + len, String::new()));
                    min_affected = min_affected.min(*idx);
                }
                Mutation::Delete { idx, text } => {
                    self.text.insert(*idx, text);
                    lsp_deltas.push((*idx, *idx, text.clone()));
                    min_affected = min_affected.min(*idx);
                }
            }
        }

        self.cursor_char_idx = tx.cursor_before;
        self.selection_anchor = None;

        if min_affected != usize::MAX {
            let current_line = self.text.char_to_line(min_affected);
            self.min_dirty_line = self.min_dirty_line.min(current_line);
            self.syntax_cache.truncate(self.min_dirty_line);
        }

        self.redo_stack.push(tx);
        Some(lsp_deltas)
    }

    /// Repite la transacción desechada más reciente.
    pub fn redo(&mut self) -> Option<Vec<(usize, usize, String)>> {
        self.commit_tx();
        let tx = self.redo_stack.pop()?;

        let mut lsp_deltas = Vec::new();
        let mut min_affected = usize::MAX;

        for muta in tx.mutations.iter() {
            match muta {
                Mutation::Insert { idx, text } => {
                    self.text.insert(*idx, text);
                    lsp_deltas.push((*idx, *idx, text.clone()));
                    min_affected = min_affected.min(*idx);
                }
                Mutation::Delete { idx, text } => {
                    let len = text.chars().count();
                    self.text.remove(*idx..(*idx + len));
                    lsp_deltas.push((*idx, *idx + len, String::new()));
                    min_affected = min_affected.min(*idx);
                }
            }
        }

        self.cursor_char_idx = tx.cursor_after;
        self.selection_anchor = None;

        if min_affected != usize::MAX {
            let current_line = self.text.char_to_line(min_affected);
            self.min_dirty_line = self.min_dirty_line.min(current_line);
            self.syntax_cache.truncate(self.min_dirty_line);
        }

        self.undo_stack.push(tx);
        Some(lsp_deltas)
    }

    pub fn char_idx_to_visual_col(&self, char_idx: usize) -> usize {
        let line_idx = self.text.char_to_line(char_idx);
        let line_start = self.text.line_to_char(line_idx);
        let offset = char_idx.saturating_sub(line_start);

        let line_str = self.text.line(line_idx).to_string();
        let prefix: String = line_str.chars().take(offset).collect();
        prefix.width()
    }

    pub fn visual_col_to_char_idx(&self, line_idx: usize, target_col: usize) -> usize {
        let line_str = self.text.line(line_idx).to_string();
        let mut current_col = 0;
        let mut char_offset = 0;

        for g in line_str.graphemes(true) {
            if g == "\n" || g == "\r\n" { break; }
            let w = g.width();
            if current_col + w > target_col && current_col > 0 {
                break;
            }
            current_col += w;
            char_offset += g.chars().count();
        }

        self.text.line_to_char(line_idx) + char_offset
    }

    pub fn insert_char(&mut self, ch: char) {
        let cursor_before = self.cursor_char_idx;
        self.text.insert_char(self.cursor_char_idx, ch);
        self.cursor_char_idx += 1;

        self.record_edit(Mutation::Insert { idx: cursor_before, text: ch.to_string() }, cursor_before, self.cursor_char_idx);
        self.mark_dirty();
        self.target_visual_col = self.char_idx_to_visual_col(self.cursor_char_idx);
    }

    pub fn insert_str(&mut self, s: &str) {
        if s.is_empty() { return; }
        let cursor_before = self.cursor_char_idx;
        self.text.insert(self.cursor_char_idx, s);
        self.cursor_char_idx += s.chars().count();

        self.record_edit(Mutation::Insert { idx: cursor_before, text: s.to_string() }, cursor_before, self.cursor_char_idx);
        self.mark_dirty();
        self.target_visual_col = self.char_idx_to_visual_col(self.cursor_char_idx);
    }

    pub fn get_selection_range(&self) -> Option<Range<usize>> {
        self.selection_anchor.map(|anchor| {
            if anchor < self.cursor_char_idx { anchor..self.cursor_char_idx }
            else { self.cursor_char_idx..anchor }
        }).filter(|r| r.start != r.end)
    }

    pub fn get_selected_text(&self) -> Option<String> {
        self.get_selection_range().map(|r| self.text.slice(r).to_string())
    }

    pub fn delete_selection(&mut self) -> Option<String> {
        if let Some(range) = self.get_selection_range() {
            let cursor_before = self.cursor_char_idx;
            let text = self.text.slice(range.clone()).to_string();

            self.commit_tx(); // Aísla la transacción

            self.text.remove(range.clone());
            self.cursor_char_idx = range.start;
            self.selection_anchor = None;

            self.record_edit(Mutation::Delete { idx: range.start, text: text.clone() }, cursor_before, self.cursor_char_idx);
            self.commit_tx();
            self.mark_dirty();

            Some(text)
        } else {
            None
        }
    }

    pub fn delete_backwards(&mut self) {
        if self.delete_selection().is_some() { return; }
        if self.cursor_char_idx == 0 { return; }

        let cursor_before = self.cursor_char_idx;
        let current_line_idx = self.text.char_to_line(self.cursor_char_idx);
        let line_start = self.text.line_to_char(current_line_idx);

        // Smart delete para tabs lógicos (espacios)
        let col = self.cursor_char_idx - line_start;
        if col > 0 {
            let rem = col % 4;
            let step = if rem == 0 { 4 } else { rem };
            if col >= step {
                let start_idx = self.cursor_char_idx - step;
                if self.text.slice(start_idx..self.cursor_char_idx).chars().all(|c| c == ' ') {
                    let text = self.text.slice(start_idx..self.cursor_char_idx).to_string();
                    self.cursor_char_idx -= step;
                    self.text.remove(self.cursor_char_idx..(self.cursor_char_idx + step));
                    self.record_edit(Mutation::Delete { idx: self.cursor_char_idx, text }, cursor_before, self.cursor_char_idx);
                    self.mark_dirty();
                    self.target_visual_col = self.char_idx_to_visual_col(self.cursor_char_idx);
                    return;
                }
            }
        }

        // Borrado por grafema exacto en vez de char escalar
        let chars_to_delete = if self.cursor_char_idx == line_start {
            if self.cursor_char_idx > 1 && self.text.char(self.cursor_char_idx - 2) == '\r' { 2 } else { 1 }
        } else {
            let offset = self.cursor_char_idx - line_start;
            let prefix: String = self.text.line(current_line_idx).chars().take(offset).collect();
            prefix.graphemes(true).last().map_or(1, |g| g.chars().count())
        };

        let start_idx = self.cursor_char_idx - chars_to_delete;
        let text = self.text.slice(start_idx..self.cursor_char_idx).to_string();

        self.text.remove(start_idx..self.cursor_char_idx);
        self.cursor_char_idx = start_idx;

        self.record_edit(Mutation::Delete { idx: self.cursor_char_idx, text }, cursor_before, self.cursor_char_idx);
        self.mark_dirty();
        self.target_visual_col = self.char_idx_to_visual_col(self.cursor_char_idx);
    }

    pub fn mark_dirty(&mut self) {
        let current_line = self.text.char_to_line(self.cursor_char_idx);
        self.min_dirty_line = self.min_dirty_line.min(current_line);
        self.syntax_cache.truncate(self.min_dirty_line);
    }

    fn update_selection(&mut self, selecting: bool) {
        if selecting {
            if self.selection_anchor.is_none() {
                self.selection_anchor = Some(self.cursor_char_idx);
            }
        } else {
            self.selection_anchor = None;
        }
    }

    fn line_len_without_nl(&self, line_idx: usize) -> usize {
        let line = self.text.line(line_idx);
        let mut len = line.len_chars();
        if len > 0 && line.char(len - 1) == '\n' {
            len -= 1;
            if len > 0 && line.char(len - 1) == '\r' { len -= 1; }
        }
        len
    }

    pub fn move_cursor_left(&mut self, selecting: bool) {
        self.update_selection(selecting);
        if self.cursor_char_idx > 0 {
            let line_idx = self.text.char_to_line(self.cursor_char_idx);
            let line_start = self.text.line_to_char(line_idx);

            if self.cursor_char_idx == line_start {
                self.cursor_char_idx -= 1;
                if self.cursor_char_idx > 0 && self.text.char(self.cursor_char_idx - 1) == '\r' {
                    self.cursor_char_idx -= 1;
                }
            } else {
                let offset = self.cursor_char_idx - line_start;
                let prefix: String = self.text.line(line_idx).chars().take(offset).collect();
                if let Some(last_g) = prefix.graphemes(true).last() {
                    self.cursor_char_idx -= last_g.chars().count();
                }
            }
        }
        self.target_visual_col = self.char_idx_to_visual_col(self.cursor_char_idx);
    }

    pub fn move_cursor_right(&mut self, selecting: bool) {
        self.update_selection(selecting);
        if self.cursor_char_idx < self.text.len_chars() {
            let line_idx = self.text.char_to_line(self.cursor_char_idx);
            let line_start = self.text.line_to_char(line_idx);
            let offset = self.cursor_char_idx - line_start;

            let suffix: String = self.text.line(line_idx).chars().skip(offset).collect();
            if suffix.starts_with("\r\n") {
                self.cursor_char_idx += 2;
            } else if suffix.starts_with('\n') {
                self.cursor_char_idx += 1;
            } else if let Some(first_g) = suffix.graphemes(true).next() {
                self.cursor_char_idx += first_g.chars().count();
            }
        }
        self.target_visual_col = self.char_idx_to_visual_col(self.cursor_char_idx);
    }

    pub fn move_cursor_up(&mut self, selecting: bool) {
        self.update_selection(selecting);
        let current_line = self.text.char_to_line(self.cursor_char_idx);
        if current_line > 0 {
            self.cursor_char_idx = self.visual_col_to_char_idx(current_line - 1, self.target_visual_col);
        }
    }

    pub fn move_cursor_down(&mut self, selecting: bool) {
        self.update_selection(selecting);
        let current_line = self.text.char_to_line(self.cursor_char_idx);
        if current_line + 1 < self.text.len_lines() {
            self.cursor_char_idx = self.visual_col_to_char_idx(current_line + 1, self.target_visual_col);
        }
    }

    pub fn ensure_cursor_visible(&mut self, view_width: usize, view_height: usize) {
        let cursor_y = self.text.char_to_line(self.cursor_char_idx);
        let cursor_x_vis = self.char_idx_to_visual_col(self.cursor_char_idx);

        let margin_y = view_height.saturating_sub(1) / 3;
        let margin_x = 4;

        if cursor_y < self.scroll_y + margin_y {
            self.scroll_y = cursor_y.saturating_sub(margin_y);
        } else if cursor_y + margin_y >= self.scroll_y + view_height {
            self.scroll_y = (cursor_y + margin_y + 1).saturating_sub(view_height);
        }

        if cursor_x_vis < self.scroll_x + margin_x {
            self.scroll_x = cursor_x_vis.saturating_sub(margin_x);
        } else if cursor_x_vis + margin_x >= self.scroll_x + view_width {
            self.scroll_x = (cursor_x_vis + margin_x + 1).saturating_sub(view_width);
        }
    }

    pub fn set_cursor_from_screen(&mut self, screen_x: u16, screen_y: u16, editor_start_x: u16, editor_start_y: u16, gutter_width: u16, selecting: bool) {
        if screen_x < editor_start_x + gutter_width || screen_y < editor_start_y { return; }

        self.update_selection(selecting);

        let rel_x = (screen_x - (editor_start_x + gutter_width)) as usize;
        let rel_y = (screen_y - editor_start_y) as usize;

        let target_line = (self.scroll_y + rel_y).min(self.text.len_lines().saturating_sub(1));
        let target_col = self.scroll_x + rel_x;

        self.cursor_char_idx = self.visual_col_to_char_idx(target_line, target_col);
        self.target_visual_col = self.char_idx_to_visual_col(self.cursor_char_idx);
    }

    pub fn get_lsp_position(&self) -> (u32, u32) {
        let line = self.text.char_to_line(self.cursor_char_idx);
        let line_start = self.text.line_to_char(line);
        let col_chars = self.cursor_char_idx - line_start;

        let utf16_col: usize = self.text.line(line)
            .chars()
            .take(col_chars)
            .map(|c| c.len_utf16())
            .sum();

        (line as u32, utf16_col as u32)
    }

    pub fn get_full_text(&self) -> String { self.text.to_string() }

    pub fn get_current_word_prefix(&self) -> String {
        let mut prefix = String::new();
        let mut idx = self.cursor_char_idx;
        while idx > 0 {
            idx -= 1;
            let ch = self.text.char(idx);
            if ch.is_alphanumeric() || ch == '_' { prefix.insert(0, ch); }
            else { break; }
        }
        prefix
    }

    pub fn get_current_line_indentation(&self) -> String {
        let current_line_idx = self.text.char_to_line(self.cursor_char_idx);
        let line = self.text.line(current_line_idx);
        let mut indent = String::new();
        for ch in line.chars() {
            if ch == ' ' || ch == '\t' { indent.push(ch); } else { break; }
        }
        indent
    }

    pub fn char_at_cursor(&self) -> Option<char> {
        if self.cursor_char_idx < self.text.len_chars() { Some(self.text.char(self.cursor_char_idx)) }
        else { None }
    }

    pub fn move_to_start_of_line(&mut self, selecting: bool) {
        self.update_selection(selecting);
        let line_idx = self.text.char_to_line(self.cursor_char_idx);
        self.cursor_char_idx = self.text.line_to_char(line_idx);
    }

    pub fn move_to_end_of_line(&mut self, selecting: bool) {
        self.update_selection(selecting);
        let line_idx = self.text.char_to_line(self.cursor_char_idx);
        let line_start = self.text.line_to_char(line_idx);
        let len = self.line_len_without_nl(line_idx);
        self.cursor_char_idx = line_start + len;
    }

    pub fn move_page_up(&mut self, selecting: bool, lines: usize) {
        self.update_selection(selecting);
        let current_line = self.text.char_to_line(self.cursor_char_idx);
        let target_line = current_line.saturating_sub(lines);
        self.cursor_char_idx = self.visual_col_to_char_idx(target_line, self.target_visual_col);
    }

    pub fn move_page_down(&mut self, selecting: bool, lines: usize) {
        self.update_selection(selecting);
        let current_line = self.text.char_to_line(self.cursor_char_idx);
        let target_line = (current_line + lines).min(self.text.len_lines().saturating_sub(1));
        self.cursor_char_idx = self.visual_col_to_char_idx(target_line, self.target_visual_col);
    }

    pub fn scroll_viewport_up(&mut self, lines: usize, view_height: usize, selecting: bool) {
        self.scroll_y = self.scroll_y.saturating_sub(lines);
        self.enforce_cursor_in_viewport(view_height, selecting);
    }

    pub fn scroll_viewport_down(&mut self, lines: usize, view_height: usize, selecting: bool) {
        let max_scroll = self.text.len_lines().saturating_sub(1);
        self.scroll_y = (self.scroll_y + lines).min(max_scroll);
        self.enforce_cursor_in_viewport(view_height, selecting);
    }

    fn enforce_cursor_in_viewport(&mut self, view_height: usize, selecting: bool) {
        let current_line = self.text.char_to_line(self.cursor_char_idx);
        let current_col_vis = self.char_idx_to_visual_col(self.cursor_char_idx);

        let margin_y = view_height.saturating_sub(1) / 3;

        let target_line = if current_line < self.scroll_y + margin_y {
            self.scroll_y + margin_y
        } else if view_height > 0 && current_line >= self.scroll_y + view_height.saturating_sub(margin_y) {
            (self.scroll_y + view_height).saturating_sub(margin_y + 1)
        } else {
            return;
        };

        let max_line = self.text.len_lines().saturating_sub(1);
        let safe_target = target_line.min(max_line);

        self.update_selection(selecting);
        self.cursor_char_idx = self.visual_col_to_char_idx(safe_target, current_col_vis);
        self.target_visual_col = self.char_idx_to_visual_col(self.cursor_char_idx);
    }

    pub fn find_text(&self, query: &str) -> Vec<usize> {
        let mut results = Vec::new();
        if query.is_empty() { return results; }

        for (line_idx, line) in self.text.lines().enumerate() {
            if let Some(chunk) = line.as_str() {
                let mut start_byte = 0;
                while let Some(byte_offset) = chunk[start_byte..].find(query) {
                    let match_byte = start_byte + byte_offset;
                    let char_offset = chunk[start_byte..].chars().count();
                    results.push(self.text.line_to_char(line_idx) + char_offset);
                    start_byte = match_byte + query.len();
                }
            } else {
                let line_str = line.to_string();
                let mut start_byte = 0;
                while let Some(byte_offset) = line_str[start_byte..].find(query) {
                    let match_byte = start_byte + byte_offset;
                    let char_offset = line_str[start_byte..].chars().count();
                    results.push(self.text.line_to_char(line_idx) + char_offset);
                    start_byte = match_byte + query.len();
                }
            }
        }
        results
    }

    pub fn get_lsp_position_utf16_at(&self, char_idx: usize) -> (u32, u32) {
        let safe_idx = char_idx.min(self.text.len_chars());
        let line = self.text.char_to_line(safe_idx);
        let line_start = self.text.line_to_char(line);
        let col_chars = safe_idx - line_start;

        let utf16_col: usize = self.text.line(line)
            .chars()
            .take(col_chars)
            .map(|c| c.len_utf16())
            .sum();

        (line as u32, utf16_col as u32)
    }
}