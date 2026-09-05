mod app;
mod editor;
mod ui;
mod explorer;
mod lsp;
mod git;

use app::{App, AppState, Action, Focus};
use ratatui::layout::Direction;
use crossterm:: {
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
use std::{io::{self, stdout}, time::{Duration, Instant}};
use std::panic;
use crate::ui::render;
use fuzzy_matcher::FuzzyMatcher;
use fuzzy_matcher::skim::SkimMatcherV2;

fn main() -> io::Result<()> {
    panic::set_hook(Box::new(|info| {
        let _ = disable_raw_mode();
        let _ = stdout().execute(LeaveAlternateScreen);
        let _ = stdout().execute(DisableMouseCapture);
        let _ = stdout().execute(DisableBracketedPaste);
        eprintln!("{}", info);
    }));

    let mut app = App::new();
    let args: Vec<String> = std::env::args().collect();

    if args.len() > 1 {
        let path = std::path::PathBuf::from(&args[1]);
        if path.is_dir() {
            app.working_dir = path.clone();
            app.explorer = explorer::FileExplorer::new(path);
            app.show_tree = true;
            app.focus = Focus::Tree;
            app.state = AppState::Workspace;
        } else {
            if let Some(parent) = path.parent() {
                let parent_path = if parent.as_os_str().is_empty() {
                    std::path::Path::new(".")
                } else { parent };
                app.working_dir = parent_path.to_path_buf();
                app.explorer = explorer::FileExplorer::new(parent_path.to_path_buf());
            }
            if path.exists() {
                app.load_file(path);
            } else {
                app.new_blank_file();
                if let Some(doc) = app.active_document_mut() {
                    doc.filepath = Some(path);
                }
            }
        }
    }

    enable_raw_mode()?;
    stdout().execute(EnterAlternateScreen)?
            .execute(EnableMouseCapture)?
            .execute(EnableBracketedPaste)?;

    let mut terminal = Terminal::new(CrosstermBackend::new(stdout()))?;
    let res = run_app(&mut terminal, &mut app);
    if let Some(mut lsp) = app.lsp_client.take() {
        lsp.shutdown_and_exit();
    }

    disable_raw_mode()?;
    stdout().execute(LeaveAlternateScreen)?
            .execute(DisableMouseCapture)?
            .execute(DisableBracketedPaste)?;

    res
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

        let is_anim = app.is_animating();
        let timeout = if is_anim || app.pending_completion_id.is_some() {
            Duration::from_millis(16) // Target a 60FPS tick temporal para animaciones activas
        } else {
            Duration::from_millis(500)
        };

        let term_area = terminal.size()?;

        if event::poll(timeout)? {
            match event::read()? {
                Event::Key(key) => {
                    if key.kind == event::KeyEventKind::Press || key.kind == event::KeyEventKind::Repeat {
                        app.needs_redraw = true;
                        if app.prompt.is_some() {
                            handle_prompt_key(app, key);
                        } else {
                            handle_normal_keys(app, key, term_area.height);
                        }
                    }
                }
                Event::Mouse(mouse_event) => {
                    app.needs_redraw = true;
                    if app.prompt.is_none() && !app.show_help {
                        handle_mouse_event(app, mouse_event, term_area.width, term_area.height);
                    }
                }
                Event::Paste(text) => {
                    app.needs_redraw = true;
                    if let Some(p) = &mut app.prompt {
                        p.input.push_str(&text);
                        app.update_prompt_search();
                    } else {
                        execute_paste_text(app, text);
                    }
                }
                _ => {}
            }
        }
        if process_lsp_messages(app) {
            app.needs_redraw = true;
        }
        if app.quit { break; }
    }
    Ok(())
}

