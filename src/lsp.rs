use lsp_types::{
    ClientCapabilities, CompletionItem, CompletionParams, CompletionResponse, 
    Diagnostic, DidChangeTextDocumentParams, DidOpenTextDocumentParams, 
    InitializeParams, Position, PublishDiagnosticsParams, TextDocumentContentChangeEvent, 
    TextDocumentIdentifier, TextDocumentItem, TextDocumentPositionParams, Uri, VersionedTextDocumentIdentifier, WorkDoneProgressParams
};
use serde_json::{json, Value};
use std::env;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::PathBuf;
use std::process::{ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread;

/// Mensaje ya decodificado proveniente del servidor de lenguaje, entregado
/// de forma asíncrona al hilo principal a través de `LspClient::receiver`.
#[derive(Debug, Clone)]
pub enum LspMessage {
    /// Notificación del servidor sin `id` (no espera respuesta), distinta de diagnósticos.
    Notification { method: String, params: Value },
    /// Respuesta a una petición previamente enviada, identificada por su `id`.
    Response { id: u64, result: Value },
    /// Caso especial de notificación: diagnósticos (`textDocument/publishDiagnostics`).
    Diagnostics(PublishDiagnosticsParams),
    /// El proceso del servidor murió o envió algo irreconocible.
    Error(String),
}

/// Cliente de un servidor de lenguaje (LSP) lanzado como subproceso.
/// Habla el protocolo JSON-RPC sobre stdin/stdout con framing
/// `Content-Length`. La lectura de stdout corre en un hilo aparte que
/// empuja los mensajes ya parseados a `receiver`, para no bloquear el
/// bucle principal de la interfaz mientras se espera al servidor.
pub struct LspClient {
    stdin: ChildStdin,
    pub receiver: Receiver<LspMessage>,
    next_id: u64,
    /// ID de la petición `initialize` enviada al arrancar, para poder
    /// reconocer su respuesta y disparar el handshake `initialized`.
    pub init_id: u64,
    pub is_initialized: bool,
}

impl LspClient {
    /// Intenta lanzar un servidor de lenguaje apropiado para la extensión
    /// de archivo dada, eligiendo entre los binarios disponibles en el
    /// PATH del sistema. Devuelve `None` si la extensión no tiene un
    /// servidor soportado o si ninguno de los candidatos está instalado.
    pub fn start_for_extension(ext: &str, workspace_root: PathBuf) -> Option<Self> {
        let cmd = match ext {
            "rs" if is_in_path("rust-analyzer") => "rust-analyzer",
            "c" | "cpp" | "h" if is_in_path("clangd") => "clangd",
            "py" if is_in_path("pyright-langserver") => "pyright-langserver",
            "py" if is_in_path("pylsp") => "pylsp",
            _ => return None,
        };

        let mut process = Command::new(cmd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            // El stderr del servidor se descarta para no ensuciar la TUI,
            // que ya está usando la terminal en modo alternativo.
            .stderr(Stdio::null())
            .spawn()
            .ok()?;

        let stdin = process.stdin.take()?;
        let stdout = process.stdout.take()?;

        let (tx, rx) = mpsc::channel();

        // Hilo lector: bloquea en `read_line`/`read_exact` sobre stdout del
        // servidor, reconstruye cada mensaje JSON-RPC según su cabecera
        // `Content-Length` y lo reenvía ya parseado por el canal `tx`.
        // Si la lectura falla o llega una línea vacía, se asume que el
        // proceso del servidor murió y el hilo termina tras notificarlo.
        thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).is_err() || line.is_empty() {
                    let _ = tx.send(LspMessage::Error("LSP process died".into()));
                    break;
                }

                if line.starts_with("Content-Length: ") {
                    let len_str = line.trim_start_matches("Content-Length: ").trim();
                    if let Ok(len) = len_str.parse::<usize>() {
                        let mut empty_line = String::new();
                        let _ = reader.read_line(&mut empty_line);

                        let mut payload = vec![0; len];
                        if reader.read_exact(&mut payload).is_ok() {
                            if let Ok(msg) = serde_json::from_slice::<Value>(&payload) {
                                let _ = tx.send(parse_rpc_message(msg));
                            }
                        }
                    }
                }
            }
        });

        let mut client = Self {
            stdin,
            receiver: rx,
            next_id: 1,
            init_id: 0,
            is_initialized: false,
        };

        client.init_id = client.initialize(workspace_root);
        Some(client)
    }

    /// Envía una petición JSON-RPC (con `id`, espera respuesta) y devuelve
    /// el `id` asignado para que el llamador pueda emparejarlo luego con la respuesta.
    pub fn send_request(&mut self, method: &str, params: Value) -> u64 {
        let id = self.next_id;
        self.next_id += 1;

        let request = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params
        });

        self.write_message(&request);
        id
    }

    /// Envía una notificación JSON-RPC (sin `id`, no espera respuesta).
    pub fn send_notification(&mut self, method: &str, params: Value) {
        let notification = json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params
        });

        self.write_message(&notification);
    }

    /// Serializa el mensaje y lo escribe a stdin del servidor con el
    /// framing `Content-Length` que exige el protocolo LSP.
    fn write_message(&mut self, msg: &Value) {
        let json_str = msg.to_string();
        let payload = format!("Content-Length: {}\r\n\r\n{}", json_str.len(), json_str);
        let _ = self.stdin.write_all(payload.as_bytes());
        let _ = self.stdin.flush();
    }

    /// Construye y envía la petición `initialize`, primer mensaje del
    /// handshake LSP. La URI de la raíz del workspace se deriva de
    /// `workspace_root`; si la conversión a `file://` falla, se usa
    /// `file:///` como valor de respaldo.
    fn initialize(&mut self, workspace_root: PathBuf) -> u64 {
        let file_url = url::Url::from_file_path(&workspace_root)
            .unwrap_or_else(|_| url::Url::parse("file:///").unwrap());
        let uri: Uri = file_url.as_str().parse().unwrap();
        
        #[allow(deprecated)] 
        let params = InitializeParams {
            process_id: Some(std::process::id()),
            root_path: Some(workspace_root.to_string_lossy().into_owned()),
            root_uri: Some(uri),
            initialization_options: None,
            capabilities: ClientCapabilities::default(),
            trace: None,
            workspace_folders: None,
            client_info: None,
            locale: None,
            work_done_progress_params: WorkDoneProgressParams::default(),
        };

        self.send_request("initialize", serde_json::to_value(params).unwrap())
    }

    /// Notificación `initialized`, segundo paso del handshake, que debe
    /// enviarse solo después de recibir la respuesta a `initialize`.
    pub fn send_initialized(&mut self) {
        self.send_notification("initialized", json!({}));
    }

   /// Notifica al servidor que un documento fue abierto, enviando su
   /// contenido completo, versión inicial e identificador de lenguaje
   /// (p. ej. "rust", "python").
   pub fn did_open(&mut self, uri: Uri, text: String, version: i32, language_id: &str) {
        let params = DidOpenTextDocumentParams {
            text_document: TextDocumentItem {
                uri,
                language_id: language_id.to_string(),
                version,
                text,
            },
        };
        self.send_notification("textDocument/didOpen", serde_json::to_value(params).unwrap());
    }

    /// Notifica al servidor un cambio en el documento. Se usa sincronización
    /// completa (todo el texto reemplazado en cada cambio, `range: None`)
    /// en vez de cambios incrementales, lo que simplifica el cliente a
    /// costa de más tráfico por edición.
    pub fn did_change(&mut self, uri: Uri, text: String, version: i32) {
        let params = DidChangeTextDocumentParams {
            text_document: VersionedTextDocumentIdentifier { uri, version },
            content_changes: vec![TextDocumentContentChangeEvent {
                range: None,
                range_length: None,
                text,
            }],
        };
        self.send_notification("textDocument/didChange", serde_json::to_value(params).unwrap());
    }

    /// Envía una petición `textDocument/completion` en la posición dada y
    /// devuelve el `id` de la petición para poder identificar la respuesta
    /// cuando llegue de forma asíncrona.
    pub fn request_completion(&mut self, uri: Uri, line: u32, character: u32) -> u64 {
        let params = CompletionParams {
            text_document_position: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri },
                position: Position { line, character },
            },
            work_done_progress_params: WorkDoneProgressParams::default(),
            partial_result_params: lsp_types::PartialResultParams::default(),
            context: None,
        };
        self.send_request("textDocument/completion", serde_json::to_value(params).unwrap())
    }
}

