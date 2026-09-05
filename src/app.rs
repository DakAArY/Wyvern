use crate::editor::EditorBuffer;
use crate::explorer::{find_files_in_project, FileExplorer, find_text_in_project};
use crate::lsp::LspClient;
use std::path::PathBuf;
use std::time::Instant;
use std::collections::HashMap;
use lsp_types::{Diagnostic, CompletionItemKind, Uri, InlayHint};
use ratatui::widgets::ListState;
use ratatui::layout::{Direction, Rect};
use crossterm::event::{KeyCode, KeyModifiers};
use syntect::parsing::SyntaxSet;
use syntect::highlighting::ThemeSet;

pub type BufferId = usize;

#[derive(Clone)]
pub struct Window {
    pub id: usize,
    pub buffer_id: BufferId,
}

#[derive(PartialEq, Clone, Copy, Debug)]
pub enum Focus {
    Tree,
    Window(usize),
}

pub struct Document {
    pub buffer: EditorBuffer,
    pub filepath: Option<PathBuf>,
    pub uri: Option<Uri>,
    pub version: i32,
    pub is_dirty: bool,
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
    NextSearchResult, Copy, Cut, Paste, Undo, Redo, Quit, Save,
    ScrollViewportUp(bool), ScrollViewportDown(bool),
    ScrollUp1(bool), ScrollDown1(bool),
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
            SplitNode::Split(dir, a, b, ) => {
                let new_a = a.remove_leaf(target_id);
                let new_b = b.remove_leaf(target_id);
                match (new_a, new_b) {
                    (Some(a), Some(b)) => Some(SplitNode::Split(dir, Box::new(a), Box::new(b))),
                    (Some(a), None) => Some(a),
                    (None, Some(b)) => Some(b),
                    (None, None) => None,
                }
            }
        }
    }
}

#[derive(Clone)]
pub struct CompletionOption {
    pub label: String,
    pub kind: Option<CompletionItemKind>,
    pub detail: Option<String>,
    pub insert_text: String,
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
    pub text_results: Vec<crate::explorer::WorkspaceTextMatch>,
}

impl PromptState {
    pub fn new (intent: PromptIntent) -> Self {
        let mut selection_state = ListState::default();
        selection_state.select(None);
        Self { intent, input: String::new(), selection_state, file_results: Vec::new(), text_results: Vec::new() }
    }
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
    pub lsp_client: Option<LspClient>,
    pub needs_redraw: bool,
    pub show_help: bool,
    pub clipboard: Option<String>,
    pub last_click: Option<(Instant, u16, u16)>,
    pub diagnostics: HashMap<usize, Vec<Diagnostic>>,
    pub completions: Vec<CompletionOption>,
    pub completion_state: ListState,
    pub pending_completion_id: Option<u64>,
    pub working_dir: PathBuf,
    pub prompt: Option<PromptState>,
    pub git_ctx: crate::git::GitContext,
    pub keybindings: HashMap<KeyCombo, Action>,
    pub last_search_query: Option<String>,
    pub text_search_results: Vec<usize>,
    pub current_search_idx: usize,
    pub syntax_set: SyntaxSet,
    pub theme_set: ThemeSet,
    pub inlay_hints: HashMap<usize, Vec<InlayHint>>,
    pub pending_inlay_hints_id: Option<u64>,

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
        let git_ctx = crate::git::GitContext::refresh(&current_dir, None);
        let clipboard_ctx = arboard::Clipboard::new().ok();

