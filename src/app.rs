use crate::completion;
use crate::config::{self, Config};
use crate::editor::EditorBuffer;
use crate::explorer::{fuzzy_filter_files, list_project_files, search_text, FileExplorer, WorkspaceTextMatch};
use crate::lsp::{self, LspClient, LspMessage};
use crate::terminal::Terminal;
use crate::theme::Theme;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};
use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc};
use lsp_types::{Diagnostic, CompletionItemKind, Uri, InlayHint};
use ratatui::widgets::ListState;
use ratatui::layout::{Direction, Rect};
use crossterm::event::{KeyCode, KeyModifiers};
use serde_json::Value;
use syntect::parsing::SyntaxSet;

pub type BufferId = usize;

#[derive(Clone)]
pub struct Window {
    pub buffer_id: BufferId,
}

#[derive(PartialEq, Clone, Copy, Debug)]
pub enum Focus {
    Tree,
    Window(usize),
    Terminal,
}

pub struct Document {
    pub buffer: EditorBuffer,
    pub filepath: Option<PathBuf>,
    pub uri: Option<Uri>,
    pub version: i32,
    pub is_dirty: bool,
}

impl Document {
    fn ext(&self) -> Option<String> {
        self.filepath.as_ref()?.extension()?.to_str().map(str::to_string)
    }

    fn full_range(&self) -> lsp_types::Range {
        lsp_types::Range {
            start: lsp_types::Position { line: 0, character: 0 },
            end: lsp_types::Position { line: self.buffer.text.len_lines() as u32, character: 0 },
        }
    }
}

#[derive(Debug, Clone, Hash, Eq, PartialEq)]
pub struct KeyCombo {
    pub code: KeyCode,
    pub modifiers: KeyModifiers,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    ToggleHelp, ToggleTree,
    FocusUp, FocusDown, FocusLeft, FocusRight,
    SplitVertical, SplitHorizontal, CloseWindow,
    NextSearchResult, Copy, Cut, Paste, Undo, Redo, Quit, Save, ToggleTerminal, CycleTheme,
    ScrollViewportUp, ScrollViewportDown,
    SearchText, SearchFile, Cancel, Confirm, Backspace, Indent, Delete,
    MoveStartOfLine(bool), MoveEndOfLine(bool),
    PageUp(bool), PageDown(bool),
    MoveUp(bool), MoveDown(bool), MoveLeft(bool), MoveRight(bool),
}

#[derive(Clone)]
pub enum SplitNode {
    Leaf(usize),
    Split(Direction, Box<SplitNode>, Box<SplitNode>),
}

impl SplitNode {
    pub fn replace_leaf(&mut self, target_id: usize, new_node: SplitNode) -> bool {
        match self {
            SplitNode::Leaf(id) => {
                if *id == target_id {
                    *self = new_node;
                    return true;
                }
                false
            }
            SplitNode::Split(_, a, b) => {
                a.replace_leaf(target_id, new_node.clone()) || b.replace_leaf(target_id, new_node)
            }
        }
    }

    pub fn remove_leaf(self, target_id: usize) -> Option<SplitNode> {
        match self {
            SplitNode::Leaf(id) => if id == target_id { None } else { Some(self) },
            SplitNode::Split(dir, a, b) => {
                match (a.remove_leaf(target_id), b.remove_leaf(target_id)) {
                    (Some(a), Some(b)) => Some(SplitNode::Split(dir, Box::new(a), Box::new(b))),
                    (Some(n), None) | (None, Some(n)) => Some(n),
                    (None, None) => None,
                }
            }
        }
    }

    pub fn first_leaf(&self) -> usize {
        match self {
            SplitNode::Leaf(id) => *id,
            SplitNode::Split(_, a, _) => a.first_leaf(),
        }
    }
}

#[derive(Clone, Default)]
pub struct CompletionOption {
    pub label: String,
    pub kind: Option<CompletionItemKind>,
    pub detail: Option<String>,
    /// Texto pegado a la etiqueta, p. ej. la firma "(a: i32)".
    pub label_detail: Option<String>,
    pub insert_text: String,
    pub filter_text: String,
    pub sort_text: String,
    /// Chars de `label` que coinciden con lo escrito.
    pub matched: Vec<usize>,
    /// Identifica la sugerencia entre refiltrados para aplicarle su `resolve`.
    pub id: usize,
    /// Markdown o texto plano.
    pub documentation: Option<String>,
    /// Servidor e ítem original pendientes de `completionItem/resolve`.
    pub resolve: Option<(usize, Value)>,
    /// Rango que el servidor indica reemplazar; `None` si hay que deducirlo.
    pub edit_range: Option<lsp_types::Range>,
    pub additional_edits: Vec<lsp_types::TextEdit>,
    /// Primer tabstop del snippet, en chars relativos a `insert_text`.
    pub tabstop: Option<Range<usize>>,
}

#[derive(PartialEq)]
pub enum AppState {
    Intro,
    Workspace,
}

#[derive(Clone)]
pub enum PromptIntent {
    SaveAs(PathBuf), Rename(PathBuf), Delete(PathBuf),
    SearchText, SearchFile, ConfirmQuit,
}

pub struct PromptState {
    pub intent: PromptIntent,
    pub input: String,
    pub selection_state: ListState,
    pub file_results: Vec<PathBuf>,
    pub text_results: Vec<WorkspaceTextMatch>,
    /// Archivos del proyecto, listados una vez al abrir el prompt.
    pub files: Arc<Vec<PathBuf>>,
}

impl PromptState {
    pub fn new(intent: PromptIntent) -> Self {
        Self {
            intent,
            input: String::new(),
            selection_state: ListState::default(),
            file_results: Vec::new(),
            text_results: Vec::new(),
            files: Arc::default(),
        }
    }
}

/// Directorio del proyecto que contiene `start`: el repo git más cercano o,
/// si no hay, el primer directorio con un manifiesto conocido.
pub fn find_project_root(start: &Path) -> PathBuf {
    const MARKERS: [&str; 5] = ["Cargo.toml", "package.json", "pyproject.toml", "go.mod", "compile_commands.json"];
    start.ancestors().find(|d| d.join(".git").exists())
        .or_else(|| start.ancestors().find(|d| MARKERS.iter().any(|m| d.join(m).exists())))
        .unwrap_or(start)
        .to_path_buf()
}

pub struct App {
    pub state: AppState,
    pub focus: Focus,
    pub show_tree: bool,
    pub explorer: FileExplorer,
    pub documents: HashMap<BufferId, Document>,
    pub windows: HashMap<usize, Window>,
    pub layout: SplitNode,
    pub active_window: usize,
    pub next_buffer_id: BufferId,
    pub next_window_id: usize,
    pub window_areas: HashMap<usize, Rect>,
    pub quit: bool,
    pub status_msg: Option<String>,
    pub lsp_clients: HashMap<usize, LspClient>,
    pub lsp_tx: mpsc::Sender<(usize, LspMessage)>,
    pub lsp_rx: mpsc::Receiver<(usize, LspMessage)>,
    pub next_lsp_id: usize,
    pub needs_redraw: bool,
    pub show_help: bool,
    /// El portapapeles de arboard debe seguir vivo: en Linux el contenido se pierde al soltarlo.
    pub clipboard: Option<arboard::Clipboard>,
    pub clipboard_fallback: Option<String>,
    pub last_click: Option<(Instant, u16, u16)>,
    /// Diagnósticos por URI y por servidor: cada publicación reemplaza el conjunto anterior.
    pub diagnostics: HashMap<String, HashMap<usize, Vec<Diagnostic>>>,
    /// Vista filtrada y ordenada de `completion_items` que muestra el popup.
    pub completions: Vec<CompletionOption>,
    pub completion_state: ListState,
    pub pending_completion_ids: Vec<(usize, u64)>,
    /// Sugerencias tal como llegaron de los servidores; se refiltran en local al teclear.
    pub completion_items: Vec<CompletionOption>,
    /// Cursor mínimo con el que la sesión sigue abierta: borrar más atrás cierra el popup.
    pub completion_anchor: usize,
    /// Hay petición nueva en curso: su primera respuesta reemplaza a `completion_items`.
    pub completion_stale: bool,
    /// Veces que se eligió cada etiqueta, para priorizarla como hace JetBrains.
    pub completion_usage: HashMap<String, u32>,
    pub next_completion_id: usize,
    /// (servidor, petición, id de sugerencia) del `resolve` en curso.
    pub pending_resolve: Option<(usize, u64, usize)>,
    /// Raíz del proyecto: ámbito de búsquedas, git y servidores LSP.
    pub working_dir: PathBuf,
    pub prompt: Option<PromptState>,
    pub git_ctx: crate::git::GitContext,
    pub keybindings: HashMap<KeyCombo, Action>,
    pub last_search_query: Option<String>,
    pub search_buffer: Option<BufferId>,
    pub text_search_results: Vec<usize>,
    pub current_search_idx: usize,
    pub search_gen: Arc<AtomicU64>,
    pub search_tx: mpsc::Sender<(u64, Vec<WorkspaceTextMatch>)>,
    pub search_rx: mpsc::Receiver<(u64, Vec<WorkspaceTextMatch>)>,
    pub search_pending: bool,
    pub syntax_set: SyntaxSet,
    pub theme: Theme,
    pub config: Config,
    /// `None` en tests: entonces no se vigila nada en disco.
    pub config_dir: Option<PathBuf>,
    /// Tema activo; puede diferir de `config.theme` tras ciclar con F5.
    pub theme_name: String,
    /// Últimas fechas de modificación vistas de config.json y del tema activo.
    pub config_mtimes: (Option<SystemTime>, Option<SystemTime>),
    pub last_config_check: Instant,
    pub inlay_hints: HashMap<String, HashMap<usize, Vec<InlayHint>>>,
    pub pending_inlay_hints_ids: Vec<(usize, u64, String)>,
    pub terminal: Option<Terminal>,
    pub show_terminal: bool,
    pub terminal_area: Option<Rect>,
    pub term_tx: mpsc::Sender<()>,
    pub term_rx: mpsc::Receiver<()>,
    // Estado para interpolación fluida de UI y cursores (Vuelo visual)
    pub visual_cursor_x: f64,
    pub visual_cursor_y: f64,
    pub target_cursor_x: f64,
    pub target_cursor_y: f64,
    pub prompt_spawn_progress: f64,
    pub completions_spawn_progress: f64,
    pub help_spawn_progress: f64,
    pub tree_spawn_progress: f64,
}