fn handle_mouse_event(app: &mut App, event: MouseEvent, term_width: u16, _term_height: u16) {
    let x = event.column;
    let y = event.row;
    let tree_width = if app.show_tree { (term_width as f32 * 0.20) as u16 } else { 0 };

    match event.kind {
        MouseEventKind::Down(MouseButton::Left) => {
            let now = Instant::now();
            let is_double_click = if let Some((last_time, last_x, last_y)) = app.last_click {
                now.duration_since(last_time) < Duration::from_millis(500) && last_x == x && last_y == y
            } else { false };
            app.last_click = Some((now, x, y));

            if app.show_tree && x < tree_width {
                if y >= 1 {
                    let click_idx = (y - 1) as usize;
                    let list_offset = app.explorer.state.offset();
                    app.explorer.state.select(Some(list_offset + click_idx));
                    if is_double_click { handle_enter(app); }
                }
            } else if app.state == AppState::Workspace {
                let mut clicked_window = None;

                for (id, rect) in &app.window_areas {
                    if x >= rect.x && x < rect.right() && y >= rect.y && y < rect.bottom() {
                        clicked_window = Some((*id, *rect));
                        break;
                    }
                }

                if let Some((id, rect)) = clicked_window {
                    app.focus = Focus::Window(id);
                    app.active_window = id;
                    if let Some(doc) = app.active_document_mut() {
                        let gutter_num_width = doc.buffer.text.len_lines().to_string().len().max(1) as u16;
                        let gutter_total_width = gutter_num_width + 3;
                        doc.buffer.set_cursor_from_screen(x, y, rect.x, rect.y + 1, gutter_total_width, false);
                    }
                }
            }
        }
        MouseEventKind::Drag(MouseButton::Left) => {
            if let Focus::Window(active_id) = app.focus {
                if let Some(rect) = app.window_areas.get(&active_id).cloned() {
                    if let Some(doc) = app.active_document_mut() {
                        let gutter_num_width = doc.buffer.text.len_lines().to_string().len().max(1) as u16;
                        let gutter_total_width = gutter_num_width + 3;
                        doc.buffer.set_cursor_from_screen(x, y, rect.x, rect.y + 1, gutter_total_width, true);
                    }
                }
            }
        }
        MouseEventKind::ScrollUp => {
            if app.focus == Focus::Tree { for _ in 0..3 { app.explorer.previous(); } }
            else if let Some(doc) = app.active_document_mut() { doc.buffer.scroll_viewport_up(3); }
        }
        MouseEventKind::ScrollDown => {
            if app.focus == Focus::Tree { for _ in 0..3 { app.explorer.next(); } }
            else if let Some(doc) = app.active_document_mut() { doc.buffer.scroll_viewport_down(3); }
        }
        MouseEventKind::ScrollLeft => {
            if let Some(doc) = app.active_document_mut() { doc.buffer.scroll_viewport_left(3); }
        }
        MouseEventKind::ScrollRight => {
            if let Some(doc) = app.active_document_mut() { doc.buffer.scroll_viewport_right(3); }
        }
        _ => {}
    }
}

fn handle_prompt_key(app: &mut App, key: KeyEvent) {
    match key.code {
        KeyCode::Esc => app.prompt = None,
        KeyCode::Enter => app.execute_prompt(),
        KeyCode::Up => {
            if let Some(prompt) = &mut app.prompt {
                let items_len = match prompt.intent {
                    app::PromptIntent::SearchFile => prompt.file_results.len(),
                    app::PromptIntent::SearchText => prompt.text_results.len(),
                    _ => 0,
                };
                if items_len > 0 {
                    let i = match prompt.selection_state.selected() {
                        Some(i) => if i == 0 { items_len.saturating_sub(1) } else { i - 1 },
                        None => 0,
                    };
                    prompt.selection_state.select(Some(i));
                }
            }
        }
        KeyCode::Down => {
            if let Some(prompt) = &mut app.prompt {
                let items_len = match prompt.intent {
                    app::PromptIntent::SearchFile => prompt.file_results.len(),
                    app::PromptIntent::SearchText => prompt.text_results.len(),
                    _ => 0,
                };
                if items_len > 0 {
                    let i = match prompt.selection_state.selected() {
                        Some(i) => if i >= items_len.saturating_sub(1) { 0 } else { i + 1 },
                        None => 0,
                    };
                    prompt.selection_state.select(Some(i));
                }
            }
        }
        KeyCode::Char(c) => {
            if let Some(p) = &mut app.prompt { p.input.push(c); }
            app.update_prompt_search();
        }
        KeyCode::Backspace => {
            if let Some(p) = &mut app.prompt { let _ = p.input.pop(); }
            app.update_prompt_search();
        }
        _ => {}
    }
}

