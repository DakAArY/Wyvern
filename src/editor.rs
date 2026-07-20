use ropey::Rope;
use std::fs::{self, File};
use std::io::{self, BufWriter};
use std::path::Path;
use std::ops::Range;

/// Buffer de texto del editor. Usa una `Rope` (árbol de cuerdas) en vez de
/// un `String` plano para que insertar/borrar caracteres en cualquier punto
/// del documento sea eficiente incluso en archivos grandes.
///
/// El cursor y la selección se representan como índices de carácter (no de
/// byte ni de línea/columna) sobre la rope; las conversiones a línea/columna
/// se calculan cuando se necesitan (ver `get_lsp_position`, `char_to_line`, etc).
pub struct EditorBuffer {
    pub text: Rope,
    pub cursor_char_idx: usize,
    /// Extremo fijo de la selección activa. `None` si no hay selección.
    /// El otro extremo siempre es `cursor_char_idx`.
    pub selection_anchor: Option<usize>,
    pub scroll_x: usize,
    pub scroll_y: usize,
}

impl EditorBuffer {
    /// Crea un buffer vacío, sin ruta asociada.
    pub fn new() -> Self {
        Self { 
            text: Rope::new(),
            cursor_char_idx: 0,
            selection_anchor: None,
            scroll_x: 0,
            scroll_y: 0,
        }
    }

    /// Carga un archivo completo en memoria. Se asume codificación UTF-8;
    /// bytes inválidos se reemplazan según las reglas de `from_utf8_lossy`.
    pub fn load_from_file<P: AsRef<Path>>(path: P) -> io::Result<Self> {
        let bytes = fs::read(path)?;
        let text_str = String::from_utf8_lossy(&bytes);
        Ok(Self { 
            text: Rope::from_str(&text_str),
            cursor_char_idx: 0,
            selection_anchor: None,
            scroll_x: 0,
            scroll_y: 0
        })
    }

    /// Escribe el contenido íntegro del buffer al archivo indicado,
    /// sobrescribiéndolo si ya existe.
    pub fn save_to_file<P: AsRef<Path>>(&self, path: P) -> io::Result<()> {
        let file = File::create(path)?;
        self.text.write_to(BufWriter::new(file))?;
        Ok(())
    }

    /// Inserta un carácter en la posición del cursor y avanza el cursor una posición.
    pub fn insert_char(&mut self, ch: char) {
        self.text.insert_char(self.cursor_char_idx, ch);
        self.cursor_char_idx += 1;
    }

    /// Inserta una cadena completa en la posición del cursor y avanza el
    /// cursor según la cantidad de caracteres insertados (no bytes).
    pub fn insert_str(&mut self, s: &str) {
        self.text.insert(self.cursor_char_idx, s);
        self.cursor_char_idx += s.chars().count();
    }

    /// Devuelve el rango `[inicio, fin)` de la selección actual, normalizado
    /// para que `inicio <= fin` sin importar la dirección en que se arrastró
    /// el cursor. Devuelve `None` si no hay ancla o si la selección está vacía.
    pub fn get_selection_range(&self) -> Option<Range<usize>> {
        self.selection_anchor.map(|anchor| {
            if anchor < self.cursor_char_idx { anchor..self.cursor_char_idx } 
            else { self.cursor_char_idx..anchor }
        }).filter(|r| r.start != r.end)
    }

    /// Extrae el texto actualmente seleccionado, si lo hay.
    pub fn get_selected_text(&self) -> Option<String> {
        self.get_selection_range().map(|r| self.text.slice(r).to_string())
    }

    /// Elimina el texto seleccionado (si existe), mueve el cursor al inicio
    /// de donde estaba la selección y limpia el ancla. Devuelve el texto
    /// borrado, útil para operaciones de cortar (`Ctrl+X`).
    pub fn delete_selection(&mut self) -> Option<String> {
        if let Some(range) = self.get_selection_range() {
            let text = self.text.slice(range.clone()).to_string();
            self.text.remove(range.clone());
            self.cursor_char_idx = range.start;
            self.selection_anchor = None;
            Some(text)
        } else {
            None
        }
    }

