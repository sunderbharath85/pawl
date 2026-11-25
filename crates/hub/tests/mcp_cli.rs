//! `pawl mcp` as a coding agent runs it: a child process speaking MCP on stdin / stdout.

use std::io::Write;
use std::process::{Command, Stdio};

fn pawl() -> Command {
    Command::new(env!("CARGO_BIN_EXE_pawl"))
}

#[test]
fn pawl_mcp_refuses_other_hosts_before_sending_anything() {
    for url in ["http://example.com:8941", "https://127.0.0.1:8941", "http://192.168.1.10:8941"] {
        let out = pawl().arg("mcp").env("PAWL_A2A_URL", url).env("PAWL_A2A_TOKEN", "secret-token-1234").stdin(Stdio::null()).output().unwrap();
        assert_eq!(out.status.code(), Some(2), "{url}");
        assert!(out.stdout.is_empty(), "stdout is for MCP only");
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(err.contains("this machine only") && !err.contains("secret-token-1234"), "{err}");
    }
    let out = pawl().arg("mcp").env_remove("PAWL_A2A_URL").env_remove("PAWL_A2A_TOKEN").stdin(Stdio::null()).output().unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("PAWL_A2A_URL is not set"));
}

#[test]
fn pawl_mcp_answers_on_stdout_and_exits_when_stdin_closes() {
    // A port nothing listens on.
    let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let mut child = pawl()
        .arg("mcp")
        .env("PAWL_A2A_URL", format!("http://127.0.0.1:{port}"))
        .env("PAWL_A2A_TOKEN", "secret-token-1234")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    stdin
        .write_all(
            concat!(
                r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"t","version":"1"}}}"#,
                "\n",
                r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
                "\n",
                r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
                "\n",
                r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"pawl_status","arguments":{}}}"#,
                "\n"
            )
            .as_bytes(),
        )
        .unwrap();
    drop(stdin);
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let text = String::from_utf8(out.stdout).unwrap();
    let lines: Vec<serde_json::Value> = text.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    assert_eq!(lines.len(), 3, "{text}");
    assert_eq!(lines[0]["result"]["protocolVersion"], "2025-11-25");
    assert_eq!(lines[1]["result"]["tools"].as_array().unwrap().len(), 6);
    // Nothing listens there: a tool error that says so, not a crash.
    assert_eq!(lines[2]["result"]["isError"], true);
    assert!(lines[2]["result"]["content"][0]["text"].as_str().unwrap().contains("not answering"));
    assert!(!text.contains("secret-token-1234"));
}