fn handle_normal_keys(app: &mut App, key: KeyEvent, term_height: u16) {
    let view_height = term_height.saturating_sub(2) as usize;
    let combo = app::KeyCombo { code: key.code, modifiers: key.modifiers };

    if let Some(action) = app.keybindings.get(&combo).cloned() {
        execute_action(app, action, view_height);
    } else if let KeyCode::Char(c) = key.code {
        if !key.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) {
            handle_char(app, c);
        }
    }
}

fn execute_action(app: &mut App, action: Action, view_height: usize) {
    use app::Action::*;
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
        NextSearchResult => app.nex_search_result(),
        Copy => {
            if let Some(doc) = app.active_document_mut() {
                if let Some(text) = doc.buffer.get_selected_text() {
                    if let Ok(mut cb) = arboard::Clipboard::new() {
                        let _ = cb.set_text(text.clone());
                    }
                    app.clipboard = Some(text);
                }
            }
        }
        Cut => {
            if let Some(doc) = app.active_document_mut() {
                if let Some(range) = doc.buffer.get_selection_range() {
                    if let Some(text) = doc.buffer.delete_selection() {
                        if let Ok(mut cb) = arboard::Clipboard::new() {
                            let _ = cb.set_text(text.clone());
                        }
                        app.clipboard = Some(text);
                        app.notify_lsp_incremental(range.start, range.end, "");
                    }
                }
            }
        }
        Paste => {
            let clipboard_text = arboard::Clipboard::new()
                .and_then(|mut cb| cb.get_text())
                .ok()
                .or_else(|| {
                    if std::env::var("WAYLAND_DISPLAY").is_ok() {
                        std::process::Command::new("wl-paste")
                            .arg("--no-newline")
                            .output()
                            .ok()
                            .and_then(|out| if out.status.success() {
                                Some(String::from_utf8_lossy(&out.stdout).into_owned())
                            } else { None })
                    } else {
                        None
                    }
                })
                .or_else(|| app.clipboard.clone());

            if let Some(text) = clipboard_text {
                execute_paste_text(app, text);
            }
        }
        Undo => {
            let mut notify_deltas = None;
            if let Some(doc) = app.active_document_mut() {
                if let Some(deltas) = doc.buffer.undo() {
                    doc.is_dirty = true;
                    notify_deltas = Some(deltas);
                    app.status_msg = Some("Deshacer".to_string());
                } else {
                    app.status_msg = Some("Ya en el estado mas antiguo".to_string());
                }
            }
            if let Some(deltas) = notify_deltas {
                for (start, end, text) in deltas { app.notify_lsp_incremental(start, end, &text); }
            }
        }
        Redo => {
            let mut notify_deltas = None;
            if let Some(doc) = app.active_document_mut() {
                if let Some(deltas) = doc.buffer.redo() {
                    doc.is_dirty = true;
                    notify_deltas = Some(deltas);
                    app.status_msg = Some("Rehacer".to_string());
                } else {
                    app.status_msg = Some("Ya en el estado mas reciente".to_string());
                }
            }
            if let Some(deltas) = notify_deltas {
                for (start, end, text) in deltas { app.notify_lsp_incremental(start, end, &text); }
            }
        }
        Quit => {
            let has_dirty = app.documents.values().any(|d| d.is_dirty);
            if has_dirty {
                app.open_prompt(app::PromptIntent::ConfirmQuit);
            } else {
                app.quit = true;
            }
        }
        Save => app.trigger_save(),
        ScrollViewportUp(_) => if let Some(doc) = app.active_document_mut() { doc.buffer.scroll_viewport_up(view_height / 2) },
        ScrollViewportDown(_) => if let Some(doc) = app.active_document_mut() { doc.buffer.scroll_viewport_down(view_height / 2) },
        ScrollUp1(_) => if let Some(doc) = app.active_document_mut() { doc.buffer.scroll_viewport_up(1) },
        ScrollDown1(_) => if let Some(doc) = app.active_document_mut() { doc.buffer.scroll_viewport_down(1) },
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
                if doc.buffer.delete_selection().is_some() { doc.is_dirty = true; }
            }
        }
        MoveStartOfLine(selecting) => if let Some(doc) = app.active_document_mut() { doc.buffer.move_to_start_of_line(selecting) },
        MoveEndOfLine(selecting) => if let Some(doc) = app.active_document_mut() { doc.buffer.move_to_end_of_line(selecting) },
        PageUp(selecting) => if let Some(doc) = app.active_document_mut() { doc.buffer.move_page_up(selecting, 20) },
        PageDown(selecting) => if let Some(doc) = app.active_document_mut() { doc.buffer.move_page_down(selecting, 20) },
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
        app.completions.clear();
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
                let _ = app.explorer.reload();
            } else {
                app.load_file(path);
            }
        }
    } else if !app.completions.is_empty() {
        if let Some(idx) = app.completion_state.selected() {
            if let Some(comp) = app.completions.get(idx).cloned() {
                if let Some(doc) = app.active_document_mut() {
                    let prefix_len = doc.buffer.get_current_word_prefix().len();
                    for _ in 0..prefix_len { doc.buffer.delete_backwards(); }
                    doc.buffer.insert_str(&comp.insert_text);
                }
                notify_lsp_change(app);
            }
        }
        app.completions.clear();
    } else if let Focus::Window(_) = app.focus {
        let mut notify_data = None;
        if let Some(doc) = app.active_document_mut() {
            let (start, end) = doc.buffer.get_selection_range().map_or(
                (doc.buffer.cursor_char_idx, doc.buffer.cursor_char_idx),
                |r| { doc.buffer.delete_selection(); (r.start, r.end) }
            );

            let indent = doc.buffer.get_current_line_indentation();
            doc.buffer.insert_char('\n');
            doc.buffer.insert_str(&indent);

            let inserted = format!("\n{}", indent);
            notify_data = Some((start, end, inserted));
        }
        if let Some((s, e, ins)) = notify_data {
            app.notify_lsp_incremental(s, e, &ins);
        }
        app.completions.clear();
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

    if let Focus::Window(_) = app.focus {
        let mut notify_data = None;
        let mut trigger_comp = false;

        if let Some(doc) = app.active_document_mut() {
            let (start, end) = doc.buffer.get_selection_range().map_or(
                (doc.buffer.cursor_char_idx, doc.buffer.cursor_char_idx),
                |r| { doc.buffer.delete_selection(); (r.start, r.end) }
            );

            let is_close_brackets = c == ')' || c == '}' || c == ']' || c == '"' || c == '\'';

            if is_close_brackets && doc.buffer.char_at_cursor() == Some(c) {
                doc.buffer.move_cursor_right(false);
                if start != end { notify_data = Some((start, end, String::new())); }
            } else {
                let mut inserted = c.to_string();
                doc.buffer.insert_char(c);

                let closing = match c { '(' => Some(')'), '{' => Some('}'), '[' => Some(']'), '"' => Some('"'), '\'' => Some('\''), _ => None };
                if let Some(close_char) = closing {
                    doc.buffer.insert_char(close_char);
                    doc.buffer.move_cursor_left(false);
                    inserted.push(close_char);
                }
                notify_data = Some((start, end, inserted));
            }
            if c.is_alphanumeric() || c == '.' || c == ':' { trigger_comp = true; }
        }

        if let Some((s, e, text)) = notify_data {
            app.notify_lsp_incremental(s, e, &text);
        }

        if trigger_comp { app.trigger_completion(); }
        else { app.completions.clear(); }
    }
}