        Self {
            state: AppState::Intro,
            working_dir: current_dir.clone(),
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
            status_msg: None,
            lsp_client: None,
            needs_redraw: true,
            show_help: false,
            clipboard: None,
            last_click: None,
            diagnostics: HashMap::new(),
            completions: Vec::new(),
            completion_state: ListState::default(),
            pending_completion_id: None,
            prompt: None,
            git_ctx,
            keybindings: Self::default_keybindings(),
            last_search_query: None,
            text_search_results: Vec::new(),
            current_search_idx: 0,
            syntax_set: SyntaxSet::load_defaults_newlines(),
            theme_set: ThemeSet::load_defaults(),
            inlay_hints: HashMap::new(),
            pending_inlay_hints_id: None,
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

        let prompt_target = if self.prompt.is_some() { 1.0 } else { 0.0 };
        let dp = prompt_target - self.prompt_spawn_progress;
        if dp.abs() > 0.01 {
            self.prompt_spawn_progress += dp * (1.0 - (-35.0 * dt).exp());
            changed = true;
        } else {
            self.prompt_spawn_progress = prompt_target;
        }

        let comps_target = if !self.completions.is_empty() { 1.0 } else { 0.0 };
        let dc = comps_target - self.completions_spawn_progress;
        if dc.abs() > 0.01 {
            self.completions_spawn_progress += dc * (1.0 - (-40.0 * dt).exp());
            changed = true;
        } else {
            self.completions_spawn_progress = comps_target;
        }

        let help_target = if self.show_help { 1.0 } else { 0.0 };
        let dh = help_target - self.help_spawn_progress;
        if dh.abs() > 0.01 {
            self.help_spawn_progress += dh * (1.0 - (-35.0 * dt).exp());
            changed = true;
        } else {
            self.help_spawn_progress = help_target;
        }
        
        let tree_target = if self.show_tree { 1.0 } else { 0.0 };
        let dt_tree = tree_target - self.tree_spawn_progress;
        if dt_tree.abs() > 0.01 {
            self.tree_spawn_progress += dt_tree * (1.0 - (-35.0 * dt).exp());
            changed = true;
        } else {
            self.tree_spawn_progress = tree_target;
        }

        changed
    }

    pub fn is_animating(&self) -> bool {
        (self.target_cursor_x - self.visual_cursor_x).abs() > 0.05
            || (self.target_cursor_y - self.visual_cursor_y).abs() > 0.05
            || (if self.prompt.is_some() { 1.0 } else { 0.0 } - self.prompt_spawn_progress).abs() > 0.01
            || (if !self.completions.is_empty() { 1.0 } else { 0.0 } - self.completions_spawn_progress).abs() > 0.01
            || (if self.show_help { 1.0 } else { 0.0 } - self.help_spawn_progress).abs() > 0.01
            || (if self.show_tree { 1.0 } else { 0.0 } - self.tree_spawn_progress).abs() > 0.01
    }