impl App {
    pub fn new() -> Self {
        let current_dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let working_dir = find_project_root(&current_dir);
        let git_ctx = crate::git::GitContext::refresh(&working_dir, None);
        let (lsp_tx, lsp_rx) = mpsc::channel();
        let (search_tx, search_rx) = mpsc::channel();
        let (term_tx, term_rx) = mpsc::channel();
        // Los tests no deben leer ni crear archivos en la configuración del usuario.
        let config_dir = config::config_dir().filter(|_| !cfg!(test));
        let (config, theme, errors) = match &config_dir {
            Some(dir) => config::load(dir),
            None => (Config::default(), Theme::default(), Vec::new()),
        };
        let config_mtimes = config_dir.as_deref().map_or((None, None), |d| (mtime(&d.join("config.json")), mtime(&config::theme_path(d, &config.theme))));

        Self {
            state: AppState::Intro,
            working_dir,
            show_tree: false,
            explorer: FileExplorer::new(current_dir),
            focus: Focus::Tree,
            documents: HashMap::new(),
            windows: HashMap::new(),
            layout: SplitNode::Leaf(0),
            active_window: 0,
            next_buffer_id: 1,
            next_window_id: 1,
            window_areas: HashMap::new(),
            quit: false,
            status_msg: errors.first().map(|e| format!("Config inválida, usando valores por defecto: {e}")),
            lsp_clients: HashMap::new(),
            lsp_tx,
            lsp_rx,
            next_lsp_id: 1,
            needs_redraw: true,
            show_help: false,
            clipboard: arboard::Clipboard::new().ok(),
            clipboard_fallback: None,
            last_click: None,
            diagnostics: HashMap::new(),
            completions: Vec::new(),
            completion_state: ListState::default(),
            pending_completion_ids: Vec::new(),
            completion_items: Vec::new(),
            completion_anchor: 0,
            completion_stale: false,
            completion_usage: HashMap::new(),
            next_completion_id: 0,
            pending_resolve: None,
            prompt: None,
            git_ctx,
            keybindings: Self::default_keybindings(),
            last_search_query: None,
            search_buffer: None,
            text_search_results: Vec::new(),
            current_search_idx: 0,
            search_gen: Arc::new(AtomicU64::new(0)),
            search_tx,
            search_rx,
            search_pending: false,
            syntax_set: SyntaxSet::load_defaults_newlines(),
            theme: theme.finish(crate::theme::terminal_has_truecolor()),
            theme_name: config.theme.clone(),
            config,
            config_dir,
            config_mtimes,
            last_config_check: Instant::now(),
            inlay_hints: HashMap::new(),
            pending_inlay_hints_ids: Vec::new(),
            terminal: None,
            show_terminal: false,
            terminal_area: None,
            term_tx,
            term_rx,
            visual_cursor_x: -1.0,
            visual_cursor_y: -1.0,
            target_cursor_x: -1.0,
            target_cursor_y: -1.0,
            prompt_spawn_progress: 0.0,
            completions_spawn_progress: 0.0,
            help_spawn_progress: 0.0,
            tree_spawn_progress: 0.0,
        }
    }

    /// Itera sobre todos los subsistemas visuales aplicando un decaimiento
    /// exponencial basado en dt para converger armónicamente al destino.
    pub fn update_animations(&mut self, dt: f64) -> bool {
        let mut changed = false;

        let dx = self.target_cursor_x - self.visual_cursor_x;
        let dy = self.target_cursor_y - self.visual_cursor_y;

        if dx.abs() > 0.05 || dy.abs() > 0.05 {
            // Factor derivado para simular arrastre/inercia de puntero (spring system)
            let factor = 1.0 - (-45.0 * dt).exp();
            self.visual_cursor_x += dx * factor;
            self.visual_cursor_y += dy * factor;
            changed = true;
        } else {
            self.visual_cursor_x = self.target_cursor_x;
            self.visual_cursor_y = self.target_cursor_y;
        }

        let targets = [
            (self.prompt.is_some(), &mut self.prompt_spawn_progress, 35.0),
            (!self.completions.is_empty(), &mut self.completions_spawn_progress, 40.0),
            (self.show_help, &mut self.help_spawn_progress, 35.0),
            (self.show_tree, &mut self.tree_spawn_progress, 35.0),
        ];
        for (visible, progress, speed) in targets {
            let target = if visible { 1.0 } else { 0.0 };
            let d = target - *progress;
            if d.abs() > 0.01 {
                *progress += d * (1.0 - (-speed * dt).exp());
                changed = true;
            } else {
                *progress = target;
            }
        }

        changed
    }

    pub fn is_animating(&self) -> bool {
        let pending = |visible: bool, progress: f64| ((if visible { 1.0 } else { 0.0 }) - progress).abs() > 0.01;
        (self.target_cursor_x - self.visual_cursor_x).abs() > 0.05
            || (self.target_cursor_y - self.visual_cursor_y).abs() > 0.05
            || pending(self.prompt.is_some(), self.prompt_spawn_progress)
            || pending(!self.completions.is_empty(), self.completions_spawn_progress)
            || pending(self.show_help, self.help_spawn_progress)
            || pending(self.show_tree, self.tree_spawn_progress)
    }

    pub fn default_keybindings() -> HashMap<KeyCombo, Action> {
        let mut kb = HashMap::new();
        let none = KeyModifiers::empty();
        let ctrl = KeyModifiers::CONTROL;
        let alt = KeyModifiers::ALT;
        let binds = [
            (KeyCode::F(1), none, Action::ToggleHelp),
            (KeyCode::F(2), none, Action::ToggleTree),
            (KeyCode::F(3), none, Action::NextSearchResult),
            (KeyCode::F(4), none, Action::ToggleTerminal),
            (KeyCode::F(5), none, Action::CycleTheme),
            (KeyCode::Char('c'), ctrl, Action::Copy),
            (KeyCode::Char('x'), ctrl, Action::Cut),
            (KeyCode::Char('v'), ctrl, Action::Paste),
            (KeyCode::Char('z'), ctrl, Action::Undo),
            (KeyCode::Char('y'), ctrl, Action::Redo),
            (KeyCode::Char('q'), ctrl, Action::Quit),
            (KeyCode::Char('s'), ctrl, Action::Save),
            (KeyCode::Char('t'), ctrl, Action::SearchText),
            (KeyCode::Char('f'), ctrl, Action::SearchFile),
            (KeyCode::Char('u'), ctrl, Action::ScrollViewportUp),
            (KeyCode::Char('d'), ctrl, Action::ScrollViewportDown),
            (KeyCode::Esc, none, Action::Cancel),
            (KeyCode::Enter, none, Action::Confirm),
            (KeyCode::Backspace, none, Action::Backspace),
            (KeyCode::Tab, none, Action::Indent),
            (KeyCode::Delete, none, Action::Delete),
            (KeyCode::Char('i'), alt, Action::FocusUp),
            (KeyCode::Char('k'), alt, Action::FocusDown),
            (KeyCode::Char('j'), alt, Action::FocusLeft),
            (KeyCode::Char('l'), alt, Action::FocusRight),
            (KeyCode::Char('v'), alt, Action::SplitVertical),
            (KeyCode::Char('h'), alt, Action::SplitHorizontal),
            (KeyCode::Char('w'), alt, Action::CloseWindow),
        ];
        for (code, modifiers, action) in binds {
            kb.insert(KeyCombo { code, modifiers }, action);
        }

        let directions = [
            (KeyCode::Home, Action::MoveStartOfLine(false), Action::MoveStartOfLine(true)),
            (KeyCode::End, Action::MoveEndOfLine(false), Action::MoveEndOfLine(true)),
            (KeyCode::PageUp, Action::PageUp(false), Action::PageUp(true)),
            (KeyCode::PageDown, Action::PageDown(false), Action::PageDown(true)),
            (KeyCode::Up, Action::MoveUp(false), Action::MoveUp(true)),
            (KeyCode::Down, Action::MoveDown(false), Action::MoveDown(true)),
            (KeyCode::Left, Action::MoveLeft(false), Action::MoveLeft(true)),
            (KeyCode::Right, Action::MoveRight(false), Action::MoveRight(true)),
        ];
        for (code, norm, sel) in directions {
            kb.insert(KeyCombo { code, modifiers: none }, norm);
            kb.insert(KeyCombo { code, modifiers: KeyModifiers::SHIFT }, sel);
        }

        kb
    }

