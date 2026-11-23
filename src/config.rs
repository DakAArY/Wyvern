use crate::lsp::ServerConfig;
use crate::theme::Theme;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

pub const DEFAULT_THEME: &str = "wyvern-dark";

/// `config.json`: tema activo, servidores LSP e ids de lenguaje. Se genera con
/// todos los valores por defecto la primera vez para que editarlo sea trivial.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Nombre de un archivo en `themes/` sin la extensión `.json`.
    pub theme: String,
    /// Extensión → languageId que se envía en `didOpen`; si falta se usa la extensión.
    pub languages: BTreeMap<String, String>,
    pub servers: Vec<ServerConfig>,
    /// Aspecto de bordes, separadores y texto.
    pub style: UiStyle,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UiStyle {
    /// Líneas finas y bordes redondeados.
    #[default]
    Sharp,
    /// Líneas gruesas, bloques sólidos y texto en negrita.
    Heavy,
}

impl UiStyle {
    pub fn as_str(self) -> &'static str {
        match self { Self::Sharp => "sharp", Self::Heavy => "heavy" }
    }

    pub fn toggled(self) -> Self {
        match self { Self::Sharp => Self::Heavy, Self::Heavy => Self::Sharp }
    }
}

impl Default for Config {
    fn default() -> Self {
        let languages = [
            ("rs", "rust"), ("py", "python"), ("c", "c"), ("h", "cpp"), ("cpp", "cpp"), ("cxx", "cpp"), ("cc", "cpp"), ("hpp", "cpp"),
            ("html", "html"), ("htm", "html"), ("js", "javascript"), ("jsx", "javascriptreact"), ("ts", "typescript"),
            ("tsx", "typescriptreact"), ("vue", "vue"), ("css", "css"), ("scss", "scss"), ("less", "less"), ("go", "go"),
            ("lua", "lua"), ("sh", "shellscript"), ("bash", "shellscript"), ("json", "json"), ("yaml", "yaml"), ("yml", "yaml"),
            ("zig", "zig"), ("java", "java"), ("md", "markdown"),
        ].into_iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();

        let srv = |cmd: &str, args: &[&str], exts: &[&str]| ServerConfig {
            command: cmd.into(),
            args: args.iter().map(|s| s.to_string()).collect(),
            extensions: exts.iter().map(|s| s.to_string()).collect(),
            initialization_options: serde_json::Value::Null,
            settings: serde_json::Value::Null,
        };
        const WEB: &[&str] = &["js", "jsx", "ts", "tsx"];
        let mut ts = srv("typescript-language-server", &["--stdio"], &["js", "jsx", "ts", "tsx", "vue"]);
        // typescript-language-server no ofrece auto-imports sin estas preferencias.
        ts.initialization_options = json!({ "preferences": {
            "includeCompletionsForModuleExports": true,
            "includeCompletionsWithInsertText": true,
            "allowTextChangesInNewFiles": true
        }});

        let servers = vec![
            srv("rust-analyzer", &[], &["rs"]),
            srv("pyright-langserver", &["--stdio"], &["py"]),
            srv("clangd", &[], &["c", "cpp", "cxx", "cc", "h", "hpp"]),
            ts,
            srv("tailwindcss-language-server", &["--stdio"], WEB),
            srv("vue-language-server", &["--stdio"], &["vue"]),
            srv("vscode-css-language-server", &["--stdio"], &["css", "scss", "less", "vue"]),
            srv("vscode-html-language-server", &["--stdio"], &["html", "htm"]),
            srv("emmet-language-server", &["--stdio"], &["html", "htm"]),
            srv("vscode-json-language-server", &["--stdio"], &["json"]),
            srv("yaml-language-server", &["--stdio"], &["yaml", "yml"]),
            srv("gopls", &[], &["go"]),
            srv("lua-language-server", &[], &["lua"]),
            srv("bash-language-server", &["start"], &["sh", "bash"]),
            srv("zls", &[], &["zig"]),
            srv("jdtls", &[], &["java"]),
            srv("marksman", &["server"], &["md"]),
        ];

        Self { theme: DEFAULT_THEME.into(), languages, servers, style: UiStyle::default() }
    }
}

impl Config {
    pub fn language_id<'a>(&'a self, ext: &'a str) -> &'a str {
        self.languages.get(ext).map_or(ext, String::as_str)
    }

    pub fn servers_for_ext<'a>(&'a self, ext: &'a str) -> impl Iterator<Item = &'a ServerConfig> {
        self.servers.iter().filter(move |s| s.extensions.iter().any(|e| e == ext))
    }
}

/// `$XDG_CONFIG_HOME/wyvern` o `~/.config/wyvern`.
pub fn config_dir() -> Option<PathBuf> {
    std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .map(|d| d.join("wyvern"))
}

/// Carga configuración y tema desde `dir`, generando los archivos que falten.
/// Un archivo inválido nunca impide arrancar: se usan los valores por defecto y
/// se devuelve el error para mostrarlo en la barra de estado.
pub fn load(dir: &Path) -> (Config, Theme, Vec<String>) {
    let mut errors = Vec::new();
    let _ = fs::create_dir_all(dir.join("themes"));
    write_if_missing(&dir.join("config.json"), &Config::default());
    write_if_missing(&theme_path(dir, DEFAULT_THEME), &Theme::default());

    let config = load_config(dir).map_err(|e| errors.push(e)).unwrap_or_default();
    let theme = load_theme(dir, &config.theme).map_err(|e| errors.push(e)).unwrap_or_default();
    (config, theme, errors)
}

