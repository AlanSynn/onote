//! `ShareServer` via `axum` (`CLAUDE.md` §2.8, §3.1 Share).
//!
//! Read-only HTTP delivery of a note snapshot. Security model (§3.1 "Share URL
//! should be tokenized", "read-only delivery"):
//!
//! - **Every** route is token-gated. The token is compared in constant time and
//!   a mismatch returns `404` (the token's existence is never confirmed).
//! - Only two things are ever served: the in-memory snapshot HTML at
//!   `GET /:token`, and attachment files at `GET /:token/<rest>`. There is **no**
//!   static-file fallback over the vault root — `.git/`, `.obsidian/`, and
//!   `.onote/index.sqlite` are unreachable.
//! - Attachment serving is confined to `attachment_dir`, canonicalized, and
//!   rejects dotfiles/traversal/symlink-escape.
//! - Snapshot HTML is rendered with raw-HTML disabled (see `markdown.rs`) and
//!   the title is HTML-escaped, so note content cannot inject script.
//!
//! One share at a time; `stop()` drops the oneshot sender → graceful shutdown.

use std::net::UdpSocket;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::extract::{Path as AxumPath, State};
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use tokio::runtime::Runtime;
use tokio::sync::oneshot;

use crate::domain::errors::ShareError;
use crate::domain::share::{SharePolicy, ShareSession, ShareSnapshot, ShareToken};
use crate::domain::vault::RelativeNotePath;
use crate::ports::ShareServer;

struct Running {
    /// Dropping this sender completes the graceful-shutdown signal.
    /// (Never "read": its `Drop` is the shutdown trigger.)
    #[allow(dead_code)]
    shutdown: oneshot::Sender<()>,
    local_url: String,
}

/// Immutable data shared with every request handler.
struct ShareData {
    token: String,
    html: String,
    vault_root: PathBuf,
    /// Relative attachment dir (e.g. `Attachments`). Attachments are confined here.
    attachment_dir: PathBuf,
}

pub struct HttpShareServer {
    runtime: Runtime,
    state: Mutex<Option<Running>>,
    vault_root: PathBuf,
}

impl HttpShareServer {
    pub fn new(vault_root: PathBuf) -> Result<Self, ShareError> {
        let runtime = Runtime::new().map_err(|e| ShareError::Server(format!("runtime: {e}")))?;
        Ok(Self {
            runtime,
            state: Mutex::new(None),
            vault_root,
        })
    }
}

impl ShareServer for HttpShareServer {
    fn start(
        &self,
        snapshot: ShareSnapshot,
        policy: SharePolicy,
    ) -> Result<ShareSession, ShareError> {
        let mut guard = self
            .state
            .lock()
            .map_err(|e| ShareError::Server(e.to_string()))?;
        if guard.is_some() {
            return Err(ShareError::AlreadyRunning);
        }

        let token = random_token()?;
        let bind_addr = if policy.allow_lan {
            format!("0.0.0.0:{}", policy.port)
        } else {
            format!("127.0.0.1:{}", policy.port)
        };

        // Bind synchronously so port/permission errors surface here, not in a thread.
        let listener = self
            .runtime
            .block_on(async { tokio::net::TcpListener::bind(&bind_addr).await })
            .map_err(|e| ShareError::Server(format!("bind {bind_addr}: {e}")))?;
        let actual_port = listener
            .local_addr()
            .map_err(|e| ShareError::Server(e.to_string()))?
            .port();

        let local_url = format!("http://127.0.0.1:{actual_port}/{}", token.as_str());
        let lan_url = if policy.allow_lan {
            lan_ip().map(|ip| format!("http://{ip}:{actual_port}/{}", token.as_str()))
        } else {
            None
        };

        let data = Arc::new(ShareData {
            token: token.as_str().to_string(),
            html: wrap_html(&snapshot.title, &snapshot.html, token.as_str()),
            vault_root: self.vault_root.clone(),
            attachment_dir: PathBuf::from(&snapshot.attachment_dir),
        });
        let app = Router::new()
            .route("/{token}", get(note_html))
            .route("/{token}/{*path}", get(attachment))
            // No static-file fallback: anything unmatched is a 404.
            .fallback(|| async { StatusCode::NOT_FOUND })
            .with_state(data);

        let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
        self.runtime.spawn(async move {
            if let Err(e) = axum::serve(listener, app)
                .with_graceful_shutdown(async {
                    let _ = shutdown_rx.await;
                })
                .await
            {
                tracing::warn!("share server stopped with error: {e}");
            }
        });

        *guard = Some(Running {
            shutdown: shutdown_tx,
            local_url: local_url.clone(),
        });

        Ok(ShareSession {
            id: local_url.clone(),
            token,
            local_url,
            lan_url,
        })
    }

