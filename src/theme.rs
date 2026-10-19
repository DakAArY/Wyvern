use ratatui::style::{Color, Modifier, Style};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::str::FromStr;
use syntect::highlighting as sh;

/// Marca "sin color propio" en el tema de syntect: alfa 0 nunca sale de un color del usuario.
const UNSET: sh::Color = sh::Color { r: 0, g: 0, b: 0, a: 0 };

/// Paleta ANSI de referencia: sirve para aproximar truecolor en terminales de 16
/// colores y para dar RGB a los colores con nombre usados en la sintaxis.
const ANSI16: [(u8, u8, u8, Color); 16] = [
    (0, 0, 0, Color::Black), (170, 0, 0, Color::Red), (0, 170, 0, Color::Green), (170, 85, 0, Color::Yellow),
    (0, 0, 170, Color::Blue), (170, 0, 170, Color::Magenta), (0, 170, 170, Color::Cyan), (170, 170, 170, Color::Gray),
    (85, 85, 85, Color::DarkGray), (255, 85, 85, Color::LightRed), (85, 255, 85, Color::LightGreen), (255, 255, 85, Color::LightYellow),
    (85, 85, 255, Color::LightBlue), (255, 85, 255, Color::LightMagenta), (85, 255, 255, Color::LightCyan), (255, 255, 255, Color::White),
];

// Un solo listado de campos genera struct, valores por defecto y el recorrido de colores,
// así añadir un color nuevo al tema es una línea.
macro_rules! ui_colors {
    ($($field:ident = $default:expr),* $(,)?) => {
        /// Colores de la interfaz. Acepta nombres ("cyan", "dark gray", "reset"), "#rrggbb" o índices "0"-"255" (como texto).
        #[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
        #[serde(default)]
        pub struct UiColors { $(pub $field: Color,)* }

        impl Default for UiColors {
            fn default() -> Self { Self { $($field: $default,)* } }
        }

        impl UiColors {
            fn map(&mut self, f: impl Fn(Color) -> Color) { $(self.$field = f(self.$field);)* }
        }
    };
}

ui_colors! {
    background = Color::Reset,
    foreground = Color::Reset,
    border = Color::DarkGray,
    border_focused = Color::Cyan,
    accent = Color::Cyan,
    muted = Color::DarkGray,
    logo = Color::Magenta,
    key_hint = Color::Yellow,
    line_number = Color::DarkGray,
    indent_guide = Color::DarkGray,
    selection_bg = Color::Blue,
    selection_fg = Color::White,
    search_bg = Color::DarkGray,
    search_fg = Color::LightYellow,
    search_active_bg = Color::LightYellow,
    search_active_fg = Color::Black,
    inlay_hint_fg = Color::LightCyan,
    inlay_hint_bg = Color::DarkGray,
    error = Color::Red,
    warning = Color::Yellow,
    git_added = Color::Green,
    git_modified = Color::Yellow,
    git_deleted = Color::Red,
    tab_bar_bg = Color::Rgb(20, 20, 20),
    tab_bg = Color::Rgb(30, 30, 30),
    tab_active_bg = Color::Rgb(40, 50, 75),
    tab_fg = Color::White,
    dirty = Color::Yellow,
    clean = Color::Gray,
    tree_dir = Color::Blue,
    tree_file = Color::White,
    tree_selected_bg = Color::Cyan,
    tree_selected_fg = Color::Black,
    popup_bg = Color::Rgb(25, 25, 25),
    popup_fg = Color::White,
    popup_border = Color::Yellow,
    popup_selected_bg = Color::Rgb(45, 60, 80),
    popup_selected_fg = Color::White,
    popup_match = Color::Cyan,
    popup_detail = Color::DarkGray,
    popup_kind_bg = Color::Rgb(35, 35, 35),
    doc_bg = Color::Rgb(25, 25, 25),
    doc_fg = Color::Gray,
    doc_border = Color::DarkGray,
    doc_heading = Color::Cyan,
    doc_code = Color::LightYellow,
    help_bg = Color::Rgb(20, 20, 20),
    status_bg = Color::Rgb(30, 30, 30),
    status_fg = Color::White,
    status_ok = Color::Gray,
    status_mode_bg = Color::Cyan,
    status_mode_fg = Color::Black,
    status_git_bg = Color::DarkGray,
    status_git_fg = Color::White,
    kind_function = Color::Magenta,
    kind_method = Color::LightMagenta,
    kind_type = Color::LightYellow,
    kind_variable = Color::LightBlue,
    kind_field = Color::LightCyan,
    kind_constant = Color::LightRed,
    kind_module = Color::Blue,
    kind_keyword = Color::Yellow,
    kind_snippet = Color::Green,
    kind_other = Color::Gray,
}

/// Estilo de un scope TextMate: un color suelto o un objeto con más atributos.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum SyntaxStyle {
    Fg(Color),
    Full {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        fg: Option<Color>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        bg: Option<Color>,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        bold: bool,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        italic: bool,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        underline: bool,
    },
}

