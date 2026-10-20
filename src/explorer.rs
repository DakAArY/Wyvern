use ratatui::widgets::ListState;
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use fuzzy_matcher::FuzzyMatcher;
use fuzzy_matcher::skim::SkimMatcherV2;

/// Elemento visible del explorador, ya sea archivo, directorio o padre.
pub struct ExplorerEntry {
    pub path: PathBuf,
    pub name: String,
    pub is_dir: bool,
    /// Nivel de anidamiento bajo la raíz, para la sangría del árbol.
    pub depth: usize,
}

/// Estado del panel de archivos y de su selección navegable.
pub struct FileExplorer {
    /// Raíz del árbol.
    pub current_dir: PathBuf,
    /// Directorios desplegados; sobreviven a recargas y a cambiar la raíz.
    pub expanded: HashSet<PathBuf>,
    /// Árbol aplanado en el orden en que se dibuja.
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

const IGNORED_DIRS: [&str; 4] = ["target", "node_modules", "dist", "build"];
const MAX_WALK_FILES: usize = 50_000;
const MAX_SEARCH_FILE_BYTES: u64 = 2 * 1024 * 1024;

impl FileExplorer {
    /// Crea el panel y carga inmediatamente el directorio indicado.
    pub fn new(path: PathBuf) -> Self {
        // Una ruta relativa como "." no tiene padre navegable.
        let current_dir = fs::canonicalize(&path).unwrap_or(path);
        let mut explorer = Self { current_dir, expanded: HashSet::new(), entries: Vec::new(), state: ListState::default() };
        explorer.reload();
        explorer
    }

    /// Sincroniza el árbol con el sistema de archivos conservando la selección por ruta.
    pub fn reload(&mut self) {
        let selected_path = self.get_selected().map(|e| e.path.clone());
        self.entries.clear();

        if let Some(parent) = self.current_dir.parent() {
            self.entries.push(ExplorerEntry { path: parent.to_path_buf(), name: "..".to_string(), is_dir: true, depth: 0 });
        }
        let root = self.current_dir.clone();
        self.push_children(&root, 0);

        let idx = selected_path
            .and_then(|p| self.entries.iter().position(|e| e.path == p))
            .unwrap_or(0);
        self.state.select(if self.entries.is_empty() { None } else { Some(idx) });
    }

    /// Añade `dir` y, recursivamente, sus subdirectorios desplegados: directorios
    /// primero y ambos grupos en orden alfabético. Sigue symlinks a propósito; un
    /// ciclo no es infinito porque cada nivel exige desplegar una ruta nueva.
    fn push_children(&mut self, dir: &Path, depth: usize) {
        let mut children: Vec<ExplorerEntry> = fs::read_dir(dir).into_iter().flatten().flatten()
            .map(|e| {
                let path = e.path();
                ExplorerEntry { is_dir: path.is_dir(), name: e.file_name().to_string_lossy().into_owned(), path, depth }
            })
            .collect();
        children.sort_by_cached_key(|e| (!e.is_dir, e.name.to_lowercase()));
        for child in children {
            let path = (child.is_dir && self.expanded.contains(&child.path)).then(|| child.path.clone());
            self.entries.push(child);
            if let Some(path) = path { self.push_children(&path, depth + 1); }
        }
    }

    /// Enter sobre un directorio: lo despliega o pliega; sobre "..", sube la raíz un nivel.
    pub fn toggle_selected(&mut self) {
        let Some(e) = self.get_selected() else { return };
        let (path, is_parent) = (e.path.clone(), e.name == "..");
        if is_parent {
            let old_root = std::mem::replace(&mut self.current_dir, path);
            self.expanded.insert(old_root.clone());
            self.reload();
            self.select_path(&old_root);
        } else if e.is_dir {
            if !self.expanded.remove(&path) { self.expanded.insert(path); }
            self.reload();
        }
    }

    /// →: despliega el directorio seleccionado.
    pub fn expand(&mut self) {
        let Some(path) = self.get_selected().filter(|e| e.is_dir && e.name != "..").map(|e| e.path.clone()) else { return };
        if self.expanded.insert(path) { self.reload(); }
    }

    /// ←: pliega el directorio seleccionado o, si ya lo está, salta a su padre.
    pub fn collapse(&mut self) {
        let Some(e) = self.get_selected() else { return };
        let (path, depth) = (e.path.clone(), e.depth);
        if self.expanded.remove(&path) {
            self.reload();
        } else if depth > 0 && let Some(parent) = path.parent() {
            self.select_path(parent);
        }
    }

