//! Intent labelling and slot extraction for one caller turn, done as a side chat
//! completion so it never delays the live voice conversation.
use crate::{
    pack::ClientPack,
    slots::merge_slots,
    util::js_str,
    xai::{XaiError, XaiRest},
    Doc,
};
use serde_json::{json, Value};

#[derive(Debug, Clone)]
pub struct IntentResult {
    pub intent: String,
    pub slots: Doc,
}

/// A completed earlier turn, used as conversation history.
#[derive(Debug, Clone, Default)]
pub struct HistoryTurn {
    pub transcript: String,
    pub response: String,
}

/// Build the chat messages for intent reasoning.
pub fn intent_messages(
    pack: &ClientPack,
    history: &[HistoryTurn],
    transcript: &str,
    assistant_response: &str,
    slots: &Doc,
) -> Vec<Value> {
    let known = Value::Object(merge_slots(None, Some(&Value::Object(slots.clone()))));
    let mut messages = vec![
        json!({"role": "system", "content": pack.intent_prompt()}),
        json!({"role": "system", "content": format!("Known slots so far: {known}")}),
    ];
    for turn in history {
        if !turn.transcript.is_empty() {
            messages.push(json!({"role": "user", "content": turn.transcript}));
        }
        if !turn.response.is_empty() {
            messages.push(json!({"role": "assistant", "content": turn.response}));
        }
    }
    let last = json!({"caller_utterance": transcript, "assistant_reply": assistant_response});
    messages.push(json!({"role": "user", "content": last.to_string()}));
    messages
}

/// Label the caller's intent and update slots.
///
/// Without an API key the intent is `unknown`; with no caller speech it is
/// `assistant_prompt` (or `unknown`) and no request is made.
pub async fn reason_intent(
    xai: &XaiRest,
    pack: &ClientPack,
    history: &[HistoryTurn],
    transcript: &str,
    assistant_response: &str,
    slots: &Doc,
) -> Result<IntentResult, XaiError> {
    let current = Value::Object(slots.clone());
    if !xai.has_key() {
        return Ok(IntentResult {
            intent: "unknown".into(),
            slots: merge_slots(None, Some(&current)),
        });
    }
    if transcript.trim().is_empty() {
        let intent = if assistant_response.is_empty() {
            "unknown"
        } else {
            "assistant_prompt"
        };
        return Ok(IntentResult {
            intent: intent.into(),
            slots: merge_slots(None, Some(&current)),
        });
    }
    let content = xai
        .chat_json(intent_messages(
            pack,
            history,
            transcript,
            assistant_response,
            slots,
        ))
        .await?;
    let parsed: Value =
        serde_json::from_str(&content).map_err(|e| XaiError::Shape(e.to_string()))?;
    let intent = js_str(parsed.get("intent"));
    Ok(IntentResult {
        intent: if intent.is_empty() {
            "unknown".into()
        } else {
            intent
        },
        slots: merge_slots(Some(&current), parsed.get("slots")),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{config::XaiSettings, pack::PackLoader, slots::empty_slots};
    use wiremock::{matchers::method, Mock, MockServer, ResponseTemplate};

    fn rest(base: &str, key: &str) -> XaiRest {
        XaiRest::new(XaiSettings {
            api_key: key.into(),
            base_url: base.into(),
            realtime_url: String::new(),
            grok_model: "m".into(),
            tts_voice: "eve".into(),
            language: "en".into(),
            stt_keyterm: None,
        })
    }

    fn pack() -> std::sync::Arc<ClientPack> {
        PackLoader::new("does-not-exist", "default", "eve").load("")
    }

    #[test]
    fn messages_are_ordered_like_the_bridge() {
        let history = [
            HistoryTurn {
                transcript: String::new(),
                response: "Good morning".into(),
            },
            HistoryTurn {
                transcript: "I have a debt".into(),
                response: "What type?".into(),
            },
        ];
        let msgs = intent_messages(
            &pack(),
            &history,
            "an invoice",
            "And the amount?",
            &empty_slots(),
        );
        let roles: Vec<_> = msgs.iter().map(|m| m["role"].as_str().unwrap()).collect();
        assert_eq!(
            roles,
            ["system", "system", "assistant", "user", "assistant", "user"]
        );
        assert_eq!(
            msgs[1]["content"],
            "Known slots so far: {\"debt_type\":null,\"amount\":null,\"staff_name\":null,\"reference\":null,\"escalation_offered\":false,\"action\":\"continue\"}"
        );
        assert_eq!(
            msgs[5]["content"],
            "{\"caller_utterance\":\"an invoice\",\"assistant_reply\":\"And the amount?\"}"
        );
    }

    #[tokio::test]
    async fn no_key_or_no_speech_makes_no_request() {
        let server = MockServer::start().await;
        let no_key = reason_intent(
            &rest(&server.uri(), ""),
            &pack(),
            &[],
            "hello",
            "hi",
            &empty_slots(),
        )
        .await
        .unwrap();
        assert_eq!(no_key.intent, "unknown");
        let silent = reason_intent(
            &rest(&server.uri(), "k"),
            &pack(),
            &[],
            "  ",
            "hi",
            &empty_slots(),
        )
        .await
        .unwrap();
        assert_eq!(silent.intent, "assistant_prompt");
        assert!(server.received_requests().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn parsed_slots_merge_over_existing() {
        let server = MockServer::start().await;
        let reply =
            json!({"intent": "request_transfer", "slots": {"amount": null, "action": "transfer"}});
        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(
                    json!({"choices": [{"message": {"content": reply.to_string()}}]}),
                ),
            )
            .mount(&server)
            .await;
        let mut slots = empty_slots();
        slots.insert("amount".into(), json!("5000"));
        let out = reason_intent(
            &rest(&server.uri(), "k"),
            &pack(),
            &[],
            "a person please",
            "Sure",
            &slots,
        )
        .await
        .unwrap();
        assert_eq!(out.intent, "request_transfer");
        assert_eq!(out.slots["amount"], "5000");
        assert_eq!(out.slots["action"], "transfer");
    }
}