    pub fn active_buffer_id(&self) -> Option<BufferId> {
        match self.focus {
            Focus::Window(idx) => self.windows.get(&idx).map(|w| w.buffer_id),
            Focus::Tree | Focus::Terminal => None,
        }
    }

    pub fn active_document_mut(&mut self) -> Option<&mut Document> {
        let id = self.active_buffer_id()?;
        self.documents.get_mut(&id)
    }

    pub fn get_active_document(&self) -> Option<&Document> {
        self.documents.get(&self.active_buffer_id()?)
    }

    /// Altura útil de texto de la ventana activa (sin la pestaña).
    pub fn active_view_height(&self) -> Option<usize> {
        self.window_areas.get(&self.active_window).map(|r| r.height.saturating_sub(1).max(1) as usize)
    }

    pub fn toggle_tree(&mut self) {
        self.show_tree = !self.show_tree;
        if self.show_tree {
            self.focus = Focus::Tree;
            self.explorer.reload();
        } else if self.windows.is_empty() {
            self.state = AppState::Intro;
        } else {
            self.focus = Focus::Window(self.active_window)
        }
    }

    fn editor_focus(&self) -> Focus {
        if self.windows.is_empty() { Focus::Tree } else { Focus::Window(self.active_window) }
    }
    
    pub fn toggle_terminal(&mut self) {
        if self.show_terminal && self.focus == Focus::Terminal {
            self.show_terminal = false;
            self.focus = self.editor_focus();
            return;
        }
        if self.terminal.is_none() {
            match Terminal::spawn(&self.working_dir, 10, 80, self.term_tx.clone()) {
                Ok(t) => self.terminal = Some(t),
                Err(e) => {
                    self.status_msg = Some(format!("No se pudo abrir la terminal: {}", e));
                    return;
                }
            }
        }

        self.show_terminal = true;
        self.focus = Focus::Terminal;
    }

    pub fn poll_terminal(&mut self) -> bool {
        let mut changed = self.term_rx.try_iter().count() > 0;
        if self.terminal.as_mut().is_some_and(|t| t.has_exited()) {
            self.terminal = None;
            self.show_terminal = false;
            if self.focus == Focus::Terminal {
                self.focus = self.editor_focus();
            }
            changed = true;
        }
        changed
    }
    
    pub fn move_focus_dir(&mut self, dx: i16, dy: i16) {
        if let Focus::Window(active_id) = self.focus {
            if let Some(active_rect) = self.window_areas.get(&active_id) {
                let cx = active_rect.x as i32 + (active_rect.width as i32 / 2);
                let cy = active_rect.y as i32 + (active_rect.height as i32 / 2);

                let mut best_id = active_id;
                let mut best_dist = i32::MAX;

                for (id, rect) in &self.window_areas {
                    if *id == active_id { continue; }
                    let tx = rect.x as i32 + (rect.width as i32 / 2);
                    let ty = rect.y as i32 + (rect.height as i32 / 2);

                    let valid = match (dx, dy) {
                        (1, 0) => tx > cx,
                        (-1, 0) => tx < cx,
                        (0, 1) => ty > cy,
                        (0, -1) => ty < cy,
                        _ => false
                    };

                    if valid {
                        let dist = (tx - cx).pow(2) + (ty - cy).pow(2);
                        if dist < best_dist {
                            best_dist = dist;
                            best_id = *id;
                        }
                    }
                }

                if best_id != active_id {
                    self.focus = Focus::Window(best_id);
                    self.active_window = best_id;
                } else if dx == -1 && self.show_tree {
                    self.focus = Focus::Tree;
                }
            }
        } else if self.focus == Focus::Tree && dx == 1 && !self.windows.is_empty() {
            self.focus = Focus::Window(self.active_window);
        }
    }

    pub fn split_window(&mut self, direction: Direction) {
        let Focus::Window(active_id) = self.focus else { return };
        let Some(buffer_id) = self.windows.get(&active_id).map(|w| w.buffer_id) else { return };

        let new_win_id = self.next_window_id;
        self.next_window_id += 1;
        self.windows.insert(new_win_id, Window { buffer_id });
        self.layout.replace_leaf(active_id, SplitNode::Split(
            direction,
            Box::new(SplitNode::Leaf(active_id)),
            Box::new(SplitNode::Leaf(new_win_id)),
        ));

        self.focus = Focus::Window(new_win_id);
        self.active_window = new_win_id;
        self.needs_redraw = true;
    }

    pub fn close_active_window(&mut self) {
        let Focus::Window(idx) = self.focus else { return };
        self.windows.remove(&idx);

        if let Some(new_layout) = self.layout.clone().remove_leaf(idx) {
            let next = new_layout.first_leaf();
            self.layout = new_layout;
            self.focus = Focus::Window(next);
            self.active_window = next;
        } else {
            self.layout = SplitNode::Leaf(0);
            self.focus = Focus::Tree;
            self.state = AppState::Intro;
        }
        self.needs_redraw = true;
    }

    fn insert_document(&mut self, buffer: EditorBuffer, filepath: Option<PathBuf>) -> BufferId {
        let buf_id = self.next_buffer_id;
        self.next_buffer_id += 1;
        self.documents.insert(buf_id, Document { buffer, filepath, uri: None, version: 1, is_dirty: false });
        buf_id
    }

    pub fn new_blank_file(&mut self) {
        let buf_id = self.insert_document(EditorBuffer::new(), None);

        let win_id = self.next_window_id;
        self.next_window_id += 1;
        self.windows.insert(win_id, Window { buffer_id: buf_id });

        if self.windows.len() == 1 {
            self.layout = SplitNode::Leaf(win_id);
            self.state = AppState::Workspace;
        } else {
            self.layout.replace_leaf(self.active_window, SplitNode::Split(
                Direction::Horizontal,
                Box::new(SplitNode::Leaf(self.active_window)),
                Box::new(SplitNode::Leaf(win_id)),
            ));
        }

        self.focus = Focus::Window(win_id);
        self.active_window = win_id;
        self.show_tree = false;
        self.status_msg = Some("Archivo en memoria. CTRL + S para guardar".into());
    }

    /// Muestra un buffer en la ventana activa, creando la primera ventana si no hay.
    fn show_buffer(&mut self, buf_id: BufferId) {
        if self.windows.is_empty() {
            let win_id = self.next_window_id;
            self.next_window_id += 1;
            self.windows.insert(win_id, Window { buffer_id: buf_id });
            self.layout = SplitNode::Leaf(win_id);
            self.active_window = win_id;
            self.state = AppState::Workspace;
        } else if let Some(win) = self.windows.get_mut(&self.active_window) {
            win.buffer_id = buf_id;
        }
        self.focus = Focus::Window(self.active_window);
    }

    /// Recarga config.json y el tema activo si cambiaron en disco. Los servidores
    /// LSP ya arrancados siguen igual; la nueva lista vale para los próximos archivos.
    pub fn reload_config_if_changed(&mut self) -> bool {
        if self.last_config_check.elapsed() < Duration::from_millis(500) { return false; }
        self.last_config_check = Instant::now();
        let Some(dir) = self.config_dir.clone() else { return false };
        let now = (mtime(&dir.join("config.json")), mtime(&config::theme_path(&dir, &self.theme_name)));
        if now == self.config_mtimes { return false; }
        let config_changed = now.0 != self.config_mtimes.0;
        self.config_mtimes = now;

        if config_changed {
            match config::load_config(&dir) {
                Ok(cfg) => {
                    if cfg.theme != self.config.theme { self.theme_name = cfg.theme.clone(); }
                    self.config = cfg;
                    self.status_msg = Some("Configuración recargada".into());
                }
                // Un guardado a medias no debe tirar la configuración que funciona.
                Err(e) => { self.status_msg = Some(e); return true; }
            }
        }
        let name = self.theme_name.clone();
        self.set_theme(&name);
        true
    }

    /// Aplica `themes/<name>.json`; si es inválido se conserva el tema actual.
    pub fn set_theme(&mut self, name: &str) {
        let Some(dir) = self.config_dir.clone() else { return };
        self.config_mtimes.1 = mtime(&config::theme_path(&dir, name));
        match config::load_theme(&dir, name) {
            Ok(theme) => {
                self.theme = theme.finish(crate::theme::terminal_has_truecolor());
                self.theme_name = name.to_string();
                // El estado de resaltado cachea estilos del tema anterior.
                for doc in self.documents.values_mut() { doc.buffer.syntax_cache.clear(); }
                self.status_msg = Some(format!("Tema: {name}"));
            }
            Err(e) => self.status_msg = Some(e),
        }
    }