    /// Backspace "inteligente": si hay selección la borra completa; si no,
    /// y el cursor está dentro de sangría compuesta solo por espacios,
    /// borra hasta el múltiplo de 4 anterior (como borrar un tab lógico).
    /// En cualquier otro caso, borra un solo carácter hacia atrás.
    pub fn delete_backwards(&mut self) {
        if self.delete_selection().is_some() { return; }
        if self.cursor_char_idx == 0 { return; }

        let current_line_idx = self.text.char_to_line(self.cursor_char_idx);
        let line_start_char_idx = self.text.line_to_char(current_line_idx);
        let col = self.cursor_char_idx - line_start_char_idx;

        if col > 0 {
            let rem = col % 4;
            let step = if rem == 0 { 4 } else { rem };

            if col >= step {
                let start_idx = self.cursor_char_idx - step;
                let mut all_spaces = true;
                
                for ch in self.text.slice(start_idx..self.cursor_char_idx).chars() {
                    if ch != ' ' { all_spaces = false; break; }
                }

                if all_spaces {
                    self.cursor_char_idx -= step;
                    self.text.remove(self.cursor_char_idx..(self.cursor_char_idx + step));
                    return;
                }
            }
        }
        self.cursor_char_idx -= 1;
        self.text.remove(self.cursor_char_idx..self.cursor_char_idx + 1);
    }
    
    /// Establece o limpia el ancla de selección según si el usuario está
    /// manteniendo Shift (`selecting`). Se llama al inicio de cada
    /// movimiento de cursor para decidir si ese movimiento extiende una
    /// selección o simplemente reposiciona el cursor.
    fn update_selection(&mut self, selecting: bool) {
        if selecting {
            if self.selection_anchor.is_none() {
                self.selection_anchor = Some(self.cursor_char_idx);
            }
        } else {
            self.selection_anchor = None;
        }
    }

