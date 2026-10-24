use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect, Flex},
    style::{Color, Style, Modifier},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, List, ListItem, HighlightSpacing, BorderType, Wrap, Scrollbar, ScrollbarOrientation, ScrollbarState},
};
use crate::app::{App, BufferId, CompletionOption, Focus, Document, SplitNode};
use crate::editor::{grapheme_width, TAB_WIDTH};
use crate::git::FileStatus;
use crate::theme::UiColors;
use lsp_types::InlayHint;
use ratatui::widgets::Clear;
use std::collections::HashMap;

use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;
use syntect::highlighting::{HighlightIterator, HighlightState, Highlighter};
use syntect::parsing::{ParseState, ScopeStack};

/// Renderiza un frame completo del editor y coordina sus capas visuales.
pub fn render(f: &mut Frame, app: &mut App) {
    f.render_widget(Block::default().style(Style::default().bg(app.theme.ui.background).fg(app.theme.ui.foreground)), f.area());
    // Se sigue dibujando mientras se cierra, hasta que la animación termina.
    let term_height = if app.terminal.is_some() {
        let full = ((f.area().height as f64 * app.term_ratio).round() as u16).max(3);
        (full as f64 * app.term_spawn_progress).round() as u16
    } else {
        0
    };
    let root_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(term_height), Constraint::Length(1)])
        .split(f.area());

    let target_tree_width = (f.area().width as f64 * app.tree_ratio).round() as u16;
    let current_tree_width = (target_tree_width as f64 * app.tree_spawn_progress).round() as u16;
    
    let main_layout = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(current_tree_width),
            Constraint::Min(0)
        ])
        .split(root_layout[0]);
        
    if current_tree_width >= 2 {
        render_tree(f, app, main_layout[0]);
    }
    
    let editor_area = main_layout[1];
    
    app.window_areas.clear();
    app.split_areas.clear();
    app.tab_areas.clear();
    
    if app.windows.is_empty() {
        render_intro(f, &app.theme.ui, editor_area);
    } else {
        let layout_root = app.layout.clone();
        render_split_tree(f, app, &layout_root, editor_area, &mut Vec::new());
    }

    app.terminal_area = None;
    if term_height >= 2 {
        render_terminal(f, app, root_layout[1])
    }
    render_status_line(f, app, root_layout[2]);
    render_context_menu(f, app);
    
    if app.show_help || app.help_spawn_progress > 0.01 {
        render_help(f, app);
    } else if app.prompt.is_some() || app.prompt_spawn_progress > 0.01 {
        render_prompt(f, app);
    }
}

fn render_terminal(f: &mut Frame, app: &mut App, area: Rect) {
    let focused = app.focus == Focus::Terminal;
    let block = Block::default()
        .borders(Borders::TOP)
        .title(" Term (f4) ")
        .border_style(Style::default().fg(if focused { app.theme.ui.border_focused } else { app.theme.ui.border }));
    let inner = block.inner(area);
    f.render_widget(block, area);
    // Mientras se cierra sigue visible, pero ya no recibe clics.
    app.terminal_area = app.show_terminal.then_some(area);

    let Some(term) = app.terminal.as_mut() else {return};
    // Redimensionar el PTY en cada frame de la animación haría que la shell redibujase sin parar.
    if app.term_spawn_progress >= 1.0 {
        term.resize(inner.height, inner.width);
    }
    if let Some(pos) = crate::terminal::render(term, f.buffer_mut(), inner)
        && focused
    {
        f.set_cursor_position(pos);
    }
}

/// Recorre el árbol de divisiones y asigna un área de pantalla a cada ventana.
fn render_split_tree(f: &mut Frame, app: &mut App, node: &SplitNode, area: Rect, path: &mut Vec<bool>) {
    match node {
        SplitNode::Leaf(win_id) => {
            app.window_areas.insert(*win_id, area);
            let is_focused = app.focus == Focus::Window(*win_id);

            if let Some(buffer_id) = app.windows.get(win_id).map(|w| w.buffer_id)
                && let Some(mut doc) = app.documents.remove(&buffer_id) {
                    render_document(f, app, &mut doc, buffer_id, area, is_focused);
                    app.documents.insert(buffer_id, doc);
                }
            render_tabs(f, app, *win_id, Rect { height: 1.min(area.height), ..area }, is_focused);
        }
        SplitNode::Split(dir, ratio, a, b) => {
            let layout = Layout::default()
                .direction(*dir)
                .constraints([Constraint::Percentage(*ratio), Constraint::Fill(1)])
                .split(area);

            app.split_areas.push((path.clone(), *dir, area, layout[1]));
            path.push(false);
            render_split_tree(f, app, a, layout[0], path);
            *path.last_mut().unwrap() = true;
            render_split_tree(f, app, b, layout[1], path);
            path.pop();
        }
    }
}

/// Barra de pestañas de una ventana; guarda dónde quedó cada una para los clics.
fn render_tabs(f: &mut Frame, app: &mut App, win_id: usize, area: Rect, is_focused: bool) {
    let Some(win) = app.windows.get(&win_id) else { return };
    let ui = &app.theme.ui;
    let border_color = if is_focused { ui.border_focused } else { ui.border };
    let mut spans = Vec::new();
    let mut x = area.x;
    for &buf_id in &win.tabs {
        let Some(doc) = app.documents.get(&buf_id) else { continue };
        let file_name = doc.filepath.as_ref().map_or("Nuevo".to_string(), |p| p.file_name().unwrap_or_default().to_string_lossy().into_owned());
        let (icon, icon_color) = match doc.ext().as_deref().unwrap_or("") {
            "rs" => (" ", Color::Rgb(222, 90, 44)),
            "py" => ("󰌠 ", Color::Yellow),
            "md" => (" ", Color::LightBlue),
            "c" | "cpp" => (" ", Color::LightBlue),
            "html" | "htm" => (" ", Color::Rgb(227, 79, 38)),
            _ => (" ", ui.tab_fg),
        };
        let active = buf_id == win.buffer_id;
        let tab_bg = if active && is_focused { ui.tab_active_bg } else { ui.tab_bg };
        let name_style = Style::default().fg(if active { ui.tab_fg } else { ui.muted }).bg(tab_bg);
        let name = format!("{} ", file_name);
        let parts = [
            Span::styled(" ", Style::default().bg(tab_bg)),
            Span::styled(icon, Style::default().fg(icon_color).bg(tab_bg)),
            Span::styled(name, if active { name_style.add_modifier(Modifier::ITALIC) } else { name_style }),
            Span::styled(if doc.is_dirty { "●" } else { "×" }, Style::default().fg(if doc.is_dirty { ui.dirty } else { ui.clean }).bg(tab_bg)),
        ];
        let width: u16 = parts.iter().map(|s| s.width() as u16).sum();
        // Las pestañas que no caben no se dibujan, así que tampoco reciben clics.
        if x.saturating_add(width) <= area.right() {
            app.tab_areas.push((win_id, buf_id, Rect { x, width, ..area }, x + width - 1));
        }
        x = x.saturating_add(width + 3);
        spans.extend(parts);
        spans.push(Span::styled(" │ ", Style::default().fg(border_color).bg(ui.tab_bar_bg)));
    }
    f.render_widget(Paragraph::new(Line::from(spans)).style(Style::default().bg(ui.tab_bar_bg)), area);
}

/// Ancho del gutter (números de línea + marca git) para `line_count` líneas.
pub fn gutter_width(line_count: usize) -> u16 {
    line_count.to_string().len() as u16 + 3
}

/// Etiqueta tal como la manda el servidor (`: T`, `name:`…), en una sola línea y
/// recortada para que tipos largos como `impl Iterator<…>` no tapen el código.
fn hint_label(hint: &InlayHint) -> String {
    const MAX_HINT_WIDTH: usize = 30;
    let label: String = match &hint.label {
        lsp_types::InlayHintLabel::String(s) => s.clone(),
        lsp_types::InlayHintLabel::LabelParts(parts) => parts.iter().map(|p| p.value.as_str()).collect(),
    };
    truncate(&label.split_whitespace().collect::<Vec<_>>().join(" "), MAX_HINT_WIDTH)
}

/// El padding va fuera del fondo de la pista para que no parezca parte del código.
fn push_hint(spans: &mut Vec<Span<'static>>, hint: &InlayHint, style: Style) {
    if hint.padding_left == Some(true) { spans.push(Span::raw(" ")); }
    spans.push(Span::styled(hint_label(hint), style));
    if hint.padding_right == Some(true) { spans.push(Span::raw(" ")); }
}

