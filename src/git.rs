use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Estado visual de una línea comparada con la revisión `HEAD`.
#[derive(Clone, Copy, PartialEq)]
pub enum GitLineStatus {
    Added,
    Modified,
    Deleted,
}

/// Estado de un archivo en el árbol de trabajo. El orden define la prioridad
/// con la que un directorio hereda el estado de sus hijos.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum FileStatus {
    Deleted,
    Untracked,
    Added,
    Renamed,
    Modified,
    Conflict,
}

impl FileStatus {
    pub fn letter(self) -> &'static str {
        match self {
            Self::Deleted => "D",
            Self::Untracked => "U",
            Self::Added => "A",
            Self::Renamed => "R",
            Self::Modified => "M",
            Self::Conflict => "!",
        }
    }
}

/// Información de Git que la interfaz necesita para el workspace actual.
#[derive(Default, Clone)]
pub struct GitContext {
    pub branch: Option<String>,
    pub is_repo: bool,
    /// Estado por línea, indexado desde cero, frente a `HEAD`.
    pub line_statuses: HashMap<usize, GitLineStatus>,
    /// Totales de líneas añadidas, modificadas y eliminadas.
    pub stats: (usize, usize, usize),
    /// Estado por ruta absoluta; los directorios llevan el más grave de su contenido.
    pub file_statuses: HashMap<PathBuf, FileStatus>,
    pub untracked_dirs: Vec<PathBuf>,
}

impl GitContext {
    /// Obtiene la rama y el diff del documento activo.
    /// Si Git no está disponible o el directorio no es un repositorio, devuelve
    /// un contexto vacío que permite continuar en modo local.
    pub fn refresh(workspace_root: &Path, current_file: Option<&Path>) -> Self {
        let root_str = workspace_root.to_string_lossy();
        
        let mut ctx = Self::default();

        if let Ok(out) = Command::new("git").args(["-C", &root_str, "branch", "--show-current"]).output()
            && out.status.success() {
                ctx.is_repo = true;
                let branch = String::from_utf8_lossy(&out.stdout).trim().to_string();
                if !branch.is_empty() {
                    ctx.branch = Some(branch);
                }
            }

        if !ctx.is_repo {
            return ctx;
        }

        // `status` da rutas relativas a la raíz del repo, no a `workspace_root`.
        let top = Command::new("git").args(["-C", &root_str, "rev-parse", "--show-toplevel"]).output();
        let status = Command::new("git").args(["-C", &root_str, "status", "--porcelain=v1", "-z"]).output();
        if let (Ok(top), Ok(status)) = (top, status)
            && top.status.success() && status.status.success() {
                let top = PathBuf::from(String::from_utf8_lossy(&top.stdout).trim_end());
                (ctx.file_statuses, ctx.untracked_dirs) = parse_status_z(&top, &status.stdout);
            }
        
        if let Some(file) = current_file {
            let file_str = file.to_string_lossy();
            let diff_out = Command::new("git")
                .args(["-C", &root_str, "diff", "-U0", "HEAD", "--", &file_str])
                .output();

            if let Ok(out) = diff_out
                && out.status.success() {
                    let diff_str = String::from_utf8_lossy(&out.stdout);
                    let (statuses, stats) = parse_git_diff_u0(&diff_str);
                    ctx.line_statuses = statuses;
                    ctx.stats = stats;
                }
        }

        ctx
    }
}

impl GitContext {
    /// Estado de `path`; dentro de un directorio sin seguimiento todo es `Untracked`
    /// porque git solo lista el directorio, no sus archivos.
    pub fn file_status(&self, path: &Path) -> Option<FileStatus> {
        self.file_statuses.get(path).copied()
            .or_else(|| self.untracked_dirs.iter().any(|d| path.starts_with(d)).then_some(FileStatus::Untracked))
    }
}

/// Interpreta `git status --porcelain=v1 -z`: devuelve el estado por ruta, con cada
/// directorio ancestro (bajo `top`) llevando el más grave de su contenido, y los
/// directorios que git lista enteros como sin seguimiento.
fn parse_status_z(top: &Path, out: &[u8]) -> (HashMap<PathBuf, FileStatus>, Vec<PathBuf>) {
    let mut map: HashMap<PathBuf, FileStatus> = HashMap::new();
    let mut untracked_dirs = Vec::new();
    let mut entries = out.split(|&b| b == 0);
    while let Some(e) = entries.next() {
        if e.len() < 4 { continue; }
        let (x, y) = (e[0], e[1]);
        let status = match (x, y) {
            (b'?', b'?') => FileStatus::Untracked,
            (b'U', _) | (_, b'U') | (b'A', b'A') | (b'D', b'D') => FileStatus::Conflict,
            (b'M', _) | (_, b'M') => FileStatus::Modified,
            (b'R' | b'C', _) => FileStatus::Renamed,
            (b'A', _) => FileStatus::Added,
            (b'D', _) | (_, b'D') => FileStatus::Deleted,
            _ => continue,
        };
        // Renombrados y copias llevan la ruta de origen como entrada extra.
        if matches!(x, b'R' | b'C') { entries.next(); }
        let rel = String::from_utf8_lossy(&e[3..]);
        let path = top.join(rel.trim_end_matches('/'));
        if rel.ends_with('/') { untracked_dirs.push(path.clone()); }
        for p in path.ancestors().take_while(|p| p.starts_with(top) && *p != top) {
            let s = map.entry(p.to_path_buf()).or_insert(status);
            *s = (*s).max(status);
        }
    }
    (map, untracked_dirs)
}

