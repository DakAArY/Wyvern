use std::collections::HashMap;
use std::path::Path;
use std::process::Command;

#[derive(Clone, Copy, PartialEq)]
pub enum FileGitStatus {
    Modified,
    Added,
    Untracked,
    Deleted,
}

/// Estado visual de una línea comparada con la revisión `HEAD`.
#[derive(Clone, Copy, PartialEq)]
pub enum GitLineStatus {
    Added,
    Modified,
    Deleted,
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
    pub file_statuses: HashMap<std::path::PathBuf, FileGitStatus>,
}

impl GitContext {
    /// Obtiene la rama, el estado de los archivos y el diff del documento activo.
    /// Si Git no está disponible o el directorio no es un repositorio, devuelve
    /// un contexto vacío que permite continuar en modo local.
    pub fn refresh(workspace_root: &Path, current_file: Option<&Path>) -> Self {
        let root_str = workspace_root.to_string_lossy();
        
        let branch_out = Command::new("git")
            .args(["-C", &root_str, "branch", "--show-current"])
            .output()
            .ok();

        let mut ctx = Self::default();

        if let Some(out) = branch_out {
            if out.status.success() {
                ctx.is_repo = true;
                let branch = String::from_utf8_lossy(&out.stdout).trim().to_string();
                if !branch.is_empty() {
                    ctx.branch = Some(branch);
                }
            }
        }

        if !ctx.is_repo {
            return ctx;
        }
        
        if let Some(out) = Command::new("git").args(["-C", &root_str, "status", "--porcelain"]).output().ok() {
            if out.status.success() {
                let status_str = String::from_utf8_lossy(&out.stdout);
                for line in status_str.lines() {
                    if line.len() > 3 {
                        let code = &line[0..2];
                        let path_str = &line[3..];
                        let abs_path = workspace_root.join(path_str);
                        
                        let status = match code {
                            "??" => FileGitStatus::Untracked,
                            _ if code.contains('M') => FileGitStatus::Modified,
                            _ if code.contains('A') => FileGitStatus::Added,
                            _ if code.contains('D') => FileGitStatus::Deleted,
                            _ => continue
                        };
                        ctx.file_statuses.insert(abs_path, status);
                    }
                }
            }
        }

        if let Some(file) = current_file {
            let file_str = file.to_string_lossy();
            let diff_out = Command::new("git")
                .args(["-C", &root_str, "diff", "-U0", "HEAD", "--", &file_str])
                .output()
                .ok();

            if let Some(out) = diff_out {
                if out.status.success() {
                    let diff_str = String::from_utf8_lossy(&out.stdout);
                    let (statuses, stats) = parse_git_diff_u0(&diff_str);
                    ctx.line_statuses = statuses;
                    ctx.stats = stats;
                }
            }
        }

        ctx
    }
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
