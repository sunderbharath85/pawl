//! Tool-call confirmations as Structured Input (A2A extension, experimental v1:
//! <https://github.com/a2aproject/experimental-ext-structured-input/blob/main/v1/spec.md>).
//!
//! An ACP `session/request_permission` becomes `TASK_STATE_INPUT_REQUIRED` whose status message
//! carries: a text line saying what the agent wants to do, a `DataPart` with the tool call and the
//! options (names and kinds, for display), and one `input_schema` part: an object with a single
//! `optionId` property whose `enum` is the ACP option ids. The input request id is the Pawl
//! permission request id (global), so a late answer can never match a later request.
//!
//! The answer is a `structured_input` part `{ "optionId": "<id>" }` tagged with the request id, or
//! (spec 6.4, extension-unaware clients) a single untagged `DataPart` that validates.

use a2a::{Message, Part, PartContent, Role};
use pawl_protocol::{PermissionOptionKind, PermissionRequest, ToolKind};
use serde_json::{json, Map, Value};

use crate::ext::{ext_metadata, part_ext, STRUCTURED_INPUT_EXT};

/// JSON Schema dialect of the schemas Pawl sends (extension param `jsonschema_draft`).
pub const JSONSCHEMA_DRAFT: &str = "2020-12";

/// `application/schema+json`, set on `input_schema` parts (spec 6.2, optional).
pub const SCHEMA_MEDIA_TYPE: &str = "application/schema+json";

/// The property of the answer object.
pub const OPTION_FIELD: &str = "optionId";

/// Part types of the extension (`metadata[STRUCTURED_INPUT_EXT].type`).
pub mod part_type {
    pub const INPUT_SCHEMA: &str = "input_schema";
    pub const VALIDATION_ERROR: &str = "validation_error";
    pub const STRUCTURED_INPUT: &str = "structured_input";
}

/// The schema for answering `req`: `{ optionId: one of the ACP option ids }`, nothing else.
pub fn permission_schema(req: &PermissionRequest) -> Value {
    let ids: Vec<&str> = req.options.iter().map(|o| o.option_id.as_str()).collect();
    json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "type": "object",
        "title": "permission_answer",
        "properties": {
            OPTION_FIELD: {
                "type": "string",
                "enum": ids,
                "description": "The ACP permission option to choose."
            }
        },
        "required": [OPTION_FIELD],
        "additionalProperties": false
    })
}

fn kind_name(k: PermissionOptionKind) -> &'static str {
    match k {
        PermissionOptionKind::AllowOnce => "allow_once",
        PermissionOptionKind::AllowAlways => "allow_always",
        PermissionOptionKind::RejectOnce => "reject_once",
        PermissionOptionKind::RejectAlways => "reject_always",
    }
}

fn tool_kind_name(k: ToolKind) -> String {
    serde_json::to_value(k).ok().and_then(|v| v.as_str().map(str::to_string)).unwrap_or_else(|| "other".into())
}

/// One line for people: "Codex wants to run a tool (execute): cargo test".
pub fn summary(req: &PermissionRequest) -> String {
    let what = req.tool_call.title.as_deref().filter(|t| !t.trim().is_empty()).unwrap_or("a tool call");
    match req.tool_call.kind {
        Some(k) => format!("The agent asks permission ({}): {what}", tool_kind_name(k)),
        None => format!("The agent asks permission: {what}"),
    }
}

/// The `input_schema` part for `req`.
pub fn input_schema_part(req: &PermissionRequest) -> Part {
    let mut p = Part::data(permission_schema(req)).with_media_type(SCHEMA_MEDIA_TYPE);
    p.metadata = Some(ext_metadata(
        STRUCTURED_INPUT_EXT,
        json!({ "type": part_type::INPUT_SCHEMA, "id": req.request_id }),
    ));
    p
}

/// The display data of `req` (untagged: context, not the schema).
pub fn request_details_part(req: &PermissionRequest) -> Part {
    let options: Vec<Value> = req
        .options
        .iter()
        .map(|o| json!({ "optionId": o.option_id, "name": o.name, "kind": kind_name(o.kind) }))
        .collect();
    Part::data(json!({
        "requestId": req.request_id,
        "toolCall": serde_json::to_value(&req.tool_call).unwrap_or(Value::Null),
        "options": options,
    }))
    .with_media_type("application/json")
}