fn hint_width(hint: &InlayHint) -> usize {
    hint_label(hint).width() + usize::from(hint.padding_left == Some(true)) + usize::from(hint.padding_right == Some(true))
}

/// Dibuja una pestaña y el contenido visible de un documento con interpolación de puntero.
fn render_document(f: &mut Frame, app: &mut App, doc: &mut Document, buffer_id: BufferId, area: Rect, is_focused: bool) {
    let edit_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(0)])
        .split(area);

    let text_area = edit_layout[1];

    let max_lines = doc.buffer.text.len_lines();
    let gutter = gutter_width(max_lines).min(text_area.width);
    let gutter_num_width = gutter_width(max_lines) as usize - 3;
    // El gutter va en su propia área para que el scroll horizontal no lo desplace.
    let gutter_area = Rect { width: gutter, ..text_area };
    // La última columna queda para la barra de scroll.
    let content_area = Rect { x: text_area.x + gutter, width: text_area.width.saturating_sub(gutter + 1), ..text_area };

    let view_height = text_area.height as usize;
    let view_width = content_area.width as usize;

    if is_focused {
        let cursor_changed = doc.buffer.cursor_char_idx != doc.buffer.last_sync_cursor_idx;
        let resize_changed = view_width != doc.buffer.last_view_width || view_height != doc.buffer.last_view_height;

        if cursor_changed || resize_changed || doc.buffer.force_sync_cursor {
            doc.buffer.ensure_cursor_visible(view_width, view_height);
            doc.buffer.last_sync_cursor_idx = doc.buffer.cursor_char_idx;
            doc.buffer.last_view_width = view_width;
            doc.buffer.last_view_height = view_height;
            doc.buffer.force_sync_cursor = false;
        }
    }

    let start_line = doc.buffer.scroll_y;
    let end_line = (start_line + view_height).min(max_lines);

    let uri_key = doc.uri.as_ref().map(|u| u.as_str().to_string());
    // Tramos subrayados por línea (columnas UTF-16, errores primero para que ganen al
    // solaparse) y el mensaje más grave que empieza en cada línea.
    let mut diag_ranges: HashMap<usize, Vec<(usize, usize, Color)>> = HashMap::new();
    let mut diag_msgs: HashMap<usize, (bool, Color, String)> = HashMap::new();
    if let Some(per_server) = uri_key.as_ref().and_then(|k| app.diagnostics.get(k)) {
        for d in per_server.values().flatten() {
            let (is_error, color) = match d.severity {
                Some(lsp_types::DiagnosticSeverity::ERROR) => (true, app.theme.ui.error),
                Some(lsp_types::DiagnosticSeverity::WARNING) => (false, app.theme.ui.warning),
                _ => continue,
            };
            let (from, to) = (d.range.start, d.range.end);
            for line in (from.line as usize).max(start_line)..=(to.line as usize).min(end_line) {
                let a = if line == from.line as usize { from.character as usize } else { 0 };
                let b = if line == to.line as usize { to.character as usize } else { usize::MAX };
                // Un rango vacío marca al menos el carácter donde empieza.
                let span = (a, b.max(a + 1), color);
                let v = diag_ranges.entry(line).or_default();
                if is_error { v.insert(0, span) } else { v.push(span) }
            }
            let msg = (is_error, color, d.message.lines().next().unwrap_or("").to_string());
            let entry = diag_msgs.entry(from.line as usize).or_insert_with(|| msg.clone());
            if is_error && !entry.0 { *entry = msg; }
        }
    }
    let mut hints_by_line: HashMap<usize, Vec<&InlayHint>> = HashMap::new();
    if let Some(per_server) = uri_key.as_ref().and_then(|k| app.inlay_hints.get(k)) {
        for h in per_server.values().flatten() {
            hints_by_line.entry(h.position.line as usize).or_default().push(h);
        }
    }
    for hints in hints_by_line.values_mut() { hints.sort_by_key(|h| h.position.character); }

    let syntax = doc.filepath.as_ref()
        .and_then(|p| p.extension())
        .and_then(|e| e.to_str())
        .and_then(|e| app.syntax_set.find_syntax_by_extension(e))
        .unwrap_or_else(|| app.syntax_set.find_syntax_plain_text());

    let highlighter = Highlighter::new(&app.theme.syntect);
    let ui = app.theme.ui.clone();

    // Se parte del último estado conocido para que comentarios y strings
    // multilínea se coloreen bien aunque empiecen fuera de la vista.
    // shortcut: guarda un estado por línea; si la memoria pesa en archivos enormes, guardar cada N líneas.
    if doc.buffer.syntax_cache.is_empty() {
        doc.buffer.syntax_cache.push((ParseState::new(syntax), HighlightState::new(&highlighter, ScopeStack::new())));
    }
    let first_line = start_line.min(doc.buffer.syntax_cache.len() - 1);
    let (mut parse_state, mut hl_state) = doc.buffer.syntax_cache[first_line].clone();

    let selection_range = doc.buffer.get_selection_range();
    let mut gutter_lines = Vec::with_capacity(view_height);
    let mut lines = Vec::with_capacity(view_height);

    let search_matches: &[usize] = if app.search_buffer == Some(buffer_id) { &app.text_search_results } else { &[] };
    let search_query_len = app.last_search_query.as_ref().map_or(0, |q| q.chars().count());
    let active_search_idx = search_matches.get(app.current_search_idx).copied();

    for line_idx in first_line..end_line {
        let line_str = doc.buffer.text.line(line_idx).to_string();
        let ops = parse_state.parse_line(&line_str, &app.syntax_set).unwrap_or_default();
        let ranges: Vec<(syntect::highlighting::Style, &str)> =
            HighlightIterator::new(&mut hl_state, &ops, &line_str, &highlighter).collect();
        if line_idx + 1 == doc.buffer.syntax_cache.len() {
            doc.buffer.syntax_cache.push((parse_state.clone(), hl_state.clone()));
        }
        if line_idx < start_line { continue; }

        let hints_for_line = hints_by_line.get(&line_idx);
        let line_start_char = doc.buffer.text.line_to_char(line_idx);
        let line_end_char = line_start_char + line_str.chars().count();
        let matches_in_line: Vec<usize> = search_matches.iter()
            .filter(|&&idx| idx >= line_start_char && idx < line_end_char)
            .copied()
            .collect();

        let (git_sym, git_color) = match app.git_ctx.line_statuses.get(&line_idx) {
            Some(crate::git::GitLineStatus::Added) => ("▌", ui.git_added),
            Some(crate::git::GitLineStatus::Modified) => ("▌", ui.git_modified),
            Some(crate::git::GitLineStatus::Deleted) => ("_", ui.git_deleted),
            None => (" ", Color::Reset),
        };
        gutter_lines.push(Line::from(vec![
            Span::styled(format!(" {:>w$} ", line_idx + 1, w = gutter_num_width), Style::default().fg(ui.line_number)),
            Span::styled(git_sym, Style::default().fg(git_color)),
        ]));

        let hint_style = Style::default().fg(ui.inlay_hint_fg).bg(ui.inlay_hint_bg);
        let mut spans = Vec::new();
        let mut current_global_idx = line_start_char;
        let mut current_utf16_col = 0;
        let mut visual_col = 0;
        let mut in_leading_ws = true;

        for (style, text) in ranges {
            let clean_text = text.replace(['\n', '\r'], "");
            if clean_text.is_empty() { continue; }

            let base_style = app.theme.syntax_style(style);

            let mut segment = String::new();
            let mut active_style = base_style;
            let mut is_first = true;

            for grapheme in clean_text.graphemes(true) {
                for hint in hints_for_line.into_iter().flatten() {
                    if hint.position.character as usize == current_utf16_col {
                        if !segment.is_empty() {
                            spans.push(Span::styled(std::mem::take(&mut segment), active_style));
                        }
                        push_hint(&mut spans, hint, hint_style);
                    }
                }

                let g_chars = grapheme.chars().count();
                let g_utf16 = grapheme.encode_utf16().count();
                let g_width = grapheme_width(grapheme, visual_col);

                let mut char_style = base_style;

                if let Some(&(_, _, color)) = diag_ranges.get(&line_idx).into_iter().flatten()
                    .find(|(a, b, _)| (*a..*b).contains(&current_utf16_col)) {
                    char_style = char_style.add_modifier(Modifier::UNDERLINED).underline_color(color);
                }

                if search_query_len > 0
                    && let Some(&match_start) = matches_in_line.iter()
                        .find(|&&m| current_global_idx >= m && current_global_idx < m + search_query_len)
                    {
                        char_style = if Some(match_start) == active_search_idx {
                            char_style.bg(ui.search_active_bg).fg(ui.search_active_fg)
                        } else {
                            char_style.bg(ui.search_bg).fg(ui.search_fg)
                        };
                    }

                if let Some(ref sel) = selection_range
                    && current_global_idx >= sel.start && current_global_idx < sel.end {
                        char_style = char_style.bg(ui.selection_bg).fg(ui.selection_fg);
                    }

                if is_first {
                    active_style = char_style;
                    is_first = false;
                } else if char_style != active_style {
                    if !segment.is_empty() {
                        spans.push(Span::styled(std::mem::take(&mut segment), active_style));
                    }
                    active_style = char_style;
                }

                if in_leading_ws && grapheme.chars().all(|c| c == ' ' || c == '\t') {
                    for _ in 0..g_width {
                        if visual_col % TAB_WIDTH == 0 {
                            if !segment.is_empty() {
                                spans.push(Span::styled(std::mem::take(&mut segment), active_style));
                            }
                            spans.push(Span::styled("│", Style::default().fg(ui.indent_guide)));
                        } else {
                            segment.push(' ');
                        }
                        visual_col += 1;
                    }
                } else {
                    in_leading_ws = false;
                    if grapheme == "\t" {
                        segment.extend(std::iter::repeat_n(' ', g_width));
                    } else {
                        segment.push_str(grapheme);
                    }
                    visual_col += g_width;
                }

                current_global_idx += g_chars;
                current_utf16_col += g_utf16;
            }
            if !segment.is_empty() { spans.push(Span::styled(segment, active_style)); }
        }

        // Pistas situadas al final de la línea (p. ej. tipos de cadenas de métodos).
        for hint in hints_for_line.into_iter().flatten() {
            if hint.position.character as usize >= current_utf16_col {
                push_hint(&mut spans, hint, hint_style);
            }
        }
        if let Some((is_error, color, msg)) = diag_msgs.get(&line_idx) {
            let icon = if *is_error { "■" } else { "▲" };
            spans.push(Span::styled(format!("    {icon} {msg}"), Style::default().fg(*color).add_modifier(Modifier::ITALIC)));
        }
        lines.push(Line::from(spans));
    }

    // main.rs usa la última columna de la ventana para saltar con el ratón.
    let mut scroll_state = ScrollbarState::new(max_lines).position(start_line).viewport_content_length(view_height);
    let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
        .begin_symbol(None).end_symbol(None)
        .track_symbol(Some("│")).thumb_symbol("┃")
        .track_style(Style::default().fg(ui.indent_guide)).thumb_style(Style::default().fg(ui.muted));
    f.render_stateful_widget(scrollbar, text_area, &mut scroll_state);
    f.render_widget(Paragraph::new(gutter_lines), gutter_area);
    f.render_widget(Paragraph::new(lines).scroll((0, doc.buffer.scroll_x as u16)), content_area);

    if !is_focused { return; }

    let cursor_y = doc.buffer.text.char_to_line(doc.buffer.cursor_char_idx);
    let mut visual_cursor_x = doc.buffer.char_idx_to_visual_col(doc.buffer.cursor_char_idx);

    let utf16_col = doc.buffer.get_lsp_position().1 as usize;
    let line_end = doc.buffer.text.line_to_char(cursor_y) + doc.buffer.line_len_without_nl(cursor_y);
    let line_utf16_len = doc.buffer.get_lsp_position_utf16_at(line_end).1 as usize;
    for hint in hints_by_line.get(&cursor_y).into_iter().flatten() {
        let pos = hint.position.character as usize;
        // Las pistas de fin de línea se dibujan tras el texto: no desplazan un cursor en EOL.
        if pos <= utf16_col && pos < line_utf16_len {
            visual_cursor_x += hint_width(hint);
        }
    }

    let screen_x = content_area.x + visual_cursor_x.saturating_sub(doc.buffer.scroll_x) as u16;
    let screen_y = text_area.y + cursor_y.saturating_sub(doc.buffer.scroll_y) as u16;

    let sf_x = screen_x as f64;
    let sf_y = screen_y as f64;

    if app.visual_cursor_x < 0.0 {
        app.visual_cursor_x = sf_x;
        app.visual_cursor_y = sf_y;
        app.target_cursor_x = sf_x;
        app.target_cursor_y = sf_y;
    } else if (app.target_cursor_x - sf_x).abs() > 0.01 || (app.target_cursor_y - sf_y).abs() > 0.01 {
        app.target_cursor_x = sf_x;
        app.target_cursor_y = sf_y;
    }

    if app.completions_spawn_progress > 0.0 {
        let comp_width = 85;
        let max_comp_height = app.completions.len().clamp(1, 10) as u16 + 2;
        let comp_height = (max_comp_height as f64 * app.completions_spawn_progress).ceil() as u16;

        if comp_height > 0 {
            let popup_y = if screen_y + 1 + max_comp_height <= text_area.bottom() {
                screen_y + 1
            } else {
                screen_y.saturating_sub(comp_height)
            };

            let safe_screen_x = screen_x.min(text_area.right().saturating_sub(comp_width));
            let popup_area = Rect::new(safe_screen_x, popup_y, comp_width, comp_height).intersection(f.area());

            let inner_width = comp_width.saturating_sub(2) as usize;
            let items: Vec<ListItem> = app.completions.iter().map(|c| completion_line(c, &ui, inner_width)).collect();

            let list = List::new(items)
                .block(Block::default().borders(Borders::ALL).border_type(BorderType::Rounded).style(Style::default().bg(ui.popup_bg)))
                .highlight_style(Style::default().bg(ui.popup_selected_bg).fg(ui.popup_selected_fg).add_modifier(Modifier::BOLD));

            f.render_widget(Clear, popup_area);
            if popup_area.height >= 2 {
                f.render_stateful_widget(list, popup_area, &mut app.completion_state);
                let doc = app.completion_state.selected().and_then(|i| app.completions.get(i)).and_then(|c| c.documentation.as_deref());
                if let Some(doc) = doc.filter(|d| !d.trim().is_empty()) {
                    render_completion_doc(f, &ui, doc, popup_area);
                }
            }
        }
    }

    // El cursor siempre se renderiza y sigue libre para interpolación
    let cursor_in_view = cursor_y >= doc.buffer.scroll_y && cursor_y < doc.buffer.scroll_y + view_height
        && visual_cursor_x >= doc.buffer.scroll_x && visual_cursor_x < doc.buffer.scroll_x + view_width;
    if cursor_in_view && selection_range.is_none() && content_area.width > 0 {
        let safe_x = (app.visual_cursor_x.round() as u16).clamp(content_area.x, content_area.right() - 1);
        let safe_y = (app.visual_cursor_y.round() as u16).clamp(text_area.y, text_area.bottom().saturating_sub(1));
        f.set_cursor_position((safe_x, safe_y));
    }
}

