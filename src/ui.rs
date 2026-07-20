use lsp_types::selection_range;
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect, Flex},
    style::{Color, Style, Modifier},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, List, ListItem, HighlightSpacing, BorderType},
};
use syntect::easy::HighlightLines;
use crate::app::{App, AppState};
use ratatui::widgets::Clear;

/// Punto de entrada del renderizado de un frame. Arma el layout raíz
/// (contenido + barra de estado), reserva la franja izquierda para el
/// árbol de archivos si está visible, dibuja la vista principal según el
/// estado de la app, y por encima de todo superpone el prompt modal y la
/// ayuda si están activos.
pub fn render(f: &mut Frame, app: &mut App) {
    let root_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(1)])
        .split(f.area());
    
    let main_layout = Layout::default()
        .direction(Direction::Horizontal)
        .constraints(if app.show_tree {
            vec![Constraint::Percentage(20), Constraint::Percentage(80)]
        } else {
            vec![Constraint::Percentage(100)]
        })
        .split(root_layout[0]);

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
    
    // El prompt modal (guardar como / renombrar / eliminar) se dibuja
    // centrado sobre todo lo demás, con su propio texto según la intención.
    if let Some(prompt) = &app.prompt { 
        let (title, prompt_text) = match &prompt.intent { 
            crate::app::PromptIntent::SaveAs(dir) => (" Guardar Como ", format!("Ruta base: {}\nNombre:", dir.display())),
            crate::app::PromptIntent::Rename(_) => (" Renombrar ", "Nuevo nombre:".to_string()), 
            crate::app::PromptIntent::Delete(p) => (" Confirmar ", format!("¿Eliminar '{}'? (y/N):", p.file_name().unwrap_or_default().to_string_lossy())),
        }; 

        let block = Block::default() 
            .borders(Borders::ALL) 
            .title(title)
            .style(Style::default().bg(Color::Rgb(25, 25, 25)).fg(Color::White).add_modifier(Modifier::BOLD))
            .border_style(Style::default().fg(Color::Yellow)); 

        let input_display = format!("> {}█", prompt.input); 
        
        let text = vec![ 
            Line::from(prompt_text), 
            Line::from(""),
            Line::from(Span::styled(input_display, Style::default().fg(Color::Cyan))),
        ]; 
        let paragraph = Paragraph::new(text).block(block).alignment(ratatui::layout::Alignment::Left);
        
        let [center_y] = Layout::vertical([Constraint::Length(6)]).flex(Flex::Center).areas(f.area());   
        let [center_x] = Layout::horizontal([Constraint::Length(60)]).flex(Flex::Center).areas(center_y);    

        f.render_widget(Clear, center_x);   
        f.render_widget(paragraph, center_x);   
    }

    if app.show_help {
        render_help(f);
    }
    
    render_status_line(f, app, root_layout[1]) 
}

/// Dibuja el cuadro de ayuda con la lista de atajos, centrado en pantalla
/// sobre un fondo limpio (`Clear`) para que tape el contenido de abajo.
fn render_help(f: &mut Frame) {
    let help_text = vec![
        Line::from(Span::styled(" COMANDOS WYVERN ", Style::default().add_modifier(Modifier::BOLD).fg(Color::Cyan))),
        Line::from(""),
        Line::from(vec![Span::styled(" F1             ", Style::default().fg(Color::Yellow)), Span::raw("- Mostrar/Ocultar esta ayuda")]),
        Line::from(vec![Span::styled(" F2             ", Style::default().fg(Color::Yellow)), Span::raw("- Explorar archivos")]),
        Line::from(vec![Span::styled(" Ctrl + S       ", Style::default().fg(Color::Yellow)), Span::raw("- Guardar archivo")]),
        Line::from(vec![Span::styled(" Ctrl + Q       ", Style::default().fg(Color::Yellow)), Span::raw("- Salir")]),
        Line::from(vec![Span::styled(" Ctrl + C/X/V   ", Style::default().fg(Color::Yellow)), Span::raw("- Copiar, Cortar, Pegar")]),
        Line::from(vec![Span::styled(" Shift + Flechas", Style::default().fg(Color::Yellow)), Span::raw("- Seleccionar texto")]),
        Line::from(vec![Span::styled(" Mouse Clic     ", Style::default().fg(Color::Yellow)), Span::raw("- Mover cursor")]),
        Line::from(vec![Span::styled(" Mouse Drag     ", Style::default().fg(Color::Yellow)), Span::raw("- Seleccionar con Mouse")]),
        Line::from(vec![Span::styled(" Mouse Dbl-Clic ", Style::default().fg(Color::Yellow)), Span::raw("- Abrir en explorador")]),
    ];
    let block = Block::default()
        .borders(Borders::ALL)
        .style(Style::default().bg(Color::Rgb(20, 20, 20)))
        .border_style(Style::default().fg(Color::Cyan));
        
    let paragraph = Paragraph::new(help_text).block(block).alignment(ratatui::layout::Alignment::Left);
    let [center_y] = Layout::vertical([Constraint::Length(13)]).flex(Flex::Center).areas(f.area());   
    let [center_x] = Layout::horizontal([Constraint::Length(50)]).flex(Flex::Center).areas(center_y);    
    f.render_widget(Clear, center_x);   
    f.render_widget(paragraph, center_x); 
}

