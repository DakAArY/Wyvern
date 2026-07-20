use std::collections::HashMap;
use std::path::Path;
use std::process::Command;

#[derive(Clone, Copy, PartialEq)]
pub enum GitLineStatus {
    Added,
    Modified,
    Deleted,
}

#[derive(Default, Clone)]
pub struct GitContext {
    pub branch: Option<String>,
    pub is_repo: bool,
    // Mapeo 0-indexed de la línea a su estado en git
    pub line_statuses: HashMap<usize, GitLineStatus>,
    // Estadísticas: (Añadidas, Modificadas, Eliminadas)
    pub stats: (usize, usize, usize),
}

impl GitContext {
    /// Ingesta el estado de git. Ejecuta el diff contra HEAD solo si hay un archivo activo.
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

/// Parsea el output unificado sin contexto (-U0) de git diff
fn parse_git_diff_u0(diff: &str) -> (HashMap<usize, GitLineStatus>, (usize, usize, usize)) {
    let mut statuses = HashMap::new();
    let (mut adds, mut mods, mut dels) = (0, 0, 0);

    for line in diff.lines() {
        if line.starts_with("@@ ") {
            // Formato esperado: @@ -R,r +A,a @@
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() < 3 { continue; }
            
            let old_file_part = parts[1]; // -R,r
            let new_file_part = parts[2]; // +A,a
            
            if !new_file_part.starts_with('+') || !old_file_part.starts_with('-') { continue; }
            
            // Extracción de inicio y cantidad (nueva versión)
            let a_part = &new_file_part[1..];
            let (start_str, count_str) = if let Some(idx) = a_part.find(',') {
                (&a_part[..idx], &a_part[idx + 1..])
            } else {
                (a_part, "1")
            };
            let start: usize = start_str.parse().unwrap_or(1);
            let count: usize = count_str.parse().unwrap_or(1);

            // Extracción de cantidad (versión vieja) para determinar el tipo de diff
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