    fn select_path(&mut self, path: &Path) {
        if let Some(i) = self.entries.iter().position(|e| e.path == path) { self.state.select(Some(i)); }
    }

    /// Avanza la selección sin superar la última entrada disponible.
    pub fn next(&mut self) {
        let last = self.entries.len().saturating_sub(1);
        let i = self.state.selected().map_or(0, |i| (i + 1).min(last));
        self.state.select(Some(i));
    }

    /// Retrocede la selección sin salir del comienzo de la lista.
    pub fn previous(&mut self) {
        let i = self.state.selected().map_or(0, |i| i.saturating_sub(1));
        self.state.select(Some(i));
    }

    /// Obtiene la entrada seleccionada, si el índice sigue siendo válido.
    pub fn get_selected(&self) -> Option<&ExplorerEntry> {
        self.state.selected().and_then(|i| self.entries.get(i))
    }
}

/// Archivos del proyecto. En un repo usa git para respetar `.gitignore`;
/// si no, recorre el árbol sin seguir symlinks (evita ciclos infinitos).
pub fn list_project_files(root: &Path) -> Vec<PathBuf> {
    let git = Command::new("git")
        .args(["-C"]).arg(root)
        .args(["ls-files", "-z", "--cached", "--others", "--exclude-standard"])
        .output();
    if let Ok(out) = git
        && out.status.success() {
            return out.stdout
                .split(|&b| b == 0)
                .filter(|p| !p.is_empty())
                .map(|p| root.join(String::from_utf8_lossy(p).as_ref()))
                .filter(|p| p.is_file())
                .collect();
        }

    let mut files = Vec::new();
    let mut dirs = vec![root.to_path_buf()];
    while let Some(dir) = dirs.pop() {
        let Ok(entries) = fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with('.') || IGNORED_DIRS.contains(&name.as_ref()) { continue; }
            let Ok(ft) = entry.file_type() else { continue };
            if ft.is_dir() {
                dirs.push(entry.path());
            } else if ft.is_file() {
                files.push(entry.path());
                if files.len() >= MAX_WALK_FILES { return files; }
            }
        }
    }
    files
}

/// Mejores coincidencias difusas sobre la ruta relativa a `root`.
pub fn fuzzy_filter_files(root: &Path, files: &[PathBuf], query: &str) -> Vec<PathBuf> {
    if query.is_empty() { return Vec::new(); }
    let matcher = SkimMatcherV2::default();
    let mut scored: Vec<(i64, &PathBuf)> = files.iter()
        .filter_map(|p| {
            let rel = p.strip_prefix(root).unwrap_or(p).to_string_lossy();
            matcher.fuzzy_match(&rel, query).map(|s| (s, p))
        })
        .collect();
    scored.sort_unstable_by_key(|(s, _)| std::cmp::Reverse(*s));
    scored.into_iter().take(50).map(|(_, p)| p.clone()).collect()
}