/// The status message of an `INPUT_REQUIRED` task waiting on `req`. `errors`: validation errors of
/// a rejected answer (JSON Schema basic output units), re-sent with the same request id (spec 7).
pub fn input_required_message(req: &PermissionRequest, context_id: &str, task_id: &str, errors: Option<Vec<Value>>) -> Message {
    let mut parts = vec![Part::text(summary(req)), request_details_part(req), input_schema_part(req)];
    if let Some(errors) = errors {
        parts.push(Part::text("That answer did not match the schema:"));
        let mut e = Part::data(json!({ "valid": false, "errors": errors }));
        e.metadata = Some(ext_metadata(STRUCTURED_INPUT_EXT, json!({ "type": part_type::VALIDATION_ERROR })));
        parts.push(e);
    }
    let mut m = Message::new(Role::Agent, parts);
    m.context_id = Some(context_id.to_string());
    m.task_id = Some(task_id.to_string());
    m
}

/// A client's answer part (what an extension-aware client sends).
pub fn structured_input_part(request_id: Option<&str>, data: Value) -> Part {
    let mut meta = Map::new();
    meta.insert("type".into(), json!(part_type::STRUCTURED_INPUT));
    if let Some(id) = request_id {
        meta.insert("id".into(), json!(id));
    }
    let mut p = Part::data(data);
    p.metadata = Some(ext_metadata(STRUCTURED_INPUT_EXT, Value::Object(meta)));
    p
}

/// Why a continuation message did not answer the pending request.
#[derive(Debug, Clone, PartialEq)]
pub enum AnswerError {
    /// No structured input at all (e.g. only text): the request stays pending.
    Missing,
    /// A `structured_input` part for another input request (spec 6.3: never used as the input).
    WrongRequest,
    /// The data did not validate; JSON Schema basic output units.
    Invalid(Vec<Value>),
    /// More than one `structured_input` part (spec 6.2).
    Ambiguous,
}

fn error_unit(keyword: &str, instance: &str, error: impl Into<String>) -> Value {
    json!({ "keywordLocation": keyword, "instanceLocation": instance, "error": error.into() })
}

/// Validate `data` against [`permission_schema`] for `option_ids`; Ok(option id).
pub fn validate_answer(data: &Value, option_ids: &[String]) -> Result<String, Vec<Value>> {
    let Some(obj) = data.as_object() else {
        return Err(vec![error_unit("/type", "", "expected an object")]);
    };
    let mut errors = Vec::new();
    for k in obj.keys().filter(|k| k.as_str() != OPTION_FIELD) {
        errors.push(error_unit("/additionalProperties", &format!("/{k}"), format!("unexpected property '{k}'")));
    }
    let chosen = match obj.get(OPTION_FIELD) {
        None => {
            errors.push(error_unit("/required", "", format!("missing properties: '{OPTION_FIELD}'")));
            None
        }
        Some(Value::String(s)) if option_ids.iter().any(|o| o == s) => Some(s.clone()),
        Some(Value::String(_)) => {
            errors.push(error_unit(
                &format!("/properties/{OPTION_FIELD}/enum"),
                &format!("/{OPTION_FIELD}"),
                format!("must be one of: {}", option_ids.join(", ")),
            ));
            None
        }
        Some(_) => {
            errors.push(error_unit(&format!("/properties/{OPTION_FIELD}/type"), &format!("/{OPTION_FIELD}"), "expected a string"));
            None
        }
    };
    match (chosen, errors.is_empty()) {
        (Some(c), true) => Ok(c),
        _ => Err(errors),
    }
}

/// The option a continuation message chooses for the pending request `request_id` (spec 6.3, 6.4,
/// 7). Untagged input is accepted only as the message's single `DataPart` and only if it validates
/// (otherwise it is context, so [`AnswerError::Missing`]).
pub fn parse_answer(msg: &Message, request_id: &str, option_ids: &[String]) -> Result<String, AnswerError> {
    let tagged: Vec<&Part> = msg
        .parts
        .iter()
        .filter(|p| {
            part_ext(&p.metadata, STRUCTURED_INPUT_EXT)
                .and_then(|m| m.get("type"))
                .and_then(Value::as_str)
                == Some(part_type::STRUCTURED_INPUT)
        })
        .collect();
    if tagged.len() > 1 {
        return Err(AnswerError::Ambiguous);
    }
    if let Some(p) = tagged.first() {
        let id = part_ext(&p.metadata, STRUCTURED_INPUT_EXT).and_then(|m| m.get("id")).and_then(Value::as_str);
        if id.is_some_and(|id| id != request_id) {
            return Err(AnswerError::WrongRequest);
        }
        let PartContent::Data(data) = &p.content else {
            return Err(AnswerError::Invalid(vec![error_unit("/type", "", "structured input must be a data part")]));
        };
        return validate_answer(data, option_ids).map_err(AnswerError::Invalid);
    }
    let data: Vec<&Value> = msg
        .parts
        .iter()
        .filter_map(|p| match &p.content {
            PartContent::Data(d) => Some(d),
            _ => None,
        })
        .collect();
    match data.as_slice() {
        [one] => validate_answer(one, option_ids).map_err(|_| AnswerError::Missing),
        _ => Err(AnswerError::Missing),
    }
}

