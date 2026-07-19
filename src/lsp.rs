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

#[derive(Debug, Clone)]
pub enum LspMessage {
    Notification { method: String, params: Value },
    Response { id: u64, result: Value },
    Diagnostics(PublishDiagnosticsParams),
    Error(String),
}

pub struct LspClient {
    stdin: ChildStdin,
    pub receiver: Receiver<LspMessage>,
    next_id: u64,
    pub init_id: u64,
    pub is_initialized: bool,
}

impl LspClient {
    pub fn start_for_extension(ext: &str, workspace_root: PathBuf) -> Option<Self> {
        // El servidor a lanzar se determina por extensión de archivo,
        // comprobando cuáles están disponibles en el PATH del sistema.
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
            // stderr se descarta para no interferir con el renderizado de la TUI.
            .stderr(Stdio::null())
            .spawn()
            .ok()?;

        let stdin = process.stdin.take()?;
        let stdout = process.stdout.take()?;

        let (tx, rx) = mpsc::channel();

        // Hilo en segundo plano encargado de leer stdout del servidor LSP
        // y parsear los mensajes JSON-RPC entrantes.
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

    pub fn send_notification(&mut self, method: &str, params: Value) {
        let notification = json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params
        });

        self.write_message(&notification);
    }

    fn write_message(&mut self, msg: &Value) {
        let json_str = msg.to_string();
        let payload = format!("Content-Length: {}\r\n\r\n{}", json_str.len(), json_str);
        let _ = self.stdin.write_all(payload.as_bytes());
        let _ = self.stdin.flush();
    }

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

    pub fn send_initialized(&mut self) {
        self.send_notification("initialized", json!({}));
    }

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

// Comprueba si un ejecutable está disponible en el PATH, de forma multiplataforma.
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