fn handle_backspace(app: &mut App) {
    app.status_msg = None;
    if let Focus::Window(_) = app.focus {
        let mut notify_data = None;
        if let Some(doc) = app.active_document_mut() {
            if let Some(range) = doc.buffer.get_selection_range() {
                doc.buffer.delete_selection();
                notify_data = Some((range.start, range.end));
            } else {
                let end_idx = doc.buffer.cursor_char_idx;
                doc.buffer.delete_backwards();
                let start_idx = doc.buffer.cursor_char_idx;

                if start_idx != end_idx { notify_data = Some((start_idx, end_idx)); }
            }
        }
        if let Some((s, e)) = notify_data {
            app.notify_lsp_incremental(s, e, "");
        }
        app.completions.clear();
    }
}

fn handle_tab(app: &mut App) {
    app.status_msg = None;
    if let Focus::Window(_) = app.focus {
        if !app.completions.is_empty() {
            let i = match app.completion_state.selected() {
                Some(i) => if i >= app.completions.len().saturating_sub(1) { 0 } else { i + 1 },
                None => 0,
            };
            app.completion_state.select(Some(i));
        } else if let Some(doc) = app.active_document_mut() {
            doc.buffer.delete_selection();
            doc.buffer.insert_str("    ");
        }
    }
}

