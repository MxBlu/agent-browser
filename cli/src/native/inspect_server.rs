use std::sync::Arc;

use futures_util::{SinkExt, StreamExt};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tokio::sync::Mutex;
use tokio_tungstenite::tungstenite::Message;

use super::cdp::client::InspectProxyHandle;

/// Lightweight HTTP + WebSocket server for `agent-browser inspect`.
///
/// Serves two purposes:
/// - `GET /` redirects to Chrome's built-in DevTools frontend with `ws=` pointing to this server
/// - WebSocket connections proxy CDP messages through the daemon's existing browser-level
///   connection, injecting/stripping `sessionId` so the DevTools frontend sees a page-level view
pub struct InspectServer {
    port: u16,
    _handle: tokio::task::JoinHandle<()>,
}

impl InspectServer {
    /// Start the inspect proxy server.
    ///
    /// - `proxy_handle`: lightweight handle for sending/receiving raw CDP messages
    /// - `session_id`: the flattened session ID for the active page target
    /// - `chrome_host_port`: the Chrome debug server address (e.g. "127.0.0.1:9222")
    pub async fn start(
        proxy_handle: InspectProxyHandle,
        session_id: String,
        chrome_host_port: String,
    ) -> Result<Self, String> {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .map_err(|e| format!("Failed to bind inspect server: {}", e))?;
        let port = listener
            .local_addr()
            .map_err(|e| format!("Failed to get local addr: {}", e))?
            .port();

        let proxy = Arc::new(proxy_handle);

        let handle = tokio::spawn(accept_loop(
            listener,
            proxy,
            session_id,
            chrome_host_port,
            port,
        ));

        Ok(Self {
            port,
            _handle: handle,
        })
    }

    pub fn port(&self) -> u16 {
        self.port
    }
}

async fn accept_loop(
    listener: TcpListener,
    proxy: Arc<InspectProxyHandle>,
    session_id: String,
    chrome_host_port: String,
    proxy_port: u16,
) {
    loop {
        let (stream, _) = match listener.accept().await {
            Ok(s) => s,
            Err(_) => continue,
        };

        let proxy = proxy.clone();
        let sid = session_id.clone();
        let chp = chrome_host_port.clone();

        tokio::spawn(async move {
            if let Err(e) = handle_connection(stream, proxy, sid, chp, proxy_port).await {
                eprintln!("[inspect] connection error: {}", e);
            }
        });
    }
}

async fn handle_connection(
    stream: tokio::net::TcpStream,
    proxy: Arc<InspectProxyHandle>,
    session_id: String,
    chrome_host_port: String,
    proxy_port: u16,
) -> Result<(), String> {
    // Peek at the request line to determine routing WITHOUT consuming bytes.
    // This is critical: tokio_tungstenite::accept_async needs to read the full
    // HTTP upgrade request itself, so we must not consume anything for WS paths.
    let mut peek_buf = [0u8; 32];
    let n = stream
        .peek(&mut peek_buf)
        .await
        .map_err(|e| e.to_string())?;
    let peek = String::from_utf8_lossy(&peek_buf[..n]);

    if peek.starts_with("GET /ws") {
        return handle_ws_proxy(stream, proxy, session_id).await;
    }

    if peek.starts_with("GET / ") {
        let buf_reader = BufReader::new(stream);
        return handle_http_redirect(buf_reader, chrome_host_port, proxy_port).await;
    }

    // Unknown request -- consume and respond 404
    let mut stream = stream;
    let mut discard = [0u8; 4096];
    let _ = stream.read(&mut discard).await;
    let resp = "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
    stream
        .write_all(resp.as_bytes())
        .await
        .map_err(|e| e.to_string())?;
    Ok(())
}