impl SyntaxStyle {
    fn modifier(&self) -> sh::StyleModifier {
        match *self {
            Self::Fg(c) => sh::StyleModifier { foreground: Some(to_rgb(c)), background: None, font_style: None },
            Self::Full { fg, bg, bold, italic, underline } => {
                let mut fs = sh::FontStyle::empty();
                fs.set(sh::FontStyle::BOLD, bold);
                fs.set(sh::FontStyle::ITALIC, italic);
                fs.set(sh::FontStyle::UNDERLINE, underline);
                sh::StyleModifier { foreground: fg.map(to_rgb), background: bg.map(to_rgb), font_style: Some(fs) }
            }
        }
    }
}

/// Tema completo tal como se lee de `themes/<nombre>.json`.
#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Theme {
    pub ui: UiColors,
    /// Selector de scope TextMate (p. ej. "entity.name.function, support.function") → estilo.
    pub syntax: BTreeMap<String, SyntaxStyle>,
    #[serde(skip)]
    pub syntect: sh::Theme,
    #[serde(skip)]
    truecolor: bool,
}

impl Default for Theme {
    fn default() -> Self {
        let fg = |hex: &str| SyntaxStyle::Fg(Color::from_str(hex).unwrap());
        let full = |hex: &str, bold, italic, underline| SyntaxStyle::Full { fg: Color::from_str(hex).ok(), bg: None, bold, italic, underline };
        // Paleta base16-ocean: la misma que usaba el editor antes de tener temas.
        let syntax = [
            ("comment", full("#65737e", false, true, false)),
            ("string", fg("#a3be8c")),
            ("constant", fg("#d08770")),
            ("constant.character.escape, string.regexp", fg("#96b5b4")),
            ("keyword, storage", fg("#b48ead")),
            ("keyword.operator", fg("#c0c5ce")),
            ("entity.name.function, meta.function-call", fg("#8fa1b3")),
            ("support.function, support.macro, entity.name.macro", fg("#96b5b4")),
            ("entity.name.type, entity.name.class, entity.name.struct, entity.name.enum, support.type, support.class", fg("#ebcb8b")),
            ("entity.name.namespace, entity.name.module", fg("#ebcb8b")),
            ("variable", fg("#bf616a")),
            ("variable.parameter", fg("#d08770")),
            ("variable.language", full("#bf616a", false, true, false)),
            ("entity.name.tag", fg("#bf616a")),
            ("entity.other.attribute-name", fg("#d08770")),
            ("support.type.property-name", fg("#8fa1b3")),
            ("markup.heading", full("#8fa1b3", true, false, false)),
            ("markup.bold", SyntaxStyle::Full { fg: None, bg: None, bold: true, italic: false, underline: false }),
            ("markup.italic", SyntaxStyle::Full { fg: None, bg: None, bold: false, italic: true, underline: false }),
            ("markup.underline.link", full("#96b5b4", false, false, true)),
            ("invalid", full("#bf616a", false, false, true)),
        ].into_iter().map(|(k, v)| (k.to_string(), v)).collect();

        Self { ui: UiColors::default(), syntax, syntect: sh::Theme::default(), truecolor: true }
    }
}

impl Theme {
    /// Parsea un tema; los scopes no definidos heredan los del tema por defecto.
    pub fn from_json(json: &str) -> serde_json::Result<Self> {
        let mut t: Self = serde_json::from_str(json)?;
        let mut syntax = Self::default().syntax;
        syntax.append(&mut t.syntax);
        t.syntax = syntax;
        Ok(t)
    }

    /// Adapta los colores a la terminal y construye el tema de syntect.
    pub fn finish(mut self, truecolor: bool) -> Self {
        self.truecolor = truecolor;
        if !truecolor { self.ui.map(nearest_ansi); }
        let scopes = self.syntax.iter().filter_map(|(sel, st)| Some(sh::ThemeItem {
            scope: sh::ScopeSelectors::from_str(sel).ok()?,
            style: st.modifier(),
        })).collect();
        self.syntect = sh::Theme {
            settings: sh::ThemeSettings { foreground: Some(UNSET), background: Some(UNSET), ..Default::default() },
            scopes,
            ..Default::default()
        };
        self
    }

    /// Estilo de ratatui para un fragmento resaltado por syntect.
    pub fn syntax_style(&self, s: sh::Style) -> Style {
        let fit = |c: sh::Color| {
            let c = Color::Rgb(c.r, c.g, c.b);
            if self.truecolor { c } else { nearest_ansi(c) }
        };
        let mut style = Style::default().fg(if s.foreground == UNSET { self.ui.foreground } else { fit(s.foreground) });
        if s.background != UNSET { style = style.bg(fit(s.background)); }
        for (fs, m) in [(sh::FontStyle::BOLD, Modifier::BOLD), (sh::FontStyle::ITALIC, Modifier::ITALIC), (sh::FontStyle::UNDERLINE, Modifier::UNDERLINED)] {
            if s.font_style.contains(fs) { style = style.add_modifier(m); }
        }
        style
    }
}

