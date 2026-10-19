mod app;
mod completion;
mod config;
mod editor;
mod ui;
mod explorer;
mod lsp;
mod git;
mod terminal;
mod theme;

use app::{App, AppState, Action, Focus};
use ratatui::layout::{Direction, Rect};
use crossterm::{
    event::{
        self, Event, KeyCode, KeyEvent, KeyModifiers, MouseEvent, MouseEventKind,
        MouseButton, EnableMouseCapture, DisableMouseCapture,
        EnableBracketedPaste, DisableBracketedPaste
    },
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
    ExecutableCommand,
};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use std::{io::{self, stdout}, path::{Path, PathBuf}, time::{Duration, Instant}};
use std::panic;
use crate::ui::{gutter_width, render};

fn main() -> io::Result<()> {
    panic::set_hook(Box::new(|info| {
        let _ = disable_raw_mode();
        let _ = stdout().execute(LeaveAlternateScreen);
        let _ = stdout().execute(DisableMouseCapture);
        let _ = stdout().execute(DisableBracketedPaste);
        eprintln!("{}", info);
    }));

    let mut app = App::new();
    if let Some(arg) = std::env::args_os().nth(1) {
        open_cli_path(&mut app, PathBuf::from(arg));
    }

    enable_raw_mode()?;
    stdout().execute(EnterAlternateScreen)?
            .execute(EnableMouseCapture)?
            .execute(EnableBracketedPaste)?;

    let mut terminal = Terminal::new(CrosstermBackend::new(stdout()))?;
    let res = run_app(&mut terminal, &mut app);
    for (_, mut lsp) in app.lsp_clients.drain() {
        lsp.shutdown_and_exit();
    }

    disable_raw_mode()?;
    stdout().execute(LeaveAlternateScreen)?
            .execute(DisableMouseCapture)?
            .execute(DisableBracketedPaste)?;

    res
}

/// Un directorio pasa a ser la raíz del proyecto; un archivo se abre (o se crea
/// en memoria) y la raíz se deduce subiendo desde su carpeta.
fn open_cli_path(app: &mut App, path: PathBuf) {
    if path.is_dir() {
        let dir = std::fs::canonicalize(&path).unwrap_or(path);
        app.working_dir = dir.clone();
        app.explorer = explorer::FileExplorer::new(dir);
        app.show_tree = true;
        app.focus = Focus::Tree;
        app.state = AppState::Workspace;
    } else {
        let parent = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
        let parent = std::fs::canonicalize(parent).unwrap_or_else(|_| parent.to_path_buf());
        app.working_dir = app::find_project_root(&parent);
        app.explorer = explorer::FileExplorer::new(parent.clone());

        if path.exists() {
            app.load_file(path);
        } else {
            app.new_blank_file();
            if let Some(id) = app.active_buffer_id() {
                if let Some(doc) = app.documents.get_mut(&id) {
                    doc.filepath = Some(parent.join(path.file_name().unwrap_or_default()));
                }
                app.setup_lsp_for_buffer(id);
            }
        }
    }
    app.refresh_git();
}

fn run_app(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>, app: &mut App) -> io::Result<()> {
    let mut last_frame = Instant::now();

    loop {
        let now = Instant::now();
        let dt = now.duration_since(last_frame).as_secs_f64();
        last_frame = now;

        if app.update_animations(dt) {
            app.needs_redraw = true;
        }

        if app.needs_redraw {
            terminal.draw(|f| render(f, app))?;
            app.needs_redraw = false;
        }

        let busy = app.is_animating() || !app.pending_completion_ids.is_empty() || app.pending_resolve.is_some()
            || app.search_pending || app.show_terminal;
        // 60 FPS mientras hay animaciones o respuestas pendientes; en reposo basta poco.
        let timeout = Duration::from_millis(if busy { 16 } else { 500 });

        if event::poll(timeout)? {
            match event::read()? {
                Event::Key(key) => {
                    if key.kind == event::KeyEventKind::Press || key.kind == event::KeyEventKind::Repeat {
                        app.needs_redraw = true;
                        if app.prompt.is_some() {
                            handle_prompt_key(app, key);
                        } else if app.focus == Focus::Terminal && key.code != KeyCode::F(4) {
                            handle_terminal_key(app, key);
                        } else {
                            handle_normal_keys(app, key);
                        }
                    }
                }
                Event::Mouse(mouse_event) => {
                    app.needs_redraw = true;
                    if app.prompt.is_none() && !app.show_help {
                        handle_mouse_event(app, mouse_event, terminal.size()?.width);
                    }
                }
                Event::Paste(text) => {
                    app.needs_redraw = true;
                    if let Some(p) = &mut app.prompt {
                        p.input.push_str(&text);
                        app.update_prompt_search();
                    } else if app.focus == Focus::Terminal {
                        if let Some(t) = &app.terminal { t.paste(&text); }
                    } else {
                        paste_text(app, &text);
                    }
                }
                Event::Resize(..) => app.needs_redraw = true,
                _ => {}
            }
            app.after_edit();
        }
        if app.poll_terminal() {
            app.needs_redraw = true;
        }

        if app.poll_text_search() {
            app.needs_redraw = true;
        }
        if app.process_lsp_messages() {
            app.needs_redraw = true;
        }
        app.resolve_selected_completion();
        // Editar el tema o config.json se refleja sin reiniciar.
        if app.reload_config_if_changed() {
            app.needs_redraw = true;
        }
        if app.quit { break; }
    }
    Ok(())
}

