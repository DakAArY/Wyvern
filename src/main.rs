mod app;
mod editor;
mod ui;
mod explorer;
mod lsp;
mod git;

use app::{App, AppState};
use crossterm::{
    event::{self, Event, KeyCode, KeyEvent, KeyModifiers, MouseEvent, MouseEventKind, MouseButton, EnableMouseCapture, DisableMouseCapture},
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
    ExecutableCommand,
};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use std::{io::{self, stdout}, time::{Duration, Instant}};

/// Punto de entrada: prepara la terminal en modo alternativo con captura de
/// mouse y raw mode, corre el bucle principal, y garantiza que la terminal
/// quede restaurada a su estado normal al salir (incluso si `run_app` falla).
fn main() -> io::Result<()> {
    enable_raw_mode()?;
    stdout().execute(EnterAlternateScreen)?.execute(EnableMouseCapture)?;
    
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout()))?;
    let mut app = App::new();

    let res = run_app(&mut terminal, &mut app);

    disable_raw_mode()?;
    stdout().execute(LeaveAlternateScreen)?.execute(DisableMouseCapture)?;
    
    res
}

/// Bucle principal de la aplicación: dibuja un frame, espera hasta 16ms por
/// un evento de entrada (teclado o mouse) y lo despacha al manejador
/// correspondiente, procesa mensajes pendientes del LSP, y repite hasta que
/// se solicite salir. Los eventos de teclado se enrutan al manejador del
/// prompt modal si hay uno activo; en caso contrario van al manejador normal.
/// Los eventos de mouse se ignoran mientras el prompt o la ayuda están abiertos.
fn run_app(terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>, app: &mut App) -> io::Result<()> {
    loop {
        let term_area = terminal.size()?;
        terminal.draw(|f| ui::render(f, app))?;
        
        if event::poll(Duration::from_millis(16))? {
            match event::read()? {
                Event::Key(key) => {
                    if app.prompt.is_some() {
                        handle_prompt_key(app, key);
                    } else {
                        handle_normal_key(app, key, term_area.height);
                    }
                }
                Event::Mouse(mouse_event) => {
                    if app.prompt.is_none() && !app.show_help {
                        handle_mouse_event(app, mouse_event, term_area.width, term_area.height);
                    }
                }
                _ => {}
            }
        }

        process_lsp_messages(app);

        if app.quit { break; }
    }
    Ok(())
}

/// Despacha un evento de mouse según su tipo: click simple/doble (navegar
/// árbol o posicionar cursor), arrastre (extender selección), y scroll
/// (desplazar viewport o mover selección en el árbol). Las dimensiones del
/// árbol y del área de texto se recalculan aquí porque deben coincidir
/// exactamente con el layout que arma `ui.rs`.
fn handle_mouse_event(app: &mut App, event: MouseEvent, term_width: u16, term_height: u16) {
    let x = event.column;
    let y = event.row;
    
    let tree_width = if app.show_tree { (term_width as f32 * 0.20) as u16 } else { 0 };
    let view_height = term_height.saturating_sub(2) as usize;
    
    match event.kind {
        MouseEventKind::Down(MouseButton::Left) => {
            let now = Instant::now();
            let is_double_click = if let Some((last_time, last_x, last_y)) = app.last_click {
                now.duration_since(last_time) < Duration::from_millis(500) && last_x == x && last_y == y
            } else { false };
            
            app.last_click = Some((now, x, y));

            if app.show_tree && x < tree_width {
                if y >= 1 && y < term_height.saturating_sub(1) { // fila 0 es el borde superior, la última fila es la barra de estado
                    let click_idx = (y - 1) as usize;
                    let list_offset = app.explorer.state.offset();
                    app.explorer.state.select(Some(list_offset + click_idx));
                    
                    if is_double_click { handle_enter(app); }
                }
            } else if app.state == AppState::Editing {
                let max_lines = app.buffer.text.len_lines();
                let gutter_num_width = app.buffer.text.len_lines().to_string().len().max(1) as u16;
                let gutter_total_width = gutter_num_width + 3;
                
                app.buffer.set_cursor_from_screen(x, y, tree_width + 1, 1, gutter_total_width, false);
            }
        }
        MouseEventKind::Drag(MouseButton::Left) => {
            if app.state == AppState::Editing && x >= tree_width {
                let max_lines = app.buffer.text.len_lines();
                let gutter_num_width = app.buffer.text.len_lines().to_string().len().max(1) as u16;
                let gutter_total_width = gutter_num_width + 3;
                app.buffer.set_cursor_from_screen(x, y, tree_width + 1, 1, gutter_total_width, true);
            }
        }
        MouseEventKind::ScrollUp => {
            if app.state == AppState::Exploring { for _ in 0..3 { app.explorer.previous(); } }
            else { app.buffer.scroll_viewport_up(3, view_height, false) }
        }
        MouseEventKind::ScrollDown => {
            if app.state == AppState::Exploring { for _ in 0..3 { app.explorer.next(); } }
            else { app.buffer.scroll_viewport_down(3, view_height, false); }
        }
        MouseEventKind::ScrollLeft => for _ in 0..3 { app.buffer.move_cursor_left(false); },         
        MouseEventKind::ScrollRight => for _ in 0..3 { app.buffer.move_cursor_right(false); },
        _ => {}
    }
}

