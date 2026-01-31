use std::sync::Arc;

use {anyhow::anyhow, async_trait::async_trait, tracing::{error, debug, warn}};

use moltis_channels::{ChannelEvent, ChannelEventSink, ChannelMessageMeta, ChannelReplyTarget};

use crate::{
    broadcast::{BroadcastOpts, broadcast},
    state::GatewayState,
};

/// Derive a per-channel session key from a reply target.
fn channel_session_key(target: &ChannelReplyTarget) -> String {
    format!("{}:{}:{}", target.channel_type, target.account_id, target.chat_id)
}

/// Broadcasts channel events over the gateway WebSocket.
///
/// Uses a deferred `OnceCell` reference so the sink can be created before
/// `GatewayState` exists (same pattern as cron callbacks).
pub struct GatewayChannelEventSink {
    state: Arc<tokio::sync::OnceCell<Arc<GatewayState>>>,
}

impl GatewayChannelEventSink {
    pub fn new(state: Arc<tokio::sync::OnceCell<Arc<GatewayState>>>) -> Self {
        Self { state }
    }
}

#[async_trait]
impl ChannelEventSink for GatewayChannelEventSink {
    async fn emit(&self, event: ChannelEvent) {
        if let Some(state) = self.state.get() {
            let payload = match serde_json::to_value(&event) {
                Ok(v) => v,
                Err(e) => {
                    warn!("failed to serialize channel event: {e}");
                    return;
                },
            };
            broadcast(
                state,
                "channel",
                payload,
                BroadcastOpts {
                    drop_if_slow: true,
                    ..Default::default()
                },
            )
            .await;
        }
    }

    async fn dispatch_to_chat(&self, text: &str, reply_to: ChannelReplyTarget, meta: ChannelMessageMeta) {
        if let Some(state) = self.state.get() {
            let session_key = channel_session_key(&reply_to);

            // Broadcast a "chat" event so the web UI shows the user message
            // in real-time (like typing from the UI).
            let payload = serde_json::json!({
                "state": "channel_user",
                "text": text,
                "channel": &meta,
                "sessionKey": &session_key,
            });
            broadcast(
                state,
                "chat",
                payload,
                BroadcastOpts {
                    drop_if_slow: true,
                    ..Default::default()
                },
            )
            .await;

            // Register the reply target so the chat "final" broadcast can
            // route the response back to the originating channel.
            state.push_channel_reply(&session_key, reply_to.clone()).await;

            let chat = state.chat().await;
            let mut params = serde_json::json!({
                "text": text,
                "channel": &meta,
                "_session_key": &session_key,
            });
            // Forward the channel's default model to chat.send() if configured.
            if let Some(ref model) = meta.model {
                params["model"] = serde_json::json!(model);
            }

            // Send a repeating "typing" indicator every 4s until chat.send()
            // completes. Telegram's typing status expires after ~5s.
            if let Some(outbound) = state.services.channel_outbound_arc() {
                let (done_tx, mut done_rx) = tokio::sync::oneshot::channel::<()>();
                let account_id = reply_to.account_id.clone();
                let chat_id = reply_to.chat_id.clone();
                tokio::spawn(async move {
                    loop {
                        if let Err(e) = outbound.send_typing(&account_id, &chat_id).await {
                            debug!("typing indicator failed: {e}");
                        }
                        tokio::select! {
                            _ = tokio::time::sleep(std::time::Duration::from_secs(4)) => {},
                            _ = &mut done_rx => break,
                        }
                    }
                });
                if let Err(e) = chat.send(params).await {
                    error!("channel dispatch_to_chat failed: {e}");
                }
                let _ = done_tx.send(());
            } else if let Err(e) = chat.send(params).await {
                error!("channel dispatch_to_chat failed: {e}");
            }
        } else {
            warn!("channel dispatch_to_chat: gateway not ready");
        }
    }

    async fn dispatch_command(&self, command: &str, reply_to: ChannelReplyTarget) -> anyhow::Result<String> {
        let state = self.state.get().ok_or_else(|| anyhow!("gateway not ready"))?;
        let session_key = channel_session_key(&reply_to);
        let chat = state.chat().await;
        let params = serde_json::json!({ "_session_key": &session_key });

        match command {
            "new" | "clear" => {
                chat.clear(params).await.map_err(|e| anyhow!("{e}"))?;
                let label = if command == "new" { "New session started." } else { "Session cleared." };
                Ok(label.to_string())
            },
            "compact" => {
                chat.compact(params).await.map_err(|e| anyhow!("{e}"))?;
                Ok("Session compacted.".to_string())
            },
            "context" => {
                let res = chat.context(params).await.map_err(|e| anyhow!("{e}"))?;
                // Format context info as a readable text summary.
                let session_info = res.get("session").cloned().unwrap_or_default();
                let msg_count = session_info.get("messageCount").and_then(|v| v.as_u64()).unwrap_or(0);
                let model = session_info.get("model").and_then(|v| v.as_str()).unwrap_or("default");
                let tokens = res.get("tokenUsage").cloned().unwrap_or_default();
                let estimated = tokens.get("estimatedTotal").and_then(|v| v.as_u64()).unwrap_or(0);
                let context_window = tokens.get("contextWindow").and_then(|v| v.as_u64()).unwrap_or(0);
                Ok(format!(
                    "Session: {session_key}\nMessages: {msg_count}\nModel: {model}\nTokens: ~{estimated}/{context_window}"
                ))
            },
            _ => Err(anyhow!("unknown command: /{command}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_event_serialization() {
        let event = ChannelEvent::InboundMessage {
            channel_type: "telegram".into(),
            account_id: "bot1".into(),
            peer_id: "123".into(),
            username: Some("alice".into()),
            sender_name: Some("Alice".into()),
            message_count: Some(5),
            access_granted: true,
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["kind"], "inbound_message");
        assert_eq!(json["channel_type"], "telegram");
        assert_eq!(json["account_id"], "bot1");
        assert_eq!(json["peer_id"], "123");
        assert_eq!(json["username"], "alice");
        assert_eq!(json["sender_name"], "Alice");
        assert_eq!(json["message_count"], 5);
        assert_eq!(json["access_granted"], true);
    }

    #[test]
    fn channel_session_key_format() {
        let target = ChannelReplyTarget {
            channel_type: "telegram".into(),
            account_id: "bot1".into(),
            chat_id: "12345".into(),
        };
        assert_eq!(channel_session_key(&target), "telegram:bot1:12345");
    }

    #[test]
    fn channel_session_key_group() {
        let target = ChannelReplyTarget {
            channel_type: "telegram".into(),
            account_id: "bot1".into(),
            chat_id: "-100999".into(),
        };
        assert_eq!(channel_session_key(&target), "telegram:bot1:-100999");
    }

    #[test]
    fn channel_event_serialization_nulls() {
        let event = ChannelEvent::InboundMessage {
            channel_type: "telegram".into(),
            account_id: "bot1".into(),
            peer_id: "123".into(),
            username: None,
            sender_name: None,
            message_count: None,
            access_granted: false,
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["kind"], "inbound_message");
        assert!(json["username"].is_null());
        assert_eq!(json["access_granted"], false);
    }
}