fn window_at(app: &App, x: u16, y: u16) -> Option<(usize, Rect)> {
    app.window_areas.iter()
        .find(|(_, r)| x >= r.x && x < r.right() && y >= r.y && y < r.bottom())
        .map(|(&id, &r)| (id, r))
}

fn handle_mouse_event(app: &mut App, event: MouseEvent, term_width: u16) {
    let x = event.column;
    let y = event.row;
    let tree_width = if app.show_tree { (term_width as f64 * 0.20) as u16 } else { 0 };
    let in_tree = app.show_tree && x < tree_width;

    if app.terminal_area.is_some_and(|r| x >= r.x && x < r.right() && y >= r.y && y < r.bottom()) {
        match event.kind {
            MouseEventKind::Down(MouseButton::Left) => app.focus = Focus::Terminal,
            MouseEventKind::ScrollUp => if let Some(t) = &app.terminal { t.scroll(3) },
            MouseEventKind::ScrollDown => if let Some(t) = &app.terminal {t.scroll(-3)},
            _ => {}
        }
        return;
    }

    match event.kind {
        MouseEventKind::Down(MouseButton::Left) => {
            let now = Instant::now();
            let is_double_click = app.last_click.is_some_and(|(t, lx, ly)| {
                now.duration_since(t) < Duration::from_millis(500) && lx == x && ly == y
            });
            app.last_click = Some((now, x, y));

            if in_tree {
                app.focus = Focus::Tree;
                if y >= 1 {
                    let list_offset = app.explorer.state.offset();
                    app.explorer.state.select(Some(list_offset + (y - 1) as usize));
                    if is_double_click { handle_enter(app); }
                }
            } else if app.state == AppState::Workspace
                && let Some((id, rect)) = window_at(app, x, y) {
                    app.focus = Focus::Window(id);
                    app.active_window = id;
                    if let Some(doc) = app.active_document_mut() {
                        let gutter = gutter_width(doc.buffer.text.len_lines());
                        doc.buffer.set_cursor_from_screen(x, y, rect.x, rect.y + 1, gutter, false);
                    }
                }
        }
        MouseEventKind::Drag(MouseButton::Left) => {
            if let Some(rect) = app.window_areas.get(&app.active_window).copied()
                && let Some(doc) = app.active_document_mut() {
                    let gutter = gutter_width(doc.buffer.text.len_lines());
                    doc.buffer.set_cursor_from_screen(x, y, rect.x, rect.y + 1, gutter, true);
                }
        }
        MouseEventKind::ScrollUp | MouseEventKind::ScrollDown | MouseEventKind::ScrollLeft | MouseEventKind::ScrollRight => {
            if in_tree {
                for _ in 0..3 {
                    match event.kind {
                        MouseEventKind::ScrollUp => app.explorer.previous(),
                        MouseEventKind::ScrollDown => app.explorer.next(),
                        _ => {}
                    }
                }
                return;
            }
            // Se desplaza la ventana bajo el puntero, no necesariamente la enfocada.
            let Some(buf_id) = window_at(app, x, y).and_then(|(id, _)| app.windows.get(&id)).map(|w| w.buffer_id) else { return };
            let Some(doc) = app.documents.get_mut(&buf_id) else { return };
            match event.kind {
                MouseEventKind::ScrollUp => doc.buffer.scroll_viewport_up(3),
                MouseEventKind::ScrollDown => doc.buffer.scroll_viewport_down(3),
                MouseEventKind::ScrollLeft => doc.buffer.scroll_viewport_left(3),
                _ => doc.buffer.scroll_viewport_right(3),
            }
        }
        _ => {}
    }
}

