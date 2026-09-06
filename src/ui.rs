use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect, Flex},
    style::{Color, Style, Modifier},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, List, ListItem, HighlightSpacing, BorderType},
};
use crate::app::{App, Focus, Document, SplitNode};
use ratatui::widgets::Clear;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;
use syntect::easy::HighlightLines;

/// Renderiza un frame completo del editor y coordina sus capas visuales.
pub fn render(f: &mut Frame, app: &mut App) {
    let root_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(1)])
        .split(f.area());
        
    let target_tree_width = (f.area().width as f64 * 0.20) as u16;
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
    
    if app.windows.is_empty() {
        render_intro(f, editor_area);
    } else {
        let layout_root = app.layout.clone();
        render_split_tree(f, app, &layout_root, editor_area);
    }
    
    render_status_line(f, app, root_layout[1]);
    
    if app.show_help || app.help_spawn_progress > 0.01 {
        render_help(f, app);
    } else if app.prompt.is_some() || app.prompt_spawn_progress > 0.01 {
        render_prompt(f, app);
    }
}

/// Recorre el árbol de divisiones y asigna un área de pantalla a cada ventana.
fn render_split_tree(f: &mut Frame, app: &mut App, node: &SplitNode, area: Rect) {
    match node {
        SplitNode::Leaf(win_id) => {
            app.window_areas.insert(*win_id, area);
            let is_focused = app.focus == Focus::Window(*win_id);

            if let Some(window) = app.windows.get(win_id).cloned() {
                if let Some(mut doc) = app.documents.remove(&window.buffer_id) {
                    render_document(f, app, &mut doc, area, is_focused);
                    app.documents.insert(window.buffer_id, doc);
                }
            }
        }
        SplitNode::Split(dir, a, b) => {
            let layout = Layout::default()
                .direction(*dir)
                .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
                .split(area);

            render_split_tree(f, app, a, layout[0]);
            render_split_tree(f, app, b, layout[1]);
        }
    }
}

