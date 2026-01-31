use std::sync::Arc;

use {
    teloxide::{
        prelude::*,
        types::{AllowedUpdate, UpdateKind},
    },
    tokio_util::sync::CancellationToken,
    tracing::{debug, error, info, warn},
};

use moltis_channels::{ChannelEventSink, message_log::MessageLog};

use crate::{
    config::TelegramAccountConfig,
    handlers,
    outbound::TelegramOutbound,
    state::{AccountState, AccountStateMap},
};

/// Start polling for a single bot account.
///
/// Spawns a background task that processes updates until the returned
/// `CancellationToken` is cancelled.
pub async fn start_polling(
    account_id: String,
    config: TelegramAccountConfig,
    accounts: AccountStateMap,
    message_log: Option<Arc<dyn MessageLog>>,
    event_sink: Option<Arc<dyn ChannelEventSink>>,
) -> anyhow::Result<CancellationToken> {
    // Build bot with a client timeout longer than the long-polling timeout (30s)
    // so the HTTP client doesn't abort the request before Telegram responds.
    let client = teloxide::net::default_reqwest_settings()
        .timeout(std::time::Duration::from_secs(45))
        .build()?;
    let bot = teloxide::Bot::with_client(&config.token, client);

    // Verify credentials and get bot username.
    let me = bot.get_me().await?;
    let bot_username = me.username.clone();

    // Delete any existing webhook so long polling works.
    bot.delete_webhook().send().await?;

    info!(
        account_id,
        username = ?bot_username,
        "telegram bot connected (webhook cleared)"
    );

    let cancel = CancellationToken::new();

    let outbound = Arc::new(TelegramOutbound {
        accounts: Arc::clone(&accounts),
    });

    let state = AccountState {
        bot: bot.clone(),
        bot_username,
        account_id: account_id.clone(),
        config,
        outbound,
        cancel: cancel.clone(),
        message_log,
        event_sink,
    };

    {
        let mut map = accounts.write().unwrap();
        map.insert(account_id.clone(), state);
    }

    let cancel_clone = cancel.clone();
    let aid = account_id.clone();
    let poll_accounts = Arc::clone(&accounts);
    tokio::spawn(async move {
        info!(account_id = aid, "starting telegram manual polling loop");
        let mut offset: i32 = 0;

        loop {
            if cancel_clone.is_cancelled() {
                info!(account_id = aid, "telegram polling stopped");
                break;
            }

            let result = bot
                .get_updates()
                .offset(offset as i32)
                .timeout(30)
                .allowed_updates(vec![AllowedUpdate::Message])
                .await;

            match result {
                Ok(updates) => {
                    debug!(
                        account_id = aid,
                        count = updates.len(),
                        "got telegram updates"
                    );
                    for update in updates {
                        offset = update.id.as_offset();
                        match update.kind {
                            UpdateKind::Message(msg) => {
                                debug!(
                                    account_id = aid,
                                    chat_id = msg.chat.id.0,
                                    "received telegram message"
                                );
                                if let Err(e) = handlers::handle_message_direct(
                                    msg,
                                    &bot,
                                    &aid,
                                    &poll_accounts,
                                )
                                .await
                                {
                                    error!(
                                        account_id = aid,
                                        error = %e,
                                        "error handling telegram message"
                                    );
                                }
                            },
                            other => {
                                debug!(account_id = aid, "ignoring non-message update: {other:?}");
                            },
                        }
                    }
                },
                Err(e) => {
                    warn!(account_id = aid, error = %e, "telegram getUpdates failed");
                    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                },
            }
        }
    });

    Ok(cancel)
}
