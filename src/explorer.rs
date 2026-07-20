use ratatui::widgets::ListState;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// Una entrada del árbol de archivos: un archivo, un directorio, o el
/// pseudo-directorio ".." que permite subir un nivel.
pub struct ExplorerEntry {
    pub path: PathBuf,
    pub name: String,
    pub is_dir: bool,
}

/// Panel lateral de exploración de archivos. Mantiene el directorio
/// actualmente listado, sus entradas ordenadas y la selección activa
/// para navegación con teclado/mouse.
pub struct FileExplorer {
    pub current_dir: PathBuf,
    pub entries: Vec<ExplorerEntry>,
    pub state: ListState,
}

impl FileExplorer {
    /// Crea el explorador apuntando a `path` y carga su contenido de inmediato.
    pub fn new(path: PathBuf) -> Self {
        let mut explorer = Self {
            current_dir: path,
            entries: Vec::new(),
            state: ListState::default(),
        };
        let _ = explorer.reload();
        explorer
    }

    /// Vuelve a leer `current_dir` desde disco y reconstruye la lista de
    /// entradas: primero ".." (si hay directorio padre), luego directorios
    /// ordenados alfabéticamente y por último archivos ordenados
    /// alfabéticamente. Selecciona la primera entrada si la lista no queda vacía.
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

    /// Mueve la selección una posición hacia abajo, sin salirse del final de la lista.
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

    /// Mueve la selección una posición hacia arriba, sin bajar de la primera entrada.
    pub fn previous(&mut self) {
        let i = match self.state.selected() {
            Some(i) => i.saturating_sub(1),
            None => 0,
        };
        self.state.select(Some(i));
    }

    /// Devuelve la entrada actualmente seleccionada, si existe.
    pub fn get_selected(&self) -> Option<&ExplorerEntry> {
        self.state.selected().and_then(|i| self.entries.get(i))
    }
}
