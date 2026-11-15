//! The HTTP transport for the MCP server.
//!
//! Stdio is what a host that launches the server expects. HTTP is what a
//! host that connects to a shared server expects — an IDE that already has
//! the agent running, a remote host, a container that outlives the client.
//!
//! The transport is deliberately small: HTTP/1.1, one JSON-RPC message per
//! request, and no dependency beyond the runtime's own tokio. `POST /` takes
//! a JSON-RPC 2.0 message and answers with the JSON-RPC response. `GET
//! /health` answers with a plain `ok`, so a supervisor can tell a live server
//! from a dead one without speaking the protocol.

use std::sync::Arc;

use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use crate::mcp_server::server::McpServer;

/// The default port the HTTP transport binds.
pub const DEFAULT_HTTP_PORT: u16 = 7781;

/// The largest request body the transport accepts.
const MAX_BODY_BYTES: usize = 8 * 1024 * 1024;

/// Serve MCP over HTTP until the process ends.
///
/// Binds `127.0.0.1:port` unless a host is given: the server runs the
/// agent's own tools, so it must not be reachable from the network by
/// accident.
pub async fn serve_http(server: McpServer, host: &str, port: u16) -> anyhow::Result<()> {
    let address = format!("{host}:{port}");
    let listener = TcpListener::bind(&address)
        .await
        .map_err(|error| anyhow::anyhow!("could not bind {address}: {error}"))?;
    let local = listener.local_addr()?;
    eprintln!("[enact-agent] MCP HTTP transport listening on http://{local}");
    let server = Arc::new(server);
    loop {
        let (stream, _peer) = listener
            .accept()
            .await
            .map_err(|error| anyhow::anyhow!("accept failed: {error}"))?;
        let server = Arc::clone(&server);
        tokio::spawn(async move {
            if let Err(error) = handle_connection(server, stream).await {
                eprintln!("[enact-agent] MCP HTTP connection failed: {error}");
            }
        });
    }
}

/// One request: the method, the path, and the body.
struct Request {
    method: String,
    path: String,
    body: Vec<u8>,
}

/// Read one HTTP request off the connection.
async fn read_request(stream: &mut TcpStream) -> anyhow::Result<Request> {
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 4096];
    let header_end = loop {
        if let Some(index) = find_header_end(&buffer) {
            break index;
        }
        let read = stream.read(&mut chunk).await?;
        if read == 0 {
            anyhow::bail!("the connection closed before the request headers arrived");
        }
        buffer.extend_from_slice(&chunk[..read]);
        if buffer.len() > MAX_BODY_BYTES {
            anyhow::bail!("the request headers are too large");
        }
    };

    let headers = String::from_utf8_lossy(&buffer[..header_end]).to_string();
    let mut lines = headers.lines();
    let request_line = lines.next().unwrap_or_default();
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_string();
    let path = parts.next().unwrap_or_default().to_string();

    let mut content_length = 0usize;
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        if name.trim().eq_ignore_ascii_case("content-length") {
            content_length = value.trim().parse().unwrap_or(0);
        }
    }
    if content_length > MAX_BODY_BYTES {
        anyhow::bail!("the request body is too large");
    }

    let body_start = header_end + 4;
    let mut body = buffer[body_start.min(buffer.len())..].to_vec();
    while body.len() < content_length {
        let read = stream.read(&mut chunk).await?;
        if read == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..read]);
    }
    body.truncate(content_length);
    Ok(Request { method, path, body })
}

/// The index of the CRLFCRLF that ends the headers.
fn find_header_end(buffer: &[u8]) -> Option<usize> {
    buffer.windows(4).position(|window| window == b"\r\n\r\n")
}

/// Write a response with a JSON body.
async fn write_json(stream: &mut TcpStream, status: &str, body: &Value) -> anyhow::Result<()> {
    let payload = serde_json::to_vec(body)?;
    let head = format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        payload.len()
    );
    stream.write_all(head.as_bytes()).await?;
    stream.write_all(&payload).await?;
    stream.flush().await?;
    Ok(())
}

/// Write a response with a plain-text body.
async fn write_text(stream: &mut TcpStream, status: &str, body: &str) -> anyhow::Result<()> {
    let head = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes()).await?;
    stream.write_all(body.as_bytes()).await?;
    stream.flush().await?;
    Ok(())
}