/// Panel de documentación junto al popup: a la derecha si cabe, si no a la izquierda.
fn render_completion_doc(f: &mut Frame, ui: &UiColors, doc: &str, popup: Rect) {
    const MAX_W: u16 = 60;
    let screen = f.area();
    let right_room = screen.right().saturating_sub(popup.right());
    let (x, w) = if right_room >= 20 {
        (popup.right(), right_room.min(MAX_W))
    } else if popup.x >= 20 {
        let w = popup.x.min(MAX_W);
        (popup.x - w, w)
    } else {
        return;
    };
    let h = popup.height.max(12).min(screen.bottom().saturating_sub(popup.y));
    let area = Rect::new(x, popup.y, w, h);

    let block = Block::default().borders(Borders::ALL).border_type(BorderType::Rounded)
        .border_style(Style::default().fg(ui.doc_border))
        .style(Style::default().bg(ui.doc_bg).fg(ui.doc_fg));
    f.render_widget(Clear, area);
    f.render_widget(Paragraph::new(doc_lines(doc, ui)).block(block).wrap(Wrap { trim: false }), area);
}

/// Markdown mínimo: bloques de código y títulos con su color; el resto tal cual.
fn doc_lines<'a>(md: &'a str, ui: &UiColors) -> Vec<Line<'a>> {
    let mut in_code = false;
    md.lines().filter_map(|l| {
        if l.trim_start().starts_with("```") {
            in_code = !in_code;
            return None;
        }
        Some(if in_code {
            Line::styled(l, Style::default().fg(ui.doc_code))
        } else if l.starts_with('#') {
            Line::styled(l.trim_start_matches('#').trim_start(), Style::default().fg(ui.doc_heading).add_modifier(Modifier::BOLD))
        } else {
            Line::raw(l)
        })
    }).collect()
}

