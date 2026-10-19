# Wyvern

Editor de texto para terminal escrito en Rust, con LSP, resaltado de sintaxis, git, splits, terminal integrada y temas configurables.

> ⚠️ **Proyecto en desarrollo activo.** Se sigue trabajando en él: puede haber bugs, cambios de comportamiento entre versiones y funciones incompletas. Guarda tu trabajo y úsalo con precaución.

## Instalación

Binario precompilado (Linux x86_64):

```bash
curl -fsSL https://raw.githubusercontent.com/DakAArY/Wyvern/main/install.sh | bash
```

Se instala como `wyvern` en `$XDG_BIN_HOME` o `~/.local/bin`.

Desde el código (Rust 1.85+, edición 2024):

```bash
git clone https://github.com/DakAArY/Wyvern.git
cd Wyvern
cargo build --release
./target/release/Wyvern
```

### Requisitos recomendados

- Una [Nerd Font](https://www.nerdfonts.com/) en la terminal: los iconos de archivos y de tipos de sugerencia la usan.
- Terminal con truecolor (`COLORTERM=truecolor`) para ver los temas con sus colores exactos; sin ella se aproximan a 16 colores.
- `git` en el `PATH` para la información de rama y cambios.
- Los servidores LSP de los lenguajes que uses (ver [Servidores LSP](#servidores-lsp)).

## Uso

```bash
wyvern                 # pantalla de inicio
wyvern archivo.rs      # abre (o crea en memoria) un archivo
wyvern ruta/proyecto   # abre el explorador en ese directorio
```

La raíz del proyecto se deduce subiendo directorios: el repo git más cercano o, si no hay, el primer directorio con `Cargo.toml`, `package.json`, `pyproject.toml`, `go.mod` o `compile_commands.json`. Las búsquedas, git y los servidores LSP trabajan sobre esa raíz.

## Funciones

- **Edición:** deshacer/rehacer por transacciones, selección con Shift o arrastrando el ratón, autocierre de `() [] {} "" '' <>`, indentación automática al pulsar Enter entre llaves y guías de indentación.
- **Guardado seguro:** se escribe en un temporal y se renombra; los archivos que no son UTF-8 no se abren para no corromperlos.
- **Resaltado de sintaxis** con `syntect`, coloreado por el tema activo.
- **LSP:** diagnósticos (subrayado y contador en la barra de estado), inlay hints, autocompletado con snippets, auto-imports y documentación.
- **Autocompletado al estilo JetBrains:**
  - Orden: exacta > empieza igual > igual sin mayúsculas > camelHumps (`gcp` → `getCurrentPosition`) > contenido > difusa.
  - Las sugerencias que ya elegiste suben en la lista (durante la sesión).
  - Al escribir o borrar se refiltra al instante; lo escrito aparece resaltado; tipo/firma alineados a la derecha.
  - Panel de documentación junto a la sugerencia seleccionada.
- **Git:** rama y totales `+ ~ -` en la barra de estado, y marcas por línea en el margen.
- **Búsqueda:** archivos (difusa) y texto en todo el proyecto, respetando `.gitignore`.
- **Splits** verticales y horizontales con navegación de foco.
- **Explorador de archivos** con creación, renombrado y borrado.
- **Terminal integrada** (F4).
- **Ratón:** clic para mover el cursor o enfocar, doble clic para abrir en el explorador, arrastrar para seleccionar y rueda para desplazar.

## Atajos

| Tecla | Acción |
|---|---|
| `F1` | Mostrar/ocultar ayuda |
| `F2` | Explorador de archivos |
| `F3` | Siguiente coincidencia de la búsqueda de texto |
| `F4` | Abrir / enfocar / ocultar la terminal |
| `F5` | Cambiar al siguiente tema de `themes/` |
| `Ctrl+S` | Guardar |
| `Ctrl+Q` | Salir (pide confirmación si hay cambios sin guardar) |
| `Ctrl+F` | Buscar archivo en el proyecto |
| `Ctrl+T` | Buscar texto en el proyecto |
| `Ctrl+C` / `Ctrl+X` / `Ctrl+V` | Copiar / cortar / pegar |
| `Ctrl+Z` / `Ctrl+Y` | Deshacer / rehacer |
| `Ctrl+U` / `Ctrl+D` | Desplazar la vista arriba / abajo |
| `Alt+V` / `Alt+H` | Split vertical / horizontal |
| `Alt+W` | Cerrar el split activo |
| `Alt+I` / `Alt+K` / `Alt+J` / `Alt+L` | Mover el foco arriba / abajo / izquierda / derecha |
| Flechas, `Inicio`, `Fin`, `RePág`, `AvPág` | Navegar (con `Shift` seleccionan) |
| `Tab` | Indentar / siguiente sugerencia |
| `↑` `↓` | Navegar sugerencias cuando el popup está abierto |
| `Enter` | Aceptar sugerencia / salto de línea / abrir en el explorador |
| `Esc` | Cerrar popup, modal o selección |

En el explorador: `n` nuevo archivo, `r` renombrar, `d` / `Supr` borrar.

## Configuración

En el primer arranque se crea `~/.config/wyvern/` (o `$XDG_CONFIG_HOME/wyvern/`). Los archivos que ya existan nunca se sobrescriben:

```
~/.config/wyvern/
├── config.json            # tema activo, lenguajes y servidores LSP
└── themes/
    └── wyvern-dark.json   # tema por defecto
```

**Recarga en caliente:** al guardar `config.json` o el tema activo, los cambios se aplican sin reiniciar. Si el JSON es inválido, se conserva lo que funcionaba y el error aparece en la barra de estado.

### config.json

```json
{
  "theme": "wyvern-dark",
  "languages": { "rs": "rust", "tsx": "typescriptreact" },
  "servers": [
    { "command": "rust-analyzer", "args": [], "extensions": ["rs"] }
  ]
}
```

- `theme`: nombre de un archivo de `themes/`, sin `.json`.
- `languages`: extensión → `languageId` que recibe el servidor. Si una extensión no aparece, se usa la propia extensión.
- `servers`: lista de servidores LSP (ver abajo).

Si falta algún campo, toma su valor por defecto.

### Temas

Para crear un tema, copia `themes/wyvern-dark.json` a `themes/mi-tema.json`, edítalo y pon `"theme": "mi-tema"` en `config.json`, o pulsa `F5` para recorrer los temas disponibles (este cambio dura solo la sesión).

```json
{
  "ui": {
    "background": "#1e1e2e",
    "accent": "light magenta",
    "line_number": "244",
    "popup_selected_bg": "#313244"
  },
  "syntax": {
    "comment": { "fg": "#6c7086", "italic": true },
    "keyword, storage": { "fg": "#cba6f7", "bold": true },
    "string": "#a6e3a1",
    "entity.name.function": "#89b4fa",
    "entity.name.function.rust": "#f9e2af"
  }
}
```

- **`ui`** contiene los colores de la interfaz: bordes, pestañas, explorador, popup, documentación (`doc_*`), barra de estado (`status_*`), búsqueda, selección, git, diagnósticos e iconos por tipo de sugerencia (`kind_*`). El archivo generado lista todos los campos.
- **`syntax`** asigna un estilo a cada selector de scope TextMate. El valor puede ser un color o un objeto `{ "fg", "bg", "bold", "italic", "underline" }`. Se puede separar por comas y usar scopes tan específicos como quieras. Los scopes que no definas se heredan del tema por defecto.
- **Formatos de color:** nombres (`"cyan"`, `"dark gray"`, `"light red"`, `"reset"` para el color de la terminal), `"#rrggbb"` o índices `"0"`–`"255"` (como texto).

### Servidores LSP

Para añadir un lenguaje solo hay que editar la configuración, sin tocar código:

```json
{
  "command": "pyright-langserver",
  "args": ["--stdio"],
  "extensions": ["py"],
  "initialization_options": {},
  "settings": { "python": { "analysis": { "typeCheckingMode": "strict" } } }
}
```

- Un servidor solo se arranca si `command` está en el `PATH` o es una ruta absoluta existente.
- `initialization_options` se envía en `initialize`.
- `settings` responde a las peticiones `workspace/configuration` por sección (`"python.analysis"` → `settings.python.analysis`) y también se envía al conectar.
- Los caracteres que abren el autocompletado (`.`, `::`, `<`…) los anuncia cada servidor.
- Varios servidores pueden atender la misma extensión (p. ej. TypeScript + Tailwind).

Servidores incluidos por defecto: `rust-analyzer`, `pyright-langserver`, `clangd`, `typescript-language-server`, `tailwindcss-language-server`, `vue-language-server`, `vscode-css-language-server`, `vscode-html-language-server`, `emmet-language-server`, `vscode-json-language-server`, `yaml-language-server`, `gopls`, `lua-language-server`, `bash-language-server`, `zls`, `jdtls` y `marksman`.

Al cambiar `servers`, la nueva lista se aplica a los archivos que abras después; los servidores que ya están corriendo no se reinician.

## Desarrollo

```bash
cargo test                 # tests unitarios
cargo test -- --ignored    # integración con rust-analyzer real (debe estar en el PATH)
cargo clippy --all-targets
```

Los releases se publican automáticamente al subir un tag `v*` (ver `.github/workflows/release.yml`).

## Limitaciones conocidas

- Solo hay binario precompilado para Linux x86_64.
- El historial de sugerencias elegidas se pierde al cerrar el editor.
- La documentación de las sugerencias se muestra como markdown casi sin procesar.
