use super::*;
use a2a::{AgentCard, Message, SendMessageRequest, StreamResponse, Task};

fn v03_message() -> Value {
    json!({
        "kind": "message",
        "messageId": "m1",
        "role": "user",
        "contextId": "c1",
        "parts": [
            { "kind": "text", "text": "run the tests" },
            { "kind": "data", "data": { "optionId": "allow" }, "metadata": { "k": 1 } },
            { "kind": "file", "file": { "name": "a.png", "mimeType": "image/png", "bytes": "aGk=" } },
            { "kind": "file", "file": { "uri": "https://x/y.pdf" } }
        ]
    })
}

#[test]
fn methods_map_both_ways() {
    for old in [
        "message/send",
        "message/stream",
        "tasks/get",
        "tasks/list",
        "tasks/cancel",
        "tasks/resubscribe",
        "tasks/pushNotificationConfig/set",
        "tasks/pushNotificationConfig/get",
        "tasks/pushNotificationConfig/list",
        "tasks/pushNotificationConfig/delete",
        "agent/getAuthenticatedExtendedCard",
    ] {
        let new = method_from_v03(old).unwrap();
        assert_eq!(method_to_v03(new), Some(old));
    }
    assert_eq!(method_from_v03("SendMessage"), None);
    assert_eq!(method_from_v03("message/sendx"), None);
}

#[test]
fn a_v03_message_becomes_a_v1_message_the_sdk_parses() {
    let v1 = message_from_v03(v03_message());
    let m: Message = serde_json::from_value(v1.clone()).unwrap();
    assert_eq!(m.role, a2a::Role::User);
    assert_eq!(m.text(), Some("run the tests"));
    assert_eq!(m.context_id.as_deref(), Some("c1"));
    assert!(matches!(m.parts[1].content, a2a::PartContent::Data(_)));
    assert_eq!(m.parts[1].metadata.as_ref().unwrap()["k"], 1);
    assert_eq!(m.parts[2].content, a2a::PartContent::Raw(b"hi".to_vec()));
    assert_eq!(m.parts[2].filename.as_deref(), Some("a.png"));
    assert_eq!(m.parts[2].media_type.as_deref(), Some("image/png"));
    assert_eq!(m.parts[3].content, a2a::PartContent::Url("https://x/y.pdf".into()));
    // And back.
    let back = message_to_v03(v1);
    assert_eq!(back["kind"], "message");
    assert_eq!(back["role"], "user");
    assert_eq!(back["parts"][0], json!({ "kind": "text", "text": "run the tests" }));
    assert_eq!(back["parts"][2]["file"], json!({ "name": "a.png", "mimeType": "image/png", "bytes": "aGk=" }));
}

#[test]
fn send_params_convert_blocking_and_push_config() {
    let p = params_from_v03(m::SEND_MESSAGE, json!({ "message": v03_message(), "configuration": { "blocking": false, "historyLength": 2, "acceptedOutputModes": ["text/plain"] }, "metadata": { "a": 1 } }));
    let req: SendMessageRequest = serde_json::from_value(p.clone()).unwrap();
    let cfg = req.configuration.unwrap();
    assert_eq!(cfg.return_immediately, Some(true));
    assert_eq!(cfg.history_length, Some(2));
    assert_eq!(req.metadata.unwrap()["a"], 1);
    // Blocking by default.
    let p = params_from_v03(m::SEND_MESSAGE, json!({ "message": v03_message(), "configuration": {} }));
    assert!(p["configuration"].get("returnImmediately").is_none());
    let back = params_to_v03(m::SEND_MESSAGE, p);
    assert_eq!(back["configuration"]["blocking"], true);
    assert_eq!(back["message"]["kind"], "message");
    // Other methods pass through.
    assert_eq!(params_from_v03(m::GET_TASK, json!({ "id": "t", "historyLength": 1 })), json!({ "id": "t", "historyLength": 1 }));
}

fn v1_task() -> Value {
    serde_json::to_value(Task {
        id: "t1".into(),
        context_id: "c1".into(),
        status: a2a::TaskStatus {
            state: a2a::TaskState::InputRequired,
            message: Some(Message::new(a2a::Role::Agent, vec![a2a::Part::text("ok?")])),
            timestamp: None,
        },
        artifacts: Some(vec![a2a::Artifact {
            artifact_id: "a".into(),
            name: Some("response".into()),
            description: None,
            parts: vec![a2a::Part::text("hello")],
            metadata: None,
            extensions: None,
        }]),
        history: Some(vec![Message::new(a2a::Role::User, vec![a2a::Part::text("hi")])]),
        metadata: None,
    })
    .unwrap()
}