/// Manejador de teclado mientras hay un prompt modal activo: Esc cancela,
/// Enter confirma y ejecuta la acción pendiente, y el resto de teclas editan
/// el campo de texto del prompt.
fn handle_prompt_key(app: &mut App, key: KeyEvent) {
    match key.code {
        KeyCode::Esc => app.prompt = None,
        KeyCode::Enter => app.execute_prompt(),
        KeyCode::Char(c) => { if let Some(p) = &mut app.prompt { p.input.push(c); } }
        KeyCode::Backspace => { if let Some(p) = &mut app.prompt { let _ = p.input.pop(); } }
        _ => {}
    }
}

/// Manejador de teclado principal (sin prompt activo): atajos globales
/// (ayuda, portapapeles, guardar, salir, alternar árbol) y navegación del
/// cursor/selección, delegando en funciones específicas para Enter,
/// caracteres normales, Backspace y Tab.
fn handle_normal_key(app: &mut App, key: KeyEvent, term_height: u16) {
    let selecting = key.modifiers.contains(KeyModifiers::SHIFT);
    let view_height = term_height.saturating_sub(2) as usize;

    match key.code {
        KeyCode::F(1) => app.show_help = !app.show_help,
        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            if let Some(text) = app.buffer.get_selected_text() { app.clipboard = Some(text); }
        }
        KeyCode::Char('x') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            if let Some(text) = app.buffer.delete_selection() {
                app.clipboard = Some(text);
                notify_lsp_change(app);
            }
        }
        KeyCode::Char('v') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            app.buffer.delete_selection();
            if let Some(text) = &app.clipboard {
                app.buffer.insert_str(text);
                notify_lsp_change(app);
            }
        }
        KeyCode::Char('q') if key.modifiers.contains(KeyModifiers::CONTROL) => app.quit = true,
        KeyCode::Char('s') if key.modifiers.contains(KeyModifiers::CONTROL) => app.trigger_save(),
        KeyCode::F(2) => app.toggle_tree(),
        KeyCode::Esc => handle_escape(app),
        KeyCode::Enter => handle_enter(app),
        KeyCode::Char(c) => handle_char(app, c),
        KeyCode::Backspace => handle_backspace(app),
        KeyCode::Tab => handle_tab(app),
        KeyCode::Home => app.buffer.move_to_start_of_line(selecting),        
        KeyCode::End => app.buffer.move_to_end_of_line(selecting),        
        KeyCode::PageUp => app.buffer.move_page_up(selecting, 20),
        KeyCode::PageDown => app.buffer.move_page_down(selecting, 20),
        KeyCode::Up if key.modifiers.contains(KeyModifiers::CONTROL) => app.buffer.scroll_viewport_up(1, view_height, selecting),
        KeyCode::Up => handle_up(app, selecting),
        KeyCode::Down => handle_down(app, selecting),
        KeyCode::Down if key.modifiers.contains(KeyModifiers::CONTROL) => app.buffer.scroll_viewport_down(1, view_height, selecting),
        KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => app.buffer.scroll_viewport_up(view_height / 2, view_height, selecting),
        KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => app.buffer.scroll_viewport_down(view_height / 2, view_height, selecting),
        KeyCode::Left => app.buffer.move_cursor_left(selecting),
        KeyCode::Right => app.buffer.move_cursor_right(selecting),
        KeyCode::Delete => {
            if app.state == AppState::Exploring {
                app.trigger_delete();
            } else if app.state == AppState::Editing {
                if app.buffer.delete_selection().is_some() {
                    notify_lsp_change(app);
                }
            }
        }
        _ => {}
    }
}