/// Dibuja la barra de estado inferior: a la izquierda el modo actual y el
/// resumen de git (rama + conteo de líneas añadidas/modificadas/eliminadas
/// del archivo abierto); a la derecha el conteo de diagnósticos LSP
/// (errores/warnings) y la posición del cursor junto al lenguaje detectado
/// por extensión.
fn render_status_line(f: &mut Frame, app: &App, area: Rect) {
    let mode_str = match app.state {
        AppState::Editing => " EDIT ",
        AppState::Exploring => " TREE ",
        AppState::Intro => " NORMAL ",
    };

    let stats_str = if app.git_ctx.is_repo && app.current_filepath.is_some() {
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

    let lang = app.current_filepath.as_ref()
        .and_then(|p| p.extension())
        .and_then(|e| e.to_str())
        .unwrap_or("txt")
        .to_uppercase();

    let line = app.buffer.text.char_to_line(app.buffer.cursor_char_idx) + 1;
    let col = app.buffer.cursor_char_idx - app.buffer.text.line_to_char(line - 1) + 1;
    let pos_str = format!(" Ln {}, Col {} | {} ", line, col, lang);

    let left_line = Line::from(vec![
        Span::styled(mode_str, Style::default().bg(Color::Cyan).fg(Color::Black).add_modifier(Modifier::BOLD)),
        Span::styled(&git_str, Style::default().bg(Color::DarkGray).fg(Color::White)),
    ]);

    let right_line = Line::from(vec![
        Span::styled(&diag_str, Style::default().fg(if err_count > 0 { Color::Red } else { Color::Gray })),
        Span::styled(&pos_str, Style::default().fg(Color::White)),
    ]);

    let layout = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(area);

    f.render_widget(Block::default().style(Style::default().bg(Color::Rgb(30, 30, 30))), area);
    f.render_widget(Paragraph::new(left_line).alignment(ratatui::layout::Alignment::Left), layout[0]);
    f.render_widget(Paragraph::new(right_line).alignment(ratatui::layout::Alignment::Right), layout[1]);
}

/// Pantalla de bienvenida: logo ASCII de la app centrado verticalmente,
/// seguido del número de versión y un resumen de los atajos principales.
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

/// Dibuja el árbol de archivos lateral: cada entrada con su ícono
/// (carpeta/archivo), y un indicador amarillo si es el archivo actualmente
/// abierto y tiene cambios sin guardar. El borde cambia de color cuando el
/// árbol tiene el foco (`AppState::Exploring`).
fn render_tree(f: &mut Frame, app: &mut App, area: Rect) {
    let items: Vec<ListItem> = app.explorer.entries.iter().map(|e| {
        let (prefix, color) = if e.is_dir { 
            ("📁 ", Color::Blue) 
        } else { 
            ("📄 ", Color::White) 
        };
        
        let mut spans = vec![
            Span::styled(prefix, Style::default().fg(color)),
            Span::raw(&e.name),
        ];

        if !e.is_dir && Some(&e.path) == app.current_filepath.as_ref() && app.is_dirty {
            spans.push(Span::styled(" ●", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)));
        }

        ListItem::new(Line::from(spans))
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

/// Dibuja el panel del editor completo: la línea de pestaña (tabline) con
/// el nombre del archivo e indicador de cambios sin guardar, y debajo el
/// texto resaltado por sintaxis con gutter de números de línea, marcador de
/// git por línea, guías de indentación y subrayado de diagnósticos. También
/// gestiona el popup de autocompletado y la posición visible del cursor
/// del terminal.
fn render_editor(f: &mut Frame, app: &mut App, area: Rect) {
    let edit_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(0)])
        .split(area);
    
    let tab_area = edit_layout[0];
    let text_area = edit_layout[1];

    let file_name = app.current_filepath.as_ref().map_or("Nuevo".to_string(), |p| p.file_name().unwrap_or_default().to_string_lossy().into_owned());
    let ext = app.current_filepath.as_ref().and_then(|p| p.extension()).and_then(|s| s.to_str()).unwrap_or("");
    let (icon, icon_color) = match ext {
        "rs" => (" ", Color::Rgb(222, 90, 44)), 
        "py" => ("󰌠 ", Color::Yellow),
        "md" => (" ", Color::LightBlue),
        "c" | "cpp" => (" ", Color::LightBlue),
        _ => (" ", Color::White),
    };

    let dirty_sym = if app.is_dirty { "●" } else { "×" };
    let dirty_color = if app.is_dirty { Color::Yellow } else { Color::DarkGray };
    let tab_bg = Color::Rgb(40, 40, 40);

    let tab_spans = vec![
        Span::styled(" ", Style::default().bg(tab_bg)),
        Span::styled(icon, Style::default().fg(icon_color).bg(tab_bg)),
        Span::styled(format!("{} ", file_name), Style::default().fg(Color::White).bg(tab_bg).add_modifier(Modifier::ITALIC)),
        Span::styled(dirty_sym, Style::default().fg(dirty_color).bg(tab_bg)),
        Span::styled(" │ ", Style::default().fg(Color::DarkGray).bg(tab_bg)),
    ];
    let tab_line = Paragraph::new(Line::from(tab_spans)).style(Style::default().bg(Color::Rgb(20, 20, 20)));
    f.render_widget(tab_line, tab_area);

    let max_lines = app.buffer.text.len_lines();
    let gutter_num_width = max_lines.to_string().len().max(1);
    let gutter_total_width = gutter_num_width + 3;

    // El área de texto ya no tiene bordes propios (Borders::ALL fue removido
    // de este panel), por lo que no hace falta descontar 2 columnas/filas por marco.
    let view_height = text_area.height as usize; 
    let view_width = text_area.width.saturating_sub(gutter_total_width as u16) as usize;

    app.buffer.ensure_cursor_visible(view_width, view_height);

    let start_line = app.buffer.scroll_y;
    let end_line = (start_line + view_height).min(max_lines);

    let syntax = app.current_filepath.as_ref()
        .and_then(|p| p.extension())
        .and_then(|ext| app.syntax_set.find_syntax_by_extension(ext.to_str().unwrap_or("")))
        .unwrap_or_else(|| app.syntax_set.find_syntax_by_extension("rs").unwrap());

    let theme = &app.theme_set.themes["base16-ocean.dark"];
    let mut h = HighlightLines::new(syntax, theme);

    // syntect es un resaltador con estado: hay que "recorrer" (sin dibujar)
    // todas las líneas anteriores a la vista visible para que el
    // resaltador arrastre correctamente el contexto (p. ej. comentarios de
    // bloque o strings multilínea abiertos antes del scroll actual).
    for line_idx in 0..start_line {
        let line_str = app.buffer.text.line(line_idx).to_string();
        let _ = h.highlight_line(&line_str, &app.syntax_set);
    }
    
    let selection_range = app.buffer.get_selection_range();
    let mut lines = Vec::with_capacity(view_height);
    
    for line_idx in start_line..end_line {
        let line_str = app.buffer.text.line(line_idx).to_string();
        let ranges = h.highlight_line(&line_str, &app.syntax_set).unwrap_or_default();
        
        let has_error = app.diagnostics.contains_key(&line_idx);
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

        let mut in_leading_ws = true;
        let mut char_col = 0;
        let mut current_global_idx = app.buffer.text.line_to_char(line_idx);

        // Recorremos cada rango de resaltado de syntect carácter por
        // carácter (en vez de span por span) para poder intercalar el
        // estilo de selección de texto sin romper los límites de color
        // que ya trae syntect, y para poder sustituir los espacios de
        // indentación por guías verticales cada 4 columnas.
        for (style, text) in ranges {
            let clean_text = text.replace('\n', "").replace('\r', "");
            if clean_text.is_empty() { continue; }
            
            let mut span_style = Style::default().fg(Color::Rgb(style.foreground.r, style.foreground.g, style.foreground.b));
            if has_error { span_style = span_style.add_modifier(Modifier::UNDERLINED).underline_color(Color::Red); }

            let mut segment = String::new();
            let mut active_style = span_style;
            let mut is_first = true;

            for ch in clean_text.chars() {
                let mut char_style = span_style;
                if let Some(ref sel) = selection_range {
                    if sel.contains(&current_global_idx) { char_style = char_style.bg(Color::DarkGray); }
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

                if in_leading_ws && ch == ' ' {
                    if char_col % 4 == 0 {
                        if !segment.is_empty() {
                            spans.push(Span::styled(segment.clone(), active_style));
                            segment.clear();
                        }
                        spans.push(Span::styled("│", Style::default().fg(Color::DarkGray)));
                    } else { segment.push(' '); }
                } else {
                    in_leading_ws = false;
                    segment.push(ch);
                }

                char_col += 1;
                current_global_idx += 1;
            }
            if !segment.is_empty() { spans.push(Span::styled(segment, active_style)); }
        }
        lines.push(Line::from(spans));    
    }

    // Se dibuja el texto sin bloque/borde propio: el marco visual ya lo
    // aportan la tabline de arriba y el gutter incrustado en cada línea.
    let p = Paragraph::new(lines)
        .block(Block::default()) 
        .scroll((0, app.buffer.scroll_x as u16)); 

    f.render_widget(p, text_area);

    if app.state == AppState::Editing {
        let cursor_y = app.buffer.text.char_to_line(app.buffer.cursor_char_idx);
        let cursor_x = app.buffer.cursor_char_idx - app.buffer.text.line_to_char(cursor_y);
        
        // Coordenadas de pantalla del cursor, ya con el layout sin bordes
        // (sin el antiguo desfase de +1 que compensaba el borde del bloque).
        let screen_x = text_area.x + gutter_total_width as u16 + (cursor_x.saturating_sub(app.buffer.scroll_x)) as u16;
        let screen_y = text_area.y + (cursor_y.saturating_sub(app.buffer.scroll_y)) as u16;
                        
        if !app.completions.is_empty() {
            let comp_width = 52; 
            let comp_height = (app.completions.len().min(8)) as u16 + 2;
            
            // Se calcula la posición del popup para que nunca tape al
            // cursor: se prefiere dibujarlo debajo; si no entra en el
            // espacio restante hacia abajo, se dibuja hacia arriba sin
            // desplazar `screen_y` (la posición real del cursor no cambia).
            let popup_y = if screen_y + 1 + comp_height <= text_area.bottom() { 
                screen_y + 1 
            } else { 
                screen_y.saturating_sub(comp_height) 
            };
            
            let max_x = text_area.right().saturating_sub(comp_width);
            let safe_screen_x = screen_x.min(max_x);
            let popup_area = Rect::new(safe_screen_x, popup_y, comp_width, comp_height);
            
            let items: Vec<ListItem> = app.completions.iter().take(15).map(|c| {
                let (kind_icon, kind_str, kind_color) = match c.kind {
                    Some(lsp_types::CompletionItemKind::METHOD) => ("ƒ", "Method", Color::LightMagenta),
                    Some(lsp_types::CompletionItemKind::FUNCTION) => ("ƒ", "Function", Color::Magenta),
                    Some(lsp_types::CompletionItemKind::STRUCT) => ("{}","Struct", Color::LightYellow),
                    Some(lsp_types::CompletionItemKind::MODULE) => ("📦","Module", Color::LightBlue),
                    Some(lsp_types::CompletionItemKind::KEYWORD) => ("🔑","Keyword", Color::DarkGray),
                    Some(lsp_types::CompletionItemKind::VARIABLE) => ("α", "Variable", Color::LightCyan),
                    Some(lsp_types::CompletionItemKind::PROPERTY) => ("•", "Property", Color::Cyan),
                    Some(lsp_types::CompletionItemKind::ENUM) => ("◂▸","Enum", Color::Yellow),
                    _ => (" ", "Text", Color::Gray),
                };

                // Formato estilo nvim-cmp: la etiqueta se trunca si es muy
                // larga y el tipo queda justificado contra el margen derecho.
                let max_label_len = comp_width as usize - kind_str.len() - 7;
                let mut display_label = c.label.clone();
                if display_label.len() > max_label_len {
                    display_label.truncate(max_label_len - 1);
                    display_label.push('…');
                }
                let padding = " ".repeat(max_label_len.saturating_sub(display_label.len()));

                let line = Line::from(vec![
                    Span::styled(format!(" {} ", kind_icon), Style::default().fg(kind_color).bg(Color::Rgb(35, 35, 35))),
                    Span::styled(format!(" {} ", display_label), Style::default().fg(Color::White)),
                    Span::raw(padding),
                    Span::styled(kind_str, Style::default().fg(Color::DarkGray)),
                    Span::raw(" "),
                ]);
                ListItem::new(line)
            }).collect();

            let list = List::new(items)
                .block(Block::default()
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(Style::default().fg(Color::DarkGray))
                    .style(Style::default().bg(Color::Rgb(25, 25, 25))))
                .highlight_style(Style::default().bg(Color::Rgb(45, 60, 80)).fg(Color::White).add_modifier(Modifier::BOLD));

            f.render_widget(ratatui::widgets::Clear, popup_area);
            f.render_stateful_widget(list, popup_area, &mut app.completion_state);
        } else if cursor_y >= app.buffer.scroll_y && cursor_y < app.buffer.scroll_y + view_height {
            if cursor_x >= app.buffer.scroll_x && cursor_x < app.buffer.scroll_x + view_width {
                if selection_range.is_none() {
                    f.set_cursor_position((screen_x, screen_y));
                }
            }
        } 
    }
}