/// Serve one connection.
async fn handle_connection(server: Arc<McpServer>, mut stream: TcpStream) -> anyhow::Result<()> {
    let request = match read_request(&mut stream).await {
        Ok(request) => request,
        Err(error) => {
            let _ = write_text(&mut stream, "400 Bad Request", &format!("{error}\n")).await;
            return Err(error);
        }
    };

    match (request.method.as_str(), request.path.as_str()) {
        ("GET", "/health") | ("GET", "/healthz") => {
            // The health check reports the tool count, so a supervisor can
            // see that a live server is serving the registry it expects and
            // not an empty one.
            let tools = server.surface().len();
            write_text(&mut stream, "200 OK", &format!("ok\ntools: {tools}\n")).await?;
            return Ok(());
        }
        ("POST", "/") | ("POST", "/mcp") => {}
        ("POST", _) => {
            write_text(&mut stream, "404 Not Found", "no such endpoint\n").await?;
            return Ok(());
        }
        (_, _) => {
            write_text(&mut stream, "405 Method Not Allowed", "use POST\n").await?;
            return Ok(());
        }
    }

    let message: Value = match serde_json::from_slice(&request.body) {
        Ok(message) => message,
        Err(error) => {
            write_json(
                &mut stream,
                "400 Bad Request",
                &json!({
                    "jsonrpc": "2.0",
                    "id": Value::Null,
                    "error": { "code": -32700, "message": format!("parse error: {error}") }
                }),
            )
            .await?;
            return Ok(());
        }
    };

    match server.handle(message).await {
        Some(response) => write_json(&mut stream, "200 OK", &response).await?,
        // A notification has no response; 204 says so without a body.
        None => {
            stream
                .write_all(b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                .await?;
            stream.flush().await?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_header_end_is_found() {
        assert_eq!(find_header_end(b"GET / HTTP/1.1\r\n\r\nbody"), Some(14));
        assert_eq!(find_header_end(b"GET / HTTP/1.1\r\n"), None);
    }

    #[tokio::test]
    async fn a_health_check_answers_ok() {
        let server = Arc::new(McpServer::new(
            crate::mcp_server::surface::ToolSurface::empty(),
            crate::mcp_server::server::Expose::Minimal,
        ));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            handle_connection(server, stream).await.unwrap();
        });
        let mut stream = TcpStream::connect(address).await.unwrap();
        stream
            .write_all(b"GET /health HTTP/1.1\r\nHost: x\r\n\r\n")
            .await
            .unwrap();
        let mut response = Vec::new();
        stream.read_to_end(&mut response).await.unwrap();
        let text = String::from_utf8_lossy(&response);
        assert!(text.starts_with("HTTP/1.1 200 OK"), "{text}");
        assert!(text.contains("ok"), "{text}");
    }

    #[tokio::test]
    async fn a_post_is_answered_with_the_json_rpc_response() {
        let server = Arc::new(McpServer::new(
            crate::mcp_server::surface::ToolSurface::empty(),
            crate::mcp_server::server::Expose::Minimal,
        ));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            handle_connection(server, stream).await.unwrap();
        });
        let mut stream = TcpStream::connect(address).await.unwrap();
        let body = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#;
        let head = format!(
            "POST / HTTP/1.1\r\nHost: x\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
            body.len()
        );
        stream.write_all(head.as_bytes()).await.unwrap();
        stream.write_all(body.as_bytes()).await.unwrap();
        let mut response = Vec::new();
        stream.read_to_end(&mut response).await.unwrap();
        let text = String::from_utf8_lossy(&response);
        assert!(text.starts_with("HTTP/1.1 200 OK"), "{text}");
        assert!(text.contains("\"protocolVersion\""), "{text}");
    }

    #[tokio::test]
    async fn an_unknown_path_is_not_found() {
        let server = Arc::new(McpServer::new(
            crate::mcp_server::surface::ToolSurface::empty(),
            crate::mcp_server::server::Expose::Minimal,
        ));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            handle_connection(server, stream).await.unwrap();
        });
        let mut stream = TcpStream::connect(address).await.unwrap();
        stream
            .write_all(b"POST /nope HTTP/1.1\r\nHost: x\r\nContent-Length: 0\r\n\r\n")
            .await
            .unwrap();
        let mut response = Vec::new();
        stream.read_to_end(&mut response).await.unwrap();
        let text = String::from_utf8_lossy(&response);
        assert!(text.starts_with("HTTP/1.1 404 Not Found"), "{text}");
    }

    #[tokio::test]
    async fn a_malformed_body_is_a_parse_error() {
        let server = Arc::new(McpServer::new(
            crate::mcp_server::surface::ToolSurface::empty(),
            crate::mcp_server::server::Expose::Minimal,
        ));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            handle_connection(server, stream).await.unwrap();
        });
        let mut stream = TcpStream::connect(address).await.unwrap();
        let body = "{not json";
        let head = format!(
            "POST / HTTP/1.1\r\nHost: x\r\nContent-Length: {}\r\n\r\n",
            body.len()
        );
        stream.write_all(head.as_bytes()).await.unwrap();
        stream.write_all(body.as_bytes()).await.unwrap();
        let mut response = Vec::new();
        stream.read_to_end(&mut response).await.unwrap();
        let text = String::from_utf8_lossy(&response);
        assert!(text.starts_with("HTTP/1.1 400 Bad Request"), "{text}");
        assert!(text.contains("-32700"), "{text}");
    }
}