/// Comportamiento de Esc, en orden de prioridad: cerrar la ayuda si está
/// abierta; si no, cerrar el árbol si tiene el foco; si no, descartar el
/// popup de autocompletado; y en último caso, limpiar la selección de texto.
fn handle_escape(app: &mut App) {
    if app.show_help {
        app.show_help = false;
    } else if app.state == AppState::Exploring && app.show_tree {
        app.toggle_tree();
    } else if !app.completions.is_empty() {
        app.completions.clear();
    } else {
        app.buffer.selection_anchor = None;
    }
}

/// Comportamiento de Enter, distinto según el contexto: en el árbol, entra
/// al directorio o abre el archivo seleccionado; con el popup de
/// autocompletado abierto, acepta la sugerencia resaltada; y en el editor,
/// inserta un salto de línea replicando la sangría de la línea actual.
fn handle_enter(app: &mut App) {
    app.status_msg = None;
    
    if app.state == AppState::Exploring {
        if let Some(entry) = app.explorer.get_selected() {
            let path = entry.path.clone();
            if entry.is_dir {
                app.explorer.current_dir = path;
                let _ = app.explorer.reload();
            } else {
                app.load_file(path);
            }
        }
    } else if !app.completions.is_empty() {
        if let Some(idx) = app.completion_state.selected() {
            if let Some(comp) = app.completions.get(idx).cloned() {
                let prefix_len = app.buffer.get_current_word_prefix().len();
                for _ in 0..prefix_len { app.buffer.delete_backwards(); }
                app.buffer.insert_str(&comp.label);
                notify_lsp_change(app);
            }
        }
        app.completions.clear();
    } else if app.state == AppState::Editing {
        app.buffer.delete_selection();
        let indent = app.buffer.get_current_line_indentation();
        app.buffer.insert_char('\n');
        app.buffer.insert_str(&indent);
        notify_lsp_change(app);
        app.completions.clear();
    }
}

/// Maneja la entrada de un carácter normal. En el árbol, actúa como atajo
/// (nuevo archivo, renombrar, eliminar). En el editor: sobrescribe la
/// selección si la había, salta sobre un carácter de cierre existente en
/// vez de duplicarlo, autocierra paréntesis/comillas al abrirlos, notifica
/// el cambio al LSP y dispara autocompletado si el carácter es parte de un
/// identificador o un separador de miembro (`.`, `:`).
fn handle_char(app: &mut App, c: char) {
    app.status_msg = None;
    
    if app.state == AppState::Exploring {
        match c {
            'n' => app.new_blank_file(),
            'r' => app.trigger_rename(),
            'd' => app.trigger_delete(),
            _ => {}
        }
        return;
    } 
    
    if app.state == AppState::Intro { app.state = AppState::Editing; }
    
    if app.state == AppState::Editing {
        app.buffer.delete_selection();

        let is_close_bracket = c == ')' || c == '}' || c == ']' || c == '"' || c == '\'';
        
        if is_close_bracket && app.buffer.char_at_cursor() == Some(c) {
            app.buffer.move_cursor_right(false);
        } else {
            app.buffer.insert_char(c);
            let closing = match c { '(' => Some(')'), '{' => Some('}'), '[' => Some(']'), '"' => Some('"'), '\'' => Some('\''), _ => None };
            if let Some(close_char) = closing {
                app.buffer.insert_char(close_char);
                app.buffer.move_cursor_left(false);
            }
        }

        notify_lsp_change(app);
    
        if c.is_alphanumeric() || c == '.' || c == ':' { app.trigger_completion(); } 
        else { app.completions.clear(); }
    }
}

/// Backspace en el editor: delega el borrado (incluida la detección de
/// selección) al buffer, notifica el cambio al LSP y descarta el popup de
/// autocompletado activo.
fn handle_backspace(app: &mut App) {
    app.status_msg = None;
    if let AppState::Editing = app.state {
        app.buffer.delete_backwards();
        notify_lsp_change(app);
        app.completions.clear();
    }
}

/// Tab en el editor: si el popup de autocompletado está abierto, avanza la
/// selección a la siguiente sugerencia (como flecha abajo); si no, inserta
/// una sangría de 4 espacios sobrescribiendo la selección si la había.
fn handle_tab(app: &mut App) {
    app.status_msg = None;
    if let AppState::Editing = app.state {
        if !app.completions.is_empty() {
            let i = match app.completion_state.selected() {
                Some(i) => if i >= app.completions.len().saturating_sub(1) { 0 } else { i + 1 },
                None => 0,
            };
            app.completion_state.select(Some(i));
        } else {
            app.buffer.delete_selection();
            app.buffer.insert_str("    ");
        }
    }
}