pub fn load_config(dir: &Path) -> Result<Config, String> {
    read(&dir.join("config.json"), |s| serde_json::from_str(s))
}

pub fn load_theme(dir: &Path, name: &str) -> Result<Theme, String> {
    read(&theme_path(dir, name), Theme::from_json)
}

/// Cambia una clave de config.json sin tocar el resto: reserializar `Config` perdería
/// claves que esta versión no conoce.
pub fn set_key(dir: &Path, key: &str, value: &str) -> Result<(), String> {
    let path = dir.join("config.json");
    let mut json: serde_json::Value = read(&path, |s| serde_json::from_str(s))?;
    let obj = json.as_object_mut().ok_or_else(|| format!("{}: no es un objeto JSON", path.display()))?;
    obj.insert(key.into(), value.into());
    let text = serde_json::to_string_pretty(&json).map_err(|e| e.to_string())?;
    fs::write(&path, text).map_err(|e| format!("{}: {e}", path.display()))
}

/// Solo el nombre: un "theme" con ruta no debe poder leer fuera de themes/.
pub fn theme_path(dir: &Path, name: &str) -> PathBuf {
    let name = Path::new(name).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    dir.join("themes").join(format!("{name}.json"))
}

/// Nombres de los temas disponibles, ordenados.
pub fn list_themes(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir.join("themes")).into_iter().flatten().flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "json"))
        .filter_map(|p| Some(p.file_stem()?.to_string_lossy().into_owned()))
        .collect();
    names.sort();
    names
}

fn read<T>(path: &Path, parse: impl Fn(&str) -> serde_json::Result<T>) -> Result<T, String> {
    let text = fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    parse(&text).map_err(|e| format!("{}: {e}", path.display()))
}

/// `create_new` garantiza que nunca se pisa un archivo que el usuario ya editó.
fn write_if_missing<T: Serialize>(path: &Path, value: &T) {
    if let Ok(mut f) = fs::OpenOptions::new().write(true).create_new(true).open(path)
        && let Ok(json) = serde_json::to_string_pretty(value) {
            let _ = f.write_all(json.as_bytes());
        }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("wyvern_cfg_{}_{}", tag, std::process::id()));
        let _ = fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn first_run_generates_files_and_never_overwrites() {
        let d = tmp("gen");
        let (cfg, _, errors) = load(&d);
        assert!(errors.is_empty(), "{errors:?}");
        assert!(d.join("config.json").is_file() && d.join("themes/wyvern-dark.json").is_file());
        assert_eq!(cfg.language_id("tsx"), "typescriptreact");
        assert_eq!(cfg.language_id("xyz"), "xyz");

        fs::write(d.join("config.json"), r#"{"theme": "mine", "servers": [{"command": "my-ls", "extensions": ["foo"]}]}"#).unwrap();
        fs::write(d.join("themes/mine.json"), r#"{"ui": {"accent": "red"}}"#).unwrap();
        let (cfg, theme, errors) = load(&d);
        assert!(errors.is_empty(), "{errors:?}");
        assert!(fs::read_to_string(d.join("config.json")).unwrap().contains("my-ls"));
        assert_eq!(cfg.servers_for_ext("foo").map(|s| s.command.as_str()).collect::<Vec<_>>(), ["my-ls"]);
        assert_eq!(cfg.servers_for_ext("rs").count(), 0);
        assert_eq!(cfg.language_id("rs"), "rust", "los campos omitidos toman su valor por defecto");
        assert_eq!(theme.ui.accent, ratatui::style::Color::Red);
        fs::write(d.join("themes/notes.txt"), "").unwrap();
        assert_eq!(list_themes(&d), ["mine", "wyvern-dark"]);
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn broken_files_fall_back_to_defaults_with_error() {
        let d = tmp("bad");
        fs::create_dir_all(d.join("themes")).unwrap();
        fs::write(d.join("config.json"), "{ nope").unwrap();
        let (cfg, _, errors) = load(&d);
        assert_eq!(errors.len(), 1);
        assert_eq!(cfg.theme, DEFAULT_THEME);

        fs::write(d.join("config.json"), r#"{"theme": "../../etc/missing"}"#).unwrap();
        let (_, theme, errors) = load(&d);
        assert!(errors[0].contains("themes/missing.json"), "{errors:?}");
        assert_eq!(theme.ui, crate::theme::UiColors::default());
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn set_key_keeps_unknown_keys_and_style_parses() {
        let d = tmp("setkey");
        fs::create_dir_all(&d).unwrap();
        fs::write(d.join("config.json"), r#"{"theme": "a", "futuro": 1}"#).unwrap();
        set_key(&d, "style", "heavy").unwrap();
        let text = fs::read_to_string(d.join("config.json")).unwrap();
        assert!(text.contains("futuro"), "{text}");
        let cfg = load_config(&d).unwrap();
        assert_eq!((cfg.theme.as_str(), cfg.style), ("a", UiStyle::Heavy));
        assert_eq!(Config::default().style, UiStyle::Sharp);

        fs::write(d.join("config.json"), "[]").unwrap();
        assert!(set_key(&d, "style", "heavy").is_err());
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn default_servers_cover_previous_routing() {
        let c = Config::default();
        let names = |ext| c.servers_for_ext(ext).map(|s| s.command.clone()).collect::<Vec<_>>();
        assert_eq!(names("vue"), ["typescript-language-server", "vue-language-server", "vscode-css-language-server"]);
        assert_eq!(names("hpp"), ["clangd"]);
        assert_eq!(names("html"), ["vscode-html-language-server", "emmet-language-server"]);
    }
}