fn kind_icon(kind: Option<lsp_types::CompletionItemKind>, ui: &UiColors) -> (&'static str, Color) {
    use lsp_types::CompletionItemKind as K;
    match kind {
        Some(K::METHOD) => ("\u{ea8c}", ui.kind_method),
        Some(K::FUNCTION | K::CONSTRUCTOR) => ("\u{f0295}", ui.kind_function),
        Some(K::CLASS | K::STRUCT | K::INTERFACE | K::ENUM | K::TYPE_PARAMETER) => ("\u{eb5b}", ui.kind_type),
        Some(K::VARIABLE) => ("\u{ea88}", ui.kind_variable),
        Some(K::FIELD | K::PROPERTY) => ("\u{eb65}", ui.kind_field),
        Some(K::CONSTANT | K::ENUM_MEMBER | K::VALUE) => ("\u{eb5d}", ui.kind_constant),
        Some(K::MODULE | K::FILE | K::FOLDER | K::REFERENCE) => ("\u{ea8b}", ui.kind_module),
        Some(K::KEYWORD | K::OPERATOR) => ("\u{eb62}", ui.kind_keyword),
        Some(K::SNIPPET) => ("\u{eb66}", ui.kind_snippet),
        _ => ("\u{f09a8}", ui.kind_other),
    }
}

/// Recorta a `max` columnas añadiendo "…" si sobra texto.
fn truncate(s: &str, max: usize) -> String {
    if s.width() <= max { return s.to_string(); }
    let mut out = String::new();
    for g in s.graphemes(true) {
        if out.width() + g.width() + 1 > max { break; }
        out.push_str(g);
    }
    out + "…"
}

/// Fila del popup: icono de tipo, etiqueta con lo escrito resaltado, firma y,
/// alineado a la derecha, el tipo/detalle (como en los IDEs de JetBrains).
fn completion_line<'a>(c: &CompletionOption, ui: &UiColors, width: usize) -> ListItem<'a> {
    let (icon, icon_color) = kind_icon(c.kind, ui);
    let label_style = Style::default().fg(ui.popup_fg);
    let match_style = label_style.fg(ui.popup_match).add_modifier(Modifier::BOLD);
    let mut spans = vec![
        Span::styled(format!(" {} ", icon), Style::default().fg(icon_color).bg(ui.popup_kind_bg)),
        Span::raw(" "),
    ];
    let mut used = 4;

    let label = truncate(&c.label, width.saturating_sub(used) * 2 / 3);
    let mut run = String::new();
    let mut run_matched = false;
    for (i, ch) in label.chars().enumerate() {
        let m = c.matched.contains(&i);
        if m != run_matched && !run.is_empty() {
            spans.push(Span::styled(std::mem::take(&mut run), if run_matched { match_style } else { label_style }));
        }
        run_matched = m;
        run.push(ch);
    }
    spans.push(Span::styled(run, if run_matched { match_style } else { label_style }));
    used += label.width();

    let dim = Style::default().fg(ui.popup_detail);
    if let Some(ld) = c.label_detail.as_deref().filter(|s| !s.is_empty()) {
        let ld = truncate(&ld.replace('\n', " "), width.saturating_sub(used));
        used += ld.width();
        spans.push(Span::styled(ld, dim));
    }
    let room = width.saturating_sub(used + 2);
    if let Some(detail) = c.detail.as_deref().filter(|s| !s.is_empty() && room > 3) {
        let detail = truncate(&detail.replace('\n', " "), room);
        spans.push(Span::raw(" ".repeat(width - used - detail.width() - 1)));
        spans.push(Span::styled(detail, dim.add_modifier(Modifier::ITALIC)));
    }
    ListItem::new(Line::from(spans))
}

fn render_help(f: &mut Frame, app: &App) {
    if app.help_spawn_progress < 0.01 { return; }
    let ui = &app.theme.ui;
    let key = Style::default().fg(ui.key_hint);
    let section = Style::default().fg(ui.muted);

    let help_text = vec![
        Line::from(Span::styled(" COMANDOS WYVERN ", Style::default().add_modifier(Modifier::BOLD).fg(ui.accent))),
        Line::from(""),
        Line::from(Span::styled(" [GENERAL & ARCHIVOS] ", section)),
        Line::from(vec![Span::styled(" F1             ", key), Span::raw("- Mostrar/Ocultar esta ayuda")]),
        Line::from(vec![Span::styled(" F2             ", key), Span::raw("- Explorar archivos (Tree)")]),
        Line::from(vec![Span::styled(" Ctrl + S       ", key), Span::raw("- Guardar documento actual")]),
        Line::from(vec![Span::styled(" Ctrl + Q       ", key), Span::raw("- Salir del editor")]),
        Line::from(""),
        Line::from(Span::styled(" [BÚSQUEDA] ", section)),
        Line::from(vec![Span::styled(" Ctrl + F       ", key), Span::raw("- Buscar archivo en el proyecto (Fuzzy)")]),
        Line::from(vec![Span::styled(" Ctrl + T       ", key), Span::raw("- Buscar texto en el proyecto (Fuzzy)")]),
        Line::from(vec![Span::styled(" F3             ", key), Span::raw("- Saltar a la siguiente coincidencia de texto")]),
        Line::from(vec![Span::styled(" F4             ", key), Span::raw("- Abrir / enfocar / ocultar la terminal")]),
        Line::from(vec![Span::styled(" F5             ", key), Span::raw("- Cambiar de tema (themes/*.json)")]),
        Line::from(""),
        Line::from(Span::styled(" [EDICIÓN] ", section)),
        Line::from(vec![Span::styled(" Ctrl + C/X/V   ", key), Span::raw("- Copiar, Cortar, Pegar")]),
        Line::from(vec![Span::styled(" Ctrl + Z/Y     ", key), Span::raw("- Deshacer, Rehacer")]),
        Line::from(vec![Span::styled(" Tab            ", key), Span::raw("- Indentar (4 espacios) / Navegar Autocompletado")]),
        Line::from(vec![Span::styled(" Supr / Backsp  ", key), Span::raw("- Borrar texto / Eliminar archivo en el Tree")]),
        Line::from(vec![Span::styled(" Enter          ", key), Span::raw("- Salto de línea / Confirmar Prompt / Abrir Archivo")]),
        Line::from(vec![Span::styled(" Esc            ", key), Span::raw("- Cancelar / Quitar selección / Cerrar modal")]),
        Line::from(""),
        Line::from(Span::styled(" [EXPLORADOR] ", section)),
        Line::from(vec![Span::styled(" a / A          ", key), Span::raw("- Nuevo archivo / carpeta (a/b.rs crea subcarpetas)")]),
        Line::from(vec![Span::styled(" r / d          ", key), Span::raw("- Renombrar / Eliminar la entrada seleccionada")]),
        Line::from(vec![Span::styled(" Clic derecho   ", key), Span::raw("- Menú con las mismas opciones")]),
        Line::from(""),
        Line::from(Span::styled(" [SPLITS & FOCO] ", section)),
        Line::from(vec![Span::styled(" Alt + V / H    ", key), Span::raw("- Crear Split Vertical / Horizontal")]),
        Line::from(vec![Span::styled(" Alt + W        ", key), Span::raw("- Cerrar el Split activo")]),
        Line::from(vec![Span::styled(" Alt + I/K/J/L  ", key), Span::raw("- Mover el foco (Arriba/Abajo/Izq/Der)")]),
        Line::from(""),
        Line::from(Span::styled(" [NAVEGACIÓN] ", section)),
        Line::from(vec![Span::styled(" Flechas        ", key), Span::raw("- Mover cursor (+ Shift para seleccionar texto)")]),
        Line::from(vec![Span::styled(" Inicio / Fin   ", key), Span::raw("- Ir al Principio / Final de la línea (+ Shift)")]),
        Line::from(vec![Span::styled(" RePág / AvPág  ", key), Span::raw("- Subir / Bajar página (+ Shift)")]),
        Line::from(vec![Span::styled(" Ctrl + U/D     ", key), Span::raw("- Scroll rápido Arriba / Abajo (Mueve el Viewport)")]),
    ];

    let block = Block::default()
        .borders(Borders::ALL)
        .style(Style::default().bg(ui.help_bg))
        .border_style(Style::default().fg(ui.accent));

    let paragraph = Paragraph::new(help_text).block(block).alignment(ratatui::layout::Alignment::Left);

    let target_height = 39;
    let width = 75;

    // Animación de expansión modal centrada
    let current_height = (target_height as f64 * app.help_spawn_progress).ceil() as u16;
    let center_y = f.area().height.saturating_sub(current_height) / 2;
    let center_x = f.area().width.saturating_sub(width) / 2;

    let render_area = Rect::new(center_x, center_y, width, current_height).intersection(f.area());

    f.render_widget(Clear, render_area);
    if render_area.height >= 2 {
        f.render_widget(paragraph, render_area);
    }
}