    /// Pasa al siguiente tema de `themes/` sin tocar config.json.
    pub fn cycle_theme(&mut self) {
        let Some(dir) = self.config_dir.clone() else {
            self.status_msg = Some("Sin directorio de configuración".into());
            return;
        };
        let names = config::list_themes(&dir);
        let next = names.iter().position(|n| *n == self.theme_name).map_or(0, |i| (i + 1) % names.len());
        if let Some(name) = names.get(next) { self.set_theme(&name.clone()); }
    }

    pub fn refresh_git(&mut self) {
        let path = self.get_active_document().and_then(|d| d.filepath.clone());
        self.git_ctx = crate::git::GitContext::refresh(&self.working_dir, path.as_deref());
    }

    /// Asigna URI al documento, arranca los servidores que falten y lo abre en los ya listos.
    pub fn setup_lsp_for_buffer(&mut self, buf_id: BufferId) {
        let Some(doc) = self.documents.get_mut(&buf_id) else { return };
        let Some(uri) = doc.filepath.as_deref().and_then(lsp::path_to_uri) else { return };
        doc.uri = Some(uri);
        let Some(ext) = doc.ext() else { return };

        for cfg in self.config.servers_for_ext(&ext) {
            if self.lsp_clients.values().any(|c| c.name == cfg.command) || !lsp::is_in_path(&cfg.command) { continue; }
            if let Some(client) = LspClient::start(cfg, &self.working_dir, self.next_lsp_id, self.lsp_tx.clone()) {
                self.lsp_clients.insert(self.next_lsp_id, client);
                self.next_lsp_id += 1;
            }
        }

        let doc = &self.documents[&buf_id];
        for (&server_id, client) in self.lsp_clients.iter_mut() {
            if client.is_initialized && client.handles(&ext) {
                open_in_server(client, server_id, doc, self.config.language_id(&ext), &mut self.pending_inlay_hints_ids);
            }
        }
    }

    /// Cierra el documento en sus servidores (p. ej. antes de cambiarle la ruta).
    fn close_in_lsp(&mut self, buf_id: BufferId) {
        let Some(doc) = self.documents.get_mut(&buf_id) else { return };
        let (Some(uri), Some(ext)) = (doc.uri.take(), doc.ext()) else { return };
        for client in self.lsp_clients.values_mut() {
            if client.is_initialized && client.handles(&ext) {
                client.did_close(uri.clone());
            }
        }
        self.diagnostics.remove(uri.as_str());
        self.inlay_hints.remove(uri.as_str());
    }

    pub fn load_file(&mut self, path: PathBuf) {
        let path = std::fs::canonicalize(&path).unwrap_or(path);
        let existing = self.documents.iter().find(|(_, d)| d.filepath.as_ref() == Some(&path)).map(|(&id, _)| id);

        let buf_id = match existing {
            Some(id) => id,
            None => match EditorBuffer::load_from_file(&path) {
                Ok(buffer) => self.insert_document(buffer, Some(path.clone())),
                Err(e) => {
                    // Abrir un buffer vacío aquí haría que Ctrl+S borrase el archivo.
                    self.status_msg = Some(format!("No se pudo abrir {}: {}", path.display(), e));
                    return;
                }
            },
        };

        self.show_buffer(buf_id);
        if existing.is_none() {
            self.setup_lsp_for_buffer(buf_id);
        }
        self.refresh_git();
    }

    pub fn trigger_save(&mut self) {
        if let Some(doc) = self.get_active_document() {
            if doc.filepath.is_some() {
                self.save_file();
            } else {
                self.open_prompt(PromptIntent::SaveAs(self.working_dir.clone()));
            }
        }
    }

    pub fn save_file(&mut self) -> bool {
        let Some(buf_id) = self.active_buffer_id() else { return false };
        let Some(doc) = self.documents.get_mut(&buf_id) else { return false };
        let Some(path) = doc.filepath.clone() else {
            self.status_msg = Some("No hay archivo abierto para guardar".to_string());
            return false;
        };

        if let Err(e) = doc.buffer.save_to_file(&path) {
            self.status_msg = Some(format!("Error al guardar: {}", e));
            return false;
        }
        doc.is_dirty = false;
        if let (Some(uri), Some(ext)) = (doc.uri.clone(), doc.ext()) {
            for client in self.lsp_clients.values_mut() {
                if client.is_initialized && client.handles(&ext) {
                    client.did_save(uri.clone());
                }
            }
        }
        self.status_msg = Some("Guardado Exitosamente".to_string());
        self.explorer.reload();
        self.refresh_git();
        true
    }

    pub fn trigger_rename(&mut self) {
        if let Some(entry) = self.explorer.get_selected() {
            if entry.name == ".." { return; }
            self.open_prompt(PromptIntent::Rename(entry.path.clone()));
        }
    }

    pub fn trigger_delete(&mut self) {
        if let Some(entry) = self.explorer.get_selected() {
            if entry.name == ".." { return; }
            self.open_prompt(PromptIntent::Delete(entry.path.clone()));
        }
    }

    /// Buffers cuya ruta es `path` o está dentro de él (si es un directorio).
    fn buffers_under(&self, path: &Path) -> Vec<BufferId> {
        self.documents.iter()
            .filter(|(_, d)| d.filepath.as_ref().is_some_and(|p| p.starts_with(path)))
            .map(|(&id, _)| id)
            .collect()
    }

    pub fn close_prompt(&mut self) {
        self.prompt = None;
        // Invalida cualquier búsqueda de texto en curso.
        self.search_gen.fetch_add(1, Ordering::SeqCst);
        self.search_pending = false;
    }

    pub fn execute_prompt(&mut self) {
        let Some(prompt) = self.prompt.take() else { return };
        self.close_prompt();
        let name = prompt.input.trim().to_string();

        match prompt.intent {
            PromptIntent::SaveAs(dir) => {
                if let Err(msg) = validate_file_name(&name) {
                    self.status_msg = Some(format!("No se puede guardar: {}", msg));
                    return;
                }
                let new_path = dir.join(&name);
                if new_path.exists() {
                    self.status_msg = Some(format!("Ya existe {}; elige otro nombre", new_path.display()));
                    return;
                }
                let Some(buf_id) = self.active_buffer_id() else { return };
                if let Some(doc) = self.documents.get_mut(&buf_id) {
                    doc.filepath = Some(new_path);
                    doc.buffer.syntax_cache.clear();
                }
                if self.save_file() {
                    if let Some(doc) = self.documents.get_mut(&buf_id) {
                        doc.filepath = doc.filepath.take().map(|p| std::fs::canonicalize(&p).unwrap_or(p));
                    }
                    self.setup_lsp_for_buffer(buf_id);
                } else if let Some(doc) = self.documents.get_mut(&buf_id) {
                    doc.filepath = None;
                }
            }
            PromptIntent::Rename(old_path) => {
                if let Err(msg) = validate_file_name(&name) {
                    self.status_msg = Some(format!("No se puede renombrar: {}", msg));
                    return;
                }
                let new_path = old_path.with_file_name(&name);
                if new_path.exists() {
                    self.status_msg = Some(format!("Ya existe {}; no se sobrescribe", new_path.display()));
                    return;
                }
                if let Err(e) = std::fs::rename(&old_path, &new_path) {
                    self.status_msg = Some(format!("Error al renombrar: {}", e));
                    return;
                }
                for buf_id in self.buffers_under(&old_path) {
                    self.close_in_lsp(buf_id);
                    if let Some(doc) = self.documents.get_mut(&buf_id) {
                        let old = doc.filepath.take().unwrap_or_default();
                        let rel = old.strip_prefix(&old_path).unwrap_or(Path::new(""));
                        doc.filepath = Some(if rel.as_os_str().is_empty() { new_path.clone() } else { new_path.join(rel) });
                        doc.buffer.syntax_cache.clear();
                    }
                    self.setup_lsp_for_buffer(buf_id);
                }
                self.status_msg = Some("Renombrado exitosamente".into());
                self.explorer.reload();
            }
            PromptIntent::Delete(path) => {
                if !name.eq_ignore_ascii_case("y") { return; }
                let res = if path.is_dir() { std::fs::remove_dir_all(&path) } else { std::fs::remove_file(&path) };
                if let Err(e) = res {
                    self.status_msg = Some(format!("Error al eliminar: {}", e));
                    return;
                }
                // El contenido sigue en memoria: se marca sucio para que salir pida confirmación.
                let orphaned = self.buffers_under(&path);
                for buf_id in &orphaned {
                    if let Some(doc) = self.documents.get_mut(buf_id) { doc.is_dirty = true; }
                }
                self.status_msg = Some(if orphaned.is_empty() {
                    "Eliminado exitosamente".into()
                } else {
                    "Eliminado; sus buffers abiertos quedan sin guardar".into()
                });
                self.explorer.reload();
            }
            PromptIntent::SearchText => {
                let Some(m) = prompt.selection_state.selected().and_then(|i| prompt.text_results.get(i)).cloned() else { return };
                let query = prompt.input.clone();
                self.load_file(m.path.clone());

                let Some(buf_id) = self.active_buffer_id() else { return };
                let Some(doc) = self.documents.get_mut(&buf_id) else { return };
                if doc.filepath.as_deref() != Some(std::fs::canonicalize(&m.path).unwrap_or(m.path).as_path()) { return; }

                let max_len = doc.buffer.text.len_chars();
                let line = m.line_idx.min(doc.buffer.text.len_lines().saturating_sub(1));
                let start = (doc.buffer.text.line_to_char(line) + m.char_offset).min(max_len);
                doc.buffer.cursor_char_idx = start;
                doc.buffer.selection_anchor = Some((start + query.chars().count()).min(max_len));

                self.text_search_results = doc.buffer.find_text(&query);
                self.current_search_idx = self.text_search_results.iter().position(|&p| p == start).unwrap_or(0);
                self.search_buffer = Some(buf_id);
                self.last_search_query = Some(query);
                self.show_tree = false;
            }
            PromptIntent::SearchFile => {
                if let Some(path) = prompt.selection_state.selected().and_then(|i| prompt.file_results.get(i)).cloned() {
                    self.load_file(path);
                }
            }
            PromptIntent::ConfirmQuit => {
                if name.eq_ignore_ascii_case("y") {
                    self.quit = true;
                }
            }
        }
    }

