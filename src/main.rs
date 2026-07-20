mod app;
mod editor;
mod ui;
mod explorer;
mod lsp;
mod git;

use app::{App, AppState};
use crossterm::{
    event::{self, Event, KeyCode, KeyEvent, KeyModifiers},
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
                // El prompt tiene prioridad absoluta sobre la entrada de usuario
                if app.prompt.is_some() {
                    handle_prompt_key(app, key);
                } else {
                    handle_normal_key(app, key);
                }
            }
        }

        process_lsp_messages(app);

        if app.quit {
            break;
        }
    }
    Ok(())
}

// --- Manejadores de Eventos (Event Handlers) ---

fn handle_prompt_key(app: &mut App, key: KeyEvent) {
    match key.code {
        KeyCode::Esc => app.prompt = None,
        KeyCode::Enter => app.execute_prompt(),
        KeyCode::Char(c) => {
            if let Some(p) = &mut app.prompt {
                p.input.push(c);
            }
        }
        KeyCode::Backspace => {
            if let Some(p) = &mut app.prompt {
                let _ = p.input.pop();
            }
        }
        _ => {}
    }
}

fn handle_normal_key(app: &mut App, key: KeyEvent) {
    match key.code {
        KeyCode::Char('q') if key.modifiers.contains(KeyModifiers::CONTROL) => app.quit = true,
        KeyCode::Char('s') if key.modifiers.contains(KeyModifiers::CONTROL) => app.trigger_save(),
        KeyCode::F(2) => app.toggle_tree(),
        KeyCode::Esc => handle_escape(app),
        KeyCode::Enter => handle_enter(app),
        KeyCode::Char(c) => handle_char(app, c),
        KeyCode::Backspace => handle_backspace(app),
        KeyCode::Tab => handle_tab(app),
        KeyCode::Up => handle_up(app),
        KeyCode::Down => handle_down(app),
        KeyCode::Left => app.buffer.move_cursor_left(),
        KeyCode::Right => app.buffer.move_cursor_right(),
        KeyCode::Delete => {
            if app.state == AppState::Exploring {
                app.trigger_delete();
            }
        }
        _ => {}
    }
}

fn handle_escape(app: &mut App) {
    if app.state == AppState::Exploring && app.show_tree {
        app.toggle_tree();
    } else if !app.completions.is_empty() {
        app.completions.clear();
    }
}

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
        // Aceptación de autocompletado con Enter
        if let Some(idx) = app.completion_state.selected() {
            if let Some(comp) = app.completions.get(idx).cloned() {
                let prefix_len = app.buffer.get_current_word_prefix().len();
                for _ in 0..prefix_len { app.buffer.delete_backwards(); }
                for ch in comp.label.chars() { app.buffer.insert_char(ch); }
                notify_lsp_change(app);
            }
        }
        app.completions.clear();
    } else if app.state == AppState::Editing {
        // Auto-indentación al dar salto de línea
        let indent = app.buffer.get_current_line_indentation();
        app.buffer.insert_char('\n');
        
        for ch in indent.chars() {
            app.buffer.insert_char(ch);
        }

        notify_lsp_change(app);
        app.completions.clear();
    }
}

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
    
    if app.state == AppState::Intro {
        app.state = AppState::Editing;
    }
    
    if app.state == AppState::Editing {
        // Auto-cierre de delimitadores y Step-over
        let is_close_bracket = c == ')' || c == '}' || c == ']' || c == '"' || c == '\'';
        
        if is_close_bracket && app.buffer.char_at_cursor() == Some(c) {
            app.buffer.move_cursor_right(); // Step-over: saltar el caracter existente
        } else {
            app.buffer.insert_char(c);
            let closing = match c {
                '(' => Some(')'),
                '{' => Some('}'),
                '[' => Some(']'),
                '"' => Some('"'),
                '\'' => Some('\''),
                _ => None,
            };
            if let Some(close_char) = closing {
                app.buffer.insert_char(close_char);
                app.buffer.move_cursor_left(); // Dejar el cursor dentro de la envoltura
            }
        }

        notify_lsp_change(app);
    
        if c.is_alphanumeric() || c == '.' || c == ':' {
            app.trigger_completion();
        } else {
            app.completions.clear();
        }
    }
}

fn handle_backspace(app: &mut App) {
    app.status_msg = None;
    if let AppState::Editing = app.state {
        app.buffer.delete_backwards();
        notify_lsp_change(app);
        app.completions.clear();
    }
}

fn handle_tab(app: &mut App) {
    app.status_msg = None;
    if let AppState::Editing = app.state {
        if !app.completions.is_empty() {
            // Tab sirve ahora como flecha abajo en el menú de sugerencias
            let i = match app.completion_state.selected() {
                Some(i) => if i >= app.completions.len().saturating_sub(1) { 0 } else { i + 1 },
                None => 0,
            };
            app.completion_state.select(Some(i));
        } else {
            // Inserción normal de indentación (4 espacios emulando Tab)
            for _ in 0..4 { app.buffer.insert_char(' '); }
        }
    }
}

fn handle_up(app: &mut App) {
    if app.state == AppState::Exploring {
        app.explorer.previous();
    } else if !app.completions.is_empty() {
        let i = match app.completion_state.selected() {
            Some(i) => if i == 0 { app.completions.len().saturating_sub(1) } else { i - 1 },
            None => 0,
        };
        app.completion_state.select(Some(i));
    } else {
        app.buffer.move_cursor_up();
    }
}

fn handle_down(app: &mut App) {
    if app.state == AppState::Exploring {
        app.explorer.next();
    } else if !app.completions.is_empty() {
        let i = match app.completion_state.selected() {
            Some(i) => if i >= app.completions.len().saturating_sub(1) { 0 } else { i + 1 },
            None => 0,
        };
        app.completion_state.select(Some(i));
    } else {
        app.buffer.move_cursor_down();
    }
}

// --- Subsistemas de Soporte ---

fn notify_lsp_change(app: &mut App) {
    if let (Some(client), Some(uri)) = (&mut app.lsp_client, &app.current_uri) {
        if client.is_initialized {
            app.document_version += 1;
            client.did_change(uri.clone(), app.buffer.get_full_text(), app.document_version);
        }
    }
}

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
