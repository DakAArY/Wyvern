use ropey::Rope;
use std::fs::{self, File};
use std::io::{self, BufWriter};
use std::path::Path;
use std::ops::Range;
use syntect::highlighting::HighlightState;
use syntect::parsing::ParseState;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

pub const TAB_WIDTH: usize = 4;

/// Ancho en pantalla de un grafema que empieza en la columna `col`.
pub fn grapheme_width(g: &str, col: usize) -> usize {
    if g == "\t" { TAB_WIDTH - col % TAB_WIDTH } else { g.width() }
}

/// Operación elemental necesaria para reconstruir una edición.
#[derive(Clone, Debug)]
pub enum Mutation {
    Insert { idx: usize, text: String },
    Delete { idx: usize, text: String },
}

/// Conjunto de mutaciones que el usuario puede deshacer como una unidad.
#[derive(Clone, Debug)]
pub struct Transaction {
    pub mutations: Vec<Mutation>,
    pub cursor_before: usize,
    pub cursor_after: usize,
}

/// Cambio pendiente de enviar al LSP, en coordenadas (línea, columna UTF-16)
/// del texto tal como estaba justo antes de aplicarlo.
#[derive(Clone, Debug, PartialEq)]
pub struct LspChange {
    pub start: (u32, u32),
    pub end: (u32, u32),
    pub text: String,
}

/// Modelo editable basado en `Rope`, con cursor, selección, viewport y undo/redo.
pub struct EditorBuffer {
    pub text: Rope,
    pub cursor_char_idx: usize,
    pub selection_anchor: Option<usize>,
    pub scroll_x: usize,
    pub scroll_y: usize,
    /// Estado del resaltador al inicio de cada línea ya procesada.
    pub syntax_cache: Vec<(ParseState, HighlightState)>,
    pub lsp_changes: Vec<LspChange>,

    // Las transacciones activas y sus pilas permiten agrupar la edición natural.
    pub undo_stack: Vec<Transaction>,
    pub redo_stack: Vec<Transaction>,
    pub current_tx: Option<Transaction>,
    pub target_visual_col: usize,
    pub last_sync_cursor_idx: usize,
    pub last_view_width: usize,
    pub last_view_height: usize,
    pub force_sync_cursor: bool,
}

impl EditorBuffer {
    pub fn new() -> Self {
        Self {
            text: Rope::new(),
            cursor_char_idx: 0,
            selection_anchor: None,
            scroll_x: 0,
            scroll_y: 0,
            syntax_cache: Vec::new(),
            lsp_changes: Vec::new(),
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            current_tx: None,
            target_visual_col: 0,
            last_sync_cursor_idx: usize::MAX,
            last_view_width: 0,
            last_view_height: 0,
            force_sync_cursor: true,
        }
    }

