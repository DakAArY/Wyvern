use lsp_types::{
    CompletionContext, CompletionParams, CompletionTriggerKind, DidChangeTextDocumentParams, DidOpenTextDocumentParams, Position,
    PublishDiagnosticsParams, TextDocumentContentChangeEvent, TextDocumentIdentifier, TextDocumentItem,
    TextDocumentPositionParams, Uri, VersionedTextDocumentIdentifier, WorkDoneProgressParams,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::env;
use std::io::{self, BufRead, BufReader, Write};
use std::ops::Range;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Sender};
use std::thread;
use std::time::Duration;

/// Eventos normalizados que el hilo de comunicación entrega a la aplicación.
#[derive(Debug, Clone)]
pub enum LspMessage {
    /// Petición del servidor que exige respuesta; el id puede ser número o texto.
    Request { id: Value, method: String, params: Value },
    /// Resultado (o `Null` si hubo error) de una petición enviada anteriormente.
    Response { id: u64, result: Value },
    /// Diagnósticos publicados para un documento.
    Diagnostics(PublishDiagnosticsParams),
    /// El proceso terminó o su salida dejó de ser legible.
    Error(String),
}

/// Un servidor tal como se declara en `config.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerConfig {
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    /// Extensiones de archivo que atiende.
    pub extensions: Vec<String>,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub initialization_options: Value,
    /// Respuestas a `workspace/configuration`, indexadas por sección ("python.analysis" → settings.python.analysis).
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub settings: Value,
}

/// Adaptador entre la aplicación y un servidor de lenguaje externo.
///
/// Un hilo lee stdout y otro escribe stdin, así la UI nunca se bloquea
/// aunque el servidor tarde en consumir mensajes.
pub struct LspClient {
    pub name: String,
    out: Sender<Vec<u8>>,
    child_process: Child,
    next_id: u64,
    pub init_id: u64,
    pub is_initialized: bool,
    pub incremental_sync: bool,
    pub trigger_chars: Vec<char>,
    /// El servidor completa documentación/auto-imports bajo demanda (`completionItem/resolve`).
    pub resolve_provider: bool,
    extensions: Vec<String>,
    settings: Value,
}

impl LspClient {
    /// Arranca el servidor y envía `initialize`; `None` si no se pudo lanzar.
    pub fn start(
        cfg: &ServerConfig,
        workspace_root: &Path,
        server_id: usize,
        tx: Sender<(usize, LspMessage)>,
    ) -> Option<Self> {
        let mut process = Command::new(&cfg.command)
            .args(&cfg.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;

        let mut stdin = process.stdin.take()?;
        let stdout = process.stdout.take()?;

        thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            loop {
                match read_message(&mut reader) {
                    Ok(Some(val)) => {
                        if let Some(msg) = parse_rpc_message(val)
                            && tx.send((server_id, msg)).is_err() { break; }
                    }
                    _ => {
                        let _ = tx.send((server_id, LspMessage::Error("Proceso LSP finalizado".into())));
                        break;
                    }
                }
            }
        });

        let (out, out_rx) = mpsc::channel::<Vec<u8>>();
        thread::spawn(move || {
            for msg in out_rx {
                if stdin.write_all(&msg).and_then(|_| stdin.flush()).is_err() { break; }
            }
        });