/// An outside agent's input request: the schema and id of the `input_schema` part of a status
/// message, if it has exactly one (spec 6.2).
pub fn read_input_request(msg: &Message) -> Option<(Value, Option<String>)> {
    let mut found = msg.parts.iter().filter_map(|p| {
        let m = part_ext(&p.metadata, STRUCTURED_INPUT_EXT)?;
        if m.get("type").and_then(Value::as_str) != Some(part_type::INPUT_SCHEMA) {
            return None;
        }
        let PartContent::Data(schema) = &p.content else { return None };
        Some((schema.clone(), m.get("id").and_then(Value::as_str).map(str::to_string)))
    });
    let first = found.next()?;
    found.next().is_none().then_some(first)
}

/// The values a simple "pick one" schema allows: an object schema with exactly one property that is
/// a string `enum` (Pawl's own [`permission_schema`] shape, and the usual confirmation shape).
/// Returns (property name, values).
pub fn single_enum_choice(schema: &Value) -> Option<(String, Vec<String>)> {
    let props = schema.get("properties")?.as_object()?;
    if props.len() != 1 {
        return None;
    }
    let (name, prop) = props.iter().next()?;
    let values: Vec<String> = prop.get("enum")?.as_array()?.iter().map(|v| v.as_str().map(str::to_string)).collect::<Option<_>>()?;
    (!values.is_empty()).then(|| (name.clone(), values))
}

#[cfg(test)]
mod tests {
    use super::*;
    use pawl_protocol::{PermissionOption, ToolCallPayload};

    fn req() -> PermissionRequest {
        PermissionRequest {
            request_id: "abcdef012345:req-1".into(),
            machine_id: "abcdef012345".into(),
            session_id: "abcdef012345:s1".into(),
            agent_id: "codex".into(),
            tool_call: ToolCallPayload {
                tool_call_id: "tc1".into(),
                title: Some("cargo test".into()),
                kind: Some(ToolKind::Execute),
                ..Default::default()
            },
            options: vec![
                PermissionOption { option_id: "allow".into(), name: "Allow".into(), kind: PermissionOptionKind::AllowOnce },
                PermissionOption { option_id: "reject".into(), name: "Reject".into(), kind: PermissionOptionKind::RejectOnce },
            ],
            created_at: 1,
        }
    }

    fn ids() -> Vec<String> {
        vec!["allow".into(), "reject".into()]
    }

    #[test]
    fn the_schema_enum_is_the_acp_option_ids() {
        let s = permission_schema(&req());
        assert_eq!(s["properties"][OPTION_FIELD]["enum"], json!(["allow", "reject"]));
        assert_eq!(s["required"], json!([OPTION_FIELD]));
        assert_eq!(s["additionalProperties"], json!(false));
        assert_eq!(single_enum_choice(&s), Some((OPTION_FIELD.to_string(), ids())));
    }

    #[test]
    fn the_input_required_message_has_exactly_one_schema_part_tagged_with_the_request_id() {
        let m = input_required_message(&req(), "ctx", "task", None);
        assert_eq!(m.role, Role::Agent);
        assert_eq!(m.context_id.as_deref(), Some("ctx"));
        assert!(m.text().unwrap().contains("cargo test"));
        let (schema, id) = read_input_request(&m).expect("one input_schema part");
        assert_eq!(id.as_deref(), Some("abcdef012345:req-1"));
        assert_eq!(schema, permission_schema(&req()));
        let wire = serde_json::to_value(&m).unwrap();
        let schema_part = &wire["parts"][2];
        assert_eq!(schema_part["mediaType"], SCHEMA_MEDIA_TYPE);
        assert_eq!(schema_part["metadata"][STRUCTURED_INPUT_EXT]["type"], "input_schema");
    }

    #[test]
    fn a_tagged_answer_with_the_right_id_is_accepted() {
        let m = Message::new(Role::User, vec![Part::text("ok"), structured_input_part(Some("abcdef012345:req-1"), json!({ "optionId": "allow" }))]);
        assert_eq!(parse_answer(&m, "abcdef012345:req-1", &ids()), Ok("allow".into()));
        // Without an id it is matched by its schema alone (spec 6.3).
        let m = Message::new(Role::User, vec![structured_input_part(None, json!({ "optionId": "reject" }))]);
        assert_eq!(parse_answer(&m, "abcdef012345:req-1", &ids()), Ok("reject".into()));
    }

