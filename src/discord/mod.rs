mod help;
mod team_status;
mod ticker;
mod todo;
mod update;

use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use chrono_tz::Tz;
use rusqlite::Connection;
use serenity::all::{
    ActionRowComponent, ChannelId, Client, CommandDataOption, CommandDataOptionValue,
    Context as SerenityContext, CreateCommand, CreateInteractionResponse,
    CreateInteractionResponseMessage, EventHandler, GatewayIntents, GuildId, Interaction,
    ModalInteraction, Ready,
};
use serenity::async_trait;

use crate::config::Config;

pub struct Handler {
    guild_id: GuildId,
    db: Arc<Mutex<Connection>>,
    timezone: Tz,
}

#[async_trait]
impl EventHandler for Handler {
    async fn ready(&self, ctx: SerenityContext, ready: Ready) {
        println!("dispatchd connected to Discord as {}", ready.user.name);
        let commands = vec![
            CreateCommand::new("ping").description("Check that dispatchd is alive"),
            help::command(),
            todo::command(),
            update::command(),
            team_status::command(),
        ];
        if let Err(e) = self.guild_id.set_commands(&ctx.http, commands).await {
            eprintln!("failed to register guild commands: {e}");
        }
    }

    async fn interaction_create(&self, ctx: SerenityContext, interaction: Interaction) {
        match interaction {
            Interaction::Command(command) => match command.data.name.as_str() {
                "ping" => {
                    let reply = CreateInteractionResponseMessage::new()
                        .content("pong! dispatchd is alive.");
                    if let Err(e) = command
                        .create_response(&ctx.http, CreateInteractionResponse::Message(reply))
                        .await
                    {
                        eprintln!("failed to respond to /ping: {e}");
                    }
                }
                "help" => help::handle_command(&ctx, &command).await,
                "todo" => match todo::subcommand(&command.data.options) {
                    Some(("create", _)) => todo::handle_create(&ctx, &command).await,
                    Some(("edit", opts)) => {
                        todo::handle_edit(&ctx, &command, opts, &self.db, &self.timezone).await
                    }
                    Some(("delete", opts)) => {
                        todo::handle_delete(&ctx, &command, opts, &self.db, &self.timezone).await
                    }
                    Some(("list", _)) => {
                        todo::handle_list(&ctx, &command, &self.db, &self.timezone).await
                    }
                    Some(("help", _)) => todo::handle_help(&ctx, &command).await,
                    _ => {}
                },
                "update" => update::handle_command(&ctx, &command).await,
                "team-status" => {
                    team_status::handle_command(&ctx, &command, &self.db, &self.timezone).await
                }
                _ => {}
            },
            Interaction::Autocomplete(autocomplete) => match autocomplete.data.name.as_str() {
                "todo" => {
                    todo::handle_autocomplete(&ctx, &autocomplete, &self.db, &self.timezone).await
                }
                "update" => {
                    update::handle_autocomplete(&ctx, &autocomplete, &self.db, &self.timezone).await
                }
                _ => {}
            },
            Interaction::Modal(modal) if modal.data.custom_id == todo::CREATE_MODAL_ID => {
                todo::handle_modal_submission(&ctx, &modal, &self.db, &self.timezone).await;
            }
            Interaction::Modal(modal)
                if modal.data.custom_id.starts_with(todo::EDIT_MODAL_PREFIX) =>
            {
                todo::handle_edit_modal_submission(&ctx, &modal, &self.db, &self.timezone).await;
            }
            Interaction::Modal(modal) if modal.data.custom_id.starts_with(update::MODAL_PREFIX) => {
                update::handle_modal_submission(&ctx, &modal, &self.db, &self.timezone).await;
            }
            _ => {}
        }
    }
}

/// Extracts a text input's value from a submitted modal by its custom_id.
pub(crate) fn modal_value(modal: &ModalInteraction, custom_id: &str) -> Option<String> {
    modal.data.components.iter().find_map(|row| {
        row.components.iter().find_map(|component| match component {
            ActionRowComponent::InputText(input) if input.custom_id == custom_id => {
                input.value.clone()
            }
            _ => None,
        })
    })
}

/// Reads a resolved string-valued command option by name, unwrapping an
/// in-progress autocomplete value too. Shared by `update.rs` (the `task`
/// option) and `todo.rs` (the `id` option on `edit`/`delete`).
pub(crate) fn get_option_string(options: &[CommandDataOption], name: &str) -> Option<String> {
    options
        .iter()
        .find(|o| o.name == name)
        .and_then(|o| match &o.value {
            CommandDataOptionValue::String(s) => Some(s.clone()),
            CommandDataOptionValue::Autocomplete { value, .. } => Some(value.clone()),
            _ => None,
        })
}

/// Connects to the Discord gateway and blocks until the client stops.
/// Registers guild-scoped slash commands (not global, for immediate
/// effect, per the handover doc) on every `ready` - overwriting the
/// guild's command set is idempotent and safe to re-run on reconnects.
pub async fn run(
    token: String,
    guild_id: u64,
    config: Config,
    db: Arc<Mutex<Connection>>,
) -> Result<()> {
    // GUILDS + GUILD_MESSAGES only - MESSAGE_CONTENT isn't needed since
    // everything here is slash commands and modals, not raw message text.
    let intents = GatewayIntents::GUILDS | GatewayIntents::GUILD_MESSAGES;
    let mut client = Client::builder(token, intents)
        .event_handler(Handler {
            guild_id: GuildId::new(guild_id),
            db: db.clone(),
            timezone: config.timezone,
        })
        .await
        .context("failed to build Discord client")?;

    match config.discord_standup_channel_id {
        Some(channel_id) => {
            let http = client.http.clone();
            tokio::spawn(ticker::run(http, db, ChannelId::new(channel_id), config));
        }
        None => println!(
            "reminders disabled - set discord_standup_channel_id in config.toml to enable the daily standup ticker"
        ),
    }

    client.start().await.context("Discord client error")?;
    Ok(())
}