fn prompt_items_len(prompt: &app::PromptState) -> usize {
    match prompt.intent {
        app::PromptIntent::SearchFile => prompt.file_results.len(),
        app::PromptIntent::SearchText => prompt.text_results.len(),
        _ => 0,
    }
}

fn handle_prompt_key(app: &mut App, key: KeyEvent) {
    match key.code {
        KeyCode::Esc => app.close_prompt(),
        KeyCode::Enter => app.execute_prompt(),
        KeyCode::Up | KeyCode::Down => {
            if let Some(prompt) = &mut app.prompt {
                let len = prompt_items_len(prompt);
                if len > 0 {
                    let i = prompt.selection_state.selected().unwrap_or(0);
                    let i = if key.code == KeyCode::Up { (i + len - 1) % len } else { (i + 1) % len };
                    prompt.selection_state.select(Some(i));
                }
            }
        }
        KeyCode::Char(c) => {
            if let Some(p) = &mut app.prompt { p.input.push(c); }
            app.update_prompt_search();
        }
        KeyCode::Backspace => {
            if let Some(p) = &mut app.prompt { p.input.pop(); }
            app.update_prompt_search();
        }
        _ => {}
    }
}

fn handle_terminal_key(app: &mut App, key: KeyEvent) {
    let Some(t) = &app.terminal else { return; };
    let page = app.terminal_area.map_or(10, |r| r.height as isize / 2);
    match (key.code, key.modifiers.contains(KeyModifiers::SHIFT)) {
        (KeyCode::PageUp, true) => t.scroll(page),
        (KeyCode::PageDown, true) => t.scroll(-page),
        _ => t.send_key(key),
    }
}

fn handle_normal_keys(app: &mut App, key: KeyEvent) {
    let combo = app::KeyCombo { code: key.code, modifiers: key.modifiers };

    if let Some(action) = app.keybindings.get(&combo).cloned() {
        execute_action(app, action);
    } else if let KeyCode::Char(c) = key.code
        && !key.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) {
            handle_char(app, c);
        }
}