/// Dibuja una pestaña y el contenido visible de un documento con interpolación de puntero.
fn render_document(f: &mut Frame, app: &mut App, doc: &mut Document, area: Rect, is_focused: bool) {
    let edit_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(0)])
        .split(area);

    let tab_area = edit_layout[0];
    let text_area = edit_layout[1];

    let file_name = doc.filepath.as_ref().map_or("Nuevo".to_string(), |p| p.file_name().unwrap_or_default().to_string_lossy().into_owned());
    let ext = doc.filepath.as_ref().and_then(|p| p.extension()).and_then(|s| s.to_str()).unwrap_or("");

    let (icon, icon_color) = match ext {
        "rs" => (" ", Color::Rgb(222, 90, 44)),
        "py" => ("󰌠 ", Color::Yellow),
        "md" => (" ", Color::LightBlue),
        "c" | "cpp" => (" ", Color::LightBlue),
        "html" | "htm" => (" ", Color::Rgb(227, 79, 38)),
        _ => (" ", Color::White),
    };

    let dirty_sym = if doc.is_dirty { "●" } else { "×" };
    let border_color = if is_focused { Color::Cyan } else { Color::DarkGray };
    let tab_bg = if is_focused { Color::Rgb(40, 50, 75) } else { Color::Rgb(30, 30, 30) };

    let tab_spans = vec![
        Span::styled(" ", Style::default().bg(tab_bg)),
        Span::styled(icon, Style::default().fg(icon_color).bg(tab_bg)),
        Span::styled(format!("{} ", file_name), Style::default().fg(Color::White).bg(tab_bg).add_modifier(Modifier::ITALIC)),
        Span::styled(dirty_sym, Style::default().fg(if doc.is_dirty { Color::Yellow } else { Color::Gray }).bg(tab_bg)),
        Span::styled(" │ ", Style::default().fg(border_color).bg(Color::Rgb(20, 20, 20))),
    ];
    let tab_line = Paragraph::new(Line::from(tab_spans)).style(Style::default().bg(Color::Rgb(20, 20, 20)));
    f.render_widget(tab_line, tab_area);

    let max_lines = doc.buffer.text.len_lines();
    let gutter_num_width = max_lines.to_string().len().max(1);
    let gutter_total_width = gutter_num_width + 3;

    let view_height = text_area.height as usize;
    let view_width = text_area.width.saturating_sub(gutter_total_width as u16) as usize;

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

    let syntax = doc.filepath.as_ref()
        .and_then(|p| p.extension())
        .and_then(|ext| app.syntax_set.find_syntax_by_extension(ext.to_str().unwrap_or("")))
        .unwrap_or_else(|| app.syntax_set.find_syntax_by_extension("rs").unwrap());

    let theme = &app.theme_set.themes["base16-ocean.dark"];
    let default_fg = theme.settings.foreground.unwrap_or(syntect::highlighting::Color {r: 255, g: 255, b: 255, a:255});
    let mut h = HighlightLines::new(syntax, theme);

    let selection_range = doc.buffer.get_selection_range();
    let mut lines = Vec::with_capacity(view_height);

    let search_query_len = app.last_search_query.as_ref().map(|q| q.chars().count()).unwrap_or(0);
    let active_search_idx = if !app.text_search_results.is_empty() {
        Some(app.text_search_results[app.current_search_idx])
    } else {
        None
    };

    for line_idx in start_line..end_line {
        let line_str = doc.buffer.text.line(line_idx).to_string();
        let ranges = h.highlight_line(&line_str, &app.syntax_set).unwrap_or_default();

        let has_error = app.diagnostics.contains_key(&line_idx);
        let hints_for_line = app.inlay_hints.get(&line_idx);

        let line_start_char = doc.buffer.text.line_to_char(line_idx);
        let line_end_char = line_start_char + line_str.chars().count();
        let matches_in_line: Vec<usize> = app.text_search_results.iter()
            .filter(|&&idx| idx >= line_start_char && idx < line_end_char)
            .copied()
            .collect();

        let mut spans = Vec::new();
        let line_num_str = format!(" {:>w$} ", line_idx + 1, w = gutter_num_width);
        spans.push(Span::styled(line_num_str, Style::default().fg(Color::DarkGray)));

        let (git_sym, git_color) = match app.git_ctx.line_statuses.get(&line_idx) {
            Some(crate::git::GitLineStatus::Added) => ("▌", Color::Green),
            Some(crate::git::GitLineStatus::Modified) => ("▌", Color::Yellow),
            Some(crate::git::GitLineStatus::Deleted) => ("_", Color::Red),
            None => (" ", Color::Reset),
        };
        spans.push(Span::styled(git_sym, Style::default().fg(git_color)));

        let mut current_global_idx = line_start_char;
        let mut current_utf16_col = 0;
        let mut visual_col = 0;
        let mut in_leading_ws = true;

        for (style, text) in ranges {
            let clean_text = text.replace('\n', "").replace('\r', "");
            if clean_text.is_empty() { continue; }

            let mut base_style = Style::default();

            if style.foreground.r != default_fg.r || style.foreground.g != default_fg.g || style.foreground.b != default_fg.b {
                let ansi_color = rgb_to_ansi(style.foreground.r, style.foreground.g, style.foreground.b);
                base_style = base_style.fg(ansi_color);
            }

            if has_error {
                base_style = base_style.add_modifier(Modifier::UNDERLINED).underline_color(Color::Red);
            }

            let mut segment = String::new();
            let mut active_style = base_style;
            let mut is_first = true;

            for grapheme in clean_text.graphemes(true) {
                if let Some(hints) = hints_for_line {
                    for hint in hints {
                        if hint.position.character as usize == current_utf16_col {
                            if !segment.is_empty() {
                                spans.push(Span::styled(segment.clone(), active_style));
                                segment.clear();
                            }

                            let hint_label = match &hint.label {
                                lsp_types::InlayHintLabel::String(s) => s.clone(),
                                lsp_types::InlayHintLabel::LabelParts(parts) => parts.iter().map(|p| p.value.clone()).collect(),
                            };

                            let hint_style = Style::default().fg(Color::LightCyan).bg(Color::DarkGray);
                            spans.push(Span::styled(format!(" {}: ", hint_label.trim_end_matches(':')), hint_style));
                        }
                    }
                }

                let g_chars = grapheme.chars().count();
                let g_utf16 = grapheme.encode_utf16().count();
                let (display_str, g_width) = if grapheme == "\t" {
                    ("    ", 4)
                } else {
                    (grapheme, grapheme.width())
                };

                let mut char_style = base_style;

                if search_query_len > 0 {
                    for &match_start in &matches_in_line {
                        if current_global_idx >= match_start && current_global_idx < match_start + search_query_len {
                            if Some(match_start) == active_search_idx {
                                char_style = char_style.bg(Color::LightYellow).fg(Color::Black);
                            } else {
                                char_style = char_style.bg(Color::DarkGray).fg(Color::LightYellow);
                            }
                            break;
                        }
                    }
                }

                if let Some(ref sel) = selection_range {
                    if current_global_idx >= sel.start && current_global_idx < sel.end {
                        char_style = char_style.bg(Color::Blue).fg(Color::White);
                    }
                }

                if is_first {
                    active_style = char_style;
                    is_first = false;
                } else if char_style != active_style {
                    if !segment.is_empty() {
                        spans.push(Span::styled(segment.clone(), active_style));
                        segment.clear();
                    }
                    active_style = char_style;
                }

                if in_leading_ws && grapheme.chars().all(|c| c == ' ' || c == '\t') {
                    for _ in 0..g_width {
                        if visual_col % 4 == 0 {
                            if !segment.is_empty() {
                                spans.push(Span::styled(segment.clone(), active_style));
                                segment.clear();
                            }
                            spans.push(Span::styled("│", Style::default().fg(Color::DarkGray)));
                        } else {
                            segment.push(' ');
                        }
                        visual_col += 1;
                    }
                } else {
                    in_leading_ws = false;
                    segment.push_str(display_str);
                    visual_col += g_width;
                }

                current_global_idx += g_chars;
                current_utf16_col += g_utf16;
            }
            if !segment.is_empty() { spans.push(Span::styled(segment, active_style)); }
        }
        lines.push(Line::from(spans));
    }

    doc.buffer.min_dirty_line = doc.buffer.min_dirty_line.max(end_line);

    let p = Paragraph::new(lines).block(Block::default()).scroll((0, doc.buffer.scroll_x as u16));
    f.render_widget(p, text_area);

    if is_focused {
        let cursor_y = doc.buffer.text.char_to_line(doc.buffer.cursor_char_idx);
        let visual_cursor_x = doc.buffer.char_idx_to_visual_col(doc.buffer.cursor_char_idx);
        let screen_x = text_area.x + gutter_total_width as u16 + visual_cursor_x.saturating_sub(doc.buffer.scroll_x) as u16;
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

        let is_popup_animating = (!app.completions.is_empty() || app.completions_spawn_progress > 0.01) && app.completions_spawn_progress > 0.0;

        if is_popup_animating {
            let comp_width = 85;
            let actual_len = if app.completions.is_empty() { 1 } else { app.completions.len() };
            let max_comp_height = actual_len as u16 + 2;
            let comp_height = (max_comp_height as f64 * app.completions_spawn_progress).ceil() as u16;

            if comp_height > 0 {
                let real_popup_y = if screen_y + 1 + max_comp_height <= text_area.bottom() {
                    screen_y + 1
                } else {
                    screen_y.saturating_sub(comp_height)
                };

                let max_x = text_area.right().saturating_sub(comp_width);
                let safe_screen_x = screen_x.min(max_x);
                let popup_area = Rect::new(safe_screen_x, real_popup_y, comp_width, comp_height);

                let items: Vec<ListItem> = app.completions.iter().take(15).map(|c| {
                    let (kind_icon, _kind_str, kind_color) = match c.kind {
                        Some(lsp_types::CompletionItemKind::METHOD) => ("", "Method", Color::LightMagenta),
                        Some(lsp_types::CompletionItemKind::FUNCTION) => ("󰊕", "Function", Color::Magenta),
                        Some(lsp_types::CompletionItemKind::STRUCT) => ("", "Struct", Color::LightYellow),
                        _ => ("󰦨", "Text", Color::Gray),
                    };

                    let mut spans = vec![
                        Span::styled(format!(" {} ", kind_icon), Style::default().fg(kind_color).bg(Color::Rgb(35, 35, 35))),
                        Span::styled(format!(" {} ", c.label), Style::default().fg(Color::White)),
                    ];

                    if let Some(detail) = &c.detail {
                        let clean_detail = detail.replace('\n', " ");
                        spans.push(Span::styled(format!(" {} ", clean_detail), Style::default().fg(Color::DarkGray).add_modifier(Modifier::ITALIC)));
                    }
                    ListItem::new(Line::from(spans))
                }).collect();

                let list = List::new(items)
                    .block(Block::default().borders(Borders::ALL).border_type(BorderType::Rounded).style(Style::default().bg(Color::Rgb(25, 25, 25))))
                    .highlight_style(Style::default().bg(Color::Rgb(45, 60, 80)).fg(Color::White).add_modifier(Modifier::BOLD));

                f.render_widget(Clear, popup_area);
                if comp_height >= 2 {
                    f.render_stateful_widget(list, popup_area, &mut app.completion_state);
                }
            }
        }

        // El cursor siempre se renderiza y sigue libre para interpolación
        if cursor_y >= doc.buffer.scroll_y && cursor_y < doc.buffer.scroll_y + view_height {
            if visual_cursor_x >= doc.buffer.scroll_x && visual_cursor_x < doc.buffer.scroll_x + view_width {
                if selection_range.is_none() {
                    let safe_x = (app.visual_cursor_x.round() as u16).clamp(text_area.x, text_area.right().saturating_sub(1));
                    let safe_y = (app.visual_cursor_y.round() as u16).clamp(text_area.y, text_area.bottom().saturating_sub(1));
                    f.set_cursor_position((safe_x, safe_y));
                }
            }
        }
    }
}

