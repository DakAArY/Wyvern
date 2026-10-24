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

use app::{App, AppState, Action, DragTarget, Focus};
use ratatui::layout::{Direction, Position, Rect};
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
        // Tras el reposo, dt incluye hasta 500 ms de espera en `poll`: sin el tope, el
        // primer paso de una animación recién disparada salta casi al final.
        let dt = now.duration_since(last_frame).as_secs_f64().min(1.0 / 30.0);
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
                        } else if app.context_menu.is_some() {
                            handle_menu_key(app, key);
                        } else if app.focus == Focus::Terminal && key.code != KeyCode::F(4) {
                            handle_terminal_key(app, key);
                        } else {
                            handle_normal_keys(app, key);
                        }
                    }
                }
                Event::Mouse(mouse_event) => {
                    // El movimiento sin botones no cambia nada visible; redibujar por él satura el bucle.
                    app.needs_redraw |= mouse_event.kind != MouseEventKind::Moved || app.context_menu.is_some();
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
    let tree_width = if app.show_tree { (term_width as f64 * app.tree_ratio).round() as u16 } else { 0 };
    let in_tree = app.show_tree && x < tree_width;
    if app.context_menu.is_some() {
        return handle_menu_mouse(app, event);
    }
    let on_tab = app.tab_areas.iter().find(|(_, _, r, _)| r.contains(Position { x, y })).copied();

    if let Some(target) = app.drag.clone() {
        match event.kind {
            MouseEventKind::Drag(MouseButton::Left) => return resize_drag(app, &target, x, y, term_width),
            MouseEventKind::Up(_) => return app.drag = None,
            _ => app.drag = None,
        }
    }
    if event.kind == MouseEventKind::Down(MouseButton::Left) && let Some(target) = drag_target_at(app, x, y, tree_width) {
        app.drag = Some(target);
        // La fila de pestañas también es borde de una división vertical: el clic sigue valiendo para la pestaña.
        if on_tab.is_none() { return; }
    }

    if let Some(r) = app.terminal_area.filter(|r| r.contains(Position { x, y })) {
        let (col, row) = (x - r.x, y.saturating_sub(r.y + 1));
        match event.kind {
            MouseEventKind::Down(MouseButton::Left) => app.focus = Focus::Terminal,
            MouseEventKind::ScrollUp => if let Some(t) = &app.terminal { t.wheel(true, col, row) },
            MouseEventKind::ScrollDown => if let Some(t) = &app.terminal { t.wheel(false, col, row) },
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
            } else if let Some((win, buf, _, close_x)) = on_tab {
                if x == close_x { app.request_close_tab(win, buf) } else { app.select_tab(win, buf) }
            } else if app.state == AppState::Workspace
                && let Some((id, rect)) = window_at(app, x, y) {
                    app.focus = Focus::Window(id);
                    app.active_window = id;
                    if x == rect.right() - 1 && y > rect.y {
                        scroll_to_screen_y(app, rect, y);
                    } else if let Some(doc) = app.active_document_mut() {
                        let gutter = gutter_width(doc.buffer.text.len_lines());
                        doc.buffer.set_cursor_from_screen(x, y, rect.x, rect.y + 1, gutter, false);
                    }
                }
        }
        MouseEventKind::Down(MouseButton::Right) if in_tree => {
            app.focus = Focus::Tree;
            let idx = app.explorer.state.offset() + y.saturating_sub(1) as usize;
            let on_entry = y >= 1 && idx < app.explorer.entries.len();
            if on_entry { app.explorer.state.select(Some(idx)); }
            app.context_menu = Some(app::ContextMenu { x, y, on_entry, selected: 0 });
            app.menu_spawn_progress = 0.0;
        }
        MouseEventKind::Drag(MouseButton::Left) => {
            let Some(rect) = app.window_areas.get(&app.active_window).copied() else { return };
            // Un arrastre que empezó en la barra de scroll sigue desplazando aunque el puntero se salga de ella.
            if app.last_click.is_some_and(|(_, lx, ly)| lx == rect.right().saturating_sub(1) && ly > rect.y && ly < rect.bottom()) {
                scroll_to_screen_y(app, rect, y);
            } else if let Some(doc) = app.active_document_mut() {
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
            let shift = event.modifiers.contains(KeyModifiers::SHIFT);
            match (event.kind, shift) {
                (MouseEventKind::ScrollUp, false) => doc.buffer.scroll_viewport_up(3),
                (MouseEventKind::ScrollDown, false) => doc.buffer.scroll_viewport_down(3),
                (MouseEventKind::ScrollLeft, _) | (MouseEventKind::ScrollUp, true) => doc.buffer.scroll_viewport_left(6),
                _ => doc.buffer.scroll_viewport_right(6),
            }
        }
        _ => {}
    }
}