    fn stop(&self) -> Result<(), ShareError> {
        let mut guard = self
            .state
            .lock()
            .map_err(|e| ShareError::Server(e.to_string()))?;
        if guard.is_none() {
            return Err(ShareError::NotRunning);
        }
        // Dropping the sender resolves the shutdown future → server exits.
        *guard = None;
        Ok(())
    }

    fn local_url(&self) -> Option<String> {
        self.state
            .lock()
            .ok()
            .and_then(|g| g.as_ref().map(|r| r.local_url.clone()))
    }
}

// ── Handlers ────────────────────────────────────────────────────────────────

/// `GET /:token` → the pre-rendered snapshot HTML. Token is compared in
/// constant time; a mismatch returns 404 (never reveals whether the token is
/// valid).
async fn note_html(AxumPath(t): AxumPath<String>, State(st): State<Arc<ShareData>>) -> Response {
    if !ct_eq(t.as_bytes(), st.token.as_bytes()) {
        return StatusCode::NOT_FOUND.into_response();
    }
    // Defense-in-depth: block any script entirely. `default-src 'none'`
    // denies inline/external script + framing; `img-src 'self' data:'`
    // serves token-gated attachments + inline data images; `style-src
    // 'unsafe-inline''` permits the document `<style>` block (no script).
    let mut resp = Html(st.html.clone()).into_response();
    resp.headers_mut().insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(
            "default-src 'none'; img-src 'self' data:; style-src 'unsafe-inline'",
        ),
    );
    resp
}

/// `GET /:token/<rest>` → an attachment file, confined to `attachment_dir`.
async fn attachment(
    AxumPath((t, sub)): AxumPath<(String, String)>,
    State(st): State<Arc<ShareData>>,
) -> Response {
    if !ct_eq(t.as_bytes(), st.token.as_bytes()) {
        return StatusCode::NOT_FOUND.into_response();
    }
    serve_attachment(&st, &sub)
}

/// Resolve and serve a single attachment, defending the vault boundary:
/// dotfiles/traversal rejected, path canonicalized and required to stay under
/// `vault_root/attachment_dir` (defeats symlink escape).
fn serve_attachment(st: &ShareData, sub: &str) -> Response {
    // Reject any dotfile/dotdir segment (`.git`, `.env`, `..`, …).
    if sub.split(['/', '\\']).any(|s| s.starts_with('.')) {
        return StatusCode::NOT_FOUND.into_response();
    }
    let rel = match RelativeNotePath::from_user(sub) {
        Ok(r) => r,
        Err(_) => return StatusCode::NOT_FOUND.into_response(),
    };

    let canon_root = match st.vault_root.canonicalize() {
        Ok(c) => c,
        Err(_) => return StatusCode::NOT_FOUND.into_response(),
    };
    let attach_root = canon_root.join(&st.attachment_dir);
    // `rel` is a full vault-relative path (e.g. `Attachments/seal.png`); resolve
    // it under the vault root, then confine to the attachment dir.
    let candidate = canon_root.join(rel.as_path());

    // Canonicalize (follows symlinks) and require the result to remain under
    // the attachment dir. A symlink planted in the vault pointing outside is
    // rejected here.
    let canon = match candidate.canonicalize() {
        Ok(c) => c,
        Err(_) => return StatusCode::NOT_FOUND.into_response(),
    };
    if !canon.starts_with(&attach_root) {
        return StatusCode::NOT_FOUND.into_response();
    }

    let bytes = match std::fs::read(&canon) {
        Ok(b) => b,
        Err(_) => return StatusCode::NOT_FOUND.into_response(),
    };
    let mut resp = Response::new(Body::from(bytes));
    resp.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static(content_type(&canon)),
    );
    // Defense-in-depth: forbid MIME sniffing on the opaque octet-stream we
    // serve for unknown/unsupported types.
    resp.headers_mut().insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    resp
}

