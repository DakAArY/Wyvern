use crate::editor::EditorBuffer;
use crate::explorer::FileExplorer;
use crate::lsp::LspClient;
use std::path::PathBuf;
use std::time::Instant;
use syntect::parsing::SyntaxSet;
use syntect::highlighting::ThemeSet;
use std::collections::HashMap;
use lsp_types::{Uri, Diagnostic, CompletionItemKind};
use ratatui::widgets::ListState;

/// Un ítem de autocompletado ya resuelto y listo para mostrarse en el popup.
/// Es una versión simplificada del `CompletionItem` de `lsp_types`: solo
/// conserva lo necesario para el renderizado (etiqueta y tipo/ícono).
#[derive(Clone)]
pub struct CompletionOption {
    pub label: String,
    pub kind: Option<CompletionItemKind>,
}

/// Modo de interacción actual de la interfaz. Determina qué panel recibe
/// el foco del teclado y qué vista se dibuja como contenido principal.
#[derive(PartialEq)]
pub enum AppState {
    /// Pantalla de bienvenida, mostrada antes de abrir o crear un archivo.
    Intro,
    /// Foco en el buffer de texto: edición normal.
    Editing,
    /// Foco en el árbol de archivos lateral.
    Exploring,
}

/// Acción pendiente de confirmación por parte del usuario a través del
/// cuadro de diálogo modal (`PromptState`). Cada variante lleva la ruta
/// sobre la que se va a actuar una vez el usuario confirme la entrada.
#[derive(Clone)]
pub enum PromptIntent {
    /// Guardar el buffer actual bajo un nombre nuevo, dentro del directorio dado.
    SaveAs(PathBuf),
    /// Renombrar la entrada del explorador ubicada en esta ruta.
    Rename(PathBuf),
    /// Eliminar la entrada del explorador ubicada en esta ruta (requiere "y" para confirmar).
    Delete(PathBuf),
}

/// Estado de un cuadro de diálogo modal de una sola línea, usado para
/// pedir al usuario un nombre de archivo o una confirmación.
pub struct PromptState {
    pub intent: PromptIntent,
    pub input: String,
}

/// Estado global de la aplicación. Se pasa por referencia mutable a lo
/// largo de todo el ciclo de eventos (entrada de teclado/mouse, renderizado
/// y procesamiento de mensajes del LSP).
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
    /// Número de versión del documento enviado al LSP (protocolo `textDocument/didChange`).
    /// Debe incrementarse en cada notificación de cambio.
    pub document_version: i32,
    /// URI del archivo actualmente abierto, tal como la espera el protocolo LSP.
    pub current_uri: Option<Uri>,
    /// Indica si el buffer tiene cambios sin guardar desde el último `save_file`/`load_file`.
    pub is_dirty: bool,

    // --- Estado de interfaz y funciones auxiliares ---
    pub show_help: bool,
    pub clipboard: Option<String>,
    /// Marca de tiempo y coordenadas del último click, usada para detectar doble click.
    pub last_click: Option<(Instant, u16, u16)>,

    /// Diagnósticos del LSP indexados por número de línea (0-based).
    pub diagnostics: HashMap<usize, Vec<Diagnostic>>,
    pub completions: Vec<CompletionOption>,
    pub completion_state: ListState,
    /// ID de la petición `textDocument/completion` en curso, para poder
    /// identificar la respuesta correspondiente cuando llegue del LSP.
    pub pending_completion_id: Option<u64>,
    pub working_dir: PathBuf,
    /// Diálogo modal activo, si el usuario está a mitad de una acción
    /// (guardar como, renombrar o eliminar).
    pub prompt: Option<PromptState>,
    pub git_ctx: crate::git::GitContext,
}