    #[test]
    fn an_answer_for_another_request_is_never_used() {
        let m = Message::new(Role::User, vec![structured_input_part(Some("abcdef012345:req-0"), json!({ "optionId": "allow" }))]);
        assert_eq!(parse_answer(&m, "abcdef012345:req-1", &ids()), Err(AnswerError::WrongRequest));
    }

    #[test]
    fn invalid_tagged_answers_report_basic_output_errors() {
        let m = Message::new(Role::User, vec![structured_input_part(None, json!({ "optionId": "allow_always", "extra": 1 }))]);
        let Err(AnswerError::Invalid(errors)) = parse_answer(&m, "r", &ids()) else { panic!() };
        assert_eq!(errors.len(), 2);
        assert!(errors.iter().any(|e| e["keywordLocation"] == "/additionalProperties"));
        assert!(errors.iter().any(|e| e["keywordLocation"] == "/properties/optionId/enum"));
        let m = Message::new(Role::User, vec![structured_input_part(None, json!({}))]);
        let Err(AnswerError::Invalid(errors)) = parse_answer(&m, "r", &ids()) else { panic!() };
        assert_eq!(errors[0]["keywordLocation"], "/required");
        let m = Message::new(Role::User, vec![structured_input_part(None, json!({ "optionId": 3 }))]);
        assert!(matches!(parse_answer(&m, "r", &ids()), Err(AnswerError::Invalid(_))));
        let mut text = structured_input_part(None, json!(null));
        text.content = PartContent::Text("allow".into());
        let m = Message::new(Role::User, vec![text]);
        assert!(matches!(parse_answer(&m, "r", &ids()), Err(AnswerError::Invalid(_))));
    }

    #[test]
    fn untagged_input_counts_only_when_it_is_the_single_valid_data_part() {
        let one = Message::new(Role::User, vec![Part::text("go"), Part::data(json!({ "optionId": "allow" }))]);
        assert_eq!(parse_answer(&one, "r", &ids()), Ok("allow".into()));
        let invalid = Message::new(Role::User, vec![Part::data(json!({ "optionId": "nope" }))]);
        assert_eq!(parse_answer(&invalid, "r", &ids()), Err(AnswerError::Missing));
        let two = Message::new(Role::User, vec![Part::data(json!({ "optionId": "allow" })), Part::data(json!({ "x": 1 }))]);
        assert_eq!(parse_answer(&two, "r", &ids()), Err(AnswerError::Missing));
        let text = Message::new(Role::User, vec![Part::text("allow")]);
        assert_eq!(parse_answer(&text, "r", &ids()), Err(AnswerError::Missing));
    }

    #[test]
    fn two_structured_input_parts_are_ambiguous() {
        let p = || structured_input_part(None, json!({ "optionId": "allow" }));
        let m = Message::new(Role::User, vec![p(), p()]);
        assert_eq!(parse_answer(&m, "r", &ids()), Err(AnswerError::Ambiguous));
    }

    #[test]
    fn validation_errors_are_re_sent_with_the_same_request_id() {
        let m = input_required_message(&req(), "ctx", "task", Some(vec![error_unit("/required", "", "missing")]));
        let (_, id) = read_input_request(&m).unwrap();
        assert_eq!(id.as_deref(), Some("abcdef012345:req-1"));
        let wire = serde_json::to_value(&m).unwrap();
        let last = wire["parts"].as_array().unwrap().last().unwrap().clone();
        assert_eq!(last["metadata"][STRUCTURED_INPUT_EXT]["type"], "validation_error");
        assert_eq!(last["data"]["valid"], false);
    }

    #[test]
    fn a_status_message_with_two_schemas_is_not_an_input_request() {
        let mut m = input_required_message(&req(), "ctx", "task", None);
        m.parts.push(input_schema_part(&req()));
        assert_eq!(read_input_request(&m), None);
    }

    #[test]
    fn single_enum_choice_rejects_other_shapes() {
        assert_eq!(single_enum_choice(&json!({ "type": "object", "properties": { "a": { "type": "string" } } })), None);
        assert_eq!(single_enum_choice(&json!({ "properties": { "a": { "enum": ["x"] }, "b": { "enum": ["y"] } } })), None);
        assert_eq!(single_enum_choice(&json!({ "properties": { "a": { "enum": [1] } } })), None);
        assert_eq!(single_enum_choice(&json!({ "properties": { "decision": { "enum": ["approve", "deny"] } } })), Some(("decision".into(), vec!["approve".into(), "deny".into()])));
    }
}