fn render_status_line(f: &mut Frame, app: &App, area: Rect) {
    const BAR: usize = 12;
    let ui = &app.theme.ui;
    let base = Style::default().bg(ui.status_bg).fg(ui.status_fg);
    let mode_style = Style::default().bg(ui.status_mode_bg).fg(ui.status_mode_fg).add_modifier(Modifier::BOLD);
    let mode = match app.focus {
        Focus::Window(_) => " EDIT ",
        Focus::Tree => " TREE ",
        Focus::Terminal => " TERM ",
    };
    let doc = app.get_active_document();

    let mut left = vec![Span::styled(mode, mode_style)];
    if app.git_ctx.is_repo {
        let (adds, mods, dels) = app.git_ctx.stats;
        let mut git = format!(" \u{e725} {} ", app.git_ctx.branch.as_deref().unwrap_or("detached"));
        for (n, sym) in [(adds, '+'), (mods, '~'), (dels, '-')] {
            if n > 0 { git += &format!("{sym}{n} "); }
        }
        left.push(Span::styled(git, Style::default().bg(ui.status_git_bg).fg(ui.status_git_fg)));
    }
    if let Some(doc) = doc {
        let name = doc.filepath.as_ref().and_then(|p| p.file_name()).map_or("[sin nombre]".into(), |n| n.to_string_lossy());
        left.push(Span::styled(format!(" {name}"), base.add_modifier(Modifier::BOLD)));
        if doc.is_dirty { left.push(Span::styled(" \u{25cf}", base.fg(ui.dirty))); }
        left.push(Span::styled(" ", base));
    }

    // Lo que el servidor está haciendo tiene prioridad sobre el último mensaje.
    let mut middle = Vec::new();
    if let Some((server_id, _, task)) = app.lsp_progress.last() {
        let name = app.lsp_clients.get(server_id).map_or("LSP", |c| c.name.as_str());
        middle.push(Span::styled(format!(" {name}: {}", task.title), base.fg(ui.accent)));
        if let Some(pct) = task.percentage {
            let filled = pct.min(100) as usize * BAR / 100;
            middle.push(Span::styled(format!(" {}", "\u{2588}".repeat(filled)), base.fg(ui.accent)));
            middle.push(Span::styled(format!("{} {pct}%", "\u{2591}".repeat(BAR - filled)), base.fg(ui.muted)));
        }
        if let Some(msg) = &task.message { middle.push(Span::styled(format!(" {msg}"), base.fg(ui.status_ok))); }
        if app.lsp_progress.len() > 1 { middle.push(Span::styled(format!(" (+{})", app.lsp_progress.len() - 1), base.fg(ui.muted))); }
    } else if let Some(msg) = &app.status_msg {
        middle.push(Span::styled(format!(" {msg}"), base));
    }

    let mut right = Vec::new();
    if let Some(doc) = doc {
        let (mut errs, mut warns) = (0, 0);
        let diags = doc.uri.as_ref().and_then(|u| app.diagnostics.get(u.as_str()));
        for d in diags.into_iter().flat_map(|m| m.values()).flatten() {
            match d.severity {
                Some(lsp_types::DiagnosticSeverity::ERROR) => errs += 1,
                Some(lsp_types::DiagnosticSeverity::WARNING) => warns += 1,
                _ => {}
            }
        }
        right.push(Span::styled(format!(" \u{f057} {errs}"), base.fg(if errs > 0 { ui.error } else { ui.status_ok })));
        right.push(Span::styled(format!(" \u{f071} {warns} "), base.fg(if warns > 0 { ui.warning } else { ui.status_ok })));

        let ext = doc.ext();
        let mut servers: Vec<&str> = app.lsp_clients.values()
            .filter(|c| c.is_initialized && ext.as_deref().is_some_and(|e| c.handles(e)))
            .map(|c| c.name.as_str())
            .collect();
        servers.sort_unstable();
        let lsp = if servers.is_empty() { "sin LSP".to_string() } else { servers.join(", ") };
        let lang = ext.as_deref().map_or("texto", |e| app.config.language_id(e));
        right.push(Span::styled(format!(" {lsp} \u{2502} {lang} "), base.fg(ui.status_ok)));

        let line = doc.buffer.text.char_to_line(doc.buffer.cursor_char_idx);
        let col = doc.buffer.cursor_char_idx - doc.buffer.text.line_to_char(line) + 1;
        let pct = (line + 1) * 100 / doc.buffer.text.len_lines().max(1);
        right.push(Span::styled(format!(" {}:{col} {pct}% ", line + 1), mode_style));
    }

    let (left, middle, right) = (Line::from(left), Line::from(middle), Line::from(right));
    let [l, m, r] = Layout::horizontal([
        Constraint::Length(left.width() as u16),
        Constraint::Min(0),
        Constraint::Length(right.width() as u16),
    ]).areas(area);
    f.render_widget(Block::default().style(base), area);
    f.render_widget(Paragraph::new(left), l);
    f.render_widget(Paragraph::new(middle), m);
    f.render_widget(Paragraph::new(right), r);
}

