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

#[derive(Clone)]
pub enum PromptIntent {
    SaveAs(PathBuf),
    Rename(PathBuf),
    Delete(PathBuf),
}

pub struct PromptState {
    pub intent: PromptIntent,
    pub input: String,
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
    pub working_dir: PathBuf,
    pub prompt: Option<PromptState>,
    pub git_ctx: crate::git::GitContext,
}

impl App {
    pub fn new() -> Self {
        let current_dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let git_ctx = crate::git::GitContext::refresh(&current_dir, None);
        Self {
            state: AppState::Intro,
            buffer: EditorBuffer::new(),
            current_filepath: None,
            working_dir: current_dir.clone(),
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
            prompt: None,
            git_ctx,
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

    pub fn new_blank_file(&mut self) {
        self.buffer = EditorBuffer::new();
        self.current_filepath = None;
        self.working_dir = self.explorer.current_dir.clone();
        self.state = AppState::Editing;
        self.show_tree = false;
        self.lsp_client = None;
        self.current_uri = None;
        self.document_version = 0;
        self.diagnostics.clear();
        self.completions.clear();
        self.status_msg = Some("Archivo en memoria. CTRL + S para guardar y asignar ruta".into());
    }

    pub fn setup_lsp_for_current_file(&mut self) {
        if let Some(path) = & self.current_filepath {
            let file_url = url::Url::from_file_path(path).unwrap_or_else(|_| url::Url::parse("file:///").unwrap());
            let uri: lsp_types::Uri = file_url.as_str().parse().unwrap();
            self.current_uri = Some(uri.clone());

            if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
                let current_dir = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
                if let Some(client) = crate::lsp::LspClient::start_for_extension(ext, current_dir) {
                    self.status_msg = Some(format!("LSP Iniciadio ({})...", ext));
                    self.lsp_client = Some(client);
                }
            }
        }
    }
    
    pub fn load_file(&mut self, path: std::path::PathBuf) {
        if let Ok(buf) = crate::editor::EditorBuffer::load_from_file(&path) {
            self.buffer = buf;
            self.state = crate::app::AppState::Editing;
            self.show_tree = false;
            self.document_version = 1;
            self.current_filepath = Some(path.clone());
            self.setup_lsp_for_current_file();
            self.working_dir = path.parent().map(|p| p.to_path_buf()).unwrap_or_else(|| PathBuf::from("."));
            self.git_ctx = crate::git::GitContext::refresh(&self.working_dir, self.current_filepath.as_deref());

            if self.lsp_client.is_none() {
                self.status_msg = Some(format!("Cargado: {}", path.display()));
            }
        }
    }

    pub fn trigger_save(&mut self) {
        if self.current_filepath.is_some() {
            self.save_file();
        } else {
            self.prompt = Some(PromptState {
                intent: PromptIntent::SaveAs(self.working_dir.clone()),
                input: String::new()
            });
        }
    }

    pub fn save_file(&mut self) {
        if let Some(path) = &self.current_filepath {
            match self.buffer.save_to_file(path) {
                Ok(_) => {
                    self.status_msg = Some("Guardado Exitosamente".to_string());
                    let _ = self.explorer.reload();
                    self.git_ctx = crate::git::GitContext::refresh(&self.working_dir, Some(path))
                },
                Err(e) => self.status_msg = Some(format!("Error: {}", e)),
            }
        } else {
            self.status_msg = Some("No hay archivo abierto para guardar".to_string());
        }
    }

    pub fn trigger_rename(&mut self) {
        if let Some(entry) = self.explorer.get_selected() {
            if entry.name == ".." { return; }
            self.prompt = Some(PromptState { 
                intent: PromptIntent::Rename(entry.path.clone()),
                input: entry.name.clone(),
            });
        }
    }

    pub fn trigger_delete(&mut self) {
        if let Some(entry) = self.explorer.get_selected() {
            if entry.name == ".." { return; }
            self.prompt = Some(PromptState { 
                intent: PromptIntent::Delete(entry.path.clone()),
                input: String::new(),
            });
        }
    }

    pub fn execute_prompt(&mut self) {
        if let Some(prompt) = self.prompt.take() {
            match prompt.intent {
                PromptIntent::SaveAs(dir) => {
                    if prompt.input.trim().is_empty() {
                        self.status_msg = Some("No se puede guardar: Nombre vacio".into());
                        return;
                    }
                    let new_path = dir.join(prompt.input.trim());
                    self.current_filepath = Some(new_path.clone());
                    self.save_file();
                    if self.lsp_client.is_none() {
                        self.setup_lsp_for_current_file();
                    }
                }
                PromptIntent::Rename(old_path) => {
                    if prompt.input.trim().is_empty() { return; }
                    let new_path = old_path.with_file_name(prompt.input.trim());
                    if std::fs::rename(&old_path, &new_path).is_ok() {
                        self.status_msg = Some("Renombrado Exitosamente".into());
                        let _ = self.explorer.reload();
                        if self.current_filepath.as_ref() == Some(&old_path) {
                            self.current_filepath = Some(new_path);
                        }
                    } else {
                        self.status_msg = Some("Error al renombrar".into());
                    }
                }
                PromptIntent::Delete(path) => {
                    if prompt.input.trim().eq_ignore_ascii_case("y") {
                        let is_dir = path.is_dir();
                        let res = if is_dir { std::fs::remove_dir_all(&path) } else { std::fs::remove_file(&path) };

                        if res.is_ok() {
                            self.status_msg = Some("Eliminado exitosamente".into());
                            let _ = self.explorer.reload();
                            if self.current_filepath.as_ref() == Some(&path) {
                                self.buffer = EditorBuffer::new();
                                self.current_filepath = None;
                                self.lsp_client = None;
                                self.state = AppState::Intro;
                            }
                        } else {
                            self.status_msg = Some("Error al eliminar.".into());
                        }
                    }
                }
            }
            
            self.git_ctx = crate::git::GitContext::refresh(&self.working_dir, self.current_filepath.as_deref());
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
