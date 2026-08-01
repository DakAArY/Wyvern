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
            // Los mensajes de diagnóstico del proceso no deben interferir con
            // la terminal alternativa que utiliza la interfaz.
            .stderr(Stdio::null())
            .spawn()
            .ok()?;

        let stdin = process.stdin.take()?;
        let stdout = process.stdout.take()?;

        let (tx, rx) = mpsc::channel();

        // El lector reconstruye los mensajes delimitados por Content-Length
        // y los publica ya deserializados para que el hilo principal no bloquee.
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