/// Con el menú abierto el ratón solo sirve para elegir una opción o, fuera de él, cerrarlo.
fn handle_menu_mouse(app: &mut App, event: MouseEvent) {
    let pos = Position { x: event.column, y: event.row };
    let item = app.menu_area
        .filter(|r| r.contains(pos) && pos.y > r.y && pos.y < r.bottom() - 1)
        .map(|r| (pos.y - r.y - 1) as usize);
    match (event.kind, item) {
        (MouseEventKind::Moved, Some(i)) => if let Some(m) = &mut app.context_menu { m.selected = i },
        (MouseEventKind::Down(MouseButton::Left), Some(i)) => app.run_menu_item(i),
        (MouseEventKind::Down(_), None) => app.context_menu = None,
        _ => {}
    }
}

fn handle_menu_key(app: &mut App, key: KeyEvent) {
    let Some(menu) = &mut app.context_menu else { return };
    let len = app::MENU_ITEMS.len();
    match key.code {
        KeyCode::Up => menu.selected = (menu.selected + len - 1) % len,
        KeyCode::Down => menu.selected = (menu.selected + 1) % len,
        KeyCode::Enter => { let i = menu.selected; app.run_menu_item(i) }
        _ => app.context_menu = None,
    }
}

/// Borde redimensionable bajo el puntero: el derecho del explorador, el superior de la
/// terminal o el inicio del segundo hijo de una división (su gutter o su fila de pestañas).
fn drag_target_at(app: &App, x: u16, y: u16, tree_width: u16) -> Option<DragTarget> {
    if app.terminal_area.is_some_and(|r| y == r.y && x >= r.x && x < r.right()) {
        return Some(DragTarget::Terminal);
    }
    if tree_width > 0 && x == tree_width - 1 && app.terminal_area.is_none_or(|r| y < r.y) {
        return Some(DragTarget::Tree);
    }
    app.split_areas.iter().find(|(_, dir, _, b)| match dir {
        Direction::Horizontal => x == b.x && y >= b.y && y < b.bottom(),
        Direction::Vertical => y == b.y && x >= b.x && x < b.right(),
    }).map(|(path, dir, area, _)| DragTarget::Split(path.clone(), *dir, *area))
}

fn resize_drag(app: &mut App, target: &DragTarget, x: u16, y: u16, term_width: u16) {
    match target {
        DragTarget::Tree => app.tree_ratio = ((x + 1) as f64 / term_width.max(1) as f64).clamp(0.05, 0.8),
        DragTarget::Terminal => if let Some(r) = app.terminal_area {
            // Debajo de la terminal solo queda la línea de estado.
            let total = (r.bottom() + 1) as f64;
            app.term_ratio = (r.bottom().saturating_sub(y) as f64 / total).clamp(0.1, 0.85);
        },
        DragTarget::Split(path, dir, area) => if let Some(ratio) = app.layout.ratio_mut(path) {
            let (pos, start, len) = match dir {
                Direction::Horizontal => (x, area.x, area.width),
                Direction::Vertical => (y, area.y, area.height),
            };
            *ratio = (pos.saturating_sub(start) as u32 * 100 / len.max(1) as u32).clamp(10, 90) as u16;
        },
    }
}

