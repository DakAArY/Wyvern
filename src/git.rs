use std::collections::HashMap;
use std::path::Path;
use std::process::Command;

/// Tipo de cambio detectado en una línea respecto a `HEAD`, usado para
/// pintar el indicador de git en el margen del editor.
#[derive(Clone, Copy, PartialEq)]
pub enum GitLineStatus {
    Added,
    Modified,
    Deleted,
}

/// Snapshot del estado de git relevante para la sesión de edición actual:
/// rama activa y, si hay un archivo abierto, el diff línea por línea de
/// ese archivo contra `HEAD`.
#[derive(Default, Clone)]
pub struct GitContext {
    pub branch: Option<String>,
    pub is_repo: bool,
    /// Estado por línea (0-indexado) respecto a `HEAD`.
    pub line_statuses: HashMap<usize, GitLineStatus>,
    /// Conteo total de líneas (añadidas, modificadas, eliminadas) del archivo actual.
    pub stats: (usize, usize, usize),
}

impl GitContext {
    /// Recalcula el estado de git para `workspace_root`. Primero comprueba
    /// si el directorio es un repositorio y obtiene la rama activa; si
    /// además se pasa `current_file`, corre `git diff -U0` contra `HEAD`
    /// para ese archivo específico y lo parsea a estados por línea.
    /// Cualquier fallo al invocar `git` (binario ausente, no es un repo,
    /// etc.) deja el contexto en su estado por defecto.
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

/// Parsea la salida de `git diff -U0` (formato unificado sin líneas de
/// contexto) y la reduce a dos cosas: un mapa de línea -> tipo de cambio,
/// y el conteo total de líneas añadidas/modificadas/eliminadas.
///
/// Solo se procesan los encabezados de hunk (`@@ -R,r +A,a @@`), que ya
/// traen toda la información necesaria sin tener que inspeccionar el
/// contenido línea por línea del diff:
/// - Si el lado viejo tiene 0 líneas y el nuevo tiene `count` > 0 → adición pura.
/// - Si el lado nuevo tiene 0 líneas → eliminación pura (se marca solo el
///   punto de inserción, ya que las líneas borradas no existen en el archivo nuevo).
/// - En cualquier otro caso, se trata como modificación.
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