    /// Pide sugerencias a los servidores del documento. Mientras llegan, si el
    /// popup ya estaba abierto, se refiltra lo que hay para no parpadear.
    pub fn trigger_completion(&mut self, trigger: Option<char>) {
        // El servidor debe conocer el carácter recién escrito antes de calcular sugerencias.
        self.after_edit();
        self.pending_completion_ids.clear();

        let Some(doc) = self.get_active_document() else { return };
        let (Some(uri), Some(ext)) = (doc.uri.clone(), doc.ext()) else { return };
        let (line, col) = doc.buffer.get_lsp_position();
        let cursor = doc.buffer.cursor_char_idx;
        let word_start = cursor - doc.buffer.get_current_word_prefix().chars().count();

        if trigger.is_some() || self.completions.is_empty() {
            self.completion_items.clear();
            self.completions.clear();
            // Tras un `.` vale la lista sin filtrar; tras una palabra, borrarla entera la cierra.
            self.completion_anchor = if trigger.is_some() { cursor } else { word_start + 1 };
        } else {
            self.refilter_completions();
        }
        self.completion_stale = true;

        for (&server_id, client) in self.lsp_clients.iter_mut() {
            if client.is_initialized && client.handles(&ext) {
                let id = client.request_completion(uri.clone(), line, col, trigger);
                self.pending_completion_ids.push((server_id, id));
            }
        }
    }

    /// Si `c` es carácter de disparo de algún servidor del documento activo (`.`, `::`, `<`...).
    pub fn is_completion_trigger(&self, c: char) -> bool {
        let Some(ext) = self.get_active_document().and_then(|d| d.ext()) else { return false };
        self.lsp_clients.values().any(|cl| cl.is_initialized && cl.handles(&ext) && cl.trigger_chars.contains(&c))
    }

    /// Pide la documentación de la sugerencia seleccionada si el servidor la da bajo demanda.
    pub fn resolve_selected_completion(&mut self) {
        let Some(sel) = self.completion_state.selected().and_then(|i| self.completions.get(i)) else { return };
        let Some((server_id, raw)) = sel.resolve.clone() else { return };
        if self.pending_resolve.is_some_and(|(_, _, id)| id == sel.id) { return; }
        let id = sel.id;
        if let Some(client) = self.lsp_clients.get_mut(&server_id) {
            let req = client.send_request("completionItem/resolve", raw);
            self.pending_resolve = Some((server_id, req, id));
        }
    }

    fn handle_resolve_response(&mut self, item_id: usize, result: Value) {
        let resolved = serde_json::from_value::<lsp_types::CompletionItem>(result).ok();
        for c in self.completion_items.iter_mut().chain(self.completions.iter_mut()).filter(|c| c.id == item_id) {
            // Se marca resuelto aunque falle: si no, se repetiría la petición en cada vuelta del bucle.
            c.resolve = None;
            let Some(r) = &resolved else { continue };
            if let Some(doc) = r.documentation.as_ref().map(doc_text) { c.documentation = Some(doc); }
            if c.detail.is_none() { c.detail = r.detail.clone(); }
            if c.additional_edits.is_empty() && let Some(e) = &r.additional_text_edits { c.additional_edits = e.clone(); }
        }
    }

    pub fn clear_completions(&mut self) {
        self.completions.clear();
        self.completion_items.clear();
        self.pending_completion_ids.clear();
    }

    /// Recalcula el popup a partir de `completion_items` y del texto ya escrito.
    pub fn refilter_completions(&mut self) {
        let Some(buf) = self.get_active_document().map(|d| &d.buffer) else { return self.clear_completions() };
        let cursor = buf.cursor_char_idx;
        let cursor_line = buf.text.char_to_line(cursor);
        if cursor < self.completion_anchor || buf.text.char_to_line(self.completion_anchor) != cursor_line {
            return self.clear_completions();
        }
        let word = buf.get_current_word_prefix();
        let matcher = fuzzy_matcher::skim::SkimMatcherV2::default().ignore_case();
        let mut seen = HashSet::new();
        let mut ranked = Vec::new();

        for item in &self.completion_items {
            // Con rango del servidor lo escrito es lo que va de su inicio al cursor (incluye `-`, `<`, `@`...).
            let typed = match item.edit_range {
                Some(r) if r.start.line as usize == cursor_line => {
                    let start = buf.lsp_position_to_char_idx(r.start.line, r.start.character);
                    if start > cursor { continue; }
                    buf.text.slice(start..cursor).to_string()
                }
                Some(_) => continue,
                None => word.clone(),
            };
            let Some(m) = completion::match_candidate(&matcher, &typed, &item.filter_text) else { continue };
            // Varios servidores (p. ej. ts + vue) devuelven las mismas sugerencias.
            if !seen.insert((item.label.as_str(), item.insert_text.as_str())) { continue; }
            let usage = self.completion_usage.get(&item.label).copied().unwrap_or(0);
            ranked.push((m, usage, item));
        }
        ranked.sort_by(|a, b| {
            completion::sort_key(&a.0, a.1, a.2.kind, &a.2.sort_text, &a.2.label)
                .cmp(&completion::sort_key(&b.0, b.1, b.2.kind, &b.2.sort_text, &b.2.label))
        });

        self.completions = ranked.into_iter().map(|(m, _, item)| CompletionOption {
            // rust-analyzer etiqueta "push(…)" con filterText "push": los índices siguen valiendo.
            matched: if item.label.starts_with(&item.filter_text) { m.indices } else { Vec::new() },
            ..item.clone()
        }).collect();
        self.completion_state.select(if self.completions.is_empty() { None } else { Some(0) });
    }

    /// Inserta la sugerencia usando el rango del servidor si lo dio, más sus ediciones extra (auto-imports).
    pub fn accept_completion(&mut self, comp: &CompletionOption) {
        *self.completion_usage.entry(comp.label.clone()).or_default() += 1;
        let Some(doc) = self.active_document_mut() else { return };
        let buf = &mut doc.buffer;
        let cursor = buf.cursor_char_idx;

        let range = match comp.edit_range {
            Some(r) => {
                let start = buf.lsp_position_to_char_idx(r.start.line, r.start.character).min(cursor);
                // Si se escribió más desde la petición, lo escrito también se reemplaza.
                let end = buf.lsp_position_to_char_idx(r.end.line, r.end.character).max(cursor);
                start..end
            }
            None => cursor - buf.completion_replace_len(&comp.insert_text)..cursor,
        };

        let mut edits = vec![(range, comp.insert_text.clone())];
        for e in &comp.additional_edits {
            let s = buf.lsp_position_to_char_idx(e.range.start.line, e.range.start.character);
            let t = buf.lsp_position_to_char_idx(e.range.end.line, e.range.end.character);
            edits.push((s..t, e.new_text.clone()));
        }

        let start = buf.apply_edits(&edits, 0);
        if let Some(ts) = &comp.tabstop {
            buf.cursor_char_idx = start + ts.end;
            buf.selection_anchor = (ts.start != ts.end).then_some(start + ts.start);
            buf.target_visual_col = buf.char_idx_to_visual_col(buf.cursor_char_idx);
        }
    }

