//! The development bridge as a program: `hotend-bridge` started the way the Playwright
//! tests start it, and spoken to over plain HTTP.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "a test stops at the first surprise"
)]
#![cfg(feature = "dev-bridge")]

mod support;

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

struct Bridge {
    child: Child,
    port: u16,
    token: String,
    answers_dialogs: bool,
}

impl Drop for Bridge {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn start(extra: &[&str]) -> Bridge {
    support::ensure_tools();
    let mut child = Command::new(env!("CARGO_BIN_EXE_hotend-bridge"))
        .args(extra)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("hotend-bridge starts");
    let mut line = String::new();
    BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    let ready: Value = serde_json::from_str(&line).expect("the first line is the bridge's address");
    assert_eq!(ready["hotend_bridge"], 1);
    let url = ready["url"].as_str().unwrap();
    // Loopback only, by address: not a name that could resolve elsewhere.
    let port = url
        .strip_prefix("http://127.0.0.1:")
        .expect("the bridge is on 127.0.0.1");
    Bridge {
        child,
        port: port.parse().unwrap(),
        token: ready["token"].as_str().unwrap().to_owned(),
        answers_dialogs: ready["answers_dialogs"].as_bool().unwrap(),
    }
}

struct Answer {
    status: u16,
    body: String,
}

impl Answer {
    fn json(&self) -> Value {
        serde_json::from_str(&self.body).unwrap_or_else(|_| panic!("not JSON: {}", self.body))
    }
}

impl Bridge {
    /// One HTTP request with exactly the given header lines.
    fn request(&self, method: &str, path: &str, headers: &[String], body: &str) -> Answer {
        let mut stream = TcpStream::connect(("127.0.0.1", self.port)).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(120)))
            .unwrap();
        let mut request = format!("{method} {path} HTTP/1.1\r\nConnection: close\r\n");
        for header in headers {
            request.push_str(header);
            request.push_str("\r\n");
        }
        request.push_str(&format!("Content-Length: {}\r\n\r\n{body}", body.len()));
        stream.write_all(request.as_bytes()).unwrap();
        let mut text = String::new();
        stream.read_to_string(&mut text).unwrap();
        let (head, body) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
        let status = head
            .split_whitespace()
            .nth(1)
            .and_then(|code| code.parse().ok())
            .expect("a status line");
        Answer {
            status,
            body: body.to_owned(),
        }
    }

    fn host(&self) -> String {
        format!("Host: 127.0.0.1:{}", self.port)
    }

    fn bearer(&self) -> String {
        format!("Authorization: Bearer {}", self.token)
    }

    /// A command, called as the interface's transport calls it.
    fn invoke(&self, command: &str, args: Value) -> Value {
        let answer = self.request(
            "POST",
            &format!("/invoke/{command}"),
            &[
                self.host(),
                self.bearer(),
                "Content-Type: application/json".to_owned(),
            ],
            &args.to_string(),
        );
        assert_eq!(answer.status, 200, "{command}: {}", answer.body);
        answer.json()
    }

    fn data(&self, command: &str, args: Value) -> Value {
        let answer = self.invoke(command, args);
        assert_eq!(answer["ok"], true, "{command}: {answer}");
        answer["data"].clone()
    }

    /// The event stream: every `data:` payload, as it arrives.
    fn events(&self) -> mpsc::Receiver<Value> {
        let mut stream = TcpStream::connect(("127.0.0.1", self.port)).unwrap();
        let request = format!(
            "GET /events HTTP/1.1\r\n{}\r\n{}\r\n\r\n",
            self.host(),
            self.bearer()
        );
        stream.write_all(request.as_bytes()).unwrap();
        let (send, receive) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stream).lines() {
                let Ok(line) = line else { return };
                if let Some(data) = line.strip_prefix("data:")
                    && send
                        .send(serde_json::from_str(data.trim()).unwrap())
                        .is_err()
                {
                    return;
                }
            }
        });
        receive
    }
}

/// A project with the cable box built, sliced and proposed. Returns the pending job's id.
fn pending_job(bridge: &Bridge) -> String {
    let project = bridge.data("project_create", json!({"name": "Cable box"}));
    let id = project["id"].as_str().unwrap();
    let built = bridge.data(
        "model_build",
        json!({"projectId": id, "code": support::cable_box_code(), "params": {}, "note": "test"}),
    );
    assert_eq!(built["version"]["number"], 1, "{built}");
    bridge.data(
        "slice_run",
        json!({"projectId": id, "profile": "demo-0.20mm", "plate": null}),
    );
    let job = bridge.data("print_propose", json!({"projectId": id, "note": "test"}));
    assert_eq!(job["status"], "pending");
    job["id"].as_str().unwrap().to_owned()
}