async fn handle_http_redirect(
    buf_reader: BufReader<tokio::net::TcpStream>,
    chrome_host_port: String,
    proxy_port: u16,
) -> Result<(), String> {
    let mut br = buf_reader;
    loop {
        let mut line = String::new();
        br.read_line(&mut line).await.map_err(|e| e.to_string())?;
        if line == "\r\n" || line == "\n" || line.is_empty() {
            break;
        }
    }

    let location = format!(
        "http://{}/devtools/devtools_app.html?ws=127.0.0.1:{}/ws",
        chrome_host_port, proxy_port
    );
    let body = format!(
        "<html><body>Redirecting to <a href=\"{url}\">{url}</a></body></html>",
        url = location
    );
    let resp = format!(
        "HTTP/1.1 302 Found\r\n\
         Location: {}\r\n\
         Content-Type: text/html\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\r\n\
         {}",
        location,
        body.len(),
        body
    );
    let mut stream = br.into_inner();
    stream
        .write_all(resp.as_bytes())
        .await
        .map_err(|e| e.to_string())?;
    Ok(())
}

async fn handle_ws_proxy(
    stream: tokio::net::TcpStream,
    proxy: Arc<InspectProxyHandle>,
    session_id: String,
) -> Result<(), String> {
    let ws_stream = tokio_tungstenite::accept_async(stream)
        .await
        .map_err(|e| format!("WebSocket handshake failed: {}", e))?;

    let (ws_tx, mut ws_rx) = ws_stream.split();
    let ws_tx = Arc::new(Mutex::new(ws_tx));

    let mut raw_rx = proxy.subscribe_raw();
    let ws_tx_clone = ws_tx.clone();
    let session_id_clone = session_id.clone();

    // Chrome -> DevTools: forward messages matching our session, strip sessionId
    let chrome_to_devtools = tokio::spawn(async move {
        while let Ok(raw_msg) = raw_rx.recv().await {
            if raw_msg.session_id.as_deref() != Some(&session_id_clone) {
                continue;
            }

            let stripped = strip_session_id(&raw_msg.text);

            let mut tx = ws_tx_clone.lock().await;
            if tx.send(Message::Text(stripped)).await.is_err() {
                break;
            }
        }
    });

    // DevTools -> Chrome: inject sessionId and forward
    let proxy_for_send = proxy.clone();
    let devtools_to_chrome = tokio::spawn(async move {
        while let Some(Ok(msg)) = ws_rx.next().await {
            let text = match msg {
                Message::Text(t) => t,
                Message::Close(_) => break,
                _ => continue,
            };

            let injected = inject_session_id(&text, &session_id);
            if proxy_for_send.send_raw(injected).await.is_err() {
                break;
            }
        }
    });

    tokio::select! {
        _ = chrome_to_devtools => {},
        _ = devtools_to_chrome => {},
    }

    Ok(())
}

fn inject_session_id(json: &str, session_id: &str) -> String {
    if let Some(pos) = json.find('{') {
        let mut result = String::with_capacity(json.len() + session_id.len() + 20);
        result.push_str(&json[..=pos]);
        result.push_str("\"sessionId\":\"");
        result.push_str(session_id);
        result.push_str("\",");
        result.push_str(&json[pos + 1..]);
        result
    } else {
        json.to_string()
    }
}

fn strip_session_id(json: &str) -> String {
    if let Ok(mut val) = serde_json::from_str::<serde_json::Value>(json) {
        if let Some(obj) = val.as_object_mut() {
            obj.remove("sessionId");
        }
        serde_json::to_string(&val).unwrap_or_else(|_| json.to_string())
    } else {
        json.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_inject_session_id() {
        let input = r#"{"id":1,"method":"DOM.getDocument"}"#;
        let result = inject_session_id(input, "abc123");
        assert!(result.contains("\"sessionId\":\"abc123\""));
        assert!(result.contains("\"method\":\"DOM.getDocument\""));
    }

    #[test]
    fn test_strip_session_id() {
        let input = r#"{"id":1,"result":{},"sessionId":"abc123"}"#;
        let result = strip_session_id(input);
        assert!(!result.contains("sessionId"));
        assert!(result.contains("\"id\":1"));
    }
}