    /// Longitud en caracteres de una línea sin contar su terminador
    /// (`\n` o `\r\n`), usada para no dejar el cursor "más allá" del
    /// contenido visible de la línea al moverse verticalmente.
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
        if self.cursor_char_idx > 0 { self.cursor_char_idx -= 1; }
    }

    pub fn move_cursor_right(&mut self, selecting: bool) {
        self.update_selection(selecting);
        if self.cursor_char_idx < self.text.len_chars() { self.cursor_char_idx += 1; }
    }

    /// Mueve el cursor una línea hacia arriba, intentando preservar la
    /// columna actual (recortada a la longitud de la línea destino).
    pub fn move_cursor_up(&mut self, selecting: bool) {
        self.update_selection(selecting);
        let current_line = self.text.char_to_line(self.cursor_char_idx);
        if current_line > 0 {
            let current_col = self.cursor_char_idx - self.text.line_to_char(current_line);
            let target_line = current_line - 1;
            let target_col = current_col.min(self.line_len_without_nl(target_line));
            self.cursor_char_idx = self.text.line_to_char(target_line) + target_col;
        }
    }

    /// Análogo a `move_cursor_up` pero una línea hacia abajo.
    pub fn move_cursor_down(&mut self, selecting: bool) {
        self.update_selection(selecting);
        let current_line = self.text.char_to_line(self.cursor_char_idx);
        if current_line + 1 < self.text.len_lines() {
            let current_col = self.cursor_char_idx - self.text.line_to_char(current_line);
            let target_line = current_line + 1;
            let target_col = current_col.min(self.line_len_without_nl(target_line));
            self.cursor_char_idx = self.text.line_to_char(target_line) + target_col;
        }
    }

    /// Ajusta `scroll_x`/`scroll_y` al mínimo necesario para que el cursor
    /// quede dentro del área visible de `view_width` x `view_height`.
    pub fn ensure_cursor_visible(&mut self, view_width: usize, view_height: usize) {
        let cursor_y = self.text.char_to_line(self.cursor_char_idx);
        let cursor_x = self.cursor_char_idx - self.text.line_to_char(cursor_y);

        if cursor_y < self.scroll_y { self.scroll_y = cursor_y; } 
        else if cursor_y >= self.scroll_y + view_height { self.scroll_y = cursor_y.saturating_sub(view_height - 1); }

        if cursor_x < self.scroll_x { self.scroll_x = cursor_x; } 
        else if cursor_x >= self.scroll_x + view_width { self.scroll_x = cursor_x.saturating_sub(view_width - 1); }
    }

    /// Traduce una coordenada de pantalla (columna/fila del terminal) a una
    /// posición de cursor dentro del buffer, usada para clicks y arrastre
    /// de mouse. `editor_start_x/y` es la esquina del área de edición y
    /// `gutter_width` el ancho ocupado por el margen de números de línea.
    /// Los clics fuera del área de texto (en el gutter o por encima del
    /// editor) se ignoran.
    pub fn set_cursor_from_screen(&mut self, screen_x: u16, screen_y: u16, editor_start_x: u16, editor_start_y: u16, gutter_width: u16, selecting: bool) {
        if screen_x < editor_start_x + gutter_width || screen_y < editor_start_y { return; }
        
        self.update_selection(selecting);
        
        let rel_x = (screen_x - (editor_start_x + gutter_width)) as usize;
        let rel_y = (screen_y - editor_start_y) as usize;
        
        let target_line = (self.scroll_y + rel_y).min(self.text.len_lines().saturating_sub(1));
        let line_len = self.line_len_without_nl(target_line);
        let target_col = (self.scroll_x + rel_x).min(line_len);
        
        self.cursor_char_idx = self.text.line_to_char(target_line) + target_col;
    }

    /// Posición del cursor en formato (línea, columna) 0-based, tal como lo
    /// requiere el protocolo LSP.
    pub fn get_lsp_position(&self) -> (u32, u32) {
        let line = self.text.char_to_line(self.cursor_char_idx);
        let col = self.cursor_char_idx - self.text.line_to_char(line);
        (line as u32, col as u32)
    }

    /// Devuelve una copia del contenido completo del buffer como `String`,
    /// usada para enviar el documento entero al LSP.
    pub fn get_full_text(&self) -> String { self.text.to_string() }

    /// Extrae el "prefijo de palabra" inmediatamente a la izquierda del
    /// cursor (caracteres alfanuméricos o `_`), usado para filtrar
    /// sugerencias de autocompletado mientras se escribe.
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

    /// Devuelve la sangría (espacios/tabs iniciales) de la línea donde está
    /// el cursor, usada para replicarla al insertar un salto de línea nuevo.
    pub fn get_current_line_indentation(&self) -> String {
        let current_line_idx = self.text.char_to_line(self.cursor_char_idx);
        let line = self.text.line(current_line_idx);
        let mut indent = String::new();
        for ch in line.chars() {
            if ch == ' ' || ch == '\t' { indent.push(ch); } else { break; }
        }
        indent
    }

    /// Carácter justo debajo/después del cursor, si existe. Se usa para
    /// decidir si "saltar" un carácter de cierre ya existente en vez de
    /// insertar uno nuevo al autocerrar paréntesis/comillas.
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
    
    /// Mueve el cursor `lines` líneas hacia arriba (recortado al inicio del
    /// documento), preservando la columna como en `move_cursor_up`.
    pub fn move_page_up(&mut self, selecting: bool, lines: usize) {
        self.update_selection(selecting);
        let current_line = self.text.char_to_line(self.cursor_char_idx);
        let current_col = self.cursor_char_idx - self.text.line_to_char(current_line);
        let target_line = current_line.saturating_sub(lines);
        let target_col = current_col.min(self.line_len_without_nl(target_line));
        self.cursor_char_idx = self.text.line_to_char(target_line) + target_col;
    }
    
    /// Análogo a `move_page_up` pero hacia abajo, recortado al final del documento.
    pub fn move_page_down(&mut self, selecting: bool, lines: usize) {
        self.update_selection(selecting);
        let current_line = self.text.char_to_line(self.cursor_char_idx);
        let current_col = self.cursor_char_idx - self.text.line_to_char(current_line);
        let target_line = (current_line + lines).min(self.text.len_lines().saturating_sub(1));
        let target_col = current_col.min(self.line_len_without_nl(target_line));
        self.cursor_char_idx = self.text.line_to_char(target_line) + target_col;   
    }
    
    /// Desplaza el viewport hacia arriba sin mover el cursor, salvo que éste
    /// quede fuera de la vista (ver `enforce_cursor_in_viewport`).
    pub fn scroll_viewport_up(&mut self, lines: usize, view_height: usize, selecting: bool) {
        self.scroll_y = self.scroll_y.saturating_sub(lines);
        self.enforce_cursor_in_viewport(view_height, selecting);
    }
    
    /// Análogo a `scroll_viewport_up` pero hacia abajo.
    pub fn scroll_viewport_down(&mut self, lines: usize, view_height: usize, selecting: bool) {
        let max_scroll = self.text.len_lines().saturating_sub(1);
        self.scroll_y = (self.scroll_y + lines).min(max_scroll);
        self.enforce_cursor_in_viewport(view_height, selecting);
    }
    
    /// Tras un scroll manual (rueda del mouse, Ctrl+U/D, etc.), reubica el
    /// cursor dentro del viewport si quedó fuera de rango, para que la
    /// posición de edición nunca esté oculta.
    fn enforce_cursor_in_viewport(&mut self, view_height: usize, selecting: bool) {
        let current_line = self.text.char_to_line(self.cursor_char_idx);
        let current_col = self.cursor_char_idx - self.text.line_to_char(current_line);
        
        let target_line = if current_line < self.scroll_y {
            self.scroll_y
        } else if view_height > 0 && current_line >= self.scroll_y + view_height {
            (self.scroll_y + view_height).saturating_sub(1)
        } else {
            return;
        };
        
        self.update_selection(selecting);
        let target_col = current_col.min(self.line_len_without_nl(target_line));
        self.cursor_char_idx = self.text.line_to_char(target_line) + target_col;
    }
}