/// Reduce un diff sin contexto a estados por línea y totales de cambios.
///
/// Los encabezados de cada hunk contienen los rangos antiguo y nuevo, por lo
/// que no es necesario inspeccionar el contenido del diff para clasificarlo.
fn parse_git_diff_u0(diff: &str) -> (HashMap<usize, GitLineStatus>, (usize, usize, usize)) {
    let mut statuses = HashMap::new();
    let (mut adds, mut mods, mut dels) = (0, 0, 0);

    for line in diff.lines() {
        if line.starts_with("@@ ") {
            // Cada hunk comienza con los rangos de la versión antigua y nueva.
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() < 3 { continue; }
            
            let old_file_part = parts[1];
            let new_file_part = parts[2];
            
            if !new_file_part.starts_with('+') || !old_file_part.starts_with('-') { continue; }
            
            // El rango nuevo determina dónde pintar y cuántas líneas abarca.
            let a_part = &new_file_part[1..];
            let (start_str, count_str) = if let Some(idx) = a_part.find(',') {
                (&a_part[..idx], &a_part[idx + 1..])
            } else {
                (a_part, "1")
            };
            let start: usize = start_str.parse().unwrap_or(1);
            let count: usize = count_str.parse().unwrap_or(1);

            // El tamaño antiguo permite distinguir adiciones, borrados y cambios.
            let r_part = &old_file_part[1..];
            let count_old_str = if let Some(idx) = r_part.find(',') {
                &r_part[idx + 1..]
            } else {
                "1"
            };
            let count_old: usize = count_old_str.parse().unwrap_or(1);

            if count_old == 0 && count > 0 {
                adds += count;
                for i in 0..count {
                    statuses.insert(start.saturating_sub(1) + i, GitLineStatus::Added);
                }
            } else if count == 0 && count_old > 0 {
                dels += count_old;
                statuses.insert(start.saturating_sub(1), GitLineStatus::Deleted);
            } else {
                mods += count;
                for i in 0..count {
                    statuses.insert(start.saturating_sub(1) + i, GitLineStatus::Modified);
                }
            }
        }
    }

    (statuses, (adds, mods, dels))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diff_u0_classifies_hunks() {
        let diff = "@@ -0,0 +1,2 @@\n+a\n+b\n@@ -5,2 +6,2 @@\n-x\n-y\n+X\n+Y\n@@ -10,3 +9,0 @@\n-p\n-q\n-r\n";
        let (st, stats) = parse_git_diff_u0(diff);
        assert_eq!(stats, (2, 2, 3));
        assert!(st.get(&0) == Some(&GitLineStatus::Added) && st.get(&1) == Some(&GitLineStatus::Added));
        assert!(st.get(&5) == Some(&GitLineStatus::Modified) && st.get(&6) == Some(&GitLineStatus::Modified));
        assert!(st.get(&8) == Some(&GitLineStatus::Deleted));
    }

    #[test]
    fn status_z_marks_files_and_propagates_to_dirs() {
        let top = Path::new("/r");
        let out = b" M src/a.rs\0?? src/new.rs\0R  b.rs\0old.rs\0?? tmp/\0UU c.rs\0A  src/x/y.rs\0";
        let (m, u) = parse_status_z(top, out);
        let ctx = GitContext { file_statuses: m, untracked_dirs: u, ..Default::default() };
        let st = |p: &str| ctx.file_status(Path::new(p));
        assert_eq!(st("/r/src/a.rs"), Some(FileStatus::Modified));
        assert_eq!(st("/r/src/new.rs"), Some(FileStatus::Untracked));
        assert_eq!(st("/r/b.rs"), Some(FileStatus::Renamed));
        assert_eq!(st("/r/old.rs"), None, "la ruta de origen no es una entrada");
        assert_eq!(st("/r/c.rs"), Some(FileStatus::Conflict));
        assert_eq!(st("/r/src"), Some(FileStatus::Modified), "el directorio hereda el más grave");
        assert_eq!(st("/r/src/x"), Some(FileStatus::Added));
        assert_eq!(st("/r/tmp/deep/f.txt"), Some(FileStatus::Untracked));
        assert_eq!(st("/r/src/clean.rs"), None);
        assert_eq!(st("/r"), None);
    }

    #[test]
    fn refresh_reads_real_repo_status() {
        let d = std::env::temp_dir().join(format!("wyvern_gitst_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("sub")).unwrap();
        let git = |args: &[&str]| Command::new("git").args(args).current_dir(&d).output().is_ok_and(|o| o.status.success());
        if !git(&["init", "-q"]) { return; }
        std::fs::write(d.join("a.txt"), "1").unwrap();
        std::fs::write(d.join("b.txt"), "1").unwrap();
        assert!(git(&["add", "."]) && git(&["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qm", "i"]));
        std::fs::write(d.join("a.txt"), "2").unwrap();
        std::fs::write(d.join("sub/n.txt"), "").unwrap();
        std::fs::write(d.join("staged.txt"), "").unwrap();
        assert!(git(&["add", "staged.txt"]));

        let d = std::fs::canonicalize(&d).unwrap();
        let ctx = GitContext::refresh(&d, None);
        assert_eq!(ctx.file_status(&d.join("a.txt")), Some(FileStatus::Modified));
        assert_eq!(ctx.file_status(&d.join("b.txt")), None);
        assert_eq!(ctx.file_status(&d.join("sub")), Some(FileStatus::Untracked));
        assert_eq!(ctx.file_status(&d.join("sub/n.txt")), Some(FileStatus::Untracked));
        assert_eq!(ctx.file_status(&d.join("staged.txt")), Some(FileStatus::Added));
        std::fs::remove_dir_all(&d).unwrap();
    }
}