/// Flecha arriba: navega el árbol, o el popup de autocompletado (con
/// envoltura circular), o mueve el cursor una línea hacia arriba en el editor.
fn handle_up(app: &mut App, selecting: bool) {
    if app.state == AppState::Exploring { app.explorer.previous(); } 
    else if !app.completions.is_empty() {
        let i = match app.completion_state.selected() {
            Some(i) => if i == 0 { app.completions.len().saturating_sub(1) } else { i - 1 },
            None => 0,
        };
        app.completion_state.select(Some(i));
    } else { app.buffer.move_cursor_up(selecting); }
}

/// Análogo a `handle_up` pero hacia abajo.
fn handle_down(app: &mut App, selecting: bool) {
    if app.state == AppState::Exploring { app.explorer.next(); } 
    else if !app.completions.is_empty() {
        let i = match app.completion_state.selected() {
            Some(i) => if i >= app.completions.len().saturating_sub(1) { 0 } else { i + 1 },
            None => 0,
        };
        app.completion_state.select(Some(i));
    } else { app.buffer.move_cursor_down(selecting); }
}

/// Marca el buffer como sucio y, si hay una sesión LSP inicializada, le
/// notifica el cambio incrementando la versión del documento (ver
/// `LspClient::did_change`). No hace nada si no hay cliente LSP activo.
fn notify_lsp_change(app: &mut App) {
    app.is_dirty = true;

    if let (Some(client), Some(uri)) = (&mut app.lsp_client, &app.current_uri) {
        if client.is_initialized {
            app.document_version += 1;
            client.did_change(uri.clone(), app.buffer.get_full_text(), app.document_version);
        }
    }
}

/// Drena todos los mensajes pendientes en el canal del LSP sin bloquear.
/// Maneja tres casos: diagnósticos (reemplazan por completo los anteriores,
/// indexados por línea), respuestas (ya sea el handshake `initialize` ->
/// `initialized` + `didOpen`, o el resultado de una petición de
/// autocompletado, filtrado y ordenado antes de mostrarse), y errores (que
/// terminan la sesión LSP actual). Si llega un error, el cliente LSP se
/// descarta por completo tras procesar el resto del lote.
fn process_lsp_messages(app: &mut App) {
    let mut lsp_crashed = false;
    let mut error_msg = None;

    if let Some(lsp) = &mut app.lsp_client {
        while let Ok(msg) = lsp.receiver.try_recv() {
            match msg {
                crate::lsp::LspMessage::Diagnostics(params) => {
                    app.diagnostics.clear();
                    for diag in params.diagnostics {
                        let line = diag.range.start.line as usize;
                        app.diagnostics.entry(line).or_default().push(diag);
                    }
                }
                crate::lsp::LspMessage::Response { id, result } => {
                    if id == lsp.init_id && !lsp.is_initialized {
                        lsp.send_initialized();
                        lsp.is_initialized = true;
                        
                        if let (Some(uri), Some(path)) = (&app.current_uri, &app.current_filepath) {
                            let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
                            let lang_id = match ext { "rs" => "rust", "py" => "python", _ => ext };
                            lsp.did_open(uri.clone(), app.buffer.get_full_text(), app.document_version, lang_id);
                            app.status_msg = Some(format!("LSP Listo ({})", ext));
                        }
                    } 
                    else if Some(id) == app.pending_completion_id {
                        if let Ok(response) = serde_json::from_value::<lsp_types::CompletionResponse>(result) {
                            let mut items = match response {
                                lsp_types::CompletionResponse::Array(arr) => arr,
                                lsp_types::CompletionResponse::List(list) => list.items,
                            };
                            items.sort_by(|a, b| {
                                let a_sort = a.sort_text.as_ref().unwrap_or(&a.label);
                                let b_sort = b.sort_text.as_ref().unwrap_or(&b.label);
                                a_sort.cmp(b_sort)
                            });

                            let prefix = app.buffer.get_current_word_prefix().to_lowercase();
                            app.completions = items.into_iter()
                                .filter(|i| i.label.to_lowercase().starts_with(&prefix))
                                .map(|i| crate::app::CompletionOption {
                                    label: i.label,
                                    kind: i.kind,
                                })
                                .collect();

                            if !app.completions.is_empty() {
                                app.completion_state.select(Some(0));
                            }
                        }
                        app.pending_completion_id = None;
                    }
                }
                crate::lsp::LspMessage::Error(err) => {
                    error_msg = Some(format!("Error LSP: {}", err));
                    lsp_crashed = true;
                    break;
                }
                _ => {}
            }
        }
    }    

    if lsp_crashed {
        app.status_msg = error_msg;
        app.lsp_client = None; 
    }
}