fn rgb_to_ansi(r: u8, g: u8, b: u8) -> Color {
    let ansi_palette = [
        (0, 0, 0, Color::Black),
        (170, 0, 0, Color::Red),
        (0, 170, 0, Color::Green),
        (170, 85, 0, Color::Yellow),
        (0, 0, 170, Color::Blue),
        (170, 0, 170, Color::Magenta),
        (0, 170, 170, Color::Cyan),
        (170, 170, 170, Color::Gray),
        (85, 85, 85, Color::DarkGray),
        (255, 85, 85, Color::LightRed),
        (85, 255, 85, Color::LightGreen),
        (255, 255, 85, Color::LightYellow),
        (85, 85, 255, Color::LightBlue),
        (255, 85, 255, Color::LightMagenta),
        (85, 255, 255, Color::LightCyan),
        (255, 255, 255, Color::White),
    ];

    let mut best_color = Color::Reset;
    let mut min_dist = i32::MAX;

    let r_i32 = r as i32;
    let g_i32 = g as i32;
    let b_i32 = b as i32;

    for (cr, cg, cb, color) in ansi_palette.iter() {
        let dr = *cr as i32 - r_i32;
        let dg = *cg as i32 - g_i32;
        let db = *cb as i32 - b_i32;
        let dist = dr * dr + dg * dg + db * db;

        if dist < min_dist {
            min_dist = dist;
            best_color = *color;
        }
    }
    best_color
}