/// Best-effort MIME from extension; unknown → opaque octet-stream (still
/// token-gated, so no information leaks).
fn content_type(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        // SVG deliberately omitted: serving it as `image/svg+xml` would let a
        // planted attachment execute embedded `<script>` on direct navigation in
        // the share origin. Treat SVG as opaque octet-stream so it downloads
        // rather than executes (it also renders fine via `<img>`, which is
        // sandboxed and does not run script).
        Some("png") => "image/png",
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("bmp") => "image/bmp",
        _ => "application/octet-stream",
    }
}

// ── Helpers ─────────────────────────────────────────────────────────────────

/// Constant-time byte equality. Avoids a timing oracle on the share token.
///
/// Constant-time only when `a.len() == b.len()`. Safe here because share tokens
/// are fixed-length random bytes; a future caller with variable-length secret
/// material would lose the property. The early length-mismatch exit leaks
/// nothing because the length is non-secret (16 random bytes).
fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff: u8 = 0;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// Wrap the rendered fragment in a document. `<base href="/{token}/">` rewrites
/// relative attachment links (`Attachments/…`) to the token-gated attachment
/// route. The title is HTML-escaped; body HTML is rendered with raw-HTML
/// disabled upstream, so note content cannot inject script.
fn wrap_html(title: &str, body_html: &str, token: &str) -> String {
    let safe_title = html_escape(title);
    format!(
        "<!doctype html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n\
         <base href=\"/{token}/\">\n<title>{safe_title}</title>\n<style>\n\
         body{{font:15px/1.6 -apple-system,system-ui,sans-serif;max-width:780px;margin:2rem auto;padding:0 1rem;color:#222}}\n\
         img{{max-width:100%}}pre,code{{background:#f4f4f4;border-radius:4px}}pre{{padding:.8rem;overflow:auto}}\n\
         </style>\n</head>\n<body>\n{body_html}\n</body>\n</html>\n"
    )
}

fn html_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            other => out.push(other),
        }
    }
    out
}

fn random_token() -> Result<ShareToken, ShareError> {
    // The share token gates read access to notes over LAN — it is a security
    // boundary, so a failure to source randomness must be HARD FATAL. We never
    // degrade to a near-zero-entropy time-derived token: if the OS CSPRNG is
    // unavailable, we refuse to start the share server.
    //
    // `getrandom` reads the platform CSPRNG directly (`/dev/urandom` /
    // `getentropy` on Unix, `BCryptGenRandom` on Windows), so the same path
    // works on every target — no platform-specific branch needed.
    let mut buf = [0u8; 16];
    getrandom::getrandom(&mut buf).map_err(|e| ShareError::Server(format!("getrandom: {e}")))?;
    Ok(ShareToken::from_random(&buf))
}