        let mut client = Self {
            name: cfg.command.clone(),
            out,
            child_process: process,
            next_id: 1,
            init_id: 0,
            is_initialized: false,
            incremental_sync: false,
            trigger_chars: Vec::new(),
            resolve_provider: false,
            extensions: cfg.extensions.clone(),
            settings: cfg.settings.clone(),
        };
        client.init_id = client.initialize(workspace_root, &cfg.initialization_options);
        Some(client)
    }

    /// Envía una petición JSON-RPC y devuelve su ID.
    pub fn handles(&self, ext: &str) -> bool {
        self.extensions.iter().any(|e| e == ext)
    }

    pub fn send_request(&mut self, method: &str, params: Value) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        self.write_message(&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }));
        id
    }

    pub fn send_notification(&mut self, method: &str, params: Value) {
        self.write_message(&json!({ "jsonrpc": "2.0", "method": method, "params": params }));
    }

    /// Contesta una petición del servidor; los métodos desconocidos reciben MethodNotFound.
    pub fn reply(&mut self, id: Value, method: &str, params: &Value) {
        let msg = match request_reply(method, params, &self.settings) {
            Some(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
            None => json!({ "jsonrpc": "2.0", "id": id, "error": { "code": -32601, "message": "Method not found" } }),
        };
        self.write_message(&msg);
    }

    fn write_message(&mut self, msg: &Value) {
        let json_str = msg.to_string();
        let payload = format!("Content-Length: {}\r\n\r\n{}", json_str.len(), json_str);
        let _ = self.out.send(payload.into_bytes());
    }

    fn initialize(&mut self, workspace_root: &Path, init_options: &Value) -> u64 {
        let root = std::fs::canonicalize(workspace_root).unwrap_or_else(|_| workspace_root.to_path_buf());
        let uri = path_to_uri(&root).unwrap_or_else(|| "file:///".parse().unwrap());

        let capabilities = lsp_types::ClientCapabilities {
            text_document: Some(lsp_types::TextDocumentClientCapabilities {
                completion: Some(lsp_types::CompletionClientCapabilities {
                    completion_item: Some(lsp_types::CompletionItemCapability {
                        snippet_support: Some(true),
                        label_details_support: Some(true),
                        insert_replace_support: Some(true),
                        documentation_format: Some(vec![lsp_types::MarkupKind::Markdown, lsp_types::MarkupKind::PlainText]),
                        resolve_support: Some(lsp_types::CompletionItemCapabilityResolveSupport {
                            properties: ["documentation", "detail", "additionalTextEdits"].map(String::from).to_vec(),
                        }),
                        ..Default::default()
                    }),
                    context_support: Some(true),
                    ..Default::default()
                }),
                synchronization: Some(lsp_types::TextDocumentSyncClientCapabilities {
                    did_save: Some(true),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            // Sin esto los servidores no piden `workspace/configuration` y se ignoran los settings.
            workspace: Some(lsp_types::WorkspaceClientCapabilities {
                configuration: Some(true),
                ..Default::default()
            }),
            ..Default::default()
        };

        let workspace_folders = Some(vec![lsp_types::WorkspaceFolder {
            uri: uri.clone(),
            name: root.file_name().unwrap_or_default().to_string_lossy().into_owned(),
        }]);

        let initialization_options = (!init_options.is_null()).then(|| init_options.clone());

        #[allow(deprecated)]
        let params = lsp_types::InitializeParams {
            process_id: Some(std::process::id()),
            root_path: Some(root.to_string_lossy().into_owned()),
            root_uri: Some(uri),
            initialization_options,
            capabilities,
            trace: None,
            workspace_folders,
            client_info: None,
            locale: None,
            work_done_progress_params: WorkDoneProgressParams::default(),
        };

        self.send_request("initialize", serde_json::to_value(params).unwrap())
    }

    /// Completa el handshake y adopta el modo de sincronización que anuncia el servidor.
    pub fn on_initialized(&mut self, result: &Value) {
        let sync = &result["capabilities"]["textDocumentSync"];
        self.incremental_sync = sync.as_u64().or_else(|| sync["change"].as_u64()) == Some(2);
        let provider = &result["capabilities"]["completionProvider"];
        self.resolve_provider = provider["resolveProvider"].as_bool() == Some(true);
        self.trigger_chars = provider["triggerCharacters"].as_array()
            .into_iter().flatten()
            .filter_map(|c| c.as_str()?.chars().next())
            .collect();
        self.is_initialized = true;
        self.send_notification("initialized", json!({}));
        // Algunos servidores (yaml, json) solo leen settings empujados, no los piden.
        if !self.settings.is_null() {
            let settings = self.settings.clone();
            self.send_notification("workspace/didChangeConfiguration", json!({ "settings": settings }));
        }
    }

    pub fn did_open(&mut self, uri: Uri, text: String, version: i32, language_id: &str) {
        let params = DidOpenTextDocumentParams {
            text_document: TextDocumentItem { uri, language_id: language_id.to_string(), version, text },
        };
        self.send_notification("textDocument/didOpen", serde_json::to_value(params).unwrap());
    }

    pub fn did_close(&mut self, uri: Uri) {
        self.send_notification("textDocument/didClose", json!({ "textDocument": { "uri": uri } }));
    }

    pub fn did_save(&mut self, uri: Uri) {
        self.send_notification("textDocument/didSave", json!({ "textDocument": { "uri": uri } }));
    }

    pub fn did_change(&mut self, uri: Uri, version: i32, content_changes: Vec<TextDocumentContentChangeEvent>) {
        let params = DidChangeTextDocumentParams {
            text_document: VersionedTextDocumentIdentifier { uri, version },
            content_changes,
        };
        self.send_notification("textDocument/didChange", serde_json::to_value(params).unwrap());
    }

    /// `trigger` es el carácter que disparó la petición, si es uno de los anunciados por el servidor.
    pub fn request_completion(&mut self, uri: Uri, line: u32, character: u32, trigger: Option<char>) -> u64 {
        let trigger = trigger.filter(|c| self.trigger_chars.contains(c));
        let params = CompletionParams {
            text_document_position: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri },
                position: Position { line, character },
            },
            work_done_progress_params: WorkDoneProgressParams::default(),
            partial_result_params: lsp_types::PartialResultParams::default(),
            context: Some(CompletionContext {
                trigger_kind: if trigger.is_some() { CompletionTriggerKind::TRIGGER_CHARACTER } else { CompletionTriggerKind::INVOKED },
                trigger_character: trigger.map(String::from),
            }),
        };
        self.send_request("textDocument/completion", serde_json::to_value(params).unwrap())
    }

    pub fn request_inlay_hints(&mut self, uri: Uri, range: lsp_types::Range) -> u64 {
        let params = lsp_types::InlayHintParams {
            work_done_progress_params: Default::default(),
            text_document: TextDocumentIdentifier { uri },
            range,
        };
        self.send_request("textDocument/inlayHint", serde_json::to_value(params).unwrap())
    }

    /// Pide un cierre ordenado y concede un margen breve antes de que `Drop` mate el proceso.
    pub fn shutdown_and_exit(&mut self) {
        self.send_request("shutdown", Value::Null);
        self.send_notification("exit", Value::Null);
        for _ in 0..20 {
            if let Ok(Some(_)) = self.child_process.try_wait() { return; }
            thread::sleep(Duration::from_millis(10));
        }
    }
}

impl Drop for LspClient {
    // Evita procesos zombi tanto al salir como cuando un servidor muere.
    fn drop(&mut self) {
        let _ = self.child_process.kill();
        let _ = self.child_process.wait();
    }
}

/// Lee un mensaje con framing LSP; `Ok(None)` al llegar a EOF.
fn read_message<R: BufRead>(reader: &mut R) -> io::Result<Option<Value>> {
    let mut len = None;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 { return Ok(None); }
        let line = line.trim_end();
        if line.is_empty() {
            if len.is_some() { break; }
            continue;
        }
        if let Some((key, value)) = line.split_once(':')
            && key.eq_ignore_ascii_case("content-length") {
                len = value.trim().parse::<usize>().ok();
            }
    }
    let mut payload = vec![0; len.unwrap_or(0)];
    reader.read_exact(&mut payload)?;
    Ok(Some(serde_json::from_slice(&payload).unwrap_or(Value::Null)))
}

/// Resultado para las peticiones del servidor que este cliente sabe contestar.
fn request_reply(method: &str, params: &Value, settings: &Value) -> Option<Value> {
    match method {
        // Un null para lo no configurado hace que el servidor use sus valores por defecto.
        "workspace/configuration" => Some(params["items"].as_array().into_iter().flatten().map(|item| {
            item["section"].as_str().map_or(Some(settings), |sec| sec.split('.').try_fold(settings, |v, k| v.get(k)))
                .cloned().unwrap_or(Value::Null)
        }).collect()),
        "client/registerCapability" | "client/unregisterCapability" | "window/workDoneProgress/create"
        | "window/showMessageRequest" | "workspace/workspaceFolders" | "workspace/semanticTokens/refresh"
        | "workspace/inlayHint/refresh" | "workspace/diagnostic/refresh" | "workspace/codeLens/refresh" => Some(Value::Null),
        _ => None,
    }
}

/// Convierte un valor JSON-RPC en el evento que entiende la aplicación; las
/// notificaciones que no usamos se descartan.
fn parse_rpc_message(val: Value) -> Option<LspMessage> {
    let method = val.get("method").and_then(|m| m.as_str()).map(str::to_string);
    match (val.get("id"), method) {
        (Some(id), Some(method)) => Some(LspMessage::Request { id: id.clone(), method, params: val["params"].clone() }),
        (Some(id), None) => Some(LspMessage::Response {
            id: id.as_u64()?,
            result: val.get("result").cloned().unwrap_or(Value::Null),
        }),
        (None, Some(m)) if m == "textDocument/publishDiagnostics" => {
            serde_json::from_value(val["params"].clone()).ok().map(LspMessage::Diagnostics)
        }
        _ => None,
    }
}

/// Expande un snippet LSP a texto plano: los placeholders conservan su valor por
/// defecto y las variables se vacían. Devuelve el rango (en chars) del primer
/// tabstop ($1, o $0 si no hay otro) para colocar ahí el cursor/selección.
pub fn expand_snippet(src: &str) -> (String, Option<Range<usize>>) {
    let chars: Vec<char> = src.chars().collect();
    let mut out = String::new();
    let mut stops = Vec::new();
    let mut i = 0;
    parse_snippet(&chars, &mut i, &mut out, &mut stops, false);
    let first = stops.iter().filter(|(n, _)| *n > 0).min_by_key(|(n, _)| *n)
        .or_else(|| stops.iter().find(|(n, _)| *n == 0));
    (out, first.map(|(_, r)| r.clone()))
}

fn parse_snippet(c: &[char], i: &mut usize, out: &mut String, stops: &mut Vec<(u32, Range<usize>)>, nested: bool) {
    while *i < c.len() {
        match c[*i] {
            '\\' if *i + 1 < c.len() => { out.push(c[*i + 1]); *i += 2; }
            '}' if nested => { *i += 1; return; }
            '$' => { *i += 1; parse_dollar(c, i, out, stops); }
            ch => { out.push(ch); *i += 1; }
        }
    }
}

fn parse_dollar(c: &[char], i: &mut usize, out: &mut String, stops: &mut Vec<(u32, Range<usize>)>) {
    let start = out.chars().count();
    let take_while = |i: &mut usize, f: fn(char) -> bool| {
        let s = *i;
        while *i < c.len() && f(c[*i]) { *i += 1; }
        c[s..*i].iter().collect::<String>()
    };
    let is_var_start = |ch: char| ch.is_ascii_alphabetic() || ch == '_';

    if c.get(*i).is_some_and(char::is_ascii_digit) {
        let n = take_while(i, |ch| ch.is_ascii_digit()).parse().unwrap_or(0);
        stops.push((n, start..start));
    } else if c.get(*i) == Some(&'{') {
        *i += 1;
        if c.get(*i).is_some_and(char::is_ascii_digit) {
            let n = take_while(i, |ch| ch.is_ascii_digit()).parse().unwrap_or(0);
            match c.get(*i) {
                Some(':') => { *i += 1; parse_snippet(c, i, out, stops, true); }
                Some('|') => {
                    *i += 1;
                    out.push_str(&take_while(i, |ch| ch != ',' && ch != '|'));
                    while *i < c.len() && c[*i] != '}' { *i += 1; }
                    *i += 1;
                }
                _ => *i += 1,
            }
            stops.push((n, start..out.chars().count()));
        } else if c.get(*i).is_some_and(|&ch| is_var_start(ch)) {
            take_while(i, |ch| ch.is_ascii_alphanumeric() || ch == '_');
            if c.get(*i) == Some(&':') {
                *i += 1;
                parse_snippet(c, i, out, stops, true);
            } else {
                // Variable sin valor por defecto (o con transformación): no se resuelve.
                while *i < c.len() && c[*i] != '}' { *i += 1; }
                *i += 1;
            }
        } else {
            out.push_str("${");
        }
    } else if c.get(*i).is_some_and(|&ch| is_var_start(ch)) {
        take_while(i, |ch| ch.is_ascii_alphanumeric() || ch == '_');
    } else {
        out.push('$');
    }
}

pub fn path_to_uri(path: &Path) -> Option<Uri> {
    url::Url::from_file_path(path).ok()?.as_str().parse().ok()
}

/// Comprueba si un ejecutable está disponible en el PATH (o existe, si es una ruta).
pub fn is_in_path(program: &str) -> bool {
    if program.contains(std::path::MAIN_SEPARATOR) { return Path::new(program).is_file(); }
    env::var_os("PATH").is_some_and(|path| env::split_paths(&path).any(|dir| dir.join(program).is_file()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_response_error_and_diagnostics() {
        assert!(matches!(parse_rpc_message(json!({"id": 3, "result": [1]})), Some(LspMessage::Response { id: 3, .. })));
        assert!(matches!(parse_rpc_message(json!({"id": 4, "error": {}})), Some(LspMessage::Response { id: 4, result: Value::Null })));
        let d = json!({"method": "textDocument/publishDiagnostics", "params": {"uri": "file:///a.rs", "diagnostics": []}});
        assert!(matches!(parse_rpc_message(d), Some(LspMessage::Diagnostics(_))));
        assert!(parse_rpc_message(json!({"method": "$/progress", "params": {}})).is_none());
    }

    #[test]
    fn server_requests_are_recognized_with_string_ids() {
        let req = json!({"id": "abc", "method": "workspace/configuration", "params": {"items": [{}, {}]}});
        let Some(LspMessage::Request { id, method, params }) = parse_rpc_message(req) else { panic!() };
        assert_eq!(id, json!("abc"));
        assert_eq!(request_reply(&method, &params, &Value::Null), Some(json!([null, null])));
        assert_eq!(request_reply("client/registerCapability", &Value::Null, &Value::Null), Some(Value::Null));
        assert_eq!(request_reply("foo/bar", &Value::Null, &Value::Null), None);
    }

    #[test]
    fn framing_tolerates_extra_headers_and_eof() {
        let body = r#"{"id":1,"result":null}"#;
        let raw = format!(
            "Content-Length: {}\r\nContent-Type: application/vscode-jsonrpc; charset=utf-8\r\n\r\n{}content-length: 2\r\n\r\n{{}}",
            body.len(), body
        );
        let mut r = io::Cursor::new(raw.into_bytes());
        assert_eq!(read_message(&mut r).unwrap(), Some(json!({"id": 1, "result": null})));
        assert_eq!(read_message(&mut r).unwrap(), Some(json!({})));
        assert_eq!(read_message(&mut r).unwrap(), None);
    }

    #[test]
    fn snippets_keep_defaults_and_locate_first_tabstop() {
        assert_eq!(expand_snippet("for ${1:i} in ${2:iter} {\n\t$0\n}"), ("for i in iter {\n\t\n}".into(), Some(4..5)));
        assert_eq!(expand_snippet("<div class=\"$1\">$0</div>"), ("<div class=\"\"></div>".into(), Some(12..12)));
        assert_eq!(expand_snippet("foo($0)"), ("foo()".into(), Some(4..4)));
        assert_eq!(expand_snippet("${1|a,b|} ${TM_FILENAME} ${2:${3:x}y}"), ("a  xy".into(), Some(0..1)));
        assert_eq!(expand_snippet("\\$x \\} $ ${"), ("$x } $ ${".into(), None));
        assert_eq!(expand_snippet("plain"), ("plain".into(), None));
    }

    #[test]
    fn initialize_result_sets_sync_mode_and_trigger_chars() {
        let (tx, _rx) = mpsc::channel();
        // `cat` basta como proceso: solo se prueba cómo se interpreta la respuesta.
        let cfg = ServerConfig { command: "cat".into(), args: vec![], extensions: vec!["rs".into()], initialization_options: Value::Null, settings: Value::Null };
        let mut c = LspClient::start(&cfg, Path::new("/"), 1, tx).unwrap();
        c.on_initialized(&json!({"capabilities": {"textDocumentSync": {"change": 2}, "completionProvider": {"triggerCharacters": [".", "::", "<"], "resolveProvider": true}}}));
        assert!(c.is_initialized && c.incremental_sync && c.resolve_provider && c.handles("rs") && !c.handles("py"));
        assert_eq!(c.trigger_chars, ['.', ':', '<']);
    }

    #[test]
    fn configuration_requests_are_answered_from_settings() {
        let settings = json!({"python": {"analysis": {"typeCheckingMode": "strict"}}, "yaml": {"format": true}});
        let params = json!({"items": [{"section": "python.analysis"}, {"section": "yaml"}, {"section": "rust-analyzer"}, {}]});
        assert_eq!(
            request_reply("workspace/configuration", &params, &settings),
            Some(json!([{"typeCheckingMode": "strict"}, {"format": true}, null, settings]))
        );
    }
}
