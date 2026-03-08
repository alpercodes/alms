//! Telegram Bot API types

#![allow(dead_code)]

use serde::{Deserialize, Serialize};

/// Telegram Update object
#[derive(Debug, Clone, Deserialize)]
pub struct Update {
    pub update_id: i64,
    pub message: Option<Message>,
    pub edited_message: Option<Message>,
    pub callback_query: Option<CallbackQuery>,
}

/// Telegram Message object
#[derive(Debug, Clone, Deserialize)]
pub struct Message {
    pub message_id: i64,
    pub from: Option<User>,
    pub date: i64,
    pub chat: Chat,
    pub text: Option<String>,
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
#[derive(Debug, Clone, Deserialize)]
pub struct MessageEntity {
    #[serde(rename = "type")]
    pub entity_type: String,
    pub offset: i64,
    pub length: i64,
}

/// Telegram CallbackQuery object
#[derive(Debug, Clone, Deserialize)]
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

    pub fn parse_mode(mut self, mode: impl Into<String>) -> Self {
        self.parse_mode = Some(mode.into());
        self
    }
}

/// Response from sending a message
#[derive(Debug, Clone, Deserialize)]
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
