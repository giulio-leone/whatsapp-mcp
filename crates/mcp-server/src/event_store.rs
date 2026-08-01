use anyhow::Result;
use wa_client::client::WhatsAppEvent;
use wa_domain::models::chat::{Chat, ChatId};
use wa_domain::ports::StoragePort;

pub async fn persist_whatsapp_event(
    storage: &dyn StoragePort,
    event: &WhatsAppEvent,
) -> Result<()> {
    match event {
        WhatsAppEvent::MessageReceived(message) => {
            storage
                .save_chat(&Chat {
                    id: ChatId(message.chat_id.0.clone()),
                    name: None,
                    unread_count: 0,
                    is_group: message.chat_id.0.ends_with("@g.us"),
                    last_message_timestamp: message.timestamp,
                })
                .await?;
            storage.save_message(message).await?;
        }
        WhatsAppEvent::HistorySyncBatch { chats, messages, .. } => {
            for chat in chats {
                storage.save_chat(chat).await?;
            }
            for message in messages {
                storage.save_message(message).await?;
            }
        }
        _ => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use wa_domain::models::message::{Message, MessageId};

    #[tokio::test]
    async fn history_sync_batch_is_persisted_for_mcp_reads() {
        let db_path = std::env::temp_dir().join(format!(
            "whatsapp-mcp-event-store-{}.db",
            std::process::id()
        ));
        let db_path_string = db_path.to_string_lossy().into_owned();
        let _ = std::fs::remove_file(&db_path);
        let storage = wa_storage_sqlite::SqliteStorage::new(&db_path_string).unwrap();
        let chat = Chat {
            id: ChatId("100@s.whatsapp.net".into()),
            name: Some("History test".into()),
            unread_count: 1,
            is_group: false,
            last_message_timestamp: 1_725_000_000,
        };
        let message = Message {
            id: MessageId("history-message-1".into()),
            chat_id: chat.id.clone(),
            sender_id: chat.id.0.clone(),
            text: Some("history text".into()),
            media: None,
            timestamp: 1_725_000_000,
            is_from_me: false,
            is_forwarded: false,
            reply_to_id: None,
        };

        persist_whatsapp_event(
            &storage,
            &WhatsAppEvent::HistorySyncBatch {
                chats: vec![chat.clone()],
                messages: vec![message],
                progress: Some(100),
            },
        )
        .await
        .unwrap();

        assert!(storage.get_chat(&chat.id).await.unwrap().is_some());
        assert_eq!(storage.get_messages(&chat.id, 3, None).await.unwrap().len(), 1);
        let _ = std::fs::remove_file(&db_path);
    }
}
