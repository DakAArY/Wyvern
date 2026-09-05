use lsp_types::{
    ClientCapabilities, CompletionParams, 
    DidChangeTextDocumentParams, DidOpenTextDocumentParams, 
    InitializeParams, Position, PublishDiagnosticsParams, TextDocumentContentChangeEvent, 
    TextDocumentIdentifier, TextDocumentItem, TextDocumentPositionParams, Uri, VersionedTextDocumentIdentifier, WorkDoneProgressParams
};
use serde_json::{json, Value};
use std::env;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::PathBuf;
use std::process::{ChildStdin, Command, Stdio, Child};
use std::sync::mpsc::{self, Receiver};
use std::thread;

/// Eventos normalizados que el hilo de comunicación entrega a la aplicación.
#[derive(Debug, Clone)]
pub enum LspMessage {
    #[allow(dead_code)]
    /// Evento informativo que no requiere una respuesta del cliente.
    Notification { method: String, params: Value },
    /// Resultado asociado a una petición enviada anteriormente.
    Response { id: u64, result: Value },
    /// Diagnósticos publicados para un documento abierto.
    Diagnostics(PublishDiagnosticsParams),
    /// Fallo de transporte o mensaje que no se pudo clasificar.
    Error(String),
}

/// Adaptador entre la aplicación y un servidor de lenguaje externo.
///
/// El protocolo JSON-RPC viaja por los streams del proceso y un hilo dedicado
/// lee las respuestas para mantener libre el bucle de eventos de la interfaz.
pub struct LspClient {
    stdin: ChildStdin,
    child_process: Child,
    pub receiver: Receiver<LspMessage>,
    next_id: u64,
    /// Identificador de la petición inicial, necesario para completar el handshake.
    pub init_id: u64,
    pub is_initialized: bool,
}

impl LspClient {
    /// Selecciona y arranca el servidor compatible con una extensión.
    /// Devuelve `None` cuando no existe un servidor soportado o instalado.
    pub fn start_for_extension(ext: &str, workspace_root: PathBuf) -> Option<Self> {
        let (cmd, args) = match ext {
            "rs" if is_in_path("rust-analyzer") => ("rust-analyzer", vec![]),
            "c" | "cpp" | "h" if is_in_path("clangd") => ("clangd", vec![]),
            "py" if is_in_path("pyright-langserver") => ("pyright-langserver", vec!["--stdio"]),
            "py" if is_in_path("pylsp") => ("pylsp", vec![]),
            _ => return None,
        };

        let mut process = std::process::Command::new(cmd)
            .args(&args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .ok()?;

        let stdin = process.stdin.take()?;
        let stdout = process.stdout.take()?;

        let (tx, rx) = std::sync::mpsc::channel();

        std::thread::spawn(move || {
            let mut reader = std::io::BufReader::new(stdout);
            loop {
                let mut line = String::new();
                if std::io::BufRead::read_line(&mut reader, &mut line).is_err() || line.is_empty() {
                    let _ = tx.send(LspMessage::Error("LSP process died".into()));
                    break;
                }

                if line.starts_with("Content-Length: ") {
                    let len_str = line.trim_start_matches("Content-Length: ").trim();
                    if let Ok(len) = len_str.parse::<usize>() {
                        let mut empty_line = String::new();
                        let _ = std::io::BufRead::read_line(&mut reader, &mut empty_line);
                        let mut payload = vec![0; len];
                        if std::io::Read::read_exact(&mut reader, &mut payload).is_ok() {
                            if let Ok(msg) = serde_json::from_slice::<serde_json::Value>(&payload) {
                                let _ = tx.send(parse_rpc_message(msg));
                            }
                        }
                    }
                }
            }
        });

        let mut client = Self {
            stdin,
            child_process: process,
            receiver: rx,
            next_id: 1,
            init_id: 0,
            is_initialized: false,
        };

        client.init_id = client.initialize(workspace_root);
        Some(client)
    }

    /// Envía una petición JSON-RPC identificable y devuelve su nuevo ID.
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

    /// Envía un evento JSON-RPC que no necesita respuesta.
    pub fn send_notification(&mut self, method: &str, params: Value) {
        let notification = json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params
        });

