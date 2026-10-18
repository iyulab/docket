//! Drives the built `docket-mcp` binary over stdio the way an MCP client
//! does, to pin down the handshake sequences clients actually send.
//!
//! None of these requests reach `docket-core`: `initialize`, `server/discover`
//! and `tools/list` are answered by the server itself, so the tests need no
//! running core.

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use serde_json::{Value, json};

struct Server {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

impl Server {
    fn spawn() -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_docket-mcp"))
            // Unroutable on purpose: nothing here may depend on a live core.
            .env("DOCKET_CORE_URL", "http://127.0.0.1:9")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn docket-mcp");
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        Server {
            child,
            stdin,
            stdout,
        }
    }

    fn send(&mut self, message: Value) {
        writeln!(self.stdin, "{message}").unwrap();
        self.stdin.flush().unwrap();
    }

    /// Reads lines until the response carrying `id`, skipping notifications.
    fn response(&mut self, id: u64) -> Value {
        loop {
            let mut line = String::new();
            let read = self.stdout.read_line(&mut line).unwrap();
            assert!(read > 0, "server closed stdout before answering id {id}");
            let message: Value = serde_json::from_str(&line).unwrap();
            if message["id"] == json!(id) {
                return message;
            }
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn initialize(id: u64) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "initialize",
        "params": {
            "protocolVersion": "2025-11-25",
            "capabilities": {},
            "clientInfo": { "name": "lifecycle-test", "version": "0" }
        }
    })
}

fn initialized() -> Value {
    json!({ "jsonrpc": "2.0", "method": "notifications/initialized" })
}

fn tools_list(id: u64) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "method": "tools/list", "params": {} })
}

fn tool_count(response: &Value) -> usize {
    assert!(
        response.get("error").is_none(),
        "tools/list failed: {response}"
    );
    response["result"]["tools"].as_array().unwrap().len()
}

#[test]
fn initialize_then_tools_list() {
    let mut server = Server::spawn();
    server.send(initialize(1));
    let init = server.response(1);
    // serverInfo names this binary and its release, not the MCP SDK — the
    // one place a client can see which docket-mcp version a launcher started.
    assert_eq!(init["result"]["serverInfo"]["name"], "docket-mcp");
    assert_eq!(
        init["result"]["serverInfo"]["version"],
        env!("CARGO_PKG_VERSION")
    );
    server.send(initialized());
    server.send(tools_list(2));
    assert!(tool_count(&server.response(2)) > 0);
}

/// A client that speaks the 2026-07-28 lifecycle probes with
/// `server/discover` first and falls back to `initialize` when it settles on
/// an older version. The probe must not leave the session demanding
/// per-request `_meta`: the follow-up `tools/list` carries none, as the
/// initialize lifecycle allows. With rmcp 3.5.0 it was rejected with
/// "request _meta is missing or has malformed required fields" — the client
/// showed the server as connected with zero tools.
#[test]
fn discover_probe_then_initialize_then_tools_list() {
    let mut server = Server::spawn();
    server.send(json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "server/discover",
        "params": {
            "_meta": {
                "io.modelcontextprotocol/protocolVersion": "2026-07-28",
                "io.modelcontextprotocol/clientInfo": { "name": "lifecycle-test", "version": "0" },
                "io.modelcontextprotocol/clientCapabilities": {}
            }
        }
    }));
    server.response(1);
    server.send(initialize(2));
    assert!(server.response(2).get("result").is_some());
    server.send(initialized());
    server.send(tools_list(3));
    assert!(tool_count(&server.response(3)) > 0);
}