    pub fn default_keybindings() -> HashMap<KeyCombo, Action> {
        let mut kb = HashMap::new();
        kb.insert(KeyCombo { code: KeyCode::F(1), modifiers: KeyModifiers::empty() }, Action::ToggleHelp);
        kb.insert(KeyCombo { code: KeyCode::F(2), modifiers: KeyModifiers::empty() }, Action::ToggleTree);
        kb.insert(KeyCombo { code: KeyCode::F(3), modifiers: KeyModifiers::empty() }, Action::NextSearchResult);

        kb.insert(KeyCombo { code: KeyCode::Char('c'), modifiers: KeyModifiers::CONTROL }, Action::Copy);
        kb.insert(KeyCombo { code: KeyCode::Char('x'), modifiers: KeyModifiers::CONTROL }, Action::Cut);
        kb.insert(KeyCombo { code: KeyCode::Char('v'), modifiers: KeyModifiers::CONTROL }, Action::Paste);
        kb.insert(KeyCombo { code: KeyCode::Char('z'), modifiers: KeyModifiers::CONTROL }, Action::Undo);
        kb.insert(KeyCombo { code: KeyCode::Char('y'), modifiers: KeyModifiers::CONTROL }, Action::Redo);
        kb.insert(KeyCombo { code: KeyCode::Char('q'), modifiers: KeyModifiers::CONTROL }, Action::Quit);
        kb.insert(KeyCombo { code: KeyCode::Char('s'), modifiers: KeyModifiers::CONTROL }, Action::Save);
        kb.insert(KeyCombo { code: KeyCode::Char('t'), modifiers: KeyModifiers::CONTROL }, Action::SearchText);
        kb.insert(KeyCombo { code: KeyCode::Char('f'), modifiers: KeyModifiers::CONTROL }, Action::SearchFile);

        kb.insert(KeyCombo { code: KeyCode::Char('u'), modifiers: KeyModifiers::CONTROL }, Action::ScrollViewportUp(false));
        kb.insert(KeyCombo { code: KeyCode::Char('d'), modifiers: KeyModifiers::CONTROL }, Action::ScrollViewportDown(false));
        kb.insert(KeyCombo { code: KeyCode::Esc, modifiers: KeyModifiers::empty() }, Action::Cancel);
        kb.insert(KeyCombo { code: KeyCode::Enter, modifiers: KeyModifiers::empty() }, Action::Confirm);
        kb.insert(KeyCombo { code: KeyCode::Backspace, modifiers: KeyModifiers::empty() }, Action::Backspace);
        kb.insert(KeyCombo { code: KeyCode::Tab, modifiers: KeyModifiers::empty() }, Action::Indent);
        kb.insert(KeyCombo { code: KeyCode::Delete, modifiers: KeyModifiers::empty() }, Action::Delete);

        kb.insert(KeyCombo { code: KeyCode::Char('i'), modifiers: KeyModifiers::ALT }, Action::FocusUp);
        kb.insert(KeyCombo { code: KeyCode::Char('k'), modifiers: KeyModifiers::ALT }, Action::FocusDown);
        kb.insert(KeyCombo { code: KeyCode::Char('j'), modifiers: KeyModifiers::ALT }, Action::FocusLeft);
        kb.insert(KeyCombo { code: KeyCode::Char('l'), modifiers: KeyModifiers::ALT }, Action::FocusRight);

        kb.insert(KeyCombo { code: KeyCode::Char('v'), modifiers: KeyModifiers::ALT }, Action::SplitVertical);
        kb.insert(KeyCombo { code: KeyCode::Char('h'), modifiers: KeyModifiers::ALT }, Action::SplitHorizontal);
        kb.insert(KeyCombo { code: KeyCode::Char('w'), modifiers: KeyModifiers::ALT }, Action::CloseWindow);

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
            kb.insert(KeyCombo { code, modifiers: KeyModifiers::empty() }, norm);
            kb.insert(KeyCombo { code, modifiers: KeyModifiers::SHIFT }, sel);
        }