#[test]
fn tasks_and_results_convert_both_ways() {
    let old = task_to_v03(v1_task());
    assert_eq!(old["kind"], "task");
    assert_eq!(old["status"]["state"], "input-required");
    assert_eq!(old["status"]["message"]["role"], "agent");
    assert_eq!(old["artifacts"][0]["parts"][0]["kind"], "text");
    assert_eq!(old["history"][0]["parts"][0]["text"], "hi");
    let again: Task = serde_json::from_value(task_from_v03(old.clone())).unwrap();
    assert_eq!(again.status.state, a2a::TaskState::InputRequired);
    assert_eq!(again.artifacts.unwrap()[0].parts[0].as_text(), Some("hello"));

    assert_eq!(result_to_v03(m::SEND_MESSAGE, json!({ "task": v1_task() }))["kind"], "task");
    let msg = serde_json::to_value(Message::new(a2a::Role::Agent, vec![a2a::Part::text("done")])).unwrap();
    assert_eq!(result_to_v03(m::SEND_MESSAGE, json!({ "message": msg }))["kind"], "message");
    assert_eq!(result_to_v03(m::GET_TASK, v1_task())["kind"], "task");
    let r = result_from_v03(m::SEND_MESSAGE, old);
    let parsed: a2a::SendMessageResponse = serde_json::from_value(r).unwrap();
    assert!(matches!(parsed, a2a::SendMessageResponse::Task(_)));
    let r = result_from_v03(m::SEND_MESSAGE, message_to_v03(json!({ "messageId": "x", "role": "ROLE_AGENT", "parts": [{ "text": "hey" }] })));
    assert!(matches!(serde_json::from_value::<a2a::SendMessageResponse>(r).unwrap(), a2a::SendMessageResponse::Message(_)));
    for (old, new) in STATES {
        assert_eq!(state_from_v03(old), *new);
        assert_eq!(state_to_v03(new), *old);
    }
    assert_eq!(state_from_v03("bogus"), "TASK_STATE_UNSPECIFIED");
}

#[test]
fn stream_events_convert_both_ways_with_final_flags() {
    let status = |state: &str| json!({ "statusUpdate": { "taskId": "t", "contextId": "c", "status": { "state": state } } });
    let old = stream_event_to_v03(status("TASK_STATE_WORKING"));
    assert_eq!(old["kind"], "status-update");
    assert_eq!(old["status"]["state"], "working");
    assert_eq!(old["final"], false);
    assert_eq!(stream_event_to_v03(status("TASK_STATE_COMPLETED"))["final"], true);
    assert_eq!(stream_event_to_v03(status("TASK_STATE_INPUT_REQUIRED"))["final"], true);
    let back: StreamResponse = serde_json::from_value(stream_event_from_v03(old)).unwrap();
    assert!(matches!(back, StreamResponse::StatusUpdate(_)));

    let art = json!({ "artifactUpdate": { "taskId": "t", "contextId": "c", "append": true, "lastChunk": false, "artifact": { "artifactId": "a", "parts": [{ "text": "x" }] } } });
    let old = stream_event_to_v03(art);
    assert_eq!(old["kind"], "artifact-update");
    assert_eq!(old["artifact"]["parts"][0]["kind"], "text");
    assert_eq!(old["append"], true);
    assert!(matches!(serde_json::from_value::<StreamResponse>(stream_event_from_v03(old)).unwrap(), StreamResponse::ArtifactUpdate(_)));

    let old = stream_event_to_v03(json!({ "task": v1_task() }));
    assert_eq!(old["kind"], "task");
    assert!(matches!(serde_json::from_value::<StreamResponse>(stream_event_from_v03(old)).unwrap(), StreamResponse::Task(_)));
}