    fn handle_completion_response(&mut self, server_id: usize, result: Value) {
        let Ok(response) = serde_json::from_value::<lsp_types::CompletionResponse>(result) else { return };
        let items = match response {
            lsp_types::CompletionResponse::Array(arr) => arr,
            lsp_types::CompletionResponse::List(list) => list.items,
        };
        if std::mem::take(&mut self.completion_stale) {
            self.completion_items.clear();
        }
        let can_resolve = self.lsp_clients.get(&server_id).is_some_and(|c| c.resolve_provider);

        for item in items {
            let resolve = can_resolve.then(|| serde_json::to_value(&item).ok().map(|v| (server_id, v))).flatten();
            self.next_completion_id += 1;
            let (raw, edit_range) = match &item.text_edit {
                Some(lsp_types::CompletionTextEdit::Edit(e)) => (e.new_text.clone(), Some(e.range)),
                Some(lsp_types::CompletionTextEdit::InsertAndReplace(e)) => (e.new_text.clone(), Some(e.insert)),
                None => (item.insert_text.clone().unwrap_or_else(|| item.label.clone()), None),
            };
            let (insert_text, tabstop) = if item.insert_text_format == Some(lsp_types::InsertTextFormat::SNIPPET) {
                lsp::expand_snippet(&raw)
            } else {
                (raw, None)
            };
            let details = item.label_details.unwrap_or_default();

            self.completion_items.push(CompletionOption {
                filter_text: item.filter_text.unwrap_or_else(|| item.label.clone()),
                sort_text: item.sort_text.unwrap_or_else(|| item.label.clone()),
                kind: item.kind,
                detail: item.detail.or(details.description),
                label_detail: details.detail,
                label: item.label,
                insert_text,
                edit_range,
                additional_edits: item.additional_text_edits.unwrap_or_default(),
                tabstop,
                matched: Vec::new(),
                id: self.next_completion_id,
                documentation: item.documentation.as_ref().map(doc_text),
                resolve,
            });
        }
        self.refilter_completions();
    }

    /// Procesa la cola del lector LSP; devuelve si cambió algo visible.
    pub fn process_lsp_messages(&mut self) -> bool {
        let mut ui_changed = false;

        while let Ok((server_id, msg)) = self.lsp_rx.try_recv() {
            if !self.lsp_clients.contains_key(&server_id) { continue; }

            match msg {
                LspMessage::Request { id, method, params } => {
                    if let Some(client) = self.lsp_clients.get_mut(&server_id) {
                        client.reply(id, &method, &params);
                    }
                }
                LspMessage::Diagnostics(params) => {
                    let per_server = self.diagnostics.entry(params.uri.as_str().to_string()).or_default();
                    if params.diagnostics.is_empty() {
                        per_server.remove(&server_id);
                    } else {
                        per_server.insert(server_id, params.diagnostics);
                    }
                    ui_changed = true;
                }
                LspMessage::Response { id, result } => {
                    let client = self.lsp_clients.get_mut(&server_id).unwrap();
                    if id == client.init_id && !client.is_initialized {
                        client.on_initialized(&result);
                        for doc in self.documents.values() {
                            if let Some(ext) = doc.ext().filter(|ext| client.handles(ext)) {
                                open_in_server(client, server_id, doc, self.config.language_id(&ext), &mut self.pending_inlay_hints_ids);
                            }
                        }
                        self.status_msg = Some(format!("LSP Conectado ({})", client.name));
                        ui_changed = true;
                    } else if let Some(idx) = self.pending_inlay_hints_ids.iter().position(|&(s, i, _)| s == server_id && i == id) {
                        let (_, _, uri) = self.pending_inlay_hints_ids.remove(idx);
                        let per_server = self.inlay_hints.entry(uri).or_default();
                        match serde_json::from_value::<Vec<InlayHint>>(result) {
                            Ok(hints) => { per_server.insert(server_id, hints); }
                            Err(_) => { per_server.remove(&server_id); }
                        }
                        ui_changed = true;
                    } else if let Some((_, _, item_id)) = self.pending_resolve.filter(|&(s, r, _)| (s, r) == (server_id, id)) {
                        self.pending_resolve = None;
                        self.handle_resolve_response(item_id, result);
                        ui_changed = true;
                    } else if let Some(idx) = self.pending_completion_ids.iter().position(|&p| p == (server_id, id)) {
                        self.pending_completion_ids.remove(idx);
                        self.handle_completion_response(server_id, result);
                        ui_changed = true;
                    }
                }
                LspMessage::Error(err) => {
                    if let Some(client) = self.lsp_clients.remove(&server_id) {
                        self.status_msg = Some(format!("Fallo Crítico LSP ({}): {}", client.name, err));
                    }
                    self.pending_completion_ids.retain(|&(s, _)| s != server_id);
                    if self.pending_resolve.is_some_and(|(s, _, _)| s == server_id) { self.pending_resolve = None; }
                    self.pending_inlay_hints_ids.retain(|&(s, _, _)| s != server_id);
                    for per_server in self.diagnostics.values_mut() { per_server.remove(&server_id); }
                    for per_server in self.inlay_hints.values_mut() { per_server.remove(&server_id); }
                    ui_changed = true;
                }
            }
        }
        ui_changed
    }

    /// Punto único tras cualquier edición: marca sucio, actualiza la búsqueda y
    /// envía al LSP los cambios acumulados en cada buffer.
    pub fn after_edit(&mut self) {
        for (&buf_id, doc) in self.documents.iter_mut() {
            if doc.buffer.lsp_changes.is_empty() { continue; }
            let changes = std::mem::take(&mut doc.buffer.lsp_changes);
            doc.is_dirty = true;

            if self.search_buffer == Some(buf_id)
                && let Some(q) = &self.last_search_query {
                    self.text_search_results = doc.buffer.find_text(q);
                    self.current_search_idx = self.current_search_idx.min(self.text_search_results.len().saturating_sub(1));
                }

            let (Some(uri), Some(ext)) = (doc.uri.clone(), doc.ext()) else { continue };
            doc.version += 1;
            let mut full_text = None;

            for (&server_id, client) in self.lsp_clients.iter_mut() {
                if !client.is_initialized || !client.handles(&ext) { continue; }
                let events = if client.incremental_sync {
                    changes.iter().map(|c| lsp_types::TextDocumentContentChangeEvent {
                        range: Some(lsp_types::Range {
                            start: lsp_types::Position { line: c.start.0, character: c.start.1 },
                            end: lsp_types::Position { line: c.end.0, character: c.end.1 },
                        }),
                        range_length: None,
                        text: c.text.clone(),
                    }).collect()
                } else {
                    let text = full_text.get_or_insert_with(|| doc.buffer.get_full_text()).clone();
                    vec![lsp_types::TextDocumentContentChangeEvent { range: None, range_length: None, text }]
                };
                client.did_change(uri.clone(), doc.version, events);
                let id = client.request_inlay_hints(uri.clone(), doc.full_range());
                self.pending_inlay_hints_ids.push((server_id, id, uri.as_str().to_string()));
            }
        }
    }

    pub fn jump_to_search_result(&mut self) {
        let Some(&char_idx) = self.text_search_results.get(self.current_search_idx) else { return };
        let query_len = self.last_search_query.as_ref().map_or(0, |q| q.chars().count());

        if let Some(doc) = self.active_document_mut() {
            let max_len = doc.buffer.text.len_chars();
            if char_idx >= max_len { return; }

            doc.buffer.cursor_char_idx = char_idx;
            if query_len > 0 {
                doc.buffer.selection_anchor = Some((char_idx + query_len).min(max_len));
            }
        }
        self.show_tree = false;
        self.status_msg = Some(format!("Coincidencia {}/{}", self.current_search_idx + 1, self.text_search_results.len()));
    }

    /// Avanza a la siguiente coincidencia; en otro buffer, repite la última búsqueda en él.
    pub fn next_search_result(&mut self) {
        let (Some(query), Some(buf_id)) = (self.last_search_query.clone(), self.active_buffer_id()) else { return };
        if self.search_buffer != Some(buf_id) {
            self.text_search_results = self.documents[&buf_id].buffer.find_text(&query);
            self.search_buffer = Some(buf_id);
            self.current_search_idx = self.text_search_results.len().saturating_sub(1);
        }
        if self.text_search_results.is_empty() {
            self.status_msg = Some(format!("Sin coincidencias para \"{}\"", query));
            return;
        }
        self.current_search_idx = (self.current_search_idx + 1) % self.text_search_results.len();
        self.jump_to_search_result();
    }

    pub fn copy_to_clipboard(&mut self, text: String) {
        if let Some(cb) = &mut self.clipboard {
            let _ = cb.set_text(text.clone());
        }
        self.clipboard_fallback = Some(text);
    }

    pub fn read_clipboard(&mut self) -> Option<String> {
        self.clipboard.as_mut().and_then(|cb| cb.get_text().ok())
            .or_else(|| {
                std::env::var_os("WAYLAND_DISPLAY")?;
                let out = std::process::Command::new("wl-paste").arg("--no-newline").output().ok()?;
                out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
            })
            .or_else(|| self.clipboard_fallback.clone())
    }