/// Búsqueda literal; distingue mayúsculas solo si la consulta las contiene.
/// `cancelled` se consulta por archivo para abandonar búsquedas obsoletas.
pub fn search_text(files: &[PathBuf], query: &str, max_results: usize, cancelled: &dyn Fn() -> bool) -> Vec<WorkspaceTextMatch> {
    let mut results = Vec::new();
    if query.is_empty() { return results; }
    let case_sensitive = query.chars().any(char::is_uppercase);
    let needle = if case_sensitive { query.to_string() } else { query.to_lowercase() };

    for path in files {
        if cancelled() { return Vec::new(); }
        if fs::metadata(path).map_or(true, |m| m.len() > MAX_SEARCH_FILE_BYTES) { continue; }
        let Ok(bytes) = fs::read(path) else { continue };
        if bytes[..bytes.len().min(8192)].contains(&0) { continue; }
        let Ok(content) = String::from_utf8(bytes) else { continue };

        for (line_idx, line) in content.lines().enumerate() {
            // shortcut: el offset se mide sobre la línea en minúsculas; difiere solo con
            // caracteres cuyo lowercase cambia de longitud (p. ej. 'İ'), poco frecuente en código.
            let hay = if case_sensitive { line.to_string() } else { line.to_lowercase() };
            if let Some(byte) = hay.find(&needle) {
                results.push(WorkspaceTextMatch {
                    path: path.clone(),
                    line_idx,
                    char_offset: hay[..byte].chars().count(),
                    line_preview: line.trim().to_string(),
                });
                if results.len() >= max_results { return results; }
            }
        }
    }
    results
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("wyvern_{}_{}", name, std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(d.join("src")).unwrap();
        fs::create_dir_all(d.join("target")).unwrap();
        fs::write(d.join("src/main.rs"), "fn main() {}\nlet Needle = 1;\n").unwrap();
        fs::write(d.join("target/main.rs"), "needle").unwrap();
        d
    }

    #[test]
    fn file_search_skips_ignored_dirs() {
        let d = tmp("ff");
        let files = list_project_files(&d);
        assert_eq!(fuzzy_filter_files(&d, &files, "main"), vec![d.join("src/main.rs")]);
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn text_search_is_literal_and_smart_case() {
        let d = tmp("ft");
        let files = list_project_files(&d);
        let r = search_text(&files, "needle", 10, &|| false);
        assert_eq!(r.len(), 1);
        assert_eq!((r[0].line_idx, r[0].char_offset, r[0].path.clone()), (1, 4, d.join("src/main.rs")));
        assert!(search_text(&files, "NEEDLE", 10, &|| false).is_empty());
        assert!(search_text(&files, "ndl", 10, &|| false).is_empty(), "no debe ser difusa");
        assert!(search_text(&files, "needle", 10, &|| true).is_empty());
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn text_search_skips_binary_files() {
        let d = tmp("bin");
        fs::write(d.join("src/blob"), b"needle\0\x01").unwrap();
        let r = search_text(&list_project_files(&d), "needle", 10, &|| false);
        assert!(r.iter().all(|m| !m.path.ends_with("blob")));
        fs::remove_dir_all(&d).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn walk_does_not_follow_symlink_cycles() {
        let d = tmp("loop");
        std::os::unix::fs::symlink(&d, d.join("src/back")).unwrap();
        assert_eq!(list_project_files(&d), vec![d.join("src/main.rs")]);
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn git_listing_respects_gitignore() {
        let d = tmp("git");
        let ok = Command::new("git").args(["init", "-q"]).current_dir(&d).status().is_ok_and(|s| s.success());
        if !ok { return; }
        fs::write(d.join(".gitignore"), "*.log\n").unwrap();
        fs::write(d.join("x.log"), "").unwrap();
        let mut files = list_project_files(&d);
        files.sort();
        assert_eq!(files, vec![d.join(".gitignore"), d.join("src/main.rs"), d.join("target/main.rs")]);
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn explorer_lists_dirs_first_and_keeps_selection() {
        let d = tmp("ex");
        fs::write(d.join("a.txt"), "").unwrap();
        let mut e = FileExplorer::new(d.clone());
        let names: Vec<_> = e.entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["..", "src", "target", "a.txt"]);
        e.state.select(Some(3));
        fs::write(d.join("0.txt"), "").unwrap();
        e.reload();
        assert_eq!(e.get_selected().unwrap().name, "a.txt");
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn tree_expands_collapses_and_climbs() {
        let d = tmp("tree");
        fs::create_dir_all(d.join("src/sub")).unwrap();
        fs::write(d.join("src/sub/x.rs"), "").unwrap();
        let mut e = FileExplorer::new(d.clone());
        let tree = |e: &FileExplorer| e.entries.iter().map(|x| format!("{}{}", " ".repeat(x.depth), x.name)).collect::<Vec<_>>();

        e.state.select(Some(1));
        e.toggle_selected();
        assert_eq!(tree(&e), ["..", "src", " sub", " main.rs", "target"]);
        e.state.select(Some(2));
        e.expand();
        e.state.select(Some(3));
        e.collapse();
        assert_eq!(e.get_selected().unwrap().name, "sub", "en un archivo, ← salta al directorio padre");
        e.collapse();
        assert_eq!(tree(&e), ["..", "src", " sub", " main.rs", "target"]);

        e.state.select(Some(0));
        e.toggle_selected();
        assert_eq!(e.current_dir, d.parent().unwrap());
        let sel = e.get_selected().unwrap();
        assert_eq!((sel.path.clone(), sel.depth), (d.clone(), 0), "la raíz anterior queda desplegada y seleccionada");
        assert!(e.entries.iter().any(|x| x.path == d.join("src/main.rs") && x.depth == 2));
        fs::remove_dir_all(&d).unwrap();
    }
}