/// La mayoría de terminales modernas lo anuncian así; sin él se degrada a 16 colores.
pub fn terminal_has_truecolor() -> bool {
    std::env::var("COLORTERM").is_ok_and(|v| v == "truecolor" || v == "24bit")
}

fn nearest_ansi(c: Color) -> Color {
    let Color::Rgb(r, g, b) = c else { return c };
    let d = |x: u8, y: u8| (x as i32 - y as i32).pow(2);
    ANSI16.iter().min_by_key(|(cr, cg, cb, _)| d(*cr, r) + d(*cg, g) + d(*cb, b)).map_or(c, |e| e.3)
}

fn to_rgb(c: Color) -> sh::Color {
    let (r, g, b) = match c {
        Color::Rgb(r, g, b) => (r, g, b),
        Color::Indexed(i @ 16..=231) => {
            let lvl = |v: u8| if v == 0 { 0 } else { 55 + v * 40 };
            let i = i - 16;
            (lvl(i / 36), lvl(i / 6 % 6), lvl(i % 6))
        }
        Color::Indexed(i @ 232..) => { let v = 8 + (i - 232) * 10; (v, v, v) }
        Color::Indexed(i) => (ANSI16[i as usize].0, ANSI16[i as usize].1, ANSI16[i as usize].2),
        named => ANSI16.iter().find(|e| e.3 == named).map_or((255, 255, 255), |e| (e.0, e.1, e.2)),
    };
    sh::Color { r, g, b, a: 255 }
}

#[cfg(test)]
mod tests {
    use super::*;
    use syntect::highlighting::{HighlightIterator, HighlightState, Highlighter};
    use syntect::parsing::{ParseState, ScopeStack, SyntaxSet};

    #[test]
    fn default_theme_roundtrips_through_json() {
        let json = serde_json::to_string_pretty(&Theme::default()).unwrap();
        let t = Theme::from_json(&json).unwrap();
        assert_eq!(t.ui, UiColors::default());
        assert_eq!(t.syntax, Theme::default().syntax);
    }

    #[test]
    fn partial_theme_inherits_defaults_and_accepts_any_color_format() {
        let t = Theme::from_json(r##"{
            "ui": { "accent": "light magenta", "border": "#102030", "line_number": "244" },
            "syntax": { "keyword": { "fg": "#ff0000", "bold": true }, "string": "green" }
        }"##).unwrap();
        assert_eq!(t.ui.accent, Color::LightMagenta);
        assert_eq!(t.ui.border, Color::Rgb(0x10, 0x20, 0x30));
        assert_eq!(t.ui.line_number, Color::Indexed(244));
        assert_eq!(t.ui.selection_bg, UiColors::default().selection_bg);
        assert!(t.syntax.contains_key("comment"));
        assert_eq!(t.syntax["string"], SyntaxStyle::Fg(Color::Green));
        assert!(Theme::from_json(r#"{"ui": {"accent": "notacolor"}}"#).is_err());
    }

    #[test]
    fn syntax_scopes_drive_highlighting() {
        let t = Theme::from_json(r##"{"syntax": {"keyword, storage": {"fg": "#ff0000", "bold": true}}}"##).unwrap().finish(true);
        let ss = SyntaxSet::load_defaults_newlines();
        let hl = Highlighter::new(&t.syntect);
        let mut ps = ParseState::new(ss.find_syntax_by_extension("rs").unwrap());
        let mut hs = HighlightState::new(&hl, ScopeStack::new());
        let line = "fn main() {}\n";
        let ops = ps.parse_line(line, &ss).unwrap();
        let ranges: Vec<_> = HighlightIterator::new(&mut hs, &ops, line, &hl).collect();
        let (fn_style, _) = ranges.iter().find(|(_, s)| *s == "fn").unwrap();
        let st = t.syntax_style(*fn_style);
        assert_eq!(st.fg, Some(Color::Rgb(255, 0, 0)));
        assert!(st.add_modifier.contains(Modifier::BOLD));
        // Texto sin scope propio usa el foreground de la UI (Reset = color de la terminal).
        let (sp, _) = ranges.iter().find(|(_, s)| *s == " ").unwrap();
        assert_eq!(t.syntax_style(*sp).fg, Some(Color::Reset));
    }

    #[test]
    fn colors_degrade_without_truecolor() {
        let t = Theme::from_json(r##"{"ui": {"accent": "#f05050"}}"##).unwrap().finish(false);
        assert_eq!(t.ui.accent, Color::LightRed);
        assert_eq!(nearest_ansi(Color::Cyan), Color::Cyan);
        assert_eq!(to_rgb(Color::Indexed(196)), sh::Color { r: 255, g: 0, b: 0, a: 255 });
        assert_eq!(to_rgb(Color::Indexed(232)), sh::Color { r: 8, g: 8, b: 8, a: 255 });
    }
}