/// Lleva la vista a la posición proporcional de `y` dentro de la barra de scroll.
fn scroll_to_screen_y(app: &mut App, rect: Rect, y: u16) {
    let Some(doc) = app.active_document_mut() else { return };
    let rows = rect.height.saturating_sub(2).max(1) as usize;
    let rel = (y.saturating_sub(rect.y + 1) as usize).min(rows);
    let max = doc.buffer.text.len_lines().saturating_sub(1);
    doc.buffer.scroll_y = rel * max / rows;
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
        MoveLeft(_) if app.focus == Focus::Tree => app.explorer.collapse(),
        MoveRight(_) if app.focus == Focus::Tree => app.explorer.expand(),
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
            if entry.is_dir {
                app.explorer.toggle_selected();
            } else {
                app.load_file(entry.path.clone());
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
            'a' => app.trigger_create(false, false),
            'A' => app.trigger_create(true, false),
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

    #[test]
    fn scrollbar_click_and_drag_jump_through_file() {
        let mut app = App::new();
        app.show_tree = false;
        app.new_blank_file();
        app.active_document_mut().unwrap().buffer.text = ropey::Rope::from_str(&"a\n".repeat(100));
        let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(40, 12)).unwrap();
        term.draw(|f| render(f, &mut app)).unwrap();
        let rect = app.window_areas[&app.active_window];
        let mouse = |kind, row| MouseEvent { kind, column: rect.right() - 1, row, modifiers: KeyModifiers::empty() };
        let scroll = |app: &App| app.get_active_document().unwrap().buffer.scroll_y;

        handle_mouse_event(&mut app, mouse(MouseEventKind::Down(MouseButton::Left), rect.bottom() - 1), 40);
        assert_eq!(scroll(&app), 100);
        let cursor = app.get_active_document().unwrap().buffer.cursor_char_idx;
        handle_mouse_event(&mut app, mouse(MouseEventKind::Drag(MouseButton::Left), rect.y + 1), 40);
        assert_eq!(scroll(&app), 0);
        assert_eq!(app.get_active_document().unwrap().buffer.cursor_char_idx, cursor, "la barra no mueve el cursor");
        handle_mouse_event(&mut app, mouse(MouseEventKind::Down(MouseButton::Left), rect.bottom() - 1), 40);
        handle_mouse_event(&mut app, mouse(MouseEventKind::Drag(MouseButton::Left), rect.y + 1), 40);
        handle_mouse_event(&mut app, mouse(MouseEventKind::Down(MouseButton::Left), rect.y), 40);
        assert_eq!(scroll(&app), 0, "la fila de pestañas no es parte de la barra");
    }

    fn click(app: &mut App, kind: MouseEventKind, column: u16, row: u16) {
        handle_mouse_event(app, MouseEvent { kind, column, row, modifiers: KeyModifiers::empty() }, 80);
    }

    fn draw(app: &mut App) -> ratatui::Terminal<ratatui::backend::TestBackend> {
        let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 20)).unwrap();
        term.draw(|f| render(f, app)).unwrap();
        term
    }

    #[test]
    fn tabs_switch_and_close_with_mouse() {
        let d = std::env::temp_dir().join(format!("wyvern_tabs_{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("a.txt"), "aaa").unwrap();
        std::fs::write(d.join("b.txt"), "bbb").unwrap();
        let mut app = App::new();
        app.load_file(d.join("a.txt"));
        app.load_file(d.join("b.txt"));
        app.load_file(d.join("a.txt"));
        let win = app.active_window;
        assert_eq!(app.windows[&win].tabs.len(), 2, "reabrir no duplica la pestaña");
        assert_eq!(text(&app), "aaa");

        draw(&mut app);
        let (_, b, r, _) = app.tab_areas[1];
        click(&mut app, MouseEventKind::Down(MouseButton::Left), r.x + 2, r.y);
        assert_eq!(app.active_buffer_id(), Some(b));
        assert_eq!(text(&app), "bbb");

        // Con cambios sin guardar, la × pide confirmación y "n" la conserva.
        typed(&mut app, "x");
        draw(&mut app);
        let (_, _, _, close_x) = app.tab_areas[1];
        click(&mut app, MouseEventKind::Down(MouseButton::Left), close_x, r.y);
        assert!(matches!(app.prompt.as_ref().map(|p| &p.intent), Some(app::PromptIntent::ConfirmCloseTab(..))));
        app.prompt.as_mut().unwrap().input = "n".into();
        app.execute_prompt();
        assert_eq!(app.windows[&win].tabs.len(), 2);

        app.open_prompt(app::PromptIntent::ConfirmCloseTab(win, b));
        app.prompt.as_mut().unwrap().input = "y".into();
        app.execute_prompt();
        assert_eq!(app.windows[&win].tabs.len(), 1);
        assert!(!app.documents.contains_key(&b), "el documento cerrado se libera");
        assert_eq!(text(&app), "aaa");

        draw(&mut app);
        let (_, _, r, close_x) = app.tab_areas[0];
        click(&mut app, MouseEventKind::Down(MouseButton::Left), close_x, r.y);
        assert!(app.windows.is_empty() && app.documents.is_empty());
        assert!(app.state == AppState::Intro);
        draw(&mut app);
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn right_click_menu_creates_in_clicked_folder_or_root() {
        let d = std::env::temp_dir().join(format!("wyvern_menu_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("sub")).unwrap();
        let d = std::fs::canonicalize(&d).unwrap();
        let mut app = App::new();
        app.explorer = explorer::FileExplorer::new(d.clone());
        app.show_tree = true;
        app.tree_spawn_progress = 1.0;
        draw(&mut app);
        let row = |app: &App, name: &str| app.explorer.entries.iter().position(|e| e.name == name).unwrap() as u16 + 1;

        // Clic derecho sobre "sub" → "Nueva carpeta": se crea dentro de sub.
        let y = row(&app, "sub");
        click(&mut app, MouseEventKind::Down(MouseButton::Right), 3, y);
        app.menu_spawn_progress = 1.0;
        draw(&mut app);
        let menu = app.menu_area.unwrap();
        click(&mut app, MouseEventKind::Down(MouseButton::Left), menu.x + 2, menu.y + 2);
        assert!(app.context_menu.is_none());
        assert!(matches!(&app.prompt.as_ref().unwrap().intent, app::PromptIntent::CreateDir(p) if *p == d.join("sub")));
        app.prompt.as_mut().unwrap().input = "nueva".into();
        app.execute_prompt();
        assert!(d.join("sub/nueva").is_dir());

        // En el hueco vacío: Renombrar/Eliminar desactivados y se crea en la raíz.
        draw(&mut app);
        click(&mut app, MouseEventKind::Down(MouseButton::Right), 3, 15);
        assert!(!app.menu_item_enabled(3));
        handle_menu_key(&mut app, key(KeyCode::Down));
        handle_menu_key(&mut app, key(KeyCode::Down));
        handle_menu_key(&mut app, key(KeyCode::Down));
        handle_menu_key(&mut app, key(KeyCode::Enter));
        assert!(app.prompt.is_none() && app.context_menu.is_some(), "una opción desactivada no hace nada");
        handle_menu_key(&mut app, key(KeyCode::Up));
        handle_menu_key(&mut app, key(KeyCode::Up));
        handle_menu_key(&mut app, key(KeyCode::Up));
        handle_menu_key(&mut app, key(KeyCode::Enter));
        app.prompt.as_mut().unwrap().input = "raiz.txt".into();
        app.execute_prompt();
        assert!(d.join("raiz.txt").is_file());

        // Clic fuera lo cierra sin hacer nada más.
        click(&mut app, MouseEventKind::Down(MouseButton::Right), 3, 15);
        click(&mut app, MouseEventKind::Down(MouseButton::Left), 70, 2);
        assert!(app.context_menu.is_none() && app.prompt.is_none());
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn borders_resize_with_mouse_drag() {
        let mut app = App::new();
        app.new_blank_file();
        app.show_tree = true;
        app.tree_spawn_progress = 1.0;
        app.focus = Focus::Window(app.active_window);
        app.split_window(Direction::Horizontal);
        app.show_terminal = true;
        app.terminal = Some(terminal::Terminal::spawn(Path::new("/"), 5, 40, std::sync::mpsc::channel().0).unwrap());
        app.term_spawn_progress = 1.0;
        draw(&mut app);

        // Explorador: su borde derecho.
        click(&mut app, MouseEventKind::Down(MouseButton::Left), 15, 3);
        click(&mut app, MouseEventKind::Drag(MouseButton::Left), 29, 3);
        click(&mut app, MouseEventKind::Up(MouseButton::Left), 29, 3);
        assert_eq!(app.tree_ratio, 30.0 / 80.0);
        draw(&mut app);

        // División: el inicio de la ventana derecha.
        let (_, _, area, b) = app.split_areas[0].clone();
        click(&mut app, MouseEventKind::Down(MouseButton::Left), b.x, b.y + 2);
        click(&mut app, MouseEventKind::Drag(MouseButton::Left), area.x + area.width / 4, b.y + 2);
        draw(&mut app);
        assert_eq!(app.split_areas[0].3.x, area.x + area.width / 4);

        // Terminal: su borde superior.
        let t = app.terminal_area.unwrap();
        click(&mut app, MouseEventKind::Down(MouseButton::Left), 40, t.y);
        click(&mut app, MouseEventKind::Drag(MouseButton::Left), 40, 8);
        click(&mut app, MouseEventKind::Up(MouseButton::Left), 40, 8);
        draw(&mut app);
        assert_eq!(app.terminal_area.unwrap().y, 8);

        // Soltar termina el arrastre: lo siguiente vuelve a seleccionar texto.
        assert!(app.drag.is_none());
    }
}