    /// Rechaza archivos que no son UTF-8: abrirlos con reemplazo y guardarlos
    /// destruiría los bytes originales.
    pub fn load_from_file<P: AsRef<Path>>(path: P) -> io::Result<Self> {
        let text = String::from_utf8(fs::read(path)?)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "el archivo no es UTF-8 válido"))?;
        let mut buf = Self::new();
        buf.text = Rope::from_str(&text);
        Ok(buf)
    }

    /// Escribe en un temporal y lo renombra encima: un fallo a mitad de escritura
    /// nunca deja el archivo original truncado.
    pub fn save_to_file<P: AsRef<Path>>(&self, path: P) -> io::Result<()> {
        // Se resuelve el symlink para reemplazar su destino y no el enlace.
        let path = fs::canonicalize(path.as_ref()).unwrap_or_else(|_| path.as_ref().to_path_buf());
        let name = path.file_name()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "ruta sin nombre de archivo"))?;
        let tmp = path.with_file_name(format!(".{}.wyvern-tmp", name.to_string_lossy()));

        let res = (|| {
            let mut w = BufWriter::new(File::create(&tmp)?);
            self.text.write_to(&mut w)?;
            let file = w.into_inner().map_err(|e| e.into_error())?;
            if let Ok(meta) = fs::metadata(&path) {
                file.set_permissions(meta.permissions())?;
            }
            file.sync_all()?;
            fs::rename(&tmp, &path)
        })();
        if res.is_err() {
            let _ = fs::remove_file(&tmp);
        }
        res
    }

    /// Única vía de inserción en el rope: registra el cambio para el LSP e invalida el resaltado.
    fn raw_insert(&mut self, idx: usize, text: &str) {
        let pos = self.get_lsp_position_utf16_at(idx);
        self.lsp_changes.push(LspChange { start: pos, end: pos, text: text.to_string() });
        self.invalidate_syntax_from(idx);
        self.text.insert(idx, text);
    }

    /// Única vía de borrado en el rope; devuelve el texto eliminado.
    fn raw_remove(&mut self, range: Range<usize>) -> String {
        let removed = self.text.slice(range.clone()).to_string();
        let start = self.get_lsp_position_utf16_at(range.start);
        let end = self.get_lsp_position_utf16_at(range.end);
        self.lsp_changes.push(LspChange { start, end, text: String::new() });
        self.invalidate_syntax_from(range.start);
        self.text.remove(range);
        removed
    }

    /// El estado al inicio de la línea editada sigue siendo válido; los siguientes no.
    fn invalidate_syntax_from(&mut self, char_idx: usize) {
        let line = self.text.char_to_line(char_idx.min(self.text.len_chars()));
        self.syntax_cache.truncate(line + 1);
    }

    /// Cierra la transacción actual para aislarla de las ediciones siguientes.
    pub fn commit_tx(&mut self) {
        if let Some(tx) = self.current_tx.take()
            && !tx.mutations.is_empty() {
                self.undo_stack.push(tx);
            }
    }

    /// Registra una mutación y la agrupa mientras la edición siga siendo continua.
    fn record_edit(&mut self, muta: Mutation, cursor_before: usize, cursor_after: usize) {
        self.redo_stack.clear();

        if let Some(tx) = &self.current_tx {
            let last_is_insert = matches!(tx.mutations.last(), Some(Mutation::Insert { .. }));
            let is_insert = matches!(muta, Mutation::Insert { .. });
            if cursor_before != tx.cursor_after || last_is_insert != is_insert || tx.mutations.len() >= 100 {
                self.commit_tx();
            }
        }

        let tx = self.current_tx.get_or_insert(Transaction { mutations: Vec::new(), cursor_before, cursor_after: 0 });

        let merged = match (tx.mutations.last_mut(), &muta) {
            // Las inserciones consecutivas forman una sola acción de escritura.
            (Some(Mutation::Insert { idx: l_idx, text: l_text }), Mutation::Insert { idx, text })
                if *l_idx + l_text.chars().count() == *idx => { l_text.push_str(text); true }
            // Backspace extiende el borrado hacia el inicio del rango.
            (Some(Mutation::Delete { idx: l_idx, text: l_text }), Mutation::Delete { idx, text })
                if *idx + text.chars().count() == *l_idx => { l_text.insert_str(0, text); *l_idx = *idx; true }
            // Suprimir frontal extiende el borrado desde el mismo índice.
            (Some(Mutation::Delete { idx: l_idx, text: l_text }), Mutation::Delete { idx, text })
                if *idx == *l_idx => { l_text.push_str(text); true }
            _ => false,
        };
        if !merged {
            tx.mutations.push(muta.clone());
        }
        tx.cursor_after = cursor_after;

        // Los separadores delimitan palabras y hacen más natural el historial.
        if let Mutation::Insert { text, .. } = &muta
            && text.contains(|c: char| c.is_whitespace() || c.is_ascii_punctuation()) {
                self.commit_tx();
            }
    }

    /// Revierte la última transacción; devuelve `false` si no había nada que deshacer.
    pub fn undo(&mut self) -> bool {
        self.commit_tx();
        let Some(tx) = self.undo_stack.pop() else { return false };
        for muta in tx.mutations.iter().rev() {
            match muta {
                Mutation::Insert { idx, text } => { self.raw_remove(*idx..*idx + text.chars().count()); }
                Mutation::Delete { idx, text } => self.raw_insert(*idx, text),
            }
        }
        self.cursor_char_idx = tx.cursor_before;
        self.selection_anchor = None;
        self.redo_stack.push(tx);
        true
    }

    /// Reaplica la transacción más reciente de la pila de rehacer.
    pub fn redo(&mut self) -> bool {
        self.commit_tx();
        let Some(tx) = self.redo_stack.pop() else { return false };
        for muta in tx.mutations.iter() {
            match muta {
                Mutation::Insert { idx, text } => self.raw_insert(*idx, text),
                Mutation::Delete { idx, text } => { self.raw_remove(*idx..*idx + text.chars().count()); }
            }
        }
        self.cursor_char_idx = tx.cursor_after;
        self.selection_anchor = None;
        self.undo_stack.push(tx);
        true
    }

    pub fn char_idx_to_visual_col(&self, char_idx: usize) -> usize {
        let line_idx = self.text.char_to_line(char_idx);
        let offset = char_idx - self.text.line_to_char(line_idx);
        let prefix: String = self.text.line(line_idx).chars().take(offset).collect();
        prefix.graphemes(true).fold(0, |col, g| col + grapheme_width(g, col))
    }

    pub fn visual_col_to_char_idx(&self, line_idx: usize, target_col: usize) -> usize {
        let line_str = self.text.line(line_idx).to_string();
        let mut current_col = 0;
        let mut char_offset = 0;

        for g in line_str.graphemes(true) {
            if g == "\n" || g == "\r\n" { break; }
            let w = grapheme_width(g, current_col);
            if current_col + w > target_col && current_col > 0 {
                break;
            }
            current_col += w;
            char_offset += g.chars().count();
        }

        self.text.line_to_char(line_idx) + char_offset
    }

    pub fn insert_char(&mut self, ch: char) {
        self.insert_str(ch.encode_utf8(&mut [0; 4]));
    }

    pub fn insert_str(&mut self, s: &str) {
        if s.is_empty() { return; }
        let cursor_before = self.cursor_char_idx;
        self.raw_insert(cursor_before, s);
        self.cursor_char_idx += s.chars().count();

        self.record_edit(Mutation::Insert { idx: cursor_before, text: s.to_string() }, cursor_before, self.cursor_char_idx);
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
        let range = self.get_selection_range()?;
        let cursor_before = self.cursor_char_idx;

        self.commit_tx();
        let text = self.raw_remove(range.clone());
        self.cursor_char_idx = range.start;
        self.selection_anchor = None;

        self.record_edit(Mutation::Delete { idx: range.start, text: text.clone() }, cursor_before, self.cursor_char_idx);
        self.commit_tx();
        self.target_visual_col = self.char_idx_to_visual_col(self.cursor_char_idx);
        Some(text)
    }

    /// Borra `range` dejando el cursor en su inicio, como un solo paso de historial.
    fn delete_range(&mut self, range: Range<usize>) {
        let cursor_before = self.cursor_char_idx;
        let text = self.raw_remove(range.clone());
        self.cursor_char_idx = range.start;
        self.record_edit(Mutation::Delete { idx: range.start, text }, cursor_before, range.start);
        self.target_visual_col = self.char_idx_to_visual_col(self.cursor_char_idx);
    }

    pub fn delete_backwards(&mut self) {
        if self.delete_selection().is_some() { return; }
        if self.cursor_char_idx == 0 { return; }

        let cursor = self.cursor_char_idx;
        let line_start = self.text.line_to_char(self.text.char_to_line(cursor));

        // Si la indentación está formada por espacios, elimina un nivel lógico.
        let col = cursor - line_start;
        if col > 0 {
            let step = match col % TAB_WIDTH { 0 => TAB_WIDTH, rem => rem };
            if col >= step && self.text.slice(cursor - step..cursor).chars().all(|c| c == ' ') {
                self.delete_range(cursor - step..cursor);
                return;
            }
        }

        // Se elimina el grafema completo (o el CRLF) para no separar caracteres.
        self.move_cursor_left(false);
        let start = self.cursor_char_idx;
        self.cursor_char_idx = cursor;
        self.delete_range(start..cursor);
    }

    pub fn delete_forward(&mut self) {
        if self.delete_selection().is_some() { return; }
        let start = self.cursor_char_idx;
        if start >= self.text.len_chars() { return; }

        self.move_cursor_right(false);
        let end = self.cursor_char_idx;
        self.cursor_char_idx = start;
        self.delete_range(start..end);
    }

    /// Aplica ediciones expresadas sobre el texto original como un único paso de
    /// historial. Devuelve dónde quedó el inicio de `edits[main]` tras desplazamientos.
    pub fn apply_edits(&mut self, edits: &[(Range<usize>, String)], main: usize) -> usize {
        self.commit_tx();
        self.redo_stack.clear();
        let cursor_before = self.cursor_char_idx;

        // De atrás hacia delante para que cada rango siga siendo válido.
        let mut order: Vec<usize> = (0..edits.len()).collect();
        order.sort_by_key(|&i| std::cmp::Reverse(edits[i].0.start));

        let mut mutations = Vec::new();
        for &i in &order {
            let (range, text) = &edits[i];
            if !range.is_empty() {
                let removed = self.raw_remove(range.clone());
                mutations.push(Mutation::Delete { idx: range.start, text: removed });
            }
            if !text.is_empty() {
                self.raw_insert(range.start, text);
                mutations.push(Mutation::Insert { idx: range.start, text: text.clone() });
            }
        }

        let main_start = edits[main].0.start;
        let shift: isize = edits.iter()
            .filter(|(r, _)| r.start < main_start)
            .map(|(r, t)| t.chars().count() as isize - r.len() as isize)
            .sum();
        let new_start = (main_start as isize + shift) as usize;

        self.cursor_char_idx = new_start + edits[main].1.chars().count();
        self.selection_anchor = None;
        self.target_visual_col = self.char_idx_to_visual_col(self.cursor_char_idx);
        if !mutations.is_empty() {
            self.undo_stack.push(Transaction { mutations, cursor_before, cursor_after: self.cursor_char_idx });
        }
        new_start
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
                if let Some(last_g) = prefix.graphemes(true).next_back() {
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
        self.get_lsp_position_utf16_at(self.cursor_char_idx)
    }

    pub fn get_lsp_position_utf16_at(&self, char_idx: usize) -> (u32, u32) {
        let safe_idx = char_idx.min(self.text.len_chars());
        let line = self.text.char_to_line(safe_idx);
        let col_chars = safe_idx - self.text.line_to_char(line);

        let utf16_col: usize = self.text.line(line)
            .chars()
            .take(col_chars)
            .map(|c| c.len_utf16())
            .sum();

        (line as u32, utf16_col as u32)
    }

    /// Inversa de `get_lsp_position_utf16_at`; satura en el final de línea o del texto.
    pub fn lsp_position_to_char_idx(&self, line: u32, utf16_col: u32) -> usize {
        let line = line as usize;
        if line >= self.text.len_lines() { return self.text.len_chars(); }
        let mut units = 0;
        let mut offset = 0;
        for c in self.text.line(line).chars() {
            if units >= utf16_col as usize || c == '\n' || c == '\r' { break; }
            units += c.len_utf16();
            offset += 1;
        }
        self.text.line_to_char(line) + offset
    }

    pub fn get_full_text(&self) -> String { self.text.to_string() }

    /// Identificador inmediatamente anterior al cursor.
    pub fn get_current_word_prefix(&self) -> String {
        let mut idx = self.cursor_char_idx;
        while idx > 0 {
            let ch = self.text.char(idx - 1);
            if !(ch.is_alphanumeric() || ch == '_') { break; }
            idx -= 1;
        }
        self.text.slice(idx..self.cursor_char_idx).to_string()
    }

    /// Cuántos caracteres antes del cursor reemplaza `insert` cuando el servidor no da rango.
    pub fn completion_replace_len(&self, insert: &str) -> usize {
        let line_start = self.text.line_to_char(self.text.char_to_line(self.cursor_char_idx));
        let before: Vec<char> = self.text.slice(line_start..self.cursor_char_idx).chars().collect();
        let ins: Vec<char> = insert.chars().collect();

        // Lo ya escrito que coincide con el inicio de la inserción forma parte de ella.
        for k in (1..=before.len().min(ins.len())).rev() {
            let typed = &before[before.len() - k..];
            if typed.iter().any(|c| c.is_whitespace()) { continue; }
            if typed.iter().zip(&ins).all(|(a, b)| a.to_lowercase().eq(b.to_lowercase())) {
                return k;
            }
        }
        self.get_current_word_prefix().chars().count()
    }

    pub fn get_current_line_indentation(&self) -> String {
        let line = self.text.line(self.text.char_to_line(self.cursor_char_idx));
        line.chars().take_while(|&c| c == ' ' || c == '\t').collect()
    }

    pub fn char_at_cursor(&self) -> Option<char> {
        if self.cursor_char_idx < self.text.len_chars() { Some(self.text.char(self.cursor_char_idx)) }
        else { None }
    }

    pub fn move_to_start_of_line(&mut self, selecting: bool) {
        self.update_selection(selecting);
        let line_idx = self.text.char_to_line(self.cursor_char_idx);
        self.cursor_char_idx = self.text.line_to_char(line_idx);
        self.target_visual_col = 0;
    }

    pub fn move_to_end_of_line(&mut self, selecting: bool) {
        self.update_selection(selecting);
        let line_idx = self.text.char_to_line(self.cursor_char_idx);
        self.cursor_char_idx = self.text.line_to_char(line_idx) + self.line_len_without_nl(line_idx);
        self.target_visual_col = self.char_idx_to_visual_col(self.cursor_char_idx);
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

    pub fn scroll_viewport_up(&mut self, lines: usize) {
        self.scroll_y = self.scroll_y.saturating_sub(lines);
    }

    pub fn scroll_viewport_down(&mut self, lines: usize) {
        let max_scroll = self.text.len_lines().saturating_sub(1);
        self.scroll_y = (self.scroll_y + lines).min(max_scroll);
    }

    pub fn scroll_viewport_left(&mut self, cols: usize) {
        self.scroll_x = self.scroll_x.saturating_sub(cols);
    }

    pub fn scroll_viewport_right(&mut self, cols: usize) {
        self.scroll_x = self.scroll_x.saturating_add(cols);
    }

    pub fn find_text(&self, query: &str) -> Vec<usize> {
        let mut results = Vec::new();
        if query.is_empty() { return results; }

        for (line_idx, line) in self.text.lines().enumerate() {
            let line_str = line.to_string();
            let line_start = self.text.line_to_char(line_idx);
            for (byte, _) in line_str.match_indices(query) {
                results.push(line_start + line_str[..byte].chars().count());
            }
        }
        results
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn buf(s: &str) -> EditorBuffer {
        let mut b = EditorBuffer::new();
        b.text = Rope::from_str(s);
        b
    }

    #[test]
    fn typing_then_undo_redo_restores_text() {
        let mut b = buf("");
        for c in "hola mundo".chars() { b.insert_char(c); }
        assert_eq!(b.text.to_string(), "hola mundo");
        while b.undo() {}
        assert_eq!(b.text.to_string(), "");
        while b.redo() {}
        assert_eq!(b.text.to_string(), "hola mundo");
        assert_eq!(b.cursor_char_idx, 10);
    }

    #[test]
    fn backspace_removes_whole_grapheme_and_crlf() {
        let mut b = buf("a\r\ne\u{301}");
        b.cursor_char_idx = b.text.len_chars();
        b.delete_backwards();
        assert_eq!(b.text.to_string(), "a\r\n");
        b.delete_backwards();
        assert_eq!(b.text.to_string(), "a");
    }

    #[test]
    fn backspace_removes_one_indent_level() {
        let mut b = buf("        x");
        b.cursor_char_idx = 8;
        b.delete_backwards();
        assert_eq!(b.text.to_string(), "    x");
    }

    #[test]
    fn selection_delete_and_undo() {
        let mut b = buf("abcdef");
        b.selection_anchor = Some(1);
        b.cursor_char_idx = 4;
        assert_eq!(b.delete_selection().as_deref(), Some("bcd"));
        assert_eq!(b.text.to_string(), "aef");
        b.undo();
        assert_eq!(b.text.to_string(), "abcdef");
    }

    #[test]
    fn find_text_returns_char_indices() {
        let b = buf("ñaña\nxaña");
        assert_eq!(b.find_text("aña"), vec![1, 6]);
    }

    #[test]
    fn cursor_moves_over_crlf_as_unit() {
        let mut b = buf("ab\r\ncd");
        b.cursor_char_idx = 2;
        b.move_cursor_right(false);
        assert_eq!(b.cursor_char_idx, 4);
        b.move_cursor_left(false);
        assert_eq!(b.cursor_char_idx, 2);
    }

    #[test]
    fn vertical_move_keeps_visual_column() {
        let mut b = buf("abcdef\nab\nabcdef");
        b.cursor_char_idx = 5;
        b.target_visual_col = 5;
        b.move_cursor_down(false);
        assert_eq!(b.cursor_char_idx, 9);
        b.move_cursor_down(false);
        assert_eq!(b.cursor_char_idx, 15);
    }

    #[test]
    fn lsp_position_is_utf16() {
        let mut b = buf("😀x");
        b.cursor_char_idx = 1;
        assert_eq!(b.get_lsp_position(), (0, 2));
        assert_eq!(b.lsp_position_to_char_idx(0, 2), 1);
        assert_eq!(b.lsp_position_to_char_idx(0, 99), 2);
    }

    fn tmp_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("wyvern_ed_{}_{}", tag, std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn save_and_load_roundtrip() {
        let dir = tmp_dir("rt");
        let path = dir.join("rt.txt");
        buf("uno\ndos\n").save_to_file(&path).unwrap();
        assert_eq!(EditorBuffer::load_from_file(&path).unwrap().text.to_string(), "uno\ndos\n");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn non_utf8_is_rejected() {
        let dir = tmp_dir("bin");
        let path = dir.join("bin");
        fs::write(&path, [0xff, 0xfe, b'a']).unwrap();
        assert_eq!(EditorBuffer::load_from_file(&path).err().map(|e| e.kind()), Some(io::ErrorKind::InvalidData));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn save_is_atomic_keeps_permissions_and_symlink() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tmp_dir("atomic");
        let real = dir.join("real.sh");
        fs::write(&real, "old").unwrap();
        fs::set_permissions(&real, fs::Permissions::from_mode(0o755)).unwrap();
        let link = dir.join("link.sh");
        std::os::unix::fs::symlink(&real, &link).unwrap();

        buf("new").save_to_file(&link).unwrap();
        assert_eq!(fs::read_to_string(&real).unwrap(), "new");
        assert!(fs::symlink_metadata(&link).unwrap().file_type().is_symlink());
        assert_eq!(fs::metadata(&real).unwrap().permissions().mode() & 0o777, 0o755);
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 2, "no debe quedar el temporal");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn failed_save_leaves_no_file() {
        let dir = tmp_dir("fail");
        assert!(buf("x").save_to_file(dir.join("no_existe/a.txt")).is_err());
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 0);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn tabs_advance_to_next_stop() {
        let mut b = buf("\tx\n a\tb");
        assert_eq!(b.char_idx_to_visual_col(1), 4);
        assert_eq!(b.char_idx_to_visual_col(3 + 3), 4);
        assert_eq!(b.visual_col_to_char_idx(0, 4), 1);
        b.cursor_char_idx = 1;
        b.target_visual_col = 4;
        b.move_cursor_down(false);
        assert_eq!(b.cursor_char_idx, 6);
    }

    #[test]
    fn delete_forward_handles_crlf_and_undo() {
        let mut b = buf("ab\r\ncd");
        b.cursor_char_idx = 1;
        b.delete_forward();
        b.delete_forward();
        assert_eq!(b.text.to_string(), "acd");
        b.undo();
        assert_eq!(b.text.to_string(), "ab\r\ncd");
    }

    #[test]
    fn lsp_changes_use_pre_edit_coordinates() {
        let mut b = buf("ab\ncd");
        b.cursor_char_idx = 5;
        b.delete_backwards();
        b.cursor_char_idx = 0;
        b.insert_char('😀');
        assert_eq!(b.lsp_changes, vec![
            LspChange { start: (1, 1), end: (1, 2), text: String::new() },
            LspChange { start: (0, 0), end: (0, 0), text: "😀".into() },
        ]);
    }

    #[test]
    fn apply_edits_shifts_main_and_undoes_atomically() {
        let mut b = buf("use a;\nfoo.ba");
        b.cursor_char_idx = 13;
        let start = b.apply_edits(&[(11..13, "bar()".into()), (0..0, "use b;\n".into())], 0);
        assert_eq!(b.text.to_string(), "use b;\nuse a;\nfoo.bar()");
        assert_eq!(start, 18);
        assert_eq!(b.cursor_char_idx, 23);
        b.undo();
        assert_eq!(b.text.to_string(), "use a;\nfoo.ba");
    }

    #[test]
    fn completion_replace_len_heuristic() {
        let mut b = buf("x-fo");
        b.cursor_char_idx = 4;
        assert_eq!(b.completion_replace_len("foo"), 2);
        let mut b = buf("  background-co");
        b.cursor_char_idx = 15;
        assert_eq!(b.completion_replace_len("background-color"), 13);
        let mut b = buf("let hm");
        b.cursor_char_idx = 6;
        assert_eq!(b.completion_replace_len("HashMap"), 2, "sin coincidencia de prefijo se reemplaza el identificador");
    }

    #[test]
    fn edits_invalidate_syntax_cache_from_edited_line() {
        let ss = syntect::parsing::SyntaxSet::load_defaults_newlines();
        let ts = syntect::highlighting::ThemeSet::load_defaults();
        let hl = syntect::highlighting::Highlighter::new(&ts.themes["base16-ocean.dark"]);
        let state = (ParseState::new(ss.find_syntax_plain_text()), HighlightState::new(&hl, Default::default()));
        let mut b = buf("a\nb\nc\nd");
        b.syntax_cache = vec![state; 4];
        b.cursor_char_idx = 3;
        b.insert_char('\n');
        assert_eq!(b.syntax_cache.len(), 2);
    }
}