/// Best-effort primary LAN IPv4 via a connect-less UDP socket.
fn lan_ip() -> Option<String> {
    let sock = UdpSocket::bind("0.0.0.0:0").ok()?;
    sock.connect("8.8.8.8:80").ok()?;
    sock.local_addr().ok().map(|a| a.ip().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ct_eq_handles_mismatch_without_short_circuit() {
        assert!(ct_eq(b"abcdef", b"abcdef"));
        assert!(!ct_eq(b"abcdef", b"abcdeg"));
        assert!(!ct_eq(b"abcdef", b"abcde"));
        assert!(!ct_eq(b"abcdef", b""));
    }

    #[test]
    fn html_escape_neutralizes_title_injection() {
        let wrapped = wrap_html("</title><script>alert(1)</script>", "body", "tok");
        assert!(!wrapped.contains("<script>alert(1)</script>"));
        assert!(wrapped.contains("&lt;/title&gt;"));
    }

    #[test]
    fn base_href_uses_token() {
        let wrapped = wrap_html("t", "b", "abc123");
        assert!(wrapped.contains("<base href=\"/abc123/\">"));
    }

    /// End-to-end share-server test: bind on an ephemeral port, speak HTTP over a
    /// raw `TcpStream`, and assert the security model (token gating + vault
    /// confinement + attachment serving) holds on the wire.
    #[test]
    fn share_server_serves_attachment_and_blocks_escape() {
        use std::io::{Read, Write};
        use std::net::TcpStream;
        use std::time::Duration;

        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().to_path_buf();
        std::fs::create_dir_all(root.join("Attachments")).unwrap();
        // A real attachment + a file that must NOT be reachable.
        std::fs::write(root.join("Attachments/seal.png"), b"PNG-ATTACHMENT-BYTES").unwrap();
        std::fs::write(root.join("Secret.md"), b"top secret").unwrap();
        std::fs::create_dir_all(root.join(".onote")).unwrap();
        std::fs::write(root.join(".onote/index.sqlite"), b"sqlite-blob").unwrap();

        let snapshot = ShareSnapshot {
            note_path: RelativeNotePath::from_user("Scratch.md").unwrap(),
            title: "Scratch".into(),
            html: "<p>hello</p>".into(),
            attachment_dir: "Attachments".into(),
        };
        let server = HttpShareServer::new(root).expect("server");
        // Port 0 → OS assigns an ephemeral loopback port.
        let sess = server
            .start(snapshot, SharePolicy::new(0, false))
            .expect("start");

        // local_url = http://127.0.0.1:<port>/<token>
        let url = &sess.local_url;
        let port: u16 = url
            .rsplit('/')
            .nth(1)
            .unwrap()
            .rsplit(':')
            .next()
            .unwrap()
            .parse()
            .unwrap();
        let token = url.rsplit('/').next().unwrap();

        let http_get = |path: &str| -> (u16, Vec<u8>) {
            let mut s = TcpStream::connect(("127.0.0.1", port)).expect("connect");
            s.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
            write!(
                s,
                "GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n"
            )
            .unwrap();
            let mut buf = Vec::new();
            s.read_to_end(&mut buf).expect("read");
            let text = String::from_utf8_lossy(&buf);
            let status = text
                .lines()
                .next()
                .and_then(|l| {
                    l.split_whitespace()
                        .nth(1)
                        .and_then(|c| c.parse::<u16>().ok())
                })
                .unwrap_or(0);
            let sep = b"\r\n\r\n";
            let body = buf
                .windows(4)
                .position(|w| w == sep)
                .map(|i| buf[i + 4..].to_vec())
                .unwrap_or_default();
            (status, body)
        };

        // Positive: correct token serves the note HTML.
        let (st, _) = http_get(&format!("/{token}"));
        assert_eq!(st, 200, "correct token must serve the note");

        // Positive: attachment under Attachments/ is served with its bytes.
        let (st, body) = http_get(&format!("/{token}/Attachments/seal.png"));
        assert_eq!(st, 200, "attachment must be served");
        assert_eq!(&body[..], b"PNG-ATTACHMENT-BYTES");

        // Negative: wrong token → 404 (token is actually validated).
        let (st, _) = http_get("/wrongtoken/Attachments/seal.png");
        assert_eq!(st, 404);

        // Negative: no token → 404.
        let (st, _) = http_get("/");
        assert_eq!(st, 404);

        // Negative: vault files are NOT exposed (the two BLOCKERs).
        let (st, _) = http_get("/Secret.md");
        assert_eq!(st, 404, "notes must not be served without a token route");
        let (st, _) = http_get("/.onote/index.sqlite");
        assert_eq!(st, 404, ".onote must not be reachable");

        // Negative: even with a token, escapes are blocked.
        let (st, _) = http_get(&format!("/{token}/Secret.md"));
        assert_eq!(st, 404, "non-attachment files blocked even with token");
        let (st, _) = http_get(&format!("/{token}/.onote/index.sqlite"));
        assert_eq!(st, 404, "dotfiles blocked even with token");
        let (st, _) = http_get(&format!("/{token}/../Secret.md"));
        assert_eq!(st, 404, "traversal blocked");

        let _ = server.stop();
    }

    /// Raw HTTP/1.1 GET over a fresh loopback connection. Returns the full
    /// response (status line + headers + body) so callers can assert on any
    /// part of the wire output, mirroring the closure in the test above.
    fn http_get_raw(local_url: &str, path: &str) -> String {
        use std::io::{Read, Write};
        use std::net::TcpStream;
        use std::time::Duration;

        let port: u16 = local_url
            .rsplit('/')
            .nth(1)
            .unwrap()
            .rsplit(':')
            .next()
            .unwrap()
            .parse()
            .unwrap();
        let mut s = TcpStream::connect(("127.0.0.1", port)).expect("connect");
        s.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        write!(
            s,
            "GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n"
        )
        .unwrap();
        let mut buf = Vec::new();
        s.read_to_end(&mut buf).expect("read");
        String::from_utf8_lossy(&buf).into_owned()
    }

    /// An in-vault symlink under `Attachments/` that resolves OUTSIDE the vault
    /// must be rejected by `serve_attachment`'s canonicalization check
    /// (`canon.starts_with(attach_root)`). The outside file's bytes must never
    /// reach the client. Unix-gated: symlink creation needs `std::os::unix`.
    #[cfg(unix)]
    #[test]
    fn share_attachment_rejects_in_vault_symlink_that_escapes() {
        use std::os::unix::fs::symlink;

        let vault_dir = tempfile::tempdir().expect("vault tempdir");
        let root = vault_dir.path().to_path_buf();
        std::fs::create_dir_all(root.join("Attachments")).unwrap();

        // A file OUTSIDE the vault root whose bytes must never be served.
        let outside_dir = tempfile::tempdir().expect("outside tempdir");
        let outside_file = outside_dir.path().join("escape_target.png");
        std::fs::write(&outside_file, b"SHOULD-NOT-LEAK-BYTES").unwrap();

        // Plant a symlink inside Attachments/ pointing at the outside file.
        symlink(&outside_file, root.join("Attachments").join("escape.png")).unwrap();

        let snapshot = ShareSnapshot {
            note_path: RelativeNotePath::from_user("Scratch.md").unwrap(),
            title: "Scratch".into(),
            html: "<p>hello</p>".into(),
            attachment_dir: "Attachments".into(),
        };
        let server = HttpShareServer::new(root).expect("server");
        let sess = server
            .start(snapshot, SharePolicy::new(0, false))
            .expect("start");
        let token = sess.local_url.rsplit('/').next().unwrap();

        let resp = http_get_raw(&sess.local_url, &format!("/{token}/Attachments/escape.png"));
        let status = resp
            .lines()
            .next()
            .and_then(|l| {
                l.split_whitespace()
                    .nth(1)
                    .and_then(|c| c.parse::<u16>().ok())
            })
            .unwrap_or(0);

        assert_ne!(
            status, 200,
            "in-vault symlink that escapes the vault must not be served"
        );
        assert!(
            !resp.contains("SHOULD-NOT-LEAK-BYTES"),
            "the outside file's content must not appear anywhere in the response"
        );

        let _ = server.stop();
    }

    /// Defense-in-depth: `note_html` sets a `Content-Security-Policy` header so
    /// that any HTML which slipped past the raw-HTML-disabled Markdown renderer
    /// cannot run script in the share origin. Assert the header is present on
    /// the wire and pins `default-src` to `'none'`.
    #[test]
    fn share_note_response_carries_csp_header() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().to_path_buf();
        std::fs::create_dir_all(root.join("Attachments")).unwrap();

        let snapshot = ShareSnapshot {
            note_path: RelativeNotePath::from_user("Scratch.md").unwrap(),
            title: "Scratch".into(),
            html: "<p>hello</p>".into(),
            attachment_dir: "Attachments".into(),
        };
        let server = HttpShareServer::new(root).expect("server");
        let sess = server
            .start(snapshot, SharePolicy::new(0, false))
            .expect("start");
        let token = sess.local_url.rsplit('/').next().unwrap();

        let resp = http_get_raw(&sess.local_url, &format!("/{token}"));
        assert!(
            resp.to_ascii_lowercase()
                .contains("content-security-policy:"),
            "served note must carry a Content-Security-Policy header"
        );
        assert!(
            resp.contains("default-src 'none'"),
            "CSP must set default-src to 'none' (raw response: {resp})"
        );

        let _ = server.stop();
    }
}
