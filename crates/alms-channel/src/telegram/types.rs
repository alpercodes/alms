// SPDX-License-Identifier: Apache-2.0

//! Telegram Bot API types

use serde::{Deserialize, Serialize};

/// Telegram Update object
#[derive(Debug, Clone, Deserialize)]
pub struct Update {
    pub update_id: i64,
    pub message: Option<Message>,
    pub edited_message: Option<Message>,
    // Deserialized from Telegram API; callback processing not yet implemented
    #[allow(dead_code)]
    pub callback_query: Option<CallbackQuery>,
}

/// Telegram Message object
#[derive(Debug, Clone, Deserialize)]
pub struct Message {
    pub message_id: i64,
    pub from: Option<User>,
    // Deserialized from Telegram API but not yet used in logic
    #[allow(dead_code)]
    pub date: i64,
    pub chat: Chat,
    pub text: Option<String>,
    // Deserialized from Telegram API; entity-based command parsing not yet implemented
    #[allow(dead_code)]
    pub entities: Option<Vec<MessageEntity>>,
}

/// Telegram User object
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct User {
    pub id: i64,
    pub is_bot: bool,
    pub first_name: String,
    pub last_name: Option<String>,
    pub username: Option<String>,
    pub language_code: Option<String>,
}

/// Telegram Chat object
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Chat {
    pub id: i64,
    #[serde(rename = "type")]
    pub chat_type: String,
    pub title: Option<String>,
    pub username: Option<String>,
    pub first_name: Option<String>,
    pub last_name: Option<String>,
}

/// Telegram MessageEntity object
// Deserialized from Telegram API for Message.entities; fields not yet used
#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
pub struct MessageEntity {
    #[serde(rename = "type")]
    pub entity_type: String,
    pub offset: i64,
    pub length: i64,
}

/// Telegram CallbackQuery object
// Deserialized from Telegram API for Update.callback_query; callback handling not yet implemented
#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
pub struct CallbackQuery {
    pub id: String,
    pub from: User,
    pub message: Option<Message>,
    pub data: Option<String>,
}

/// Response from Telegram API
#[derive(Debug, Clone, Deserialize)]
pub struct TelegramResponse<T> {
    pub ok: bool,
    pub result: Option<T>,
    pub description: Option<String>,
    // Deserialized from Telegram API error responses but not yet read
    #[allow(dead_code)]
    pub error_code: Option<i32>,
}

/// Request to send a message
#[derive(Debug, Clone, Serialize)]
pub struct SendMessageRequest {
    pub chat_id: i64,
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reply_to_message_id: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parse_mode: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disable_web_page_preview: Option<bool>,
}

impl SendMessageRequest {
    pub fn new(chat_id: i64, text: impl Into<String>) -> Self {
        Self {
            chat_id,
            text: text.into(),
            reply_to_message_id: None,
            parse_mode: None,
            disable_web_page_preview: None,
        }
    }

    pub fn reply_to(mut self, message_id: i64) -> Self {
        self.reply_to_message_id = Some(message_id);
        self
    }

    // TODO(dead-code): test-only method — consider gating behind #[cfg(test)]
    #[allow(dead_code)]
    pub fn parse_mode(mut self, mode: impl Into<String>) -> Self {
        self.parse_mode = Some(mode.into());
        self
    }
}

/// Response from sending a message.
///
/// Fields beyond `message_id` are required for Serde deserialization of the
/// Telegram API response but are not read by application code.
#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
pub struct SentMessage {
    pub message_id: i64,
    pub chat: Chat,
    pub date: i64,
    pub text: Option<String>,
}

/// Request to set a webhook
#[derive(Debug, Clone, Serialize)]
pub struct SetWebhookRequest {
    pub url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub secret_token: Option<String>,
}

/// Request to get updates via polling
#[derive(Debug, Clone, Serialize)]
pub struct GetUpdatesRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub offset: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout: Option<i32>,
}

impl GetUpdatesRequest {
    pub fn new() -> Self {
        Self {
            offset: None,
            limit: Some(100),
            timeout: Some(30),
        }
    }

    pub fn offset(mut self, offset: i64) -> Self {
        self.offset = Some(offset);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- Builder tests --

    #[test]
    fn send_message_request_new() {
        let req = SendMessageRequest::new(123, "hello");
        assert_eq!(req.chat_id, 123);
        assert_eq!(req.text, "hello");
        assert!(req.reply_to_message_id.is_none());
        assert!(req.parse_mode.is_none());
        assert!(req.disable_web_page_preview.is_none());
    }

    #[test]
    fn send_message_request_reply_to() {
        let req = SendMessageRequest::new(1, "hi").reply_to(456);
        assert_eq!(req.reply_to_message_id, Some(456));
    }

    #[test]
    fn send_message_request_parse_mode() {
        let req = SendMessageRequest::new(1, "hi").parse_mode("HTML");
        assert_eq!(req.parse_mode.as_deref(), Some("HTML"));
    }

    #[test]
    fn get_updates_defaults() {
        let req = GetUpdatesRequest::new();
        assert!(req.offset.is_none());
        assert_eq!(req.limit, Some(100));
        assert_eq!(req.timeout, Some(30));
    }

    #[test]
    fn get_updates_offset() {
        let req = GetUpdatesRequest::new().offset(42);
        assert_eq!(req.offset, Some(42));
    }

    // -- Serde tests --

    #[test]
    fn deserialize_update_with_message() {
        let json = r#"{
            "update_id": 100,
            "message": {
                "message_id": 1,
                "date": 1700000000,
                "chat": {"id": 55, "type": "private"},
                "text": "hello",
                "from": {"id": 10, "is_bot": false, "first_name": "Test"}
            }
        }"#;
        let update: Update = serde_json::from_str(json).unwrap();
        assert_eq!(update.update_id, 100);
        assert!(update.message.is_some());
        let msg = update.message.unwrap();
        assert_eq!(msg.message_id, 1);
        assert_eq!(msg.text.as_deref(), Some("hello"));
        assert_eq!(msg.chat.id, 55);
        assert_eq!(msg.chat.chat_type, "private");
    }

    #[test]
    fn deserialize_update_minimal() {
        let json = r#"{"update_id": 1}"#;
        let update: Update = serde_json::from_str(json).unwrap();
        assert_eq!(update.update_id, 1);
        assert!(update.message.is_none());
        assert!(update.edited_message.is_none());
        assert!(update.callback_query.is_none());
    }

    #[test]
    fn deserialize_telegram_response() {
        let json = r#"{"ok": true, "result": true}"#;
        let resp: TelegramResponse<bool> = serde_json::from_str(json).unwrap();
        assert!(resp.ok);
        assert_eq!(resp.result, Some(true));
        assert!(resp.description.is_none());
    }
}