fn handle_up(app: &mut App, selecting: bool) {
    if app.focus == Focus::Tree { app.explorer.previous(); }
    else if !app.completions.is_empty() {
        let i = match app.completion_state.selected() {
            Some(i) => if i == 0 { app.completions.len().saturating_sub(1) } else { i - 1 },
            None => 0,
        };
        app.completion_state.select(Some(i));
    } else if let Some(doc) = app.active_document_mut() { doc.buffer.move_cursor_up(selecting); }
}

fn handle_down(app: &mut App, selecting: bool) {
    if app.focus == Focus::Tree { app.explorer.next(); }
    else if !app.completions.is_empty() {
        let i = match app.completion_state.selected() {
            Some(i) => if i >= app.completions.len().saturating_sub(1) { 0 } else { i + 1 },
            None => 0,
        };
        app.completion_state.select(Some(i));
    } else if let Some(doc) = app.active_document_mut() { doc.buffer.move_cursor_down(selecting); }
}

fn notify_lsp_change(app: &mut App) {
    let update_data = if let Some(doc) = app.active_document_mut() {
        doc.is_dirty = true;
        doc.uri.as_ref().map(|uri| {
            doc.version += 1;
            (uri.clone(), doc.buffer.get_full_text(), doc.version, doc.buffer.text.len_lines())
        })
    } else {
        None
    };

    if let Some((uri, text, version, lines)) = update_data {
        if let Some(client) = &mut app.lsp_client {
            if client.is_initialized {
                client.did_change(uri.clone(), text, version);
                let range = lsp_types::Range {
                    start: lsp_types::Position { line: 0, character: 0 },
                    end: lsp_types::Position { line: lines as u32, character: 0 },
                };
                app.pending_inlay_hints_id = Some(client.request_inlay_hints(uri, range));
            }
        }
    }
}