/// Comprueba si un ejecutable con este nombre existe en algún directorio
/// del PATH, de forma multiplataforma (sin depender de `which`/`where`).
fn is_in_path(program: &str) -> bool {
    if let Some(path) = env::var_os("PATH") {
        for dir in env::split_paths(&path) {
            if dir.join(program).is_file() {
                return true;
            }
        }
    }
    false
}

/// Clasifica un mensaje JSON-RPC crudo del servidor: respuesta con
/// resultado, notificación de diagnósticos, notificación genérica, o
/// formato no reconocido.
fn parse_rpc_message(val: Value) -> LspMessage {
    if val.get("id").is_some() && val.get("result").is_some() {
        LspMessage::Response {
            id: val["id"].as_u64().unwrap_or(0),
            result: val["result"].clone(),
        }
    } else if let Some(method) = val.get("method").and_then(|m| m.as_str()) {
        if method == "textDocument/publishDiagnostics" {
            if let Ok(params) = serde_json::from_value::<PublishDiagnosticsParams>(val["params"].clone()) {
                return LspMessage::Diagnostics(params);
            }
        }
        LspMessage::Notification {
            method: method.to_string(),
            params: val["params"].clone(),
        }
    } else {
        LspMessage::Error("Formato RPC desconocido".into())
    }
}