fn execute_action(app: &mut App, action: Action) {
    use app::Action::*;
    let view_height = app.active_view_height().unwrap_or(20);
    match action {
        ToggleHelp => app.show_help = !app.show_help,
        ToggleTree => app.toggle_tree(),
        FocusDown => app.move_focus_dir(0, 1),
        FocusUp => app.move_focus_dir(0, -1),
        FocusLeft => app.move_focus_dir(-1, 0),
        FocusRight => app.move_focus_dir(1, 0),
        SplitVertical => app.split_window(Direction::Vertical),
        SplitHorizontal => app.split_window(Direction::Horizontal),
        CloseWindow => app.close_active_window(),
        NextSearchResult => app.next_search_result(),
        ToggleTerminal => app.toggle_terminal(),
        CycleTheme => app.cycle_theme(),
        Copy => {
            if let Some(text) = app.get_active_document().and_then(|d| d.buffer.get_selected_text()) {
                app.copy_to_clipboard(text);
            }
        }
        Cut => {
            if let Some(text) = app.active_document_mut().and_then(|d| d.buffer.delete_selection()) {
                app.copy_to_clipboard(text);
            }
        }
        Paste => {
            if let Some(text) = app.read_clipboard() {
                paste_text(app, &text);
            }
        }
        Undo | Redo => {
            let done = app.active_document_mut().map(|d| if action == Undo { d.buffer.undo() } else { d.buffer.redo() });
            app.status_msg = match (done, action == Undo) {
                (Some(true), true) => Some("Deshacer".into()),
                (Some(true), false) => Some("Rehacer".into()),
                (Some(false), true) => Some("Ya en el estado mas antiguo".into()),
                (Some(false), false) => Some("Ya en el estado mas reciente".into()),
                (None, _) => app.status_msg.take(),
            };
        }
        Quit => {
            if app.documents.values().any(|d| d.is_dirty) {
                app.open_prompt(app::PromptIntent::ConfirmQuit);
            } else {
                app.quit = true;
            }
        }
        Save => app.trigger_save(),
        ScrollViewportUp => if let Some(doc) = app.active_document_mut() { doc.buffer.scroll_viewport_up(view_height / 2) },
        ScrollViewportDown => if let Some(doc) = app.active_document_mut() { doc.buffer.scroll_viewport_down(view_height / 2) },
        SearchText => app.open_prompt(app::PromptIntent::SearchText),
        SearchFile => app.open_prompt(app::PromptIntent::SearchFile),
        Cancel => handle_escape(app),
        Confirm => handle_enter(app),
        Backspace => handle_backspace(app),
        Indent => handle_tab(app),
        Delete => {
            if app.focus == Focus::Tree {
                app.trigger_delete();
            } else if let Some(doc) = app.active_document_mut() {
                doc.buffer.delete_forward();
            }
        }
        MoveStartOfLine(selecting) => if let Some(doc) = app.active_document_mut() { doc.buffer.move_to_start_of_line(selecting) },
        MoveEndOfLine(selecting) => if let Some(doc) = app.active_document_mut() { doc.buffer.move_to_end_of_line(selecting) },
        PageUp(selecting) => if let Some(doc) = app.active_document_mut() { doc.buffer.move_page_up(selecting, view_height) },
        PageDown(selecting) => if let Some(doc) = app.active_document_mut() { doc.buffer.move_page_down(selecting, view_height) },
        MoveUp(selecting) => handle_up(app, selecting),
        MoveDown(selecting) => handle_down(app, selecting),
        MoveLeft(selecting) => if let Some(doc) = app.active_document_mut() { doc.buffer.move_cursor_left(selecting) },
        MoveRight(selecting) => if let Some(doc) = app.active_document_mut() { doc.buffer.move_cursor_right(selecting) },
    }
}

fn handle_escape(app: &mut App) {
    if app.show_help {
        app.show_help = false;
    } else if app.focus == Focus::Tree && app.show_tree {
        app.toggle_tree();
    } else if !app.completions.is_empty() {
        app.clear_completions();
    } else if let Some(doc) = app.active_document_mut() {
        doc.buffer.selection_anchor = None;
    }
}

fn handle_enter(app: &mut App) {
    app.status_msg = None;

    if app.focus == Focus::Tree {
        if let Some(entry) = app.explorer.get_selected() {
            let path = entry.path.clone();
            if entry.is_dir {
                app.explorer.current_dir = path;
                app.explorer.reload();
            } else {
                app.load_file(path);
            }
        }
    } else if !app.completions.is_empty() {
        if let Some(comp) = app.completion_state.selected().and_then(|i| app.completions.get(i)).cloned() {
            app.accept_completion(&comp);
        }
        app.clear_completions();
    } else if let Some(doc) = app.active_document_mut() {
        let buf = &mut doc.buffer;
        buf.delete_selection();

        let indent = buf.get_current_line_indentation();
        let before = buf.cursor_char_idx.checked_sub(1).map(|i| buf.text.char(i));
        let between_brackets = matches!(
            (before, buf.char_at_cursor()),
            (Some('{'), Some('}')) | (Some('['), Some(']')) | (Some('('), Some(')'))
        );

        if between_brackets {
            let inner = format!("{}{}", indent, " ".repeat(editor::TAB_WIDTH));
            let start = buf.cursor_char_idx;
            buf.insert_str(&format!("\n{}\n{}", inner, indent));
            buf.cursor_char_idx = start + 1 + inner.chars().count();
            buf.target_visual_col = buf.char_idx_to_visual_col(buf.cursor_char_idx);
        } else {
            buf.insert_str(&format!("\n{}", indent));
        }
    }
}