fn v1_card() -> Value {
    json!({
        "name": "Pawl",
        "description": "orchestrator",
        "version": "0.1.1",
        "supportedInterfaces": [
            { "url": "http://127.0.0.1:47802/", "protocolBinding": "JSONRPC", "protocolVersion": "1.0" },
            { "url": "http://127.0.0.1:47802", "protocolBinding": "HTTP+JSON", "protocolVersion": "1.0" }
        ],
        "capabilities": { "streaming": true, "pushNotifications": false, "extendedAgentCard": true },
        "defaultInputModes": ["text/plain"],
        "defaultOutputModes": ["text/plain"],
        "skills": [{ "id": "status", "name": "Status", "description": "d", "tags": ["status"] }],
        "securitySchemes": { "bearer": { "httpAuthSecurityScheme": { "scheme": "bearer" } } },
        "securityRequirements": [{ "schemes": { "bearer": { "list": [] } } }]
    })
}

#[test]
fn the_dual_card_parses_as_1_0_and_carries_the_0_3_fields() {
    let dual = card_dual(v1_card());
    let parsed: AgentCard = serde_json::from_value(dual.clone()).expect("1.0 readers parse the dual card");
    assert_eq!(parsed.supported_interfaces.len(), 2);
    assert_eq!(dual["url"], "http://127.0.0.1:47802/");
    assert_eq!(dual["preferredTransport"], "JSONRPC");
    assert_eq!(dual["protocolVersion"], "0.3.0");
    assert_eq!(dual["supportsAuthenticatedExtendedCard"], true);
    assert_eq!(dual["additionalInterfaces"][1]["transport"], "HTTP+JSON");
    assert!(dual.get("securitySchemes").is_none());
    // The strict 1.0 card parses with its scheme table.
    let strict: AgentCard = serde_json::from_value(v1_card()).unwrap();
    assert!(strict.security_schemes.is_some());
}

#[test]
fn the_0_3_card_uses_openapi_style_security() {
    let old = card_to_v03(v1_card());
    assert_eq!(old["securitySchemes"]["bearer"], json!({ "type": "http", "scheme": "bearer" }));
    assert_eq!(old["security"], json!([{ "bearer": [] }]));
    assert!(old.get("securityRequirements").is_none());
    // A card published in both shapes at once reads back as 1.0 (its 1.0 interfaces win).
    let both: AgentCard = serde_json::from_value(card_from_v03(old.clone())).unwrap();
    assert!(both.supported_interfaces.iter().all(|i| i.protocol_version == "1.0"));
    assert!(matches!(both.security_schemes.unwrap()["bearer"], a2a::SecurityScheme::HttpAuth(_)));
    // A pure 0.3 card reads back as 1.0, its interfaces marked 0.3.
    let mut pure = old;
    pure.as_object_mut().unwrap().remove("supportedInterfaces");
    let back = card_from_v03(pure);
    let parsed: AgentCard = serde_json::from_value(back).unwrap();
    assert!(parsed.supported_interfaces.iter().all(|i| i.protocol_version == "0.3"));
    assert_eq!(parsed.supported_interfaces[0].url, "http://127.0.0.1:47802/");
    assert_eq!(parsed.capabilities.extended_agent_card, Some(true));
    assert!(matches!(parsed.security_schemes.unwrap()["bearer"], a2a::SecurityScheme::HttpAuth(_)));
    // A 1.0 card is left alone.
    assert_eq!(card_from_v03(v1_card()), v1_card());
}

#[test]
fn a_minimal_0_3_card_from_another_sdk_reads_back() {
    let gemini_like = json!({
        "name": "Gemini SDLC Agent",
        "description": "An agent",
        "url": "http://localhost:41242/",
        "version": "0.0.2",
        "protocolVersion": "0.3.0",
        "capabilities": { "streaming": true, "pushNotifications": false, "stateTransitionHistory": true },
        "defaultInputModes": ["text"],
        "defaultOutputModes": ["text"],
        "skills": [{ "id": "code_generation", "name": "Code Generation", "description": "d", "tags": ["code"], "examples": ["x"] }],
        "securitySchemes": { "bearerAuth": { "type": "http", "scheme": "bearer" }, "oauth": { "type": "oauth2", "flows": {} } },
        "security": [{ "bearerAuth": [] }]
    });
    let parsed: AgentCard = serde_json::from_value(card_from_v03(gemini_like)).unwrap();
    assert_eq!(parsed.supported_interfaces[0].protocol_binding, "JSONRPC");
    assert_eq!(parsed.skills[0].id, "code_generation");
    let schemes = parsed.security_schemes.unwrap();
    assert_eq!(schemes.len(), 1, "unknown scheme kinds are dropped");
}