#[test]
fn the_bridge_serves_only_this_computer_and_only_with_its_token() {
    let bridge = start(&[]);
    assert!(!bridge.answers_dialogs);
    // 32 random bytes, different at every start.
    assert_eq!(bridge.token.len(), 64);
    assert!(bridge.token.chars().all(|c| c.is_ascii_hexdigit()));
    let other = start(&[]);
    assert_ne!(bridge.token, other.token);
    drop(other);

    let json_type = "Content-Type: application/json".to_owned();
    let post = |headers: &[String]| bridge.request("POST", "/invoke/projects_list", headers, "{}");

    // The token: present, whole and right, or nothing.
    assert_eq!(post(&[bridge.host(), json_type.clone()]).status, 401);
    assert_eq!(
        post(&[bridge.host(), "Authorization: Bearer wrong".to_owned()]).status,
        401
    );
    let cut = format!("Authorization: Bearer {}", &bridge.token[..63]);
    assert_eq!(post(&[bridge.host(), cut]).status, 401);
    let basic = format!("Authorization: Basic {}", bridge.token);
    assert_eq!(post(&[bridge.host(), basic]).status, 401);
    let in_query = bridge.request(
        "POST",
        &format!("/invoke/projects_list?token={}", bridge.token),
        &[bridge.host()],
        "{}",
    );
    assert_eq!(in_query.status, 401);
    let ok = post(&[bridge.host(), bridge.bearer(), json_type.clone()]);
    assert_eq!(ok.status, 200);
    assert_eq!(ok.json(), json!({"ok": true, "data": []}));
    let events = bridge.request("GET", "/events", &[bridge.host()], "");
    assert_eq!(events.status, 401);

    // The address: a request that reached the port under another name (a page that
    // re-pointed its own name at 127.0.0.1) is refused even with the token.
    for host in [
        "Host: example.com".to_owned(),
        format!("Host: example.com:{}", bridge.port),
        "Host: 127.0.0.1:1".to_owned(),
    ] {
        assert_eq!(post(&[host.clone(), bridge.bearer()]).status, 403, "{host}");
    }
    // The page: only one served from this computer.
    for origin in [
        "Origin: https://example.com",
        "Origin: http://127.0.0.1.example.com",
        "Origin: null",
    ] {
        assert_eq!(
            post(&[bridge.host(), bridge.bearer(), origin.to_owned()]).status,
            403,
            "{origin}"
        );
    }
    let from_preview = post(&[
        bridge.host(),
        bridge.bearer(),
        "Origin: http://127.0.0.1:4173".to_owned(),
    ]);
    assert_eq!(from_preview.status, 200);

    // Health says that this is the bridge and nothing else, and needs no token.
    let health = bridge.request("GET", "/health", &[bridge.host()], "");
    assert_eq!(
        (health.status, health.json()),
        (200, json!({"hotend_bridge": 1}))
    );
    assert!(!health.body.contains(&bridge.token));

    // Exactly the window's commands: an unknown name is not found, bad arguments are bad.
    let unknown = bridge.request(
        "POST",
        "/invoke/shell_execute",
        &[bridge.host(), bridge.bearer()],
        "{}",
    );
    assert_eq!(unknown.status, 404);
    let bad = bridge.request(
        "POST",
        "/invoke/project_create",
        &[bridge.host(), bridge.bearer()],
        r#"{"nam":"x"}"#,
    );
    assert_eq!(bad.status, 400);
    let not_json = bridge.request(
        "POST",
        "/invoke/project_create",
        &[bridge.host(), bridge.bearer()],
        "name=x",
    );
    assert_eq!(not_json.status, 400);

    // A command's own failure is the core's error, as the window's transport gives it.
    let missing = bridge.invoke("project_get", json!({"projectId": "nope"}));
    assert_eq!(missing["ok"], false);
    assert_eq!(missing["error"]["kind"], "not_found");
    assert!(missing["error"]["next_step"].is_string());
}

#[test]
fn without_the_test_switch_the_bridge_refuses_to_confirm_a_print() {
    let bridge = start(&[]);
    let job = pending_job(&bridge);
    // A review can be read. Confirming cannot be done: nobody is there to be asked.
    let review = bridge.data(
        "print_review",
        json!({"jobId": job, "delivery": {"kind": "simulated_printer"}}),
    );
    assert!(review["summary"]["sha256"].is_string());
    for _ in 0..2 {
        let refused = bridge.invoke(
            "print_confirm",
            json!({"jobId": job, "delivery": {"kind": "simulated_printer"}}),
        );
        assert_eq!(refused["ok"], false, "{refused}");
        assert_eq!(refused["error"]["kind"], "not_configured");
    }
    assert_eq!(
        bridge.data("print_status", json!({"jobId": job}))["status"],
        "pending"
    );
    // Pickers cancel too: nothing can be exported to a folder nobody picked.
    assert_eq!(
        bridge.data("folder_pick", json!({"title": "Export to"})),
        Value::Null
    );
}

#[test]
fn with_the_test_switch_a_print_runs_on_the_simulated_printer_and_its_events_stream() {
    let bridge = start(&["--answer-dialogs-for-tests", "--time-scale", "3600"]);
    assert!(bridge.answers_dialogs);
    let events = bridge.events();
    let job = pending_job(&bridge);
    let outcome = bridge.data(
        "print_confirm",
        json!({"jobId": job, "delivery": {"kind": "simulated_printer"}}),
    );
    assert_eq!(outcome["confirmed"], true);
    assert_eq!(outcome["job"]["id"], job.as_str());

    let deadline = Instant::now() + Duration::from_secs(120);
    let mut status = String::new();
    while Instant::now() < deadline {
        status = bridge.data("print_status", json!({"jobId": job}))["status"]
            .as_str()
            .unwrap()
            .to_owned();
        if status == "finished" {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    assert_eq!(status, "finished");

    // The same events the window's channel carries, one JSON object per message.
    let mut seen = Vec::new();
    while let Ok(event) = events.recv_timeout(Duration::from_secs(2)) {
        assert_eq!(event["channel"], "print");
        assert_eq!(event["job_id"], job.as_str());
        assert_eq!(event["simulated"], true);
        seen.push(event["event"]["type"].as_str().unwrap().to_owned());
        if event["event"]["type"] == "status" && event["event"]["job"]["status"] == "finished" {
            break;
        }
    }
    assert!(seen.iter().any(|kind| kind == "progress"), "{seen:?}");
    assert!(seen.iter().any(|kind| kind == "status"), "{seen:?}");
}