fn handle_char(app: &mut App, c: char) {
    app.status_msg = None;

    if app.focus == Focus::Tree {
        match c {
            'n' => app.new_blank_file(),
            'r' => app.trigger_rename(),
            'd' => app.trigger_delete(),
            _ => {}
        }
        return;
    }

    if app.state == AppState::Intro {
        app.new_blank_file();
    }

    let Some(doc) = app.active_document_mut() else { return };
    let buf = &mut doc.buffer;
    buf.delete_selection();

    let is_closing = matches!(c, ')' | '}' | ']' | '"' | '\'' | '>');
    if is_closing && buf.char_at_cursor() == Some(c) {
        buf.move_cursor_right(false);
    } else {
        buf.insert_char(c);
        let closing = match c {
            '(' => Some(')'), '{' => Some('}'), '[' => Some(']'),
            '"' => Some('"'), '\'' => Some('\''), '<' => Some('>'),
            _ => None
        };
        if let Some(close_char) = closing {
            buf.insert_char(close_char);
            buf.move_cursor_left(false);
        }
    }

    // Los caracteres de disparo los anuncia cada servidor; no hay lista fija por lenguaje.
    if c.is_alphanumeric() || c == '_' {
        app.trigger_completion(None);
    } else if app.is_completion_trigger(c) {
        app.trigger_completion(Some(c));
    } else {
        app.clear_completions();
    }
}

fn handle_backspace(app: &mut App) {
    app.status_msg = None;
    if let Some(doc) = app.active_document_mut() {
        doc.buffer.delete_backwards();
        // Como en JetBrains, borrar refina la lista abierta en vez de cerrarla.
        if !app.completions.is_empty() { app.refilter_completions(); }
    }
}

fn handle_tab(app: &mut App) {
    app.status_msg = None;
    if !app.completions.is_empty() {
        let i = app.completion_state.selected().map_or(0, |i| (i + 1) % app.completions.len());
        app.completion_state.select(Some(i));
    } else if let Some(doc) = app.active_document_mut() {
        doc.buffer.delete_selection();
        doc.buffer.insert_str(&" ".repeat(editor::TAB_WIDTH));
    }
}

fn handle_up(app: &mut App, selecting: bool) {
    if app.focus == Focus::Tree { app.explorer.previous(); }
    else if !app.completions.is_empty() {
        let len = app.completions.len();
        let i = app.completion_state.selected().map_or(0, |i| (i + len - 1) % len);
        app.completion_state.select(Some(i));
    } else if let Some(doc) = app.active_document_mut() { doc.buffer.move_cursor_up(selecting); }
}

fn handle_down(app: &mut App, selecting: bool) {
    if app.focus == Focus::Tree { app.explorer.next(); }
    else if !app.completions.is_empty() {
        let i = app.completion_state.selected().map_or(0, |i| (i + 1) % app.completions.len());
        app.completion_state.select(Some(i));
    } else if let Some(doc) = app.active_document_mut() { doc.buffer.move_cursor_down(selecting); }
}

fn paste_text(app: &mut App, text: &str) {
    if let Some(doc) = app.active_document_mut() {
        doc.buffer.delete_selection();
        doc.buffer.insert_str(text);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent { KeyEvent::new(code, KeyModifiers::empty()) }

    fn typed(app: &mut App, s: &str) {
        for c in s.chars() {
            handle_normal_keys(app, key(if c == '\n' { KeyCode::Enter } else { KeyCode::Char(c) }));
            app.after_edit();
        }
    }

    fn text(app: &App) -> String { app.get_active_document().unwrap().buffer.text.to_string() }

    #[test]
    fn tab_and_delete_mark_dirty() {
        let mut app = App::new();
        app.new_blank_file();
        handle_normal_keys(&mut app, key(KeyCode::Tab));
        app.after_edit();
        assert_eq!(text(&app), "    ");
        assert!(app.get_active_document().unwrap().is_dirty);

        app.active_document_mut().unwrap().buffer.cursor_char_idx = 0;
        handle_normal_keys(&mut app, key(KeyCode::Delete));
        assert_eq!(text(&app), "   ");
    }

    #[test]
    fn enter_between_braces_indents_and_autopairs() {
        let mut app = App::new();
        app.new_blank_file();
        typed(&mut app, "fn f() {\nx");
        assert_eq!(text(&app), "fn f() {\n    x\n}");
        typed(&mut app, ")");
        assert_eq!(text(&app), "fn f() {\n    x)\n}");
    }

    #[test]
    fn undo_reverts_enter_block_in_one_step() {
        let mut app = App::new();
        app.new_blank_file();
        typed(&mut app, "{");
        typed(&mut app, "\n");
        execute_action(&mut app, Action::Undo);
        assert_eq!(text(&app), "{}");
    }
}
