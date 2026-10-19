//! Test helpers: scratch directories, a config pointing caches there, and a tiny HTTP server.

use komposer_package::config::Config;
use serde_json::json;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};

/// A fresh directory under `$TMPDIR/komposer-repository-tests`.
pub fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("komposer-repository-tests").join(format!("{name}-{}", crate::util::unique_suffix()));
    std::fs::create_dir_all(&dir).unwrap();
    dir.canonicalize().unwrap()
}

/// Defaults plus a private cache dir, plain http allowed.
pub fn test_config() -> Config {
    let home = temp_dir("home");
    let mut config = Config::new(false, Some(&home));
    config.merge_config(&json!({"home": home, "cache-dir": home.join("cache"), "secure-http": false}));
    config
}

/// Runs `git` in `dir`, panicking on failure; returns stdout.
pub fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@example.org")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@example.org")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// A git repository with a `main` branch.
pub fn git_repo(name: &str) -> PathBuf {
    let dir = temp_dir(name);
    git(&dir, &["init", "-q", "-b", "main"]);
    dir
}

pub fn commit_file(dir: &Path, file: &str, contents: &str, message: &str) -> String {
    std::fs::write(dir.join(file), contents).unwrap();
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", message]);
    git(dir, &["rev-parse", "HEAD"])
}

#[derive(Clone, Debug)]
pub struct Recorded {
    pub method: String,
    pub path: String,
    pub headers: Vec<(String, String)>,
    pub body: String,
}

impl Recorded {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }
}

struct Route {
    path: String,
    status: u16,
    headers: Vec<(String, String)>,
    body: String,
    times: Option<usize>,
}

/// HTTP/1.1 server answering from a route table; routes with a count are used up first, and status 0 never
/// answers.
pub struct TestServer {
    pub url: String,
    routes: Arc<Mutex<Vec<Route>>>,
    requests: Arc<Mutex<Vec<Recorded>>>,
}

impl TestServer {
    pub fn new() -> TestServer {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let routes: Arc<Mutex<Vec<Route>>> = Arc::default();
        let requests: Arc<Mutex<Vec<Recorded>>> = Arc::default();
        let (r, q) = (routes.clone(), requests.clone());
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let (r, q) = (r.clone(), q.clone());
                std::thread::spawn(move || serve(stream, &r, &q));
            }
        });
        TestServer { url, routes, requests }
    }

    pub fn route(&self, path: &str, status: u16, headers: &[(&str, &str)], body: &str) {
        self.add(path, status, headers, body, None)
    }

    pub fn route_times(&self, path: &str, status: u16, headers: &[(&str, &str)], body: &str, times: usize) {
        self.add(path, status, headers, body, Some(times))
    }

    fn add(&self, path: &str, status: u16, headers: &[(&str, &str)], body: &str, times: Option<usize>) {
        let headers = headers.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        self.routes.lock().unwrap().push(Route { path: path.into(), status, headers, body: body.into(), times });
    }

    pub fn requests(&self) -> Vec<Recorded> {
        self.requests.lock().unwrap().clone()
    }

    pub fn paths(&self) -> Vec<String> {
        self.requests().into_iter().map(|r| r.path).collect()
    }
}

fn serve(stream: TcpStream, routes: &Mutex<Vec<Route>>, requests: &Mutex<Vec<Recorded>>) {
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut line = String::new();
    if reader.read_line(&mut line).unwrap_or(0) == 0 {
        return;
    }
    let mut parts = line.split_whitespace();
    let (method, path) = (parts.next().unwrap_or("").to_string(), parts.next().unwrap_or("").to_string());
    let mut headers = vec![];
    loop {
        let mut h = String::new();
        if reader.read_line(&mut h).unwrap_or(0) == 0 || h.trim().is_empty() {
            break;
        }
        if let Some((k, v)) = h.trim_end().split_once(':') {
            headers.push((k.trim().to_string(), v.trim().to_string()));
        }
    }
    let len = headers.iter().find(|(k, _)| k.eq_ignore_ascii_case("content-length")).and_then(|(_, v)| v.parse().ok()).unwrap_or(0);
    let mut body = vec![0; len];
    let _ = reader.read_exact(&mut body);
    requests.lock().unwrap().push(Recorded { method, path: path.clone(), headers, body: String::from_utf8_lossy(&body).into() });

    let (status, headers, body) = {
        let mut routes = routes.lock().unwrap();
        let found = routes.iter().position(|r| r.path == path && r.times != Some(0));
        match found {
            Some(i) => {
                let r = &mut routes[i];
                if let Some(t) = &mut r.times {
                    *t -= 1;
                }
                (r.status, r.headers.clone(), r.body.clone())
            }
            None => (404, vec![], String::new()),
        }
    };
    if status == 0 {
        std::thread::sleep(std::time::Duration::from_secs(60));
        return;
    }
    let mut out = format!("HTTP/1.1 {status} {}\r\nContent-Length: {}\r\nConnection: close\r\n", reason(status), body.len());
    for (k, v) in headers {
        out.push_str(&format!("{k}: {v}\r\n"));
    }
    out.push_str("\r\n");
    out.push_str(&body);
    let mut stream = stream;
    let _ = stream.write_all(out.as_bytes());
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        302 => "Found",
        304 => "Not Modified",
        401 => "Unauthorized",
        404 => "Not Found",
        503 => "Service Unavailable",
        _ => "Status",
    }
}