fn process_lsp_messages(app: &mut App) -> bool {
    let mut lsp = match app.lsp_client.take() {
        Some(client) => client,
        None => return false,
    };

    let mut lsp_crashed = false;
    let mut error_msg = None;
    let mut ui_changed = false;

    while let Ok(msg) = lsp.receiver.try_recv() {
        match msg {
            lsp::LspMessage::Diagnostics(params) => {
                app.diagnostics.clear();
                for diag in params.diagnostics {
                    let line = diag.range.start.line as usize;
                    app.diagnostics.entry(line).or_default().push(diag);
                }
                ui_changed = true;
            }
            lsp::LspMessage::Response { id, result } => {
                if id == lsp.init_id && !lsp.is_initialized {
                    lsp.send_initialized();
                    lsp.is_initialized = true;

                    if let Some(doc) = app.get_active_document() {
                        if let (Some(uri), Some(path)) = (&doc.uri, &doc.filepath) {
                            let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
                            let lang_id = match ext {
                                "rs" => "rust",
                                "py" => "python",
                                "c" => "c",
                                "cpp" | "cxx" | "cc" | "h" | "hpp"  => "cpp",
                                _ => ext
                            };
                            lsp.did_open(uri.clone(), doc.buffer.get_full_text(), doc.version, lang_id);
                            app.status_msg = Some(format!("LSP Listo ({})", ext));
                        }
                    }
                }
                if Some(id) == app.pending_inlay_hints_id {
                    if let Ok(hints) = serde_json::from_value::<Vec<lsp_types::InlayHint>>(result.clone()) {
                        app.inlay_hints.clear();
                        for hint in hints {
                            let line = hint.position.line as usize;
                            app.inlay_hints.entry(line).or_default().push(hint);
                        }
                        ui_changed = true;
                    }
                    app.pending_inlay_hints_id = None;
                }
                else if Some(id) == app.pending_completion_id {
                    if result.is_null() {
                        app.completions.clear();
                    } else if let Ok(response) = serde_json::from_value::<lsp_types::CompletionResponse>( result) {
                        let items = match response {
                            lsp_types::CompletionResponse::Array(arr) => arr,
                            lsp_types::CompletionResponse::List(list) => list.items,
                        };

                        if let Some(doc) = app.get_active_document() {
                            let prefix = doc.buffer.get_current_word_prefix();
                            let matcher = SkimMatcherV2::default();

                            let mut scored_items = Vec::new();
                            for i in items {
                                if prefix.is_empty() {
                                    scored_items.push((i, 0));
                                } else if let Some(score) = matcher.fuzzy_match(&i.label, &prefix) {
                                    scored_items.push((i, score));
                                }
                            }
                            scored_items.sort_by(|a, b| {
                                b.1.cmp(&a.1).then_with(|| {
                                    let a_sort = a.0.sort_text.as_ref().unwrap_or(&a.0.label);
                                    let b_sort = b.0.sort_text.as_ref().unwrap_or(&b.0.label);
                                    a_sort.cmp(b_sort)
                                })
                            });

                            app.completions = scored_items.into_iter()
                                .map(|(i, _)| {
                                    let mut insert_text = if let Some(edit) = &i.text_edit {
                                        match edit {
                                            lsp_types::CompletionTextEdit::Edit(e) => e.new_text.clone(),
                                            lsp_types::CompletionTextEdit::InsertAndReplace(e) => e.new_text.clone(),
                                        }
                                    } else if let Some(it) = &i.insert_text {
                                        it.clone()
                                    } else {
                                        i.label.clone()
                                    };

                                    if insert_text.contains('$') {
                                        if let Some(idx) = insert_text.find('(').or(insert_text.find('<')) {
                                            insert_text.truncate(idx);
                                        }
                                    }
                                    crate::app::CompletionOption {
                                        label: i.label,
                                        kind: i.kind,
                                        detail: i.detail,
                                        insert_text,
                                    }
                                })
                                .collect();

                            if !app.completions.is_empty() {
                                app.completion_state.select(Some(0));
                            }
                        }
                    }
                    app.pending_completion_id = None;
                    ui_changed = true;
                }
            }
            lsp::LspMessage::Error(err) => {
                error_msg = Some(format!("Error LSP: {}", err));
                lsp_crashed = true;
                ui_changed = true;
                break;
            }
            _ => {}
        }
    }

    if lsp_crashed {
        app.status_msg = error_msg;
    } else {
        app.lsp_client = Some(lsp);
    }

    ui_changed
}

fn execute_paste_text(app: &mut App, text: String) {
    let mut notify_data = None;
    if let Some(doc) = app.active_document_mut() {
        let (start, end) = doc.buffer.get_selection_range().map_or(
            (doc.buffer.cursor_char_idx, doc.buffer.cursor_char_idx),
            |r| { doc.buffer.delete_selection(); (r.start, r.end) }
        );
        doc.buffer.insert_str(&text);
        notify_data = Some((start, end));
    }
    if let Some((s, e)) = notify_data {
        app.notify_lsp_incremental(s, e, &text)
    }
}