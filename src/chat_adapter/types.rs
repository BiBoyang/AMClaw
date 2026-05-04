use serde::Deserialize;
use serde_json::Value;

use super::{compact_json, value_to_string};

#[derive(Debug, Clone, Deserialize, Default)]
pub(super) struct TextItem {
    #[serde(default)]
    pub(super) text: String,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub(super) struct MessageItem {
    #[serde(default)]
    pub(super) text_item: Option<TextItem>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub(super) enum FlexibleId {
    Str(String),
    Int(i64),
    Float(f64),
    Obj(Value),
}

impl FlexibleId {
    pub(super) fn as_string(&self) -> String {
        match self {
            Self::Str(v) => v.clone(),
            Self::Int(v) => v.to_string(),
            Self::Float(v) => {
                if v.fract() == 0.0 {
                    (*v as i64).to_string()
                } else {
                    v.to_string()
                }
            }
            Self::Obj(v) => {
                if let Some(value) = value_to_string(v) {
                    value
                } else {
                    compact_json(v)
                }
            }
        }
    }
}

#[derive(Debug, Clone, Deserialize, Default)]
pub(super) struct WireMessage {
    #[serde(default)]
    pub(super) message: Option<Box<WireMessage>>,
    #[serde(default)]
    pub(super) from_user_id: String,
    #[serde(default)]
    pub(super) client_id: String,
    #[serde(default)]
    pub(super) create_time_ms: Option<i64>,
    #[serde(default)]
    pub(super) message_type: Option<i64>,
    #[serde(default)]
    pub(super) context_token: String,
    #[serde(default)]
    pub(super) item_list: Vec<MessageItem>,
    #[serde(default)]
    pub(super) text: String,
    #[serde(default)]
    pub(super) message_id: Option<FlexibleId>,
    #[serde(default)]
    pub(super) msg_id: Option<FlexibleId>,
}

pub(super) struct GetUpdatesResult {
    pub(super) messages: Vec<WireMessage>,
    pub(super) cursor: String,
}