    pub fn open_prompt(&mut self, intent: PromptIntent) {
        let mut prompt = PromptState::new(intent);
        match &prompt.intent {
            PromptIntent::Rename(path) => {
                if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                    prompt.input = name.to_string();
                }
            }
            PromptIntent::SearchFile | PromptIntent::SearchText => {
                prompt.files = Arc::new(list_project_files(&self.working_dir));
            }
            _ => {}
        }
        self.prompt = Some(prompt);
        self.update_prompt_search();
    }

    pub fn update_prompt_search(&mut self) {
        let Some(prompt) = &mut self.prompt else { return };
        match prompt.intent {
            PromptIntent::SearchFile => {
                prompt.file_results = fuzzy_filter_files(&self.working_dir, &prompt.files, &prompt.input);
                prompt.selection_state.select(if prompt.file_results.is_empty() { None } else { Some(0) });
            }
            PromptIntent::SearchText => {
                // Cada tecla invalida la búsqueda anterior, que aborta en su siguiente archivo.
                let generation = self.search_gen.fetch_add(1, Ordering::SeqCst) + 1;
                if prompt.input.is_empty() {
                    prompt.text_results.clear();
                    prompt.selection_state.select(None);
                    self.search_pending = false;
                    return;
                }
                let (files, query) = (prompt.files.clone(), prompt.input.clone());
                let (counter, tx) = (self.search_gen.clone(), self.search_tx.clone());
                self.search_pending = true;
                std::thread::spawn(move || {
                    let results = search_text(&files, &query, 200, &|| counter.load(Ordering::SeqCst) != generation);
                    let _ = tx.send((generation, results));
                });
            }
            _ => {}
        }
    }

    /// Recoge resultados de la búsqueda de texto en segundo plano.
    pub fn poll_text_search(&mut self) -> bool {
        let mut changed = false;
        while let Ok((generation, results)) = self.search_rx.try_recv() {
            if generation != self.search_gen.load(Ordering::SeqCst) { continue; }
            self.search_pending = false;
            if let Some(p) = self.prompt.as_mut().filter(|p| matches!(p.intent, PromptIntent::SearchText)) {
                p.selection_state.select(if results.is_empty() { None } else { Some(0) });
                p.text_results = results;
                changed = true;
            }
        }
        changed
    }
}

fn mtime(p: &Path) -> Option<SystemTime> {
    std::fs::metadata(p).and_then(|m| m.modified()).ok()
}

fn doc_text(d: &lsp_types::Documentation) -> String {
    match d {
        lsp_types::Documentation::String(s) => s.clone(),
        lsp_types::Documentation::MarkupContent(m) => m.value.clone(),
    }
}

fn open_in_server(client: &mut LspClient, server_id: usize, doc: &Document, language_id: &str, pending_hints: &mut Vec<(usize, u64, String)>) {
    let Some(uri) = doc.uri.clone() else { return };
    client.did_open(uri.clone(), doc.buffer.get_full_text(), doc.version, language_id);
    let id = client.request_inlay_hints(uri.clone(), doc.full_range());
    pending_hints.push((server_id, id, uri.as_str().to_string()));
}