fn render_intro(f: &mut Frame, ui: &UiColors, area: Rect) {
    let outer_block = Block::default().borders(Borders::ALL).border_style(Style::default().fg(ui.border));
    let inner_area = outer_block.inner(area);
    f.render_widget(outer_block, area);

    let ascii_logo = r#"
    ⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀
⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⢀⣠⡤⠂⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⢀⣴⣷⣶⣦⣄⣀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀
⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⢀⣤⣶⣿⣿⠟⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⢀⣴⣿⣿⣿⣿⣿⣿⣿⣷⣦⣄⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀
⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⣠⣾⣿⣿⣿⣿⣥⣤⣤⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠁⠀⠀⣰⣿⣿⡿⣿⣿⣿⣿⣿⣷⣄⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀
⠀⠀⠀⠀⠀⠀⠀⠀⣠⣾⣿⣿⣿⣿⡿⠟⠛⠉⠀⠀⠀⢀⡀⠀⠀⢲⣄⠀⠀⠀⠀⠀⠀⠀⠀⠀⠰⠛⠉⠁⠀⢹⣿⣿⡿⠟⠻⣿⣷⣄⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀
⠀⠀⠀⠀⠀⠀⢀⣼⣿⣿⠛⠿⣿⡏⠀⠀⠀⠀⠀⠀⠀⠀⠙⢷⣦⣤⣻⣷⣄⡀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠈⣿⣿⣷⡄⠀⠹⣿⣿⣧⡀⠀⠀⠀⠀⠀⠀⠀⠀
⠀⠀⠀⠀⠀⢀⣾⣿⣿⠃⠀⣰⣿⣧⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⢙⣿⣿⣿⣿⣿⣦⡀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⣿⣿⣿⣿⡄⠀⠹⣿⣿⣷⡀⠀⠀⠀⠀⠀⠀⠀
⠀⠀⠀⠀⢀⣾⣿⣿⡏⠀⢠⣿⣿⣿⡀⠀⠀⠀⠀⠀⠀⠀⠀⣴⣿⣿⣿⣿⣿⣿⣿⣿⣿⣶⣤⣄⡀⠀⠀⠀⠀⠀⣿⣿⣿⣿⣿⡀⠀⢻⣿⣿⣿⡄⠀⠀⠀⠀⠀⠀
⠀⠀⠀⠀⣼⣿⣿⣿⠀⠀⣾⣿⣿⣿⣇⠀⠀⠀⠀⠀⠀⠀⢸⣿⣿⣿⣿⣿⣿⣿⡟⠉⢻⣿⣿⣿⣿⣷⣶⣤⠀⠀⣿⣿⣿⣿⣿⣷⠀⠀⢿⣿⣿⣿⡀⠀⠀⠀⠀⠀
⠀⠀⠀⢰⣿⣿⣿⡇⠀⢰⣿⣿⣿⣿⣿⡄⠀⠀⠀⠀⠀⠀⢸⣿⣿⣿⣿⣿⣿⣿⣿⣶⣿⣿⠇⢻⡏⠻⣿⠃⠀⢰⣿⣿⣿⣿⣿⣿⣧⠀⠈⣿⣿⣿⣷⠀⠀⠀⠀⠀
⠀⠀⠀⣿⣿⣿⣿⠁⠀⣼⣿⣿⣿⣿⣿⣿⣄⠀⠀⠀⠀⠀⢸⣿⣿⣿⣿⣿⣿⣿⣯⠛⠛⢿⣶⣤⣀⡀⠁⠀⢀⣾⣿⣿⣿⣿⣿⣿⣿⡆⠀⠸⣿⣿⣿⣇⠀⠀⠀⠀
⠀⠀⢰⣿⣿⣿⡿⠀⠀⣿⣿⣿⣿⣿⣿⣿⣿⣦⡀⠀⠀⠀⠈⣿⣿⣿⣿⣿⣿⣿⣿⣧⡀⠀⠙⠟⠋⠀⠀⢀⣾⣿⣿⣿⣿⣿⣿⣿⣿⣿⠀⠀⢿⣿⣿⣿⡄⠀⠀⠀
⠀⠀⢸⣿⣿⣿⡇⠀⢸⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣦⣀⠀⠀⠸⣿⣿⣿⣿⣿⣿⣿⣿⣿⣦⠀⠀⠀⠀⣠⣾⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣇⠀⠘⣿⣿⣿⣇⠀⠀⠀
⠀⠀⣸⣿⣿⣿⡇⠀⢸⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣷⣦⣤⣹⣿⣿⣿⣿⣿⣿⣿⣿⣿⣧⣠⣴⣾⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⡀⠀⢻⣿⣿⣿⠀⠀⠀
⠀⠀⣿⣿⣿⣿⠁⠀⣼⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣇⠀⠘⣿⣿⣿⡇⠀⠀
⠀⠀⣿⣿⣿⣿⠀⠀⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⠀⠀⢿⣿⣿⣇⠀⠀
⠀⠀⣿⣿⣿⣿⠀⠀⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⡇⠀⢸⣿⣿⣿⠀⠀
⠀⠀⢹⣿⣿⣿⠀⠀⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⠃⠀⠀⠙⢿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣷⠀⠀⣿⣿⣿⠀⠀
⠀⠀⢸⣿⣿⣿⠀⠀⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⠟⠉⠁⠈⠉⢻⣿⣿⣿⣿⣿⣿⣿⣿⣿⡗⠀⠀⠀⠀⠈⢿⣿⣿⠟⠉⠉⠻⣿⣿⣿⣿⠀⠀⢿⣿⣿⠀⠀
⠀⠀⢸⣿⣿⣿⠀⠀⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⡿⠁⠀⠀⠀⠀⠀⠀⣿⣿⣿⣿⣿⣿⣿⣿⣿⡇⠀⠀⠀⠀⠀⠸⣿⠃⠀⠀⠀⠀⢹⣿⣿⣿⡇⠀⢸⣿⣿⠀⠀
⠀⠀⠀⣿⣿⣿⠀⠀⢻⣿⣿⣿⡟⠉⠀⠈⠙⢿⣿⡇⠀⠀⠀⠀⠀⠀⠀⣿⣿⣿⣿⣿⣿⣿⣿⣿⠃⠀⠀⠀⠀⠀⠀⠇⠀⠀⠀⠀⠀⠀⣿⡿⠋⠁⠀⠸⣿⡿⠀⠀
⠀⠀⠀⢻⣿⣿⡄⠀⢸⣿⣿⣿⣷⡀⠀⠀⠀⠀⠙⡇⠀⠀⠀⠀⠀⠀⢸⣿⣿⣿⣿⣿⣿⣿⣿⡟⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⡿⠁⠀⠀⠀⠀⣿⡇⠀⠀
⠀⠀⠀⠘⣿⣿⡇⠀⢸⣿⣿⣿⣿⣷⣄⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⢠⣿⣿⣿⣿⣿⣿⣿⣿⣿⣤⣤⣀⣀⣀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⣿⠃⠀⠀
⠀⠀⠀⠀⢻⣿⣧⠀⠈⢿⣿⡏⢻⣿⣿⣷⣤⣀⠀⠀⠀⠀⠀⣀⣴⣿⣿⣿⣿⣿⣿⣿⣿⠟⠛⠿⣿⣿⣿⣿⣿⣿⣿⣷⣶⣄⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⡟⠀⠀⠀
⠀⠀⠀⠀⠈⣿⣿⠀⠀⠀⠙⢳⠀⠙⢿⣿⣿⣿⣿⣿⣶⣾⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⡅⠀⠀⠀⠀⠉⣿⣿⡿⣿⣿⡝⢿⣿⠆⠀⠀⠀⠀⠀⠀⠀⠀⠀⠁⠀⠀⠀
⠀⠀⠀⠀⠀⠘⣿⡇⠀⠀⠀⠀⠀⠀⠀⠙⠻⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⠿⠋⠙⣿⣿⣦⡀⠀⠀⠀⠹⣿⠁⢿⣿⡇⠀⢻⠁⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀
⠀⠀⠀⠀⠀⠀⠘⣷⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠉⠙⠛⠛⠛⠛⠉⠁⠀⠀⢠⣾⣿⣿⣿⣿⣦⡀⠀⠀⠙⠆⠈⢻⡇⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀
⠀⠀⠀⠀⠀⠀⠀⠈⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⣿⡿⢛⣿⣿⡿⣿⣿⡄⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀
⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⣿⠁⢸⣿⡿⠁⢹⣿⠃⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀
⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⢸⠟⠁⢀⠾⠃⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀
⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀
    "#;

    let mut intro_text: Vec<Line> = ascii_logo
        .lines()
        .map(|line| {
            Line::from(Span::styled(
                line,
                Style::default().fg(ui.logo).add_modifier(Modifier::BOLD),
            ))
        })
        .collect();

    intro_text.extend(vec![
        Line::from(""),
        Line::from(Span::styled(concat!("v", env!("CARGO_PKG_VERSION")), Style::default().fg(ui.muted))),
        Line::from(""),
        Line::from(vec![
            Span::styled(" F2       ", Style::default().fg(ui.accent)),
            Span::raw("- Explorar archivos")
        ]),
        Line::from(vec![
            Span::styled(" Ctrl + S ", Style::default().fg(ui.accent)),
            Span::raw("- Guardar documento")
        ]),
        Line::from(vec![
            Span::styled(" Ctrl + Q ", Style::default().fg(ui.accent)),
            Span::raw("- Salir del editor")
        ]),
        Line::from(vec![
            Span::styled(" Flechas  ", Style::default().fg(ui.accent)),
            Span::raw("- Mover el cursor (Edición/Explorador)")
        ]),
        Line::from(vec![
            Span::styled(" Enter    ", Style::default().fg(ui.accent)),
            Span::raw("- Abrir archivo / Salto de línea")
        ]),
        Line::from(vec![
            Span::styled(" Esc      ", Style::default().fg(ui.accent)),
            Span::raw("- Cerrar explorador y volver al buffer")
        ]),
    ]);

    let content_height = intro_text.len() as u16;

    let p = Paragraph::new(intro_text).alignment(ratatui::layout::Alignment::Center);

    let [center_area] = Layout::vertical([Constraint::Length(content_height)])
        .flex(Flex::Center)
        .areas(inner_area);

    f.render_widget(p, center_area);
}

