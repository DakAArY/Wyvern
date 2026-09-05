use ratatui::widgets::ListState;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::io::{BufRead, BufReader};
use std::fs::File;
use fuzzy_matcher::FuzzyMatcher;
use fuzzy_matcher::skim::SkimMatcherV2;


/// Elemento visible del explorador, ya sea archivo, directorio o padre.
pub struct ExplorerEntry {
    pub path: PathBuf,
    pub name: String,
    pub is_dir: bool,
}

/// Estado del panel de archivos y de su selección navegable.
pub struct FileExplorer {
    pub current_dir: PathBuf,
    pub entries: Vec<ExplorerEntry>,
    pub state: ListState,
}

#[derive(Clone, Debug)]
pub struct WorkspaceTextMatch {
    pub path: PathBuf,
    pub line_idx: usize,
    pub char_offset: usize,
    pub line_preview: String,
}

impl FileExplorer {
    /// Crea el panel y carga inmediatamente el directorio indicado.
    pub fn new(path: PathBuf) -> Self {
        let mut explorer = Self {
            current_dir: path,
            entries: Vec::new(),
            state: ListState::default(),
        };
        let _ = explorer.reload();
        explorer
    }

    /// Sincroniza la lista con el sistema de archivos y restablece la selección.
    /// Los directorios aparecen antes que los archivos, ambos en orden alfabético.
    pub fn reload(&mut self) -> io::Result<()> {
        self.entries.clear();

        if let Some(parent) = self.current_dir.parent() {
            self.entries.push(ExplorerEntry { 
                path: parent.to_path_buf(),
                name: "..".to_string(),
                is_dir: true,
            });

        }

        let mut dirs = Vec::new();
        let mut files = Vec::new();

        if let Ok(read_dir) = fs::read_dir(&self.current_dir) {
            for entry_result in read_dir {
                let entry = entry_result?;
                let path = entry.path();
                let name = entry.file_name().to_string_lossy().into_owned();
                let is_dir = entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false);

                let exp_entry = ExplorerEntry { path, name , is_dir };
                if is_dir {
                    dirs.push(exp_entry);
                } else {
                    files.push(exp_entry);
                }
            }
        }

        dirs.sort_by(|a, b| a.name.cmp(&b.name));
        files.sort_by(|a, b| a.name.cmp(&b.name));

        self.entries.extend(dirs);
        self.entries.extend(files);

        if !self.entries.is_empty() {
            self.state.select(Some(0));
        } else {
            self.state.select(None);
        }

        Ok(())
    }

    /// Avanza la selección sin superar la última entrada disponible.
    pub fn next(&mut self) {
        let i = match self.state.selected() {
            Some(i) => {
                if i >= self.entries.len().saturating_sub(1) {
                    self.entries.len().saturating_sub(1)
                } else {
                    i + 1
                }
            }
            None => 0,
        };
        self.state.select(Some(i));
    }

    /// Retrocede la selección sin salir del comienzo de la lista.
    pub fn previous(&mut self) {
        let i = match self.state.selected() {
            Some(i) => i.saturating_sub(1),
            None => 0,
        };
        self.state.select(Some(i));
    }

    /// Obtiene la entrada seleccionada, si el índice sigue siendo válido.
    pub fn get_selected(&self) -> Option<&ExplorerEntry> {
        self.state.selected().and_then(|i| self.entries.get(i))
    }
}

pub fn find_files_in_project(root: &Path, query: &str) -> Vec<PathBuf> {
    let results = Vec::new();
    if query.is_empty() { return results; }

    let matcher = SkimMatcherV2::default();

    let mut dirs = vec![root.to_path_buf()];
    let max_results = 50;

    let mut scored_results: Vec<(PathBuf, i64)> = Vec::new();

    while let Some(dir) = dirs.pop() {
        if scored_results.len() >= max_results * 5 { break; }

        if let Ok(entries) = fs::read_dir(&dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                let name = entry.file_name().to_string_lossy().into_owned();

                if name.starts_with('.') || name == "target" || name == "node_modules" || name == "dist" || name == "build" {
                    continue;
                }
                if path.is_dir() {
                    dirs.push(path);
                } else {
                    let rel_path = path.strip_prefix(root).unwrap_or(&path).to_string_lossy();

                    if let Some(score) = matcher.fuzzy_match(&rel_path, query) {
                        scored_results.push((path.clone(), score));
                    }
                }
            }
        }
    }
    scored_results.sort_unstable_by(|a, b| b.1.cmp(&a.1));
    scored_results.into_iter().take(max_results).map(|(p, _)| p).collect()
}

pub fn find_text_in_project(root: &Path, query: &str) -> Vec<WorkspaceTextMatch> {
    let results = Vec::new();
    if query.is_empty() { return results; }

    let matcher = SkimMatcherV2::default();
    let mut dirs = vec![root.to_path_buf()];
    let max_results = 100;

    let mut scored_results: Vec<(WorkspaceTextMatch, i64)> = Vec::new();
    let mut line_buf = String::new();

    while let Some(dir) = dirs.pop() {
        if scored_results.len() >= max_results * 5 { break; }

        if let Ok(entries) = fs::read_dir(&dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                let name = entry.file_name().to_string_lossy().into_owned();

                if name.starts_with('.') || matches!(name.as_str(), "target" | "node_modules" | "dist" | "build") {
                    continue;
                }

                if path.is_dir() {
                    dirs.push(path);
                } else {
                    if let Ok(file) = File::open(&path) {
                        let mut reader = BufReader::new(file);
                        let mut line_idx = 0;

                        line_buf.clear();
                        while let Ok(len) = reader.read_line(&mut line_buf) {
                            if len == 0 { break; }

                            if let Some((score, indices)) = matcher.fuzzy_indices(&line_buf, query) {
                                // fuzzy-matcher devuelve ÍNDICES DE CARÁCTER (char indices), no de bytes.
                                // Esto representa directamente la coordenada de columna visual (char_offset)
                                // requerida por el motor de renderizado del Rope.
                                let char_offset = indices.first().copied().unwrap_or(0);

                                scored_results.push((
                                    WorkspaceTextMatch {
                                        path: path.clone(),
                                        line_idx,
                                        char_offset,
                                        line_preview: line_buf.trim().to_string(),
                                    },
                                    score
                                ));

                                if scored_results.len() >= max_results * 5 { break; }
                            }
                            line_idx += 1;
                            line_buf.clear();
                        }
                    }
                }
            }
        }
    }

    scored_results.sort_unstable_by(|a, b| b.1.cmp(&a.1));
    scored_results.into_iter().take(max_results).map(|(m, _)| m).collect()
}