impl App {
    /// Crea el estado inicial de la aplicación a partir del directorio de trabajo actual.
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
            is_dirty: false,
            show_help: false,
            clipboard: None,
            last_click: None,
            diagnostics: HashMap::new(),
            completions: Vec::new(),
            completion_state: ListState::default(),
            pending_completion_id: None, 
            prompt: None,
            git_ctx,
        }
    }

    /// Alterna la visibilidad del árbol de archivos y ajusta el estado de foco
    /// en consecuencia (Explorando si se abre, Editando/Intro si se cierra).
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

    /// Descarta el buffer actual y comienza uno nuevo, vacío y sin ruta asignada.
    /// El archivo se materializa en disco recién al guardarlo (ver `trigger_save`).
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
        self.is_dirty = false;
        self.status_msg = Some("Archivo en memoria. CTRL + S para guardar y asignar ruta".into());
    }

    /// Construye la URI del archivo actual y, según su extensión, intenta
    /// lanzar el servidor de lenguaje (LSP) correspondiente disponible en el PATH.
    pub fn setup_lsp_for_current_file(&mut self) {
        if let Some(path) = &self.current_filepath {
            let file_url = url::Url::from_file_path(path).unwrap_or_else(|_| url::Url::parse("file:///").unwrap());
            let uri: lsp_types::Uri = file_url.as_str().parse().unwrap();
            self.current_uri = Some(uri.clone());

            if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
                let current_dir = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
                if let Some(client) = crate::lsp::LspClient::start_for_extension(ext, current_dir) {
                    self.status_msg = Some(format!("LSP Iniciado ({})...", ext));
                    self.lsp_client = Some(client);
                }
            }
        }
    }

    /// Carga un archivo del disco al buffer, reinicia el estado dependiente
    /// del archivo anterior (LSP, diagnósticos, versión de documento) y
    /// refresca el contexto de git para el nuevo directorio de trabajo.
    pub fn load_file(&mut self, path: std::path::PathBuf) {
        if let Ok(buf) = crate::editor::EditorBuffer::load_from_file(&path) {
            self.buffer = buf;
            self.state = crate::app::AppState::Editing;
            self.show_tree = false;
            self.document_version = 1;
            self.is_dirty = false;
            self.current_filepath = Some(path.clone());
            self.setup_lsp_for_current_file();
            self.working_dir = path.parent().map(|p| p.to_path_buf()).unwrap_or_else(|| PathBuf::from("."));
            self.git_ctx = crate::git::GitContext::refresh(&self.working_dir, self.current_filepath.as_deref());

            if self.lsp_client.is_none() {
                self.status_msg = Some(format!("Cargado: {}", path.display()));
            }
        }
    }

    /// Guarda directamente si el archivo ya tiene una ruta asignada;
    /// en caso contrario abre el prompt "Guardar Como" para pedirla.
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

    /// Escribe el contenido del buffer en `current_filepath`. No hace nada
    /// si no hay ruta asignada (ese caso lo maneja `trigger_save`).
    pub fn save_file(&mut self) {
        if let Some(path) = &self.current_filepath {
            match self.buffer.save_to_file(path) {
                Ok(_) => {
                    self.is_dirty = false;
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

    /// Abre el prompt de renombrado para la entrada actualmente seleccionada
    /// en el explorador. Ignora la entrada ".." (subir de directorio).
    pub fn trigger_rename(&mut self) {
        if let Some(entry) = self.explorer.get_selected() {
            if entry.name == ".." { return; }
            self.prompt = Some(PromptState { 
                intent: PromptIntent::Rename(entry.path.clone()),
                input: entry.name.clone(),
            });
        }
    }

    /// Abre el prompt de confirmación de borrado para la entrada seleccionada
    /// en el explorador. Ignora la entrada ".." (subir de directorio).
    pub fn trigger_delete(&mut self) {
        if let Some(entry) = self.explorer.get_selected() {
            if entry.name == ".." { return; }
            self.prompt = Some(PromptState { 
                intent: PromptIntent::Delete(entry.path.clone()),
                input: String::new(),
            });
        }
    }

    /// Ejecuta la acción asociada al prompt activo según su `PromptIntent`,
    /// usando el texto que el usuario haya escrito en el cuadro de diálogo.
    /// Al terminar, refresca el contexto de git porque cualquiera de las
    /// tres acciones puede alterar el estado del repositorio.
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

    /// Dispara una petición de autocompletado al LSP en la posición actual
    /// del cursor. Guarda el ID de la petición para poder emparejar la
    /// respuesta asíncrona más adelante (ver `process_lsp_messages` en main.rs).
    pub fn trigger_completion(&mut self) {
        if let (Some(client), Some(uri)) = (&mut self.lsp_client, &self.current_uri) {
            let (line, col) = self.buffer.get_lsp_position();
            self.pending_completion_id = Some(client.request_completion(uri.clone(), line, col));
            self.completions.clear();
        }
    }
}