fn render_tree(f: &mut Frame, app: &mut App, area: Rect) {
    let ui = &app.theme.ui;
    let items: Vec<ListItem> = app.explorer.entries.iter().map(|e| {
        let is_dirty = !e.is_dir && app.documents.values().any(|doc| {
            doc.filepath.as_ref() == Some(&e.path) && doc.is_dirty
        });
        let open = app.explorer.expanded.contains(&e.path);
        // Chevron + carpeta abierta/cerrada; los archivos se alinean con el nombre del directorio.
        let (chevron, icon, color) = match (e.is_dir, open) {
            (true, true) => ("\u{eab4} ", "\u{eaf7} ", ui.tree_dir),
            (true, false) => ("\u{eab6} ", "\u{ea83} ", ui.tree_dir),
            _ => ("  ", "\u{f40d} ", ui.tree_file),
        };
        let git = (e.name != "..").then(|| app.git_ctx.file_status(&e.path)).flatten();
        let git_color = git.map(|s| match s {
            FileStatus::Modified => ui.git_modified,
            FileStatus::Added | FileStatus::Untracked | FileStatus::Renamed => ui.git_added,
            FileStatus::Deleted | FileStatus::Conflict => ui.git_deleted,
        });
        let mut spans = vec![
            Span::styled("\u{2502} ".repeat(e.depth), Style::default().fg(ui.indent_guide)),
            Span::styled(chevron, Style::default().fg(ui.muted)),
            Span::styled(icon, Style::default().fg(color)),
            Span::styled(e.name.as_str(), git_color.map_or_else(Style::default, |c| Style::default().fg(c))),
        ];
        if is_dirty {
            spans.push(Span::styled(" \u{25cf}", Style::default().fg(ui.dirty)));
        }
        // La letra de estado va alineada a la derecha, como en VS Code.
        if let (Some(s), Some(c)) = (git, git_color) {
            let used: usize = spans.iter().map(|s| s.width()).sum();
            let pad = (area.width as usize).saturating_sub(used + 4);
            spans.push(Span::raw(" ".repeat(pad)));
            spans.push(Span::styled(s.letter(), Style::default().fg(c)));
        }
        ListItem::new(Line::from(spans))
    }).collect();

    let is_focused = app.focus == Focus::Tree;
    let ui = &app.theme.ui;
    let border_color = if is_focused { ui.border_focused } else { ui.border };

    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).border_style(Style::default().fg(border_color)).title(" Archivos "))
        .highlight_style(Style::default().bg(ui.tree_selected_bg).fg(ui.tree_selected_fg).add_modifier(Modifier::BOLD))
        .highlight_spacing(HighlightSpacing::Never);

    f.render_stateful_widget(list, area, &mut app.explorer.state);
}

/// Ruta mostrada a partir del nombre de la raíz del árbol ("Downloads/sub"), para que
/// quede claro dónde se crea o qué se elimina.
fn tree_rel(app: &App, path: &std::path::Path) -> String {
    let root = &app.explorer.current_dir;
    let base = root.parent().unwrap_or(root);
    path.strip_prefix(base).unwrap_or(path).display().to_string()
}

/// Menú contextual del explorador; se despliega hacia abajo desde el puntero.
fn render_context_menu(f: &mut Frame, app: &mut App) {
    app.menu_area = None;
    let Some(menu) = &app.context_menu else { return };
    let ui = &app.theme.ui;
    let (w, h) = (17, crate::app::MENU_ITEMS.len() as u16 + 2);
    let screen = f.area();
    let x = menu.x.min(screen.width.saturating_sub(w));
    let y = if menu.y + 1 + h <= screen.height { menu.y + 1 } else { menu.y.saturating_sub(h) };
    let full = Rect::new(x, y, w, h).intersection(screen);
    let shown = Rect { height: (full.height as f64 * app.menu_spawn_progress).ceil() as u16, ..full };

    let items: Vec<ListItem> = crate::app::MENU_ITEMS.iter().enumerate().map(|(i, label)| {
        let fg = if app.menu_item_enabled(i) { ui.popup_fg } else { ui.muted };
        ListItem::new(Span::styled(format!(" {label}"), Style::default().fg(fg)))
    }).collect();
    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).border_type(BorderType::Rounded)
            .border_style(Style::default().fg(ui.popup_border)).style(Style::default().bg(ui.popup_bg)))
        .highlight_style(Style::default().bg(ui.popup_selected_bg).fg(ui.popup_selected_fg));
    let mut state = ratatui::widgets::ListState::default().with_selected(Some(menu.selected));
    f.render_widget(Clear, shown);
    if shown.height >= 2 {
        f.render_stateful_widget(list, shown, &mut state);
    }
    app.menu_area = Some(full);
}