fn render_help(f: &mut Frame, app: &App) {
    if app.help_spawn_progress < 0.01 { return; }

    let help_text = vec![
        Line::from(Span::styled(" COMANDOS WYVERN ", Style::default().add_modifier(Modifier::BOLD).fg(Color::Cyan))),
        Line::from(""),
        Line::from(Span::styled(" [GENERAL & ARCHIVOS] ", Style::default().fg(Color::DarkGray))),
        Line::from(vec![Span::styled(" F1             ", Style::default().fg(Color::Yellow)), Span::raw("- Mostrar/Ocultar esta ayuda")]),
        Line::from(vec![Span::styled(" F2             ", Style::default().fg(Color::Yellow)), Span::raw("- Explorar archivos (Tree)")]),
        Line::from(vec![Span::styled(" Ctrl + S       ", Style::default().fg(Color::Yellow)), Span::raw("- Guardar documento actual")]),
        Line::from(vec![Span::styled(" Ctrl + Q       ", Style::default().fg(Color::Yellow)), Span::raw("- Salir del editor")]),
        Line::from(""),
        Line::from(Span::styled(" [BÚSQUEDA] ", Style::default().fg(Color::DarkGray))),
        Line::from(vec![Span::styled(" Ctrl + F       ", Style::default().fg(Color::Yellow)), Span::raw("- Buscar archivo en el proyecto (Fuzzy)")]),
        Line::from(vec![Span::styled(" Ctrl + T       ", Style::default().fg(Color::Yellow)), Span::raw("- Buscar texto en el proyecto (Fuzzy)")]),
        Line::from(vec![Span::styled(" F3             ", Style::default().fg(Color::Yellow)), Span::raw("- Saltar a la siguiente coincidencia de texto")]),
        Line::from(""),
        Line::from(Span::styled(" [EDICIÓN] ", Style::default().fg(Color::DarkGray))),
        Line::from(vec![Span::styled(" Ctrl + C/X/V   ", Style::default().fg(Color::Yellow)), Span::raw("- Copiar, Cortar, Pegar")]),
        Line::from(vec![Span::styled(" Ctrl + Z/Y     ", Style::default().fg(Color::Yellow)), Span::raw("- Deshacer, Rehacer")]),
        Line::from(vec![Span::styled(" Tab            ", Style::default().fg(Color::Yellow)), Span::raw("- Indentar (4 espacios) / Navegar Autocompletado")]),
        Line::from(vec![Span::styled(" Supr / Backsp  ", Style::default().fg(Color::Yellow)), Span::raw("- Borrar texto / Eliminar archivo en el Tree")]),
        Line::from(vec![Span::styled(" Enter          ", Style::default().fg(Color::Yellow)), Span::raw("- Salto de línea / Confirmar Prompt / Abrir Archivo")]),
        Line::from(vec![Span::styled(" Esc            ", Style::default().fg(Color::Yellow)), Span::raw("- Cancelar / Quitar selección / Cerrar modal")]),
        Line::from(""),
        Line::from(Span::styled(" [SPLITS & FOCO] ", Style::default().fg(Color::DarkGray))),
        Line::from(vec![Span::styled(" Alt + V / H    ", Style::default().fg(Color::Yellow)), Span::raw("- Crear Split Vertical / Horizontal")]),
        Line::from(vec![Span::styled(" Alt + W        ", Style::default().fg(Color::Yellow)), Span::raw("- Cerrar el Split activo")]),
        Line::from(vec![Span::styled(" Alt + I/K/J/L  ", Style::default().fg(Color::Yellow)), Span::raw("- Mover el foco (Arriba/Abajo/Izq/Der)")]),
        Line::from(""),
        Line::from(Span::styled(" [NAVEGACIÓN] ", Style::default().fg(Color::DarkGray))),
        Line::from(vec![Span::styled(" Flechas        ", Style::default().fg(Color::Yellow)), Span::raw("- Mover cursor (+ Shift para seleccionar texto)")]),
        Line::from(vec![Span::styled(" Inicio / Fin   ", Style::default().fg(Color::Yellow)), Span::raw("- Ir al Principio / Final de la línea (+ Shift)")]),
        Line::from(vec![Span::styled(" RePág / AvPág  ", Style::default().fg(Color::Yellow)), Span::raw("- Subir / Bajar página (+ Shift)")]),
        Line::from(vec![Span::styled(" Ctrl + U/D     ", Style::default().fg(Color::Yellow)), Span::raw("- Scroll rápido Arriba / Abajo (Mueve el Viewport)")]),
    ];

    let block = Block::default()
        .borders(Borders::ALL)
        .style(Style::default().bg(Color::Rgb(20, 20, 20)))
        .border_style(Style::default().fg(Color::Cyan));

    let paragraph = Paragraph::new(help_text).block(block).alignment(ratatui::layout::Alignment::Left);

    let target_height = 33;
    let width = 75;

    // Animación de expansión modal centrada
    let current_height = (target_height as f64 * app.help_spawn_progress).ceil() as u16;
    let center_y = f.area().height.saturating_sub(current_height) / 2;
    let center_x = f.area().width.saturating_sub(width) / 2;

    let render_area = Rect::new(center_x, center_y, width, current_height);

    f.render_widget(Clear, render_area);
    if render_area.height >= 2 {
        f.render_widget(paragraph, render_area);
    }
}

fn render_status_line(f: &mut Frame, app: &App, area: Rect) {
    let mode_str = match app.focus {
        Focus::Window(_) => " EDIT ",
        Focus::Tree => " TREE ",
    };

    let doc_opt = app.get_active_document();
    let stats_str = if app.git_ctx.is_repo && doc_opt.is_some() {
        let (adds, mods, dels) = app.git_ctx.stats;
        let mut parts = Vec::new();
        if adds > 0 { parts.push(format!("+{}", adds)); }
        if mods > 0 { parts.push(format!("~{}", mods)); }
        if dels > 0 { parts.push(format!("-{}", dels)); }

        if parts.is_empty() { String::new() } else { format!(" [{}]", parts.join(" ")) }
    } else {
        String::new()
    };

    let git_str = if app.git_ctx.is_repo {
        format!(" git: {}{} ", app.git_ctx.branch.as_deref().unwrap_or("detached"), stats_str)
    } else {
        " local ".to_string()
    };

    let (mut err_count, mut warn_count) = (0, 0);
    for diags in app.diagnostics.values() {
        for d in diags {
            match d.severity {
                Some(lsp_types::DiagnosticSeverity::ERROR) => err_count += 1,
                Some(lsp_types::DiagnosticSeverity::WARNING) => warn_count += 1,
                _ => {}
            }
        }
    }

    let diag_str = if err_count > 0 || warn_count > 0 {
        format!(" E:{} W:{} ", err_count, warn_count)
    } else {
        " OK ".to_string()
    };

    let (pos_str, _lang) = if let Some(doc) = doc_opt {
        let l = doc.filepath.as_ref().and_then(|p| p.extension()).and_then(|e| e.to_str()).unwrap_or("txt").to_uppercase();
        let line = doc.buffer.text.char_to_line(doc.buffer.cursor_char_idx) + 1;
        let col = doc.buffer.cursor_char_idx - doc.buffer.text.line_to_char(line - 1) + 1;
        (format!(" Ln {}, Col {} | {} ", line, col, l), l)
    } else {
        (" No Doc ".to_string(), "NONE".to_string())
    };

    let left_line = Line::from(vec![
        Span::styled(mode_str, Style::default().bg(Color::Cyan).fg(Color::Black).add_modifier(Modifier::BOLD)),
        Span::styled(&git_str, Style::default().bg(Color::DarkGray).fg(Color::White)),
    ]);

    let right_line = Line::from(vec![
        Span::styled(&diag_str, Style::default().fg(if err_count > 0 { Color::Red } else { Color::Gray })),
        Span::styled(&pos_str, Style::default().fg(Color::White)),
    ]);

    let layout = Layout::default().direction(Direction::Horizontal).constraints([Constraint::Percentage(50), Constraint::Percentage(50)]).split(area);
    f.render_widget(Block::default().style(Style::default().bg(Color::Rgb(30, 30, 30))), area);
    f.render_widget(Paragraph::new(left_line).alignment(ratatui::layout::Alignment::Left), layout[0]);
    f.render_widget(Paragraph::new(right_line).alignment(ratatui::layout::Alignment::Right), layout[1]);
}

fn render_intro(f: &mut Frame, area: Rect) {
    let outer_block = Block::default().borders(Borders::ALL);
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
                Style::default().fg(Color::Magenta).add_modifier(Modifier::BOLD),
            ))
        })
        .collect();

    intro_text.extend(vec![
        Line::from(""),
        Line::from(Span::styled("v0.1.0", Style::default().fg(Color::DarkGray))),
        Line::from(""),
        Line::from(vec![
            Span::styled(" F2       ", Style::default().fg(Color::Cyan)),
            Span::raw("- Explorar archivos")
        ]),
        Line::from(vec![
            Span::styled(" Ctrl + S ", Style::default().fg(Color::Cyan)),
            Span::raw("- Guardar documento")
        ]),
        Line::from(vec![
            Span::styled(" Ctrl + Q ", Style::default().fg(Color::Cyan)),
            Span::raw("- Salir del editor")
        ]),
        Line::from(vec![
            Span::styled(" Flechas  ", Style::default().fg(Color::Cyan)),
            Span::raw("- Mover el cursor (Edición/Explorador)")
        ]),
        Line::from(vec![
            Span::styled(" Enter    ", Style::default().fg(Color::Cyan)),
            Span::raw("- Abrir archivo / Salto de línea")
        ]),
        Line::from(vec![
            Span::styled(" Esc      ", Style::default().fg(Color::Cyan)),
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
    let items: Vec<ListItem> = app.explorer.entries.iter().map(|e| {
        let is_dirty = !e.is_dir && app.documents.values().any(|doc| {
            doc.filepath.as_ref() == Some(&e.path) && doc.is_dirty
        });
        let (prefix, color) = if e.is_dir { (" ", Color::Blue) } else { (" ", Color::White) };
        let mut spans = vec![Span::styled(prefix, Style::default().fg(color)), Span::raw(&e.name)];
        if is_dirty {
            spans.push(Span::styled(" ", Style::default().fg(Color::Yellow)));
        }
        ListItem::new(Line::from(spans))
    }).collect();

    let is_focused = app.focus == Focus::Tree;
    let border_color = if is_focused { Color::Cyan } else { Color::DarkGray };

    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).border_style(Style::default().fg(border_color)).title(" Archivos "))
        .highlight_style(Style::default().bg(Color::Cyan).fg(Color::Black).add_modifier(Modifier::BOLD))
        .highlight_symbol("▶ ")
        .highlight_spacing(HighlightSpacing::Always);

    f.render_stateful_widget(list, area, &mut app.explorer.state);
}

fn render_prompt(f: &mut Frame, app: &mut App) {
    if app.prompt_spawn_progress < 0.01 { return; }

    let (title, has_list) = if let Some(p) = &app.prompt {
        match p.intent {
            crate::app::PromptIntent::SaveAs(_) => (" Guardar Como: ", false),
            crate::app::PromptIntent::Rename(_) => (" Renombrar: ", false),
            crate::app::PromptIntent::Delete(_) => (" Eliminar archivo? (y/N): ", false),
            crate::app::PromptIntent::ConfirmQuit => (" Cambios sin guardar. Salir de todos modos? (y/N): ", false),
            crate::app::PromptIntent::SearchText => (" Buscar Texto: ", true),
            crate::app::PromptIntent::SearchFile => (" Buscar Archivo: ", true),
        }
    } else {
        (" ... ", false)
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .border_style(Style::default().fg(Color::Yellow))
        .style(Style::default().bg(Color::Rgb(25, 25, 25)));

    let max_height = if has_list { 15 } else { 3 };
    let width = 60;

    // Animación de expansión modal centrada
    let current_height = (max_height as f64 * app.prompt_spawn_progress).ceil() as u16;
    let center_y = f.area().height.saturating_sub(current_height) / 2;
    let center_x = f.area().width.saturating_sub(width) / 2;

    let render_area = Rect::new(center_x, center_y, width, current_height);

    f.render_widget(Clear, render_area);
    if render_area.height < 2 { return; } // Previene pánicos por bordes sin espacio interno

    let inner_area = block.inner(render_area);
    f.render_widget(block, render_area);

    let prompt = if let Some(p) = &mut app.prompt { p } else { return; };

    let input_line = Paragraph::new(format!("> {}█", prompt.input))
        .style(Style::default().fg(Color::White));

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
                    Span::styled(format!("{}:{} ", file_name, m.line_idx + 1), Style::default().fg(Color::Cyan)),
                    Span::raw(&m.line_preview)
                ]))
            }).collect(),
            _ => vec![],
        };

        let list = List::new(items)
            .highlight_style(Style::default().bg(Color::Rgb(45, 60, 80)).fg(Color::White).add_modifier(Modifier::BOLD))
            .highlight_symbol("▶ ");

        f.render_stateful_widget(list, chunks[1], &mut prompt.selection_state);
    } else {
        f.render_widget(input_line, inner_area);
    }
}