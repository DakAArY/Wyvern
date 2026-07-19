use crate::editor::EditorBuffer;
use crate::explorer::FileExplorer;
use crate::lsp::LspClient;
use std::path::PathBuf;
use syntect::parsing::SyntaxSet;
use syntect::highlighting::ThemeSet;
use std::collections::HashMap;
use lsp_types::{Uri, Diagnostic, CompletionItemKind};
use ratatui::widgets::ListState;

#[derive(Clone)]
pub struct CompletionOption {
    pub label: String,
    pub kind: Option<CompletionItemKind>,
}

#[derive(PartialEq)]
pub enum AppState {
    Intro,
    Editing,
    Exploring,
}

pub struct App {
    pub state: AppState,
    pub buffer: EditorBuffer,
    pub current_filepath: Option<PathBuf>,
    pub show_tree: bool,
    pub explorer: FileExplorer,
    pub quit: bool,
    pub status_msg: Option<String>,
    pub syntax_set: SyntaxSet,
    pub theme_set: ThemeSet,
    pub lsp_client: Option<LspClient>,
    pub document_version: i32,
    pub current_uri: Option<Uri>,
    
    // LSP State
    // Diagnósticos del LSP indexados por número de línea.
    pub diagnostics: HashMap<usize, Vec<Diagnostic>>,
    pub completions: Vec<CompletionOption>,
    pub completion_state: ListState,
    pub pending_completion_id: Option<u64>,
}

impl App {
    pub fn new() -> Self {
        let current_dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        Self {
            state: AppState::Intro,
            buffer: EditorBuffer::new(),
            current_filepath: None,
            show_tree: false,
            explorer: FileExplorer::new(current_dir),
            quit: false,
            status_msg: None,
            syntax_set: SyntaxSet::load_defaults_newlines(),
            theme_set: ThemeSet::load_defaults(),
            lsp_client: None,
            document_version: 0,
            current_uri: None,
            diagnostics: HashMap::new(),
            completions: Vec::new(),
            completion_state: ListState::default(),
            pending_completion_id: None,
        }
    }

    pub fn toggle_tree(&mut self) {
        self.show_tree = !self.show_tree;
        if self.show_tree {
            self.state = AppState::Exploring;
            let _ = self.explorer.reload();
        } else if self.current_filepath.is_some() {
            self.state = AppState::Editing;
        } else {
            self.state = AppState::Intro;
        }
    }
    
    pub fn load_file(&mut self, path: std::path::PathBuf) {
        if let Ok(buf) = crate::editor::EditorBuffer::load_from_file(&path) {
            self.buffer = buf;
            self.state = crate::app::AppState::Editing;
            self.show_tree = false;
            self.document_version = 1;

            // Se construye la URI del documento antes de cualquier uso condicional de `path`.
            let file_url = url::Url::from_file_path(&path).unwrap_or_else(|_| url::Url::parse("file:///").unwrap());
            let uri: lsp_types::Uri = file_url.as_str().parse().unwrap();
            self.current_uri = Some(uri.clone());

            // La extensión se obtiene por referencia para evitar mover `path` antes de tiempo.
            if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
                let current_dir = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
                
                if let Some(client) = crate::lsp::LspClient::start_for_extension(ext, current_dir) {
                    // La notificación `didOpen` no se envía aquí: se difiere hasta que
                    // el handshake de inicialización del servidor LSP se complete (ver main.rs).
                    self.status_msg = Some(format!("LSP Iniciando ({})...", ext));
                    self.lsp_client = Some(client);
                } else {
                    self.status_msg = Some(format!("Cargado: {}", path.display()));
                }
            }
            
            // `path` se mueve al final del método para respetar las reglas del borrow checker.
            self.current_filepath = Some(path);
        }
    }

    pub fn save_file(&mut self) {
        if let Some(path) = &self.current_filepath {
            match self.buffer.save_to_file(path) {
                Ok(_) => self.status_msg = Some("Guardado exitosamente".to_string()),
                Err(e) => self.status_msg = Some(format!("Error: {}", e)),
            }
        } else {
            self.status_msg = Some("No hay archivo abierto para guardar".to_string());
        }
    }

    pub fn trigger_completion(&mut self) {
        if let (Some(client), Some(uri)) = (&mut self.lsp_client, &self.current_uri) {
            let (line, col) = self.buffer.get_lsp_position();
            self.pending_completion_id = Some(client.request_completion(uri.clone(), line, col));
            // Se descartan las sugerencias anteriores para que la UI refleje de inmediato
            // que hay una nueva solicitud de autocompletado en curso.
            self.completions.clear();
        }
    }
}