fn render_prompt(f: &mut Frame, app: &mut App) {
    if app.prompt_spawn_progress < 0.01 { return; }

    let (title, has_list): (String, bool) = match app.prompt.as_ref().map(|p| &p.intent) {
        Some(crate::app::PromptIntent::SaveAs(_)) => (" Guardar Como: ".into(), false),
        Some(crate::app::PromptIntent::Rename(_)) => (" Renombrar: ".into(), false),
        Some(crate::app::PromptIntent::Delete(path)) => {
            let name = tree_rel(app, path);
            let extra = if path.is_dir() { " y todo su contenido" } else { "" };
            (format!(" Eliminar {}{}? (y/N): ", name, extra), false)
        }
        Some(crate::app::PromptIntent::ConfirmQuit) => (" Cambios sin guardar. Salir de todos modos? (y/N): ".into(), false),
        Some(crate::app::PromptIntent::CreateFile(dir)) => (format!(" Nuevo archivo en {}/ (a/b.rs crea carpetas): ", tree_rel(app, dir)), false),
        Some(crate::app::PromptIntent::CreateDir(dir)) => (format!(" Nueva carpeta en {}/: ", tree_rel(app, dir)), false),
        Some(crate::app::PromptIntent::ConfirmCloseTab(..)) => (" Cambios sin guardar. Cerrar de todos modos? (y/N): ".into(), false),
        Some(crate::app::PromptIntent::SearchText) => (" Buscar Texto: ".into(), true),
        Some(crate::app::PromptIntent::SearchFile) => (" Buscar Archivo: ".into(), true),
        None => (" ... ".into(), false),
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .border_style(Style::default().fg(app.theme.ui.popup_border))
        .style(Style::default().bg(app.theme.ui.popup_bg));

    let max_height = if has_list { 15 } else { 3 };
    let width = 60;

    // Animación de expansión modal centrada
    let current_height = (max_height as f64 * app.prompt_spawn_progress).ceil() as u16;
    let center_y = f.area().height.saturating_sub(current_height) / 2;
    let center_x = f.area().width.saturating_sub(width) / 2;

    let render_area = Rect::new(center_x, center_y, width, current_height).intersection(f.area());

    f.render_widget(Clear, render_area);
    if render_area.height < 2 { return; } // Previene pánicos por bordes sin espacio interno

    let inner_area = block.inner(render_area);
    f.render_widget(block, render_area);

    let ui = app.theme.ui.clone();
    let prompt = if let Some(p) = &mut app.prompt { p } else { return; };

    let input_line = Paragraph::new(format!("> {}█", prompt.input))
        .style(Style::default().fg(ui.popup_fg));

    if has_list && render_area.height >= 4 {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(1), Constraint::Min(0)])
            .split(inner_area);

        f.render_widget(input_line, chunks[0]);

        let items: Vec<ListItem> = match prompt.intent {
            crate::app::PromptIntent::SearchFile => prompt.file_results.iter().map(|p| {
                ListItem::new(p.to_string_lossy().into_owned())
            }).collect(),
            crate::app::PromptIntent::SearchText => prompt.text_results.iter().map(|m| {
                let file_name = m.path.file_name().unwrap_or_default().to_string_lossy();
                ListItem::new(Line::from(vec![
                    Span::styled(format!("{}:{} ", file_name, m.line_idx + 1), Style::default().fg(ui.accent)),
                    Span::raw(&m.line_preview)
                ]))
            }).collect(),
            _ => vec![],
        };

        let list = List::new(items)
            .highlight_style(Style::default().bg(ui.popup_selected_bg).fg(ui.popup_selected_fg).add_modifier(Modifier::BOLD))
            .highlight_symbol("▶ ");

        f.render_stateful_widget(list, chunks[1], &mut prompt.selection_state);
    } else {
        f.render_widget(input_line, inner_area);
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{Terminal, backend::TestBackend};

    #[test]
    fn completion_popup_renders_long_entries_with_theme_colors() {
        let mut app = App::new();
        app.new_blank_file();
        app.completion_items = vec![
            CompletionOption { label: "x".repeat(200), detail: Some("y".repeat(200)), label_detail: Some("(z)".repeat(50)), ..Default::default() },
            CompletionOption { label: "get_value".into(), detail: Some("fn() -> i32".into()), matched: vec![0, 1, 2], kind: Some(lsp_types::CompletionItemKind::FUNCTION), ..Default::default() },
        ];
        app.completions = app.completion_items.clone();
        app.completion_state.select(Some(0));
        app.completions_spawn_progress = 1.0;
        app.theme.ui.popup_match = Color::Rgb(1, 2, 3);

        let mut term = Terminal::new(TestBackend::new(160, 30)).unwrap();
        term.draw(|f| render(f, &mut app)).unwrap();
        let buf = term.backend().buffer();
        let text: String = buf.content().iter().map(|c| c.symbol()).collect();
        assert!(text.contains("get_value") && text.contains("fn() -> i32") && text.contains("…"));
        assert!(buf.content().iter().any(|c| c.symbol() == "g" && c.fg == Color::Rgb(1, 2, 3)), "lo escrito se resalta con el color del tema");

        app.completions[0].documentation = Some("# Título\n```rust\nfn doc_ejemplo()\n```\ntexto".into());
        term.draw(|f| render(f, &mut app)).unwrap();
        let buf = term.backend().buffer();
        let text: String = buf.content().iter().map(|c| c.symbol()).collect();
        assert!(text.contains("Título") && text.contains("fn doc_ejemplo()") && !text.contains("```"));
        assert!(buf.content().iter().any(|c| c.symbol() == "T" && c.fg == app.theme.ui.doc_heading));
    }

    #[test]
    fn status_line_shows_progress_bar_then_message() {
        let mut app = App::new();
        app.new_blank_file();
        app.status_msg = Some("Guardado".into());
        app.lsp_progress.push((1, lsp_types::NumberOrString::String("t".into()), crate::app::LspProgress {
            title: "Indexing".into(), message: Some("12/264".into()), percentage: Some(50),
        }));
        let mut term = Terminal::new(TestBackend::new(240, 10)).unwrap();
        let mut status = |app: &mut App| {
            term.draw(|f| render(f, app)).unwrap();
            let buf = term.backend().buffer();
            (0..240).map(|x| buf[(x, 9)].symbol().to_string()).collect::<String>()
        };
        let line = status(&mut app);
        assert!(line.contains("LSP: Indexing ██████░░░░░░ 50% 12/264") && !line.contains("Guardado"), "{line}");
        assert!(line.contains("[sin nombre]") && line.contains("sin LSP") && line.contains("1:1 100%"), "{line}");

        app.lsp_progress.clear();
        assert!(status(&mut app).contains("Guardado"));
    }

    fn row(term: &Terminal<TestBackend>, y: u16) -> String {
        let buf = term.backend().buffer();
        (0..buf.area.width).map(|x| buf[(x, y)].symbol().to_string()).collect()
    }

    #[test]
    fn inlay_hints_render_as_sent_with_padding_and_truncation() {
        let mut app = App::new();
        app.show_tree = false;
        app.new_blank_file();
        let uri = crate::lsp::path_to_uri(std::path::Path::new("/tmp/wyvern_hints.rs")).unwrap();
        let doc = app.active_document_mut().unwrap();
        doc.uri = Some(uri.clone());
        doc.buffer.text = ropey::Rope::from_str("let git = Command::new(\"git\")\nx\n");
        doc.buffer.cursor_char_idx = 29;
        let hint = |line, ch, label: &str, pl, pr| InlayHint {
            position: lsp_types::Position::new(line, ch),
            label: lsp_types::InlayHintLabel::String(label.into()),
            kind: None, text_edits: None, tooltip: None, padding_left: Some(pl), padding_right: Some(pr), data: None,
        };
        app.inlay_hints.entry(uri.as_str().to_string()).or_default().insert(0, vec![
            hint(0, 23, "program:", false, true),
            hint(0, 7, ": Result<Output, Error>", false, false),
            hint(0, 29, "Command", true, false),
            hint(1, 1, "impl Iterator<Item = PathBuf> + Send + 'static", true, false),
        ]);

        let mut term = Terminal::new(TestBackend::new(120, 8)).unwrap();
        term.draw(|f| render(f, &mut app)).unwrap();
        let l0 = row(&term, 1);
        let expected = "let git: Result<Output, Error> = Command::new(program: \"git\") Command";
        assert!(l0.contains(expected), "{l0}");
        let l1 = row(&term, 2);
        assert!(l1.contains("x impl Iterator<Item = PathBuf>… "), "{l1}");

        // El cursor en EOL queda justo tras `)`, no detrás de la pista de fin de línea.
        let col = l0[..l0.find(expected).unwrap()].chars().count() + expected.find(") Command").unwrap() + 1;
        assert_eq!(term.get_cursor_position().unwrap().x as usize, col);
    }

    #[test]
    fn diagnostics_underline_only_their_range_and_show_message() {
        let mut app = App::new();
        app.show_tree = false;
        app.new_blank_file();
        let uri = crate::lsp::path_to_uri(std::path::Path::new("/tmp/wyvern_diag.rs")).unwrap();
        let doc = app.active_document_mut().unwrap();
        doc.uri = Some(uri.clone());
        doc.buffer.text = ropey::Rope::from_str("let abc = xyz;\nok\n");
        let diag = |l1, c1, l2, c2, sev, msg: &str| lsp_types::Diagnostic {
            range: lsp_types::Range::new(lsp_types::Position::new(l1, c1), lsp_types::Position::new(l2, c2)),
            severity: Some(sev), message: msg.into(), ..Default::default()
        };
        app.diagnostics.entry(uri.as_str().to_string()).or_default().insert(0, vec![
            diag(0, 4, 0, 7, lsp_types::DiagnosticSeverity::WARNING, "unused variable"),
            diag(0, 10, 0, 13, lsp_types::DiagnosticSeverity::ERROR, "cannot find value `xyz`\nhelp: ..."),
        ]);

        let mut term = Terminal::new(TestBackend::new(80, 6)).unwrap();
        term.draw(|f| render(f, &mut app)).unwrap();
        let l0 = row(&term, 1);
        assert!(l0.contains("let abc = xyz;    ■ cannot find value `xyz`") && !l0.contains("help"), "{l0}");
        let buf = term.backend().buffer();
        let start = l0[..l0.find("let").unwrap()].chars().count() as u16;
        let underline = |col: u16| {
            let c = &buf[(start + col, 1)];
            c.modifier.contains(Modifier::UNDERLINED).then_some(c.underline_color)
        };
        assert_eq!(underline(0), None);
        assert_eq!(underline(4), Some(app.theme.ui.warning));
        assert_eq!(underline(7), None);
        assert_eq!(underline(10), Some(app.theme.ui.error));
        assert_eq!(underline(13), None);
        assert!(!row(&term, 2).contains('■'));
    }

    #[test]
    fn scrollbar_occupies_last_column() {
        let mut app = App::new();
        app.show_tree = false;
        app.new_blank_file();
        app.active_document_mut().unwrap().buffer.text = ropey::Rope::from_str(&"a\n".repeat(100));
        let mut term = Terminal::new(TestBackend::new(40, 12)).unwrap();
        term.draw(|f| render(f, &mut app)).unwrap();
        assert!(row(&term, 1).ends_with("a┃") || row(&term, 1).ends_with(" ┃"), "{}", row(&term, 1));
        assert!(row(&term, 9).ends_with("│"), "{}", row(&term, 9));
        assert!(row(&term, 1).starts_with("   1 "), "{}", row(&term, 1));
    }

    #[test]
    fn tree_shows_git_status_letters() {
        let d = std::env::temp_dir().join(format!("wyvern_treegit_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("src")).unwrap();
        std::fs::write(d.join("src/a.rs"), "").unwrap();
        std::fs::write(d.join("new.txt"), "").unwrap();
        std::fs::write(d.join("clean.txt"), "").unwrap();
        let mut app = App::new();
        app.explorer = crate::explorer::FileExplorer::new(d.clone());
        app.explorer.expanded.insert(d.join("src"));
        app.explorer.reload();
        app.git_ctx.file_statuses.insert(d.join("src"), FileStatus::Modified);
        app.git_ctx.file_statuses.insert(d.join("src/a.rs"), FileStatus::Modified);
        app.git_ctx.untracked_dirs.push(d.join("new.txt"));
        app.show_tree = true;
        app.tree_spawn_progress = 1.0;

        let mut term = Terminal::new(TestBackend::new(100, 12)).unwrap();
        term.draw(|f| render(f, &mut app)).unwrap();
        let lines: Vec<String> = (0..12).map(|y| row(&term, y).chars().take(20).collect()).collect();
        let find = |name: &str| lines.iter().find(|l| l.contains(name)).unwrap_or_else(|| panic!("{name}: {lines:?}")).trim_end().to_string();
        assert!(find("src").ends_with("M │"), "{lines:?}");
        assert!(find("a.rs").ends_with("M │"), "{lines:?}");
        assert!(find("new.txt").ends_with("U │"), "{lines:?}");
        assert!(find("clean.txt").ends_with("  │"), "{lines:?}");
        assert!(find("..").ends_with(" │"));
        std::fs::remove_dir_all(&d).unwrap();
    }
}