/// Un nombre, no una ruta: evita que renombrar/guardar mueva archivos a otro directorio.
fn validate_file_name(name: &str) -> Result<(), &'static str> {
    if name.is_empty() { return Err("nombre vacío"); }
    if name == "." || name == ".." || name.contains('/') || name.contains(std::path::MAIN_SEPARATOR) {
        return Err("el nombre no puede ser una ruta");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("wyvern_app_{}_{}", tag, std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        fs::canonicalize(&d).unwrap()
    }

    fn app_in(dir: &Path) -> App {
        let mut app = App::new();
        app.working_dir = dir.to_path_buf();
        app
    }

    fn prompt(app: &mut App, intent: PromptIntent, input: &str) {
        app.open_prompt(intent);
        app.prompt.as_mut().unwrap().input = input.into();
        app.execute_prompt();
    }

    #[test]
    fn unreadable_file_is_not_opened_as_empty() {
        let d = tmp("bad");
        fs::write(d.join("bin"), [0xff, 0xfe]).unwrap();
        let mut app = app_in(&d);
        app.load_file(d.join("bin"));
        assert!(app.documents.is_empty());
        assert!(app.status_msg.as_deref().unwrap().contains("No se pudo abrir"));
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn same_file_via_different_paths_shares_buffer() {
        let d = tmp("same");
        fs::create_dir(d.join("sub")).unwrap();
        fs::write(d.join("a.txt"), "x").unwrap();
        let mut app = app_in(&d);
        app.load_file(d.join("a.txt"));
        app.load_file(d.join("sub/../a.txt"));
        assert_eq!(app.documents.len(), 1);
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn rename_and_save_as_never_overwrite() {
        let d = tmp("ovr");
        fs::write(d.join("a.txt"), "A").unwrap();
        fs::write(d.join("b.txt"), "B").unwrap();
        let mut app = app_in(&d);

        prompt(&mut app, PromptIntent::Rename(d.join("a.txt")), "b.txt");
        assert_eq!(fs::read_to_string(d.join("b.txt")).unwrap(), "B");
        assert!(d.join("a.txt").exists());

        app.new_blank_file();
        app.active_document_mut().unwrap().buffer.insert_str("nuevo");
        prompt(&mut app, PromptIntent::SaveAs(d.clone()), "b.txt");
        assert_eq!(fs::read_to_string(d.join("b.txt")).unwrap(), "B");
        assert!(app.get_active_document().unwrap().filepath.is_none());

        prompt(&mut app, PromptIntent::SaveAs(d.clone()), "../fuera.txt");
        assert!(!d.parent().unwrap().join("fuera.txt").exists());
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn rename_updates_every_open_buffer_including_dir_contents() {
        let d = tmp("ren");
        fs::create_dir(d.join("dir")).unwrap();
        fs::write(d.join("dir/x.txt"), "x").unwrap();
        fs::write(d.join("y.txt"), "y").unwrap();
        let mut app = app_in(&d);
        app.load_file(d.join("dir/x.txt"));
        app.load_file(d.join("y.txt"));

        prompt(&mut app, PromptIntent::Rename(d.join("dir")), "dir2");
        let paths: Vec<_> = app.documents.values().filter_map(|doc| doc.filepath.clone()).collect();
        assert!(paths.contains(&d.join("dir2/x.txt")));
        assert!(paths.contains(&d.join("y.txt")));

        let id = app.documents.iter().find(|(_, doc)| doc.filepath == Some(d.join("dir2/x.txt"))).map(|(&id, _)| id).unwrap();
        app.show_buffer(id);
        assert!(app.save_file());
        assert!(!d.join("dir").exists(), "guardar no debe recrear la ruta antigua");
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn edits_mark_dirty_and_refresh_search_results() {
        let d = tmp("dirty");
        fs::write(d.join("a.txt"), "foo bar foo").unwrap();
        let mut app = app_in(&d);
        app.load_file(d.join("a.txt"));
        app.last_search_query = Some("foo".into());
        app.next_search_result();
        assert_eq!(app.text_search_results, vec![0, 8]);

        let doc = app.active_document_mut().unwrap();
        doc.buffer.selection_anchor = None;
        doc.buffer.cursor_char_idx = 0;
        doc.buffer.insert_str("    ");
        app.after_edit();
        assert!(app.get_active_document().unwrap().is_dirty);
        assert_eq!(app.text_search_results, vec![4, 12]);
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn delete_marks_open_buffers_dirty() {
        let d = tmp("del");
        fs::write(d.join("a.txt"), "a").unwrap();
        let mut app = app_in(&d);
        app.load_file(d.join("a.txt"));
        prompt(&mut app, PromptIntent::Delete(d.join("a.txt")), "y");
        assert!(!d.join("a.txt").exists());
        assert!(app.get_active_document().unwrap().is_dirty);
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn completion_uses_server_range_and_additional_edits() {
        let d = tmp("comp");
        fs::write(d.join("a.rs"), "fn f() {\n    x.fo\n}").unwrap();
        let mut app = app_in(&d);
        app.load_file(d.join("a.rs"));
        let doc = app.active_document_mut().unwrap();
        doc.buffer.cursor_char_idx = 17;

        let pos = |line, character| lsp_types::Position { line, character };
        let (text, tabstop) = lsp::expand_snippet("foo(${1:a})");
        app.accept_completion(&CompletionOption {
            label: "foo".into(), insert_text: text, tabstop,
            edit_range: Some(lsp_types::Range { start: pos(1, 6), end: pos(1, 8) }),
            additional_edits: vec![lsp_types::TextEdit { range: lsp_types::Range { start: pos(0, 0), end: pos(0, 0) }, new_text: "use m::foo;\n".into() }],
            ..Default::default()
        });
        let buf = &app.get_active_document().unwrap().buffer;
        assert_eq!(buf.text.to_string(), "use m::foo;\nfn f() {\n    x.foo(a)\n}");
        assert_eq!(buf.get_selected_text().as_deref(), Some("a"));
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn completion_session_refilters_locally_and_closes_when_prefix_is_deleted() {
        let d = tmp("refilter");
        fs::write(d.join("a.rs"), "fn f() {\n    \n}").unwrap();
        let mut app = app_in(&d);
        app.load_file(d.join("a.rs"));
        app.active_document_mut().unwrap().buffer.cursor_char_idx = 13;
        let typed = |app: &mut App, c: char| {
            app.active_document_mut().unwrap().buffer.insert_char(c);
            app.trigger_completion(None);
        };
        let labels = |app: &App| app.completions.iter().map(|c| c.label.clone()).collect::<Vec<_>>();

        typed(&mut app, 'h');
        app.handle_completion_response(0, serde_json::json!([
            {"label": "HashMap", "sortText": "1"}, {"label": "hash", "sortText": "2"},
            {"label": "HashMap", "sortText": "1"}, {"label": "other"}
        ]));
        assert_eq!(labels(&app), ["hash", "HashMap", "other"], "prefijo exacto primero y sin duplicados");

        typed(&mut app, 'a');
        typed(&mut app, 's');
        assert_eq!(labels(&app), ["hash", "HashMap"], "se refiltra sin esperar al servidor");
        assert_eq!(app.completions[1].matched, [0, 1, 2]);

        for expected in [2, 3] {
            app.active_document_mut().unwrap().buffer.delete_backwards();
            app.refilter_completions();
            assert_eq!(app.completions.len(), expected);
        }
        app.active_document_mut().unwrap().buffer.delete_backwards();
        app.refilter_completions();
        assert!(app.completions.is_empty() && app.completion_items.is_empty());

        // Una respuesta nueva reemplaza a las anteriores en vez de acumularse.
        typed(&mut app, 'x');
        app.handle_completion_response(0, serde_json::json!([{"label": "xs"}]));
        typed(&mut app, 's');
        app.handle_completion_response(0, serde_json::json!({"isIncomplete": true, "items": [{"label": "xsa"}, {"label": "xs"}]}));
        assert_eq!(labels(&app), ["xs", "xsa"]);
        fs::remove_dir_all(&d).unwrap();
    }

    /// Necesita rust-analyzer en el PATH: `cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn real_rust_analyzer_completes_after_trigger_char() {
        let d = tmp("ra");
        fs::write(d.join("Cargo.toml"), "[package]\nname = \"t\"\nversion = \"0.1.0\"\nedition = \"2021\"\n").unwrap();
        fs::create_dir_all(d.join("src")).unwrap();
        fs::write(d.join("src/main.rs"), "fn main() {\n    let v: Vec<i32> = Vec::new();\n    v\n}\n").unwrap();
        let mut app = app_in(&d);
        app.load_file(d.join("src/main.rs"));
        let wait = |app: &mut App, done: &dyn Fn(&App) -> bool| {
            let t = Instant::now();
            while !done(app) && t.elapsed().as_secs() < 60 {
                app.process_lsp_messages();
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
        };
        wait(&mut app, &|a| a.lsp_clients.values().any(|c| c.is_initialized));
        assert!(app.is_completion_trigger('.'), "rust-analyzer anuncia '.' como disparador");

        app.active_document_mut().unwrap().buffer.cursor_char_idx = 51;
        app.active_document_mut().unwrap().buffer.insert_char('.');
        // Recién arrancado, rust-analyzer puede devolver listas vacías mientras indexa.
        let t = Instant::now();
        while app.completions.iter().all(|c| c.filter_text != "push") && t.elapsed().as_secs() < 90 {
            app.trigger_completion(Some('.'));
            wait(&mut app, &|a| a.pending_completion_ids.is_empty());
        }
        app.active_document_mut().unwrap().buffer.insert_str("pu");
        app.refilter_completions();
        let top = app.completions.first().unwrap();
        assert_eq!((top.filter_text.as_str(), top.matched.as_slice()), ("push", &[0, 1][..]));
        app.resolve_selected_completion();
        wait(&mut app, &|a| a.pending_resolve.is_none());
        let doc = app.completions[0].documentation.clone().unwrap_or_default();
        assert!(doc.contains("Appends an element"), "{doc}");
        let accepted = app.completions[0].clone();
        app.accept_completion(&accepted);
        assert!(app.get_active_document().unwrap().buffer.text.to_string().contains("v.push("));
        for c in app.lsp_clients.values_mut() { c.shutdown_and_exit(); }
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn theme_and_config_reload_while_running() {
        let d = tmp("hot");
        let (_, _, errors) = config::load(&d);
        assert!(errors.is_empty());
        let mut app = App::new();
        app.config_dir = Some(d.clone());
        fs::write(d.join("a.rs"), "fn x() {}").unwrap();
        app.load_file(d.join("a.rs"));
        let hl = syntect::highlighting::Highlighter::new(&app.theme.syntect);
        let state = (syntect::parsing::ParseState::new(app.syntax_set.find_syntax_plain_text()),
            syntect::highlighting::HighlightState::new(&hl, Default::default()));
        app.active_document_mut().unwrap().buffer.syntax_cache = vec![state];

        // Fuerza mtimes distintos: dos escrituras seguidas pueden compartir marca de tiempo.
        let mut tick = SystemTime::now();
        let mut write = |path: &Path, text: &str| {
            fs::write(path, text).unwrap();
            tick += Duration::from_secs(5);
            fs::File::options().write(true).open(path).unwrap().set_modified(tick).unwrap();
        };
        let reload = |app: &mut App| { app.last_config_check = Instant::now() - Duration::from_secs(1); app.reload_config_if_changed() };
        let theme_file = d.join("themes/wyvern-dark.json");
        // App::new no conocía el directorio de prueba: se sincronizan las marcas como al arrancar.
        app.config_mtimes = (mtime(&d.join("config.json")), mtime(&theme_file));

        assert!(!reload(&mut app), "sin cambios no se recarga");
        write(&theme_file, r##"{"ui": {"accent": "#010203"}}"##);
        assert!(reload(&mut app));
        assert_eq!(app.theme.ui.accent, ratatui::style::Color::Rgb(1, 2, 3));
        assert!(app.get_active_document().unwrap().buffer.syntax_cache.is_empty(), "el resaltado se recalcula con el tema nuevo");

        write(&theme_file, r#"{"ui": {"#);
        assert!(reload(&mut app));
        assert_eq!(app.theme.ui.accent, ratatui::style::Color::Rgb(1, 2, 3), "un tema roto conserva el actual");
        assert!(app.status_msg.as_deref().unwrap().contains("wyvern-dark.json"));

        write(&d.join("themes/light.json"), r#"{"ui": {"accent": "blue"}}"#);
        write(&d.join("config.json"), r#"{"theme": "light", "servers": []}"#);
        assert!(reload(&mut app));
        assert_eq!((app.theme_name.as_str(), app.theme.ui.accent), ("light", ratatui::style::Color::Blue));
        assert!(app.config.servers.is_empty());

        app.cycle_theme();
        assert_eq!(app.theme_name, "light", "no se cambia a un tema inválido");
        write(&theme_file, "{}");
        app.cycle_theme();
        assert_eq!(app.theme_name, "wyvern-dark");
        app.cycle_theme();
        assert_eq!(app.theme_name, "light");
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn selected_completion_is_resolved_once_with_documentation() {
        let d = tmp("resolve");
        fs::write(d.join("a.rs"), "").unwrap();
        let mut app = app_in(&d);
        app.load_file(d.join("a.rs"));
        let cfg = lsp::ServerConfig { command: "cat".into(), args: vec![], extensions: vec!["rs".into()], initialization_options: Value::Null, settings: Value::Null };
        let mut client = LspClient::start(&cfg, &d, 7, app.lsp_tx.clone()).unwrap();
        client.on_initialized(&serde_json::json!({"capabilities": {"completionProvider": {"resolveProvider": true}}}));
        app.lsp_clients.insert(7, client);

        app.active_document_mut().unwrap().buffer.insert_char('f');
        app.trigger_completion(None);
        app.handle_completion_response(7, serde_json::json!([
            {"label": "foo", "documentation": {"kind": "markdown", "value": "ya incluida"}},
            {"label": "fob"}
        ]));
        assert!(app.completions.iter().all(|c| c.resolve.is_some()));

        app.completion_state.select(app.completions.iter().position(|c| c.label == "fob"));
        app.resolve_selected_completion();
        let (_, req, item) = app.pending_resolve.unwrap();
        app.resolve_selected_completion();
        assert_eq!(app.pending_resolve.unwrap().1, req, "no se repite la petición");

        app.lsp_tx.send((7, LspMessage::Response { id: req, result: serde_json::json!({
            "label": "fob", "documentation": "Docs de fob",
            "additionalTextEdits": [{"range": {"start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 0}}, "newText": "use x::fob;\n"}]
        })})).unwrap();
        app.process_lsp_messages();
        assert!(app.pending_resolve.is_none());
        for list in [&app.completions, &app.completion_items] {
            let c = list.iter().find(|c| c.id == item).unwrap();
            assert_eq!((c.documentation.as_deref(), c.resolve.is_none(), c.additional_edits.len()), (Some("Docs de fob"), true, 1));
        }
        app.refilter_completions();
        assert_eq!(app.completions.iter().find(|c| c.label == "foo").unwrap().documentation.as_deref(), Some("ya incluida"));
        assert!(app.completions.iter().find(|c| c.label == "fob").unwrap().resolve.is_none(), "refiltrar no revive el resolve");
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn project_root_prefers_git_then_manifest() {
        let d = tmp("root");
        fs::create_dir_all(d.join("crate/src")).unwrap();
        fs::write(d.join("crate/Cargo.toml"), "").unwrap();
        assert_eq!(find_project_root(&d.join("crate/src")), d.join("crate"));
        fs::create_dir(d.join(".git")).unwrap();
        assert_eq!(find_project_root(&d.join("crate/src")), d);
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn closing_window_focuses_first_leaf() {
        let mut app = App::new();
        app.new_blank_file();
        app.split_window(Direction::Vertical);
        app.split_window(Direction::Horizontal);
        let first = app.layout.first_leaf();
        app.close_active_window();
        assert_eq!(app.focus, Focus::Window(first));
    }
}
