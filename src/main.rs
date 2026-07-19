mod app;
mod editor;
mod ui;
mod explorer;
mod lsp;

use app::App;
use crossterm::{
    event::{self, Event, KeyCode, KeyModifiers},
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
    ExecutableCommand,
};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use std::{io::{self, stdout}, time::Duration};

fn main() -> io::Result<()> {
    enable_raw_mode()?;
    stdout().execute(EnterAlternateScreen)?;
    
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout()))?;
    let mut app = App::new();

    // El editor arranca siempre en modo intro; la carga de un archivo
    // específico se deja a la interacción del usuario.
    
    let res = run_app(&mut terminal, &mut app);

    disable_raw_mode()?;
    stdout().execute(LeaveAlternateScreen)?;
    
    res
}

fn run_app(terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>, app: &mut App) -> io::Result<()> {
    loop {
        terminal.draw(|f| ui::render(f, app))?;
        if event::poll(Duration::from_millis(16))? {
            if let Event::Key(key) = event::read()? {
                match key.code {
                    KeyCode::Char('q') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        app.quit = true;
                    }
                    KeyCode::Char('s') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        app.save_file();
                    }
                    KeyCode::F(2) => app.toggle_tree(),
                    KeyCode::Esc => {
                        if app.state == app::AppState::Exploring && app.show_tree {
                            app.toggle_tree();
                        } else if !app.completions.is_empty() {
                            app.completions.clear();
                        }
                    }
                    KeyCode::Enter => {
                        app.status_msg = None;
                        if app.state == app::AppState::Exploring {
                            if let Some(entry) = app.explorer.get_selected() {
                                let path = entry.path.clone();
                                if entry.is_dir {
                                    app.explorer.current_dir = path;
                                    let _ = app.explorer.reload();
                                } else {
                                    app.load_file(path);
                                }
                            }
                        } else if app.state == app::AppState::Editing {
                            app.buffer.insert_char('\n');

                            // Se notifica el cambio al servidor LSP tras insertar el salto de línea.
                            if let (Some(client), Some(uri)) = (&mut app.lsp_client, &app.current_uri) {
                                if client.is_initialized {
                                    app.document_version += 1;
                                    client.did_change(uri.clone(), app.buffer.get_full_text(), app.document_version);
                                    // El popup de autocompletado no tiene sentido tras un salto de línea.
                                    app.completions.clear();
                                }
                            }
                        }
                    }
                    KeyCode::Char(c) => {
                        app.status_msg = None;
                        if app.state == app::AppState::Intro {
                            app.state = app::AppState::Editing;
                        }
                        if let app::AppState::Editing = app.state {
                            app.buffer.insert_char(c);
                        
                            // Se notifica el cambio al servidor LSP y se dispara autocompletado.
                            if let (Some(client), Some(uri)) = (&mut app.lsp_client, &app.current_uri) {
                                // Solo se envían mensajes una vez completado el handshake de inicialización.
                                if client.is_initialized {
                                    app.document_version += 1;
                                    client.did_change(uri.clone(), app.buffer.get_full_text(), app.document_version);
                                
                                    if c.is_alphanumeric() || c == '.' || c == ':' {
                                        app.trigger_completion();
                                    } else {
                                        app.completions.clear();
                                    }
                                }
                            }
                        }
                    }
                    KeyCode::Backspace => {
                        app.status_msg = None;
                        if let app::AppState::Editing = app.state {
                            app.buffer.delete_backwards();

                            // Se notifica el cambio al servidor LSP tras el borrado.
                            if let (Some(client), Some(uri)) = (&mut app.lsp_client, &app.current_uri) {
                                if client.is_initialized {
                                    app.document_version += 1;
                                    client.did_change(uri.clone(), app.buffer.get_full_text(), app.document_version);
                                    // Borrar invalida las sugerencias mostradas.
                                    app.completions.clear();
                                }
                            }
                        }
                    }
                    KeyCode::Up => {
                        if app.state == app::AppState::Exploring {
                            app.explorer.previous();
                        } else if !app.completions.is_empty() {
                            // Con el popup de autocompletado abierto, las flechas navegan
                            // la lista de sugerencias en vez de mover el cursor del buffer.
                            let i = match app.completion_state.selected() {
                                Some(i) => if i == 0 { app.completions.len().saturating_sub(1) } else { i - 1 },
                                None => 0,
                            };
                            app.completion_state.select(Some(i));
                        } else {
                            app.buffer.move_cursor_up();
                        }
                    }
                    KeyCode::Down => {
                        if app.state == app::AppState::Exploring {
                            app.explorer.next();
                        } else if !app.completions.is_empty() {
                            // Igual que con la flecha arriba, se navega el popup en lugar
                            // de mover el cursor mientras haya sugerencias visibles.
                            let i = match app.completion_state.selected() {
                                Some(i) => if i >= app.completions.len().saturating_sub(1) { 0 } else { i + 1 },
                                None => 0,
                            };
                            app.completion_state.select(Some(i));
                        } else {
                            app.buffer.move_cursor_down();
                        }
                    }
                    KeyCode::Left => app.buffer.move_cursor_left(),
                    KeyCode::Right => app.buffer.move_cursor_right(),
                    KeyCode::Tab => {
                        app.status_msg = None;
                        if let app::AppState::Editing = app.state {
                            if !app.completions.is_empty() {
                                // Tab acepta la sugerencia seleccionada del popup.
                                if let Some(idx) = app.completion_state.selected() {
                                    if let Some(comp) = app.completions.get(idx).cloned() {
                                        let prefix_len = app.buffer.get_current_word_prefix().len();
                                        
                                        // Se elimina el prefijo que el usuario ya había escrito...
                                        for _ in 0..prefix_len {
                                            app.buffer.delete_backwards();
                                        }
                                    
                                        // ...y se inserta en su lugar la sugerencia completa.
                                        for ch in comp.label.chars() {
                                            app.buffer.insert_char(ch);
                                        }

                                        // El buffer resultante se envía al servidor LSP para
                                        // mantener sincronizado el estado del documento.
                                        if let (Some(client), Some(uri)) = (&mut app.lsp_client, &app.current_uri) {
                                            if client.is_initialized {
                                                app.document_version += 1;
                                                client.did_change(uri.clone(), app.buffer.get_full_text(), app.document_version);
                                            }
                                        }
                                    }
                                }
                                app.completions.clear();
                            } else {
                                // Sin sugerencias activas, Tab inserta 4 espacios en vez de
                                // un carácter de tabulación literal.
                                for _ in 0..4 { app.buffer.insert_char(' '); }
                            }
                        }
                    }
                    _ => {}
                }
            }
        }

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
                        // Respuesta a la solicitud "initialize": completa el handshake.
                        if id == lsp.init_id && !lsp.is_initialized {
                            lsp.send_initialized();
                            lsp.is_initialized = true;
                            
                            // Con el servidor listo, se envía el archivo actual con `didOpen`.
                            if let (Some(uri), Some(path)) = (&app.current_uri, &app.current_filepath) {
                                let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
                                let lang_id = match ext { "rs" => "rust", "py" => "python", _ => ext };
                                lsp.did_open(uri.clone(), app.buffer.get_full_text(), app.document_version, lang_id);
                                app.status_msg = Some(format!("LSP Listo ({})", ext));
                            }
                        } 
                        // Respuesta a una solicitud de autocompletado pendiente.
                        else if Some(id) == app.pending_completion_id {
                            if let Ok(response) = serde_json::from_value::<lsp_types::CompletionResponse>(result) {
                                let mut items = match response {
                                    lsp_types::CompletionResponse::Array(arr) => arr,
                                    lsp_types::CompletionResponse::List(list) => list.items,
                                };
                                
                                // Se respeta el orden sugerido por el servidor (sort_text),
                                // usando la etiqueta como respaldo si no está presente.
                                items.sort_by(|a, b| {
                                    let a_sort = a.sort_text.as_ref().unwrap_or(&a.label);
                                    let b_sort = b.sort_text.as_ref().unwrap_or(&b.label);
                                    a_sort.cmp(b_sort)
                                });

                                // El prefijo actual se usa para filtrar los resultados localmente.
                                let prefix = app.buffer.get_current_word_prefix().to_lowercase();
                                
                                // Se descartan los ítems que no coinciden con el prefijo y se
                                // convierten al tipo interno usado por la UI.
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

        // El cliente LSP se descarta fuera del alcance del préstamo mutable anterior,
        // ya que `app.lsp_client` no puede modificarse mientras `lsp` sigue prestado.
        if lsp_crashed {
            app.status_msg = error_msg;
            app.lsp_client = None; 
        }

        if app.quit {
            break;
        }
    }
    Ok(())
}