        self.write_message(&notification);
    }

    /// Serializa un mensaje y aplica el framing requerido por LSP.
    fn write_message(&mut self, msg: &Value) {
        let json_str = msg.to_string();
        let payload = format!("Content-Length: {}\r\n\r\n{}", json_str.len(), json_str);
        let _ = self.stdin.write_all(payload.as_bytes());
        let _ = self.stdin.flush();
    }

    /// Inicia el handshake LSP describiendo el proceso y la raíz del workspace.
    fn initialize(&mut self, workspace_root: PathBuf) -> u64 {
        let abs_root = if workspace_root.is_absolute() {
            workspace_root
        } else {
            std::env::current_dir().unwrap_or_default().join(workspace_root)
        };
        let canonical_root = std::fs::canonicalize(&abs_root).unwrap_or(abs_root);

        let file_url = url::Url::from_file_path(&canonical_root)
            .unwrap_or_else(|_| url::Url::parse("file:///").unwrap());
        let uri: Uri = file_url.as_str().parse().unwrap();

        let mut capabilities = ClientCapabilities::default();
        capabilities.text_document = Some(lsp_types::TextDocumentClientCapabilities {
            completion: Some(lsp_types::CompletionClientCapabilities {
                completion_item: Some(lsp_types::CompletionItemCapability {
                    snippet_support: Some(false),
                    resolve_support: Some(lsp_types::CompletionItemCapabilityResolveSupport {
                        properties: vec!["documentation".to_string(), "detail".to_string()],
                    }),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            synchronization: Some(lsp_types::TextDocumentSyncClientCapabilities {
                did_save: Some(true),
                ..Default::default()
            }),
            ..Default::default()
        });

        #[allow(deprecated)]
        let params = InitializeParams {
            process_id: Some(std::process::id()),
            root_path: Some(canonical_root.to_string_lossy().into_owned()),
            root_uri: Some(uri),
            initialization_options: None,
            capabilities,
            trace: None,
            workspace_folders: None,
            client_info: None,
            locale: None,
            work_done_progress_params: WorkDoneProgressParams::default(),
        };

        self.send_request("initialize", serde_json::to_value(params).unwrap())
    }

    /// Completa el handshake después de recibir la respuesta de inicialización.
    pub fn send_initialized(&mut self) {
        self.send_notification("initialized", json!({}));
    }

   /// Registra en el servidor un documento abierto y su contenido inicial.
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

    /// Publica el contenido completo del documento después de una edición.
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

    /// Solicita sugerencias de autocompletado para una posición del documento.
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

    pub fn did_change_incremental(&mut self, uri: Uri, version: i32, start: (u32, u32), end: (u32, u32), text: String) {
        let params = DidChangeTextDocumentParams {
            text_document: VersionedTextDocumentIdentifier { uri, version },
            content_changes: vec![TextDocumentContentChangeEvent {
                range: Some(lsp_types::Range {
                    start: Position { line: start.0, character: start.1 },
                    end: Position { line: end.0, character: end.1 },
                }),
                range_length: None,
                text,
            }],
        };
        self.send_notification("textDocument/didChange", serde_json::to_value(params).unwrap());
    }

    pub fn request_inlay_hints(&mut self, uri: Uri, range: lsp_types::Range) -> u64 {
        let params = lsp_types::InlayHintParams {
            work_done_progress_params: Default::default(),
            text_document: TextDocumentIdentifier { uri },
            range,
        };
        self.send_request("textDocument/inlayHint", serde_json::to_value(params).unwrap())
    }

    pub fn shutdown_and_exit(&mut self) {
        self.send_request("shutdown", json!(null));
        self.send_notification("exit", json!(null));
        let _ = self.child_process.kill();
        let _ = self.child_process.wait();
    }
}

/// Comprueba directamente si un ejecutable está disponible en el PATH.
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

/// Convierte un valor JSON-RPC en el evento que entiende la aplicación.
fn parse_rpc_message(val: Value) -> LspMessage {
   if let Some(id) = val.get("id").and_then(|i| i.as_u64()) {
       if let Some(result) = val.get("result") {
           return LspMessage::Response {id, result: result.clone()};
       } else if val.get("error").is_some() {
           return LspMessage::Response {id, result: serde_json::Value::Null};
       }
   }

    if let Some(method) = val.get("method").and_then(|m| m.as_str()) {
        if method == "textDocument/publishDiagnostics" {
            if let Ok(params) =serde_json::from_value::<PublishDiagnosticsParams>(val["params"].clone()) {
                return LspMessage::Diagnostics(params);
            }
        }
        return LspMessage::Notification {
            method: method.to_string(),
            params: val["params"].clone(),
        };
    }
    LspMessage::Error("Invalid JSON-RPC message".into())
}