        kb
    }

    pub fn active_document_mut(&mut self) -> Option<&mut Document> {
        if let Focus::Window(idx) = self.focus {
            if let Some(window) = self.windows.get(&idx) {
                return self.documents.get_mut(&window.buffer_id);
            }
        }
        None
    }

    pub fn get_active_document(&self) -> Option<&Document> {
        if let Focus::Window(idx) = self.focus {
            if let Some(window) = self.windows.get(&idx) {
                return self.documents.get(&window.buffer_id);
            }
        }
        None
    }

    pub fn toggle_tree(&mut self) {
        self.show_tree = !self.show_tree;
        if self.show_tree {
            self.focus = Focus::Tree;
            let _ = self.explorer.reload();
        } else {
            if self.windows.is_empty() {
                self.state = AppState::Intro;
            } else {
                self.focus = Focus::Window(self.active_window)
            }
        }
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
        } else if self.focus == Focus::Tree && dx == 1 {
            if !self.windows.is_empty() {
                self.focus = Focus::Window(self.active_window);
            }
        }
    }

    pub fn split_window(&mut self, direction: Direction) {
        if let Focus::Window(active_id) = self.focus {
            if let Some(active_win) = self.windows.get(&active_id) {
                let current_buf_id = active_win.buffer_id;
                let new_win_id = self.next_window_id;
                self.next_window_id += 1;

                self.windows.insert(new_win_id, Window { id: new_win_id, buffer_id: current_buf_id });

                let replacement = SplitNode::Split(
                    direction,
                    Box::new(SplitNode::Leaf(active_id)),
                    Box::new(SplitNode::Leaf(new_win_id)),
                );

                self.layout.replace_leaf(active_id, replacement);

                self.focus = Focus::Window(new_win_id);
                self.active_window = new_win_id;
                self.needs_redraw = true;
            }
        }
    }

    pub fn close_active_window(&mut self) {
        if let Focus::Window(idx) = self.focus {
            self.windows.remove(&idx);

            if let Some(new_layout) = self.layout.clone().remove_leaf(idx) {
                self.layout = new_layout;
                if let Some(&first_key) = self.windows.keys().next() {
                    self.focus = Focus::Window(first_key);
                    self.active_window = first_key;
                }
            } else {
                self.layout = SplitNode::Leaf(0);
                self.focus = Focus::Tree;
                self.state = AppState::Intro;
            }
            self.needs_redraw = true;
        }
    }

    pub fn new_blank_file(&mut self) {
        let buf_id = self.next_buffer_id;
        self.next_buffer_id += 1;

        let doc = Document {
            buffer: EditorBuffer::new(),
            filepath: None,
            uri: None,
            version: 1,
            is_dirty: false
        };

        self.documents.insert(buf_id, doc);

        let win_id = self.next_window_id;
        self.next_window_id += 1;

        self.windows.insert(win_id, Window { id: win_id, buffer_id: buf_id });

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

    pub fn setup_lsp_for_current_file(&mut self) {
        if let Some(doc) = self.active_document_mut() {
            if let Some(path) = &doc.filepath {
                let current_dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
                let abs_path = if path.is_absolute() { path.clone() } else { current_dir.join(path) };
                let canonical_pat = std::fs::canonicalize(&abs_path).unwrap_or(abs_path);

                if let Ok(file_url) = url::Url::from_file_path(&canonical_pat) {
                    let uri: Uri = file_url.as_str().parse().unwrap();
                    doc.uri = Some(uri.clone());

                    if let Some(ext) = canonical_pat.extension().and_then(|e| e.to_str()) {
                        if let Some(client) = LspClient::start_for_extension(ext, current_dir) {
                            self.status_msg = Some(format!("LSP iniciado: ({})", ext));
                            self.lsp_client = Some(client);
                        }
                    }
                }
            }
        }
    }

    pub fn load_file(&mut self, path: PathBuf) {
        if let Some((&existing_buf_id, _)) = self.documents.iter().find(|(_, doc)| doc.filepath.as_ref() == Some(&path)) {
            if self.windows.is_empty() {
                let win_id = self.next_window_id;
                self.next_window_id += 1;

                self.windows.insert(win_id, Window { id: win_id, buffer_id: existing_buf_id });
                self.layout = SplitNode::Leaf(win_id);
                self.focus = Focus::Window(win_id);
                self.active_window = win_id;
                self.state = AppState::Workspace;
            } else {
                if let Some(win) = self.windows.get_mut(&self.active_window) {
                    win.buffer_id = existing_buf_id;
                }
                self.focus = Focus::Window(self.active_window);
            }

            self.working_dir = path.parent().map(|p| p.to_path_buf()).unwrap_or_else(|| PathBuf::from("."));
            self.git_ctx = crate::git::GitContext::refresh(&self.working_dir, Some(&path));
            return;
        }

        let buf = EditorBuffer::load_from_file(&path).unwrap_or_else(|_| EditorBuffer::new());
        let buf_id = self.next_buffer_id;
        self.next_buffer_id += 1;

        let doc = Document {
            buffer: buf,
            filepath: Some(path.clone()),
            uri: None,
            version: 1,
            is_dirty: false,
        };

        self.documents.insert(buf_id, doc);

        if self.windows.is_empty() {
            let win_id = self.next_window_id;
            self.next_window_id += 1;

            self.windows.insert(win_id, Window { id: win_id, buffer_id: buf_id });

            self.layout = SplitNode::Leaf(win_id);
            self.focus = Focus::Window(win_id);
            self.active_window = win_id;
            self.state = AppState::Workspace;
        } else {
            if let Some(win) = self.windows.get_mut(&self.active_window) {
                win.buffer_id = buf_id;
            }
            self.focus = Focus::Window(self.active_window);
        }

        self.working_dir = path.parent().map(|p| p.to_path_buf()).unwrap_or_else(|| PathBuf::from("."));
        self.git_ctx = crate::git::GitContext::refresh(&self.working_dir, Some(&path));
        self.setup_lsp_for_current_file();
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

    pub fn save_file(&mut self) {
        let work_dir = self.working_dir.clone();
        let mut succes = false;
        let err_msg = None;

        if let Some(doc) = self.active_document_mut() {
            if let Some(path) = &doc.filepath {
                match doc.buffer.save_to_file(path) {
                    Ok(_) => {
                        doc.is_dirty = false;
                        succes = true;
                    },
                    Err(e) => self.status_msg = Some(format!("Error: {}", e)),
                }
            } else {
                self.status_msg = Some("No hay archivo abierto para guardar".to_string());
            }
        }
        if succes {
            self.status_msg = Some("Guardado Exitosamente".to_string());
            let _ = self.explorer.reload();
            let active_path = self.get_active_document().and_then(|d| d.filepath.clone());
            self.git_ctx = crate::git::GitContext::refresh(&work_dir, active_path.as_deref());
        } else if let Some(err) = err_msg {
            self.status_msg = Some(err);
        }
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

    pub fn execute_prompt(&mut self) {
        if let Some(prompt) = self.prompt.take() {
            match prompt.intent {
                PromptIntent::SaveAs(dir) => {
                    if prompt.input.trim().is_empty() {
                        self.status_msg = Some("No se puede guardar: Nombre vacio".to_string());
                        return;
                    }
                    let new_path = dir.join(prompt.input.trim());
                    if let Some(doc) = self.active_document_mut() {
                        doc.filepath = Some(new_path.clone());
                    }
                    self.save_file();
                    if self.lsp_client.is_none() {
                        self.setup_lsp_for_current_file();
                    }
                }
                PromptIntent::Rename(old_path) => {
                    if prompt.input.trim().is_empty() { return; }
                    let new_path = old_path.with_file_name(prompt.input.trim());
                    if std::fs::rename(&old_path, &new_path).is_ok() {
                        self.status_msg = Some("Renombrado exitosamente".into());
                        let _ = self.explorer.reload();
                        if let Some(doc) = self.active_document_mut() {
                            if doc.filepath.as_ref() == Some(&old_path) {
                                doc.filepath = Some(new_path);
                            }
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

                            let mut to_close = None;
                            if let Some(doc) = self.get_active_document() {
                                if doc.filepath.as_ref() == Some(&path) {
                                    if let Focus::Window(w_id) = self.focus { to_close = Some(w_id); }
                                }
                            }
                            if to_close.is_some() { self.close_active_window(); }
                        } else {
                            self.status_msg = Some("Error al eliminar".into());
                        }
                    }
                }
                PromptIntent::SearchText => {
                    if let Some(idx) = prompt.selection_state.selected() {
                        if let Some(m) = prompt.text_results.get(idx).cloned() {
                            self.load_file(m.path);

                            let input = prompt.input.clone();
                            let mut search_results = Vec::new();
                            let mut current_idx = 0;

                            if let Some(doc) = self.active_document_mut() {
                                let max_len = doc.buffer.text.len_chars();
                                let target_char = doc.buffer.text.line_to_char(m.line_idx) + m.char_offset;
                                doc.buffer.cursor_char_idx = target_char.min(max_len);

                                let query_len = input.chars().count();
                                doc.buffer.selection_anchor = Some((doc.buffer.cursor_char_idx + query_len).min(max_len));

                                search_results = doc.buffer.find_text(&input);
                                current_idx = search_results.iter().position(|&p| p == doc.buffer.cursor_char_idx).unwrap_or(0);
                            }

                            self.last_search_query = Some(input);
                            self.text_search_results = search_results;
                            self.current_search_idx = current_idx;

                            self.show_tree = false;
                        }
                    }
                }
                PromptIntent::SearchFile => {
                    if let Some(idx) = prompt.selection_state.selected() {
                        if let Some(path) = prompt.file_results.get(idx).cloned() {
                            self.load_file(path);
                        }
                    }
                }
                PromptIntent::ConfirmQuit => {
                    if prompt.input.trim().eq_ignore_ascii_case("y") {
                        self.quit = true;
                    }
                }
            }

            let active_path = self.get_active_document().and_then(|d| d.filepath.clone());
            self.git_ctx = crate::git::GitContext::refresh(&self.working_dir, active_path.as_deref());
        }
    }

    pub fn trigger_completion(&mut self) {
        let req_data = self.get_active_document().and_then(|doc| {
            doc.uri.as_ref().map(|uri| {
                let (line, col) = doc.buffer.get_lsp_position();
                (uri.clone(), line, col)
            })
        });

        if let Some((uri, line, col)) = req_data {
            if let Some(client) = &mut self.lsp_client {
                self.pending_completion_id = Some(client.request_completion(uri, line, col));
                self.completions.clear();
            }
        }
    }

    pub fn jump_to_search_result(&mut self) {
        if self.text_search_results.is_empty() { return; }

        let char_idx = self.text_search_results[self.current_search_idx];
        let query_len = self.last_search_query.as_ref().map(|q| q.chars().count()).unwrap_or(0);

        if let Some(doc) = self.active_document_mut() {
            let max_len = doc.buffer.text.len_chars();
            if char_idx >= max_len { return; }

            doc.buffer.cursor_char_idx = char_idx;
            if query_len > 0 {
                let end_idx = (char_idx + query_len).min(max_len);
                doc.buffer.selection_anchor = Some(end_idx);
            }
        }
        self.show_tree = false;
        self.status_msg = Some(format!("Coincidencia {}/{}", self.current_search_idx + 1, self.text_search_results.len()));
    }

    pub fn nex_search_result(&mut self) {
        if !self.text_search_results.is_empty() {
            self.current_search_idx = (self.current_search_idx + 1) % self.text_search_results.len();
            self.jump_to_search_result();
        }
    }

    pub fn open_prompt(&mut self, intent: PromptIntent) {
        let mut prompt = PromptState::new(intent);
        if let PromptIntent::Rename(ref path) = prompt.intent {
            if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                prompt.input = name.to_string();
            }
        }
        self.prompt = Some(prompt);
        self.update_prompt_search();
    }

    pub fn update_prompt_search(&mut self) {
        if let Some(prompt) = &mut self.prompt {
            let query = prompt.input.clone();
            match prompt.intent {
                PromptIntent::SearchFile => {
                    prompt.file_results = find_files_in_project(&self.working_dir, &query);
                    prompt.selection_state.select(if !prompt.file_results.is_empty() { Some(0) } else { None });
                }
                PromptIntent::SearchText => {
                    prompt.text_results = find_text_in_project(&self.working_dir, &query);
                    prompt.selection_state.select(if !prompt.text_results.is_empty() { Some(0) } else { None });
                }
                _ => {}
            }
        }
    }

    pub fn notify_lsp_incremental(&mut self, _start_char: usize, _end_char: usize, _inserted_text: &str) {
        let doc_data = if let Some(doc) = self.active_document_mut() {
            doc.is_dirty = true;
            doc.uri.as_ref().map(|uri| {
                doc.version += 1;
                (uri.clone(), doc.version, doc.buffer.get_full_text())
            })
        } else { None };

        if let Some((uri, version, text)) = doc_data {
            if let Some(client) = &mut self.lsp_client {
                if client.is_initialized {
                    client.did_change(uri, text, version);
                }
            }
        }
    }
}