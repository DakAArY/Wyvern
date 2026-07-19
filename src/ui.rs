use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect, Flex},
    style::{Color, Style, Modifier},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, List, ListItem, HighlightSpacing},
};
use syntect::easy::HighlightLines;
use crate::app::{App, AppState};
use ratatui::widgets::Clear;

pub fn render(f: &mut Frame, app: &mut App) {
    let main_layout = Layout::default()
        .direction(Direction::Horizontal)
        .constraints(if app.show_tree {
            vec![Constraint::Percentage(20), Constraint::Percentage(80)]
        } else {
            vec![Constraint::Percentage(100)]
        })
        .split(f.area());

    let editor_area = if app.show_tree {
        render_tree(f, app, main_layout[0]);
        main_layout[1]
    } else {
        main_layout[0]
    };

    match app.state {
        AppState::Intro => render_intro(f, editor_area),
        AppState::Editing | AppState::Exploring => render_editor(f, app, editor_area),
    }
}

fn render_tree(f: &mut Frame, app: &mut App, area: Rect) {
    let items: Vec<ListItem> = app.explorer.entries.iter().map(|e| {
        let (prefix, color) = if e.is_dir { 
            ("📁 ", Color::Blue) 
        } else { 
            ("📄 ", Color::White) 
        };
        let line = Line::from(vec![
            Span::styled(prefix, Style::default().fg(color)),
            Span::raw(&e.name),
        ]);
        ListItem::new(line)
    }).collect();

    let is_focused = app.state == AppState::Exploring;
    let border_color = if is_focused { Color::Cyan } else { Color::DarkGray };

    let list = List::new(items)
        .block(Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(border_color))
            .title(" Archivos "))
        .highlight_style(Style::default().bg(Color::Cyan).fg(Color::Black).add_modifier(Modifier::BOLD))
        .highlight_symbol("▶ ")
        .highlight_spacing(HighlightSpacing::Always);

    f.render_stateful_widget(list, area, &mut app.explorer.state);
}

fn render_intro(f: &mut Frame, area: Rect) {
    let outer_block = Block::default().borders(Borders::ALL);
    let inner_area = outer_block.inner(area);
    f.render_widget(outer_block, area);

    // Logo ASCII mostrado en la pantalla de bienvenida.
    let ascii_logo = r#"
                                                                                                                                                       ▁▃▄▅▅▅▅▆▆▆▇▇▇▇▇▇▇▆▅▂▁ ▁      ██╗    ██╗██╗   ██╗██╗   ██╗███████╗██████╗ ███╗   ██╗              
                                                                                                                                                  ▂▃▅▆██████████████████▛▛▔▔▔ ▔     ██║    ██║╚██╗ ██╔╝██║   ██║██╔════╝██╔══██╗████╗  ██║              
                                                                                                                                             ▂▃▅▆████████████▛▘▏ ▁▁ ▕▁▏▔  ▔▔        ██║ █╗ ██║ ╚████╔╝ ██║   ██║█████╗  ██████╔╝██╔██╗ ██║              
                                                                                                                                       ▁▃▄▆▇████████▜█▔█▋▐▘▘▆▇▅▁▏▔  ▔▁              ██║███╗██║  ╚██╔╝  ╚██╗ ██╔╝██╔══╝  ██╔══██╗██║╚██╗██║              
                                                                                                                                  ▂▃▅▆██████████▅▇███▙█▛▗▘▎▗  ▔▔ ▔ ▔▔▔              ╚███╔███╔╝   ██║    ╚████╔╝ ███████╗██║  ██║██║ ╚████║              
                                                                                                                               ▁▄██████████████▉▀▀▔▝▕▟█▙▎▁                           ╚══╝╚══╝    ╚═╝     ╚═══╝  ╚══════╝╚═╝  ╚═╝╚═╝  ╚═══╝              
                                                                      ▂▅███████▜▇█████▌▔▔ ▔ ▕▖▛▛▜▀▘                                        
                                                                    ▃▇██▛█▉▅████▛▜▎██▍▔     ▕▍▔▝▔▗                                         
                                                                 ▁▃████▉██████▀█▎▆▁▗▘▁▔▔    ▝ ▎▔▔                                          
                                                               ▃▇██████████▛▀▘▔▔▔▁▁▂▘▘  ▕▏▁▂▁▁                                             
                                                            ▁▅███▜▙█████▉▛▘▁▕▕▕▏▃▃▆▘▔▔▁▔▔  ▔▔                                              
                                                          ▃▆██▛▚███▛▀▜▖▝▘▔▔▁▗▍▖▃█▇▀▔ ▁▔▔    ▁                                              
                                                       ▁▄▇█▛▘▂▔▕▘▛▏▃▅▎▘▂▕▔▕▐█▝▜▍▝▏ ▕▏▔  ▕▔▁▔▘                                              
                                                     ▂▆██▜▃▝▘▃▃▃▃▇█▇▇▇▘▘▏▔▔▔▔▔▕▏  ▔▔                                                       
                                                  ▂▅▛██▜▜▛▅▇████████▊▔▁ ▁▁▁ ▁▏▔ ▕▏ ▁ ▔▏ ▕▏                                                 
                                                 ▀▔▀▘▕▁▗▃██▛██████▀▚▏▔▏▕▁▏▁▏ ▁▏   ▁▕▁▔ ▁▁▔▁                                                
                                                ▕▖  ▕▕▏▗▇███▇▟▝▚▕▇█▊▝▁▝▏▔▏▏▁▃▄▅▎▞▞▇▜▀▘▔▔▔ ▁▁                                               
                                                 ▏▁▗▗▕▕▟▃███████████▇▃▃▃▄▅▝▟▉▘▄▆▇█▟▀▇███▙▂▁▔▔                                              
                                             ▗ ▃▖▕▕▃▐▗▉█▜████████████▌▘▇████████▛▍▃███████▋▀▏▖▗▂                                           
                                            ▞▘▕▘▁▏▁▄▂█▊▔▔▂▏▔▗▛▛▀▘▇█████████████████▟▅▙▄▃▃▂▂▃▁                                              
                                          ▗▞  ▁▁▗█▛▇▛▜▉▘▖▃▃▄▇▅▅▆▇██▛▀▛▀▔▀▐▟█▅▃▅▅▆▎▐▆▍ ▁▅▄ ▔▀▀▀▘▘                                           
                                         ▖▘   ▔  ▕▕▏▘▝▝██▀▀▃▄▅▅▆▆▃▜▜▃▟▆▄███▟▃▕▝▝▂▃▗▏▏▏▕▔▔ ▁    ▔▔                                          
                                        ▂▖        ▁▏▝▎ ▔▆██████████████▛████▛▎▂▁▔▂▁▕▔ ▁  ▘                                                 
                                    ▗  ▝▘        ▁▁▂▂▃▅▆█████████▜█████▛█████▇▆▟██▇▃▁                                                      
                               ▖  ▁ ▂▅▎            ▔▀▀▜█████████▊█▚██▛▘▝▀██████▛▜█▛▔▂▂                                                     
                                 ▝▝▀▛▌             ▁▁▕▜████▜█████▙█▇▍▁▟██████▇█▙███▖▐▝                                                     
               ▁▂▂▁▁              ▁▁▔            ▁▁▁▄▄▅▄▄▄▄▄▄▃▃▂▂▀▝▜▕▜███████▋▝▔▝█▉▔▝▀▆▖                                                   
            ▁ ▗█▙▃▔▜█▜▇▆▅▄▃▁                  ▝▏▏▕▐▛███████████████▃▅▅▃▂▝▂▀▀▀▁▖▆▄▂▔▕▝▘▁▗▖                                                  
              ▔▊▘▀▜█████████▊             ▔     ▗▂▆████████████████▇███▂▅▘▁▖▁   ▔▔▀ ▖▂▁▕▗▎▗▃                                               
               ▐▍  ▐███▛▉▜███▅▂▃▂▁          ▕▄▅▄▂▄▄▃▔▀▜█████████████████████▆▖▁▁▏▖▁▁  ▔▀▀▅▃▂▔                                              
             ▖ ▞▍     ▕▕▘▔▜███████▇▆▅▄▃▂ ▂  ▔▕▔▝▀▔███▌▃▄▄▂▜███████████████▛▜▛▎▝▔▁▂▖▁                                                       
               ▕   ▂     ▔▃█████████████▅▀▙▖   ▁▝▁█▛██████▄▎▛███████████▊▆█▉▎▂▖▏▝▔▀▘                                                       
         ▁▃  ▎▗▖▘▁▁▝▅▆█▃▔▃█▛▜████████████▄▞▙▂  ▔▕▔▁▔▀▝▜▛▝▜█▌█▍▐▜███████▇▜▜▜▆▃▔ ▁ ▝                  ▔                                      
        ▄█▆▄▅███████▆██▛▇▅▟▛▘▟████████████▌▖▘   ▝▀▘▘▖▔▘▕▕▖▝▀▗▘▕▁▁▀▜█████████▜▙▃▁▔                                                          
      ▖▃██▊▎▔▀▘▂▂▄▄▟▍▂▛▜▊▙▂▜██▇▇██▇▅▇█████▎▔▁ ▁▟▖▂▏▕▍         ▕▖▔▔ ▝▜███▛▀▜▀▜▍▍▔                                                           
    ▃▆▘▁▀▀▔▔▝▔▔    ▔▔▀▜▇▂▜▜▌▝▘▝▂▀████████▚▟▍▋ ▕▂▐▔▔ ▔▔         ▔ ▔  ▁ ▀▙▁▖▂▇█▇▉                                                            
 ▁ ▀▛▀▃▖▕▏             ▔▀█▙▉██▄▂▍▁▕▉▟▔▀▜██▍▖ ▟▍▀▘                   ▔   ▀▗▔▝▔                                                              
▝▔▝▔▔▔█▗                 ▔▜█▉▀██▇▃▁▝▀▅▇▗▛▘ ▗▜▋▝▝▏▀▏                       ▘▄                                                               
     ▔▛                    ▔▜██▛███▅▖▂▞▔ ▁ ▊▖  ▔▔ ▔ ▂                      ▔▚▃                                                             
     ▖                       ▝████▊█▛▉▗▅█▏▗▉▎        ▔                       ▝▚                                                            
                              ▝█████▚▕▃▇▜▔▖▘▝▄▘▏▂▂▁▁     ▘                     ▘                                                           
                               ▝██████▉▙▐▋▜▃▇████▀██▍▄▃▁                                                                                   
                                ▝███████▙▎ ▐▙▔▀▍▐▐█▉▄▂▜▉▇▆▄▃▂                                                                              
                                 ▝▜██████▜▇▂▕▍▔▗▗▄▙███▃▟▃▃▗▍▐█▇▆▄▃▁                                                                        
                                   ▝████████▌▏▟██████████▉▘▀▀▛█████▇▅▄▂                              ▃ ▖▝                                  
                                     ▀▀▔▀███▉▐████████▖▀▀▂▆▂▂▂▙▃▂▀▁▄███▇▅▃▁                         ▗▙▏                                    
                                       ▃ ▗██▚█▇██████▌▀▀▀▀▀▀▀▀▀▀▀▀▀▟██▄▞▜▝▛▍▖                   ▂▂  ▔▔                                     
                                       ▏▅██▉▟███▜██▀▔                ▔▔▀▀█▇▆▁                 ▁▁▍▏                                         
                                     ▁▘▗▟▇███▜█▂▛▀                        ▝▜▇▂  ▃▁       ▂▃▖  ▝▘▔                                          
                                     ▝▗▄▌▜▛▀▜▜▛▔                            ▔▀▘▗▅▄▂▝▏▕▝▔▔▔▘                                                
                                     ▘▙██▟▅▄█▉                                    ▔▔▔                                                      
                              ▁▁      ▝▜█▉▔▝██▙                                                                                            
                             ▟█▛▊       ▜█▙ ▕██▙                                                                                           
                             ▘▗▄▄▖       ▁▝█▍▔▜▜▙▅▏                                                                                        
                            ▗▞▘█▌▙▃▃▂ ▂▄▇▇▂▂   ▄▐█                                                                                         
                            ▝▍▟▟▜███▀▝▇█▀▔▔▕▘▃██▛▔▘                                                                                        
                            ▝▘▕▟▉▜█▛▄▇█▋▃▂▂▃▟▙█▘                                                                                           
                                    ▐▀▘▗▊▐█▇█▛▔                                                                                            
                                      ▝▕▁▝█▀▘                                                                                              
                                      ▕▛▘▔                                                                                                 

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

    // Listado de atajos de teclado, con relleno uniforme para alinear las columnas.
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

fn render_editor(f: &mut Frame, app: &mut App, area: Rect) {
    let max_lines = app.buffer.text.len_lines();
    
    // El ancho del gutter se ajusta dinámicamente según la cantidad de dígitos
    // del número de línea más alto (p. ej. " 1000 " requiere 6 espacios).
    let gutter_width = max_lines.to_string().len() + 2; 

    let view_height = area.height.saturating_sub(2) as usize;
    let view_width = area.width.saturating_sub(2 + gutter_width as u16) as usize;

    app.buffer.ensure_cursor_visible(view_width, view_height);

    let start_line = app.buffer.scroll_y;
    let end_line = (start_line + view_height).min(max_lines);

    let syntax = app.current_filepath.as_ref()
        .and_then(|p| p.extension())
        .and_then(|ext| app.syntax_set.find_syntax_by_extension(ext.to_str().unwrap_or("")))
        .unwrap_or_else(|| app.syntax_set.find_syntax_by_extension("rs").unwrap());

    let theme = &app.theme_set.themes["base16-ocean.dark"];
    let mut h = HighlightLines::new(syntax, theme);

    for line_idx in 0..start_line {
        let line_str = app.buffer.text.line(line_idx).to_string();
        let _ = h.highlight_line(&line_str, &app.syntax_set);
    }

    let mut lines = Vec::with_capacity(view_height);
    for line_idx in start_line..end_line {
        let line_str = app.buffer.text.line(line_idx).to_string();
        let ranges = h.highlight_line(&line_str, &app.syntax_set).unwrap_or_default();
        
        let has_error = app.diagnostics.contains_key(&line_idx);

        let mut spans = Vec::new();
        
        // Número de línea mostrado en el gutter, a la izquierda del contenido.
        let line_num_str = format!(" {:>w$} ", line_idx + 1, w = gutter_width - 2);
        spans.push(Span::styled(line_num_str, Style::default().fg(Color::DarkGray)));

        let mut in_leading_ws = true;
        let mut char_col = 0;

        // Se recorren los fragmentos resaltados por el motor de sintaxis,
        // insertando además las guías visuales de indentación.
        for (style, text) in ranges {
            let clean_text = text.replace('\n', "").replace('\r', "");
            if clean_text.is_empty() { continue; }
            
            let mut span_style = Style::default().fg(Color::Rgb(style.foreground.r, style.foreground.g, style.foreground.b));
            if has_error {
                span_style = span_style.add_modifier(Modifier::UNDERLINED).underline_color(Color::Red);
            }

            if !in_leading_ws {
                spans.push(Span::styled(clean_text, span_style));
            } else {
                let mut segment = String::new();
                for ch in clean_text.chars() {
                    if in_leading_ws && ch == ' ' {
                        if char_col % 4 == 0 {
                            if !segment.is_empty() {
                                spans.push(Span::styled(segment.clone(), span_style));
                                segment.clear();
                            }
                            // Cada tabstop de 4 espacios se marca con una guía vertical.
                            spans.push(Span::styled("│", Style::default().fg(Color::DarkGray)));
                        } else {
                            segment.push(' ');
                        }
                        char_col += 1;
                    } else {
                        in_leading_ws = false;
                        segment.push(ch);
                        char_col += 1;
                    }
                }
                if !segment.is_empty() {
                    spans.push(Span::styled(segment, span_style));
                }
            }
        }
        lines.push(Line::from(spans));
    }

    let file_name = app.current_filepath.as_ref().map_or("Nuevo".to_string(), |p| p.display().to_string());
    let title = match &app.status_msg {
        Some(msg) => format!(" {} | {} ", file_name, msg),
        None => format!(" {} ", file_name),
    };

    let p = Paragraph::new(lines)
        .block(Block::default().borders(Borders::ALL).title(title))
        .scroll((0, app.buffer.scroll_x as u16)); 

    f.render_widget(p, area);

    let cursor_y = app.buffer.text.char_to_line(app.buffer.cursor_char_idx);
    let cursor_x = app.buffer.cursor_char_idx - app.buffer.text.line_to_char(cursor_y);

    if app.state == AppState::Editing {
        // La posición del cursor en pantalla se calcula sumando el ancho actual
        // del gutter y el desplazamiento (scroll) del buffer.
        let screen_x = area.x + 1 + gutter_width as u16 + (cursor_x.saturating_sub(app.buffer.scroll_x)) as u16;
        let screen_y = area.y + 1 + (cursor_y.saturating_sub(app.buffer.scroll_y)) as u16;
        
        if !app.completions.is_empty() {
            let comp_width = 45;
            let comp_height = (app.completions.len().min(8)) as u16 + 2;
            let popup_y = if screen_y + comp_height < area.bottom() { screen_y + 1 } else { screen_y.saturating_sub(comp_height) };
            
            // Se limita la posición horizontal del popup para que no se salga
            // del área visible por el borde derecho.
            let max_x = area.right().saturating_sub(comp_width);
            let safe_screen_x = screen_x.min(max_x);
            let popup_area = Rect::new(safe_screen_x, popup_y, comp_width, comp_height);
            
            let items: Vec<ListItem> = app.completions.iter().take(15).map(|c| {
                let (kind_icon, kind_color) = match c.kind {
                    Some(lsp_types::CompletionItemKind::METHOD) => ("ƒ (met)", Color::LightMagenta),
                    Some(lsp_types::CompletionItemKind::FUNCTION) => ("ƒ (fn)", Color::Magenta),
                    Some(lsp_types::CompletionItemKind::STRUCT) => ("{} (str)", Color::LightYellow),
                    Some(lsp_types::CompletionItemKind::MODULE) => ("📦 (mod)", Color::LightBlue),
                    Some(lsp_types::CompletionItemKind::KEYWORD) => ("🔑 (key)", Color::DarkGray),
                    Some(lsp_types::CompletionItemKind::VARIABLE) => ("α (var)", Color::LightCyan),
                    Some(lsp_types::CompletionItemKind::PROPERTY) => ("• (prop)", Color::Cyan),
                    Some(lsp_types::CompletionItemKind::ENUM) => ("◂▸ (enm)", Color::Yellow),
                    _ => ("  (txt)", Color::Gray),
                };

                let line = Line::from(vec![
                    Span::styled(format!("{:<10}", kind_icon), Style::default().fg(kind_color)),
                    Span::raw(&c.label),
                ]);
                ListItem::new(line)
            }).collect();

            let list = List::new(items)
                .block(Block::default().borders(Borders::ALL).style(Style::default().bg(Color::Rgb(30, 30, 30))))
                .highlight_style(Style::default().bg(Color::Cyan).fg(Color::Black).add_modifier(Modifier::BOLD));

            f.render_widget(ratatui::widgets::Clear, popup_area);
            f.render_stateful_widget(list, popup_area, &mut app.completion_state);
        } else if cursor_y >= app.buffer.scroll_y && cursor_y < app.buffer.scroll_y + view_height {
            if cursor_x >= app.buffer.scroll_x && cursor_x < app.buffer.scroll_x + view_width {
                f.set_cursor_position((screen_x, screen_y));
            }
        }
    }
}
