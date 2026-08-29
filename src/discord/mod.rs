mod team_status;
mod todo;
mod update;

use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use chrono_tz::Tz;
use rusqlite::Connection;
use serenity::all::{
    ActionRowComponent, Client, Context as SerenityContext, CreateCommand,
    CreateInteractionResponse, CreateInteractionResponseMessage, EventHandler, GatewayIntents,
    GuildId, Interaction, ModalInteraction, Ready,
};
use serenity::async_trait;

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
                "todo" => todo::handle_command(&ctx, &command).await,
                "update" => update::handle_command(&ctx, &command).await,
                "team-status" => {
                    team_status::handle_command(&ctx, &command, &self.db, &self.timezone).await
                }
                _ => {}
            },
            Interaction::Autocomplete(autocomplete) => {
                if autocomplete.data.name == "update" {
                    update::handle_autocomplete(&ctx, &autocomplete, &self.db, &self.timezone)
                        .await;
                }
            }
            Interaction::Modal(modal) if modal.data.custom_id == todo::MODAL_ID => {
                todo::handle_modal_submission(&ctx, &modal, &self.db, &self.timezone).await;
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

/// Connects to the Discord gateway and blocks until the client stops.
/// Registers guild-scoped slash commands (not global, for immediate
/// effect, per the handover doc) on every `ready` - overwriting the
/// guild's command set is idempotent and safe to re-run on reconnects.
pub async fn run(
    token: String,
    guild_id: u64,
    timezone: Tz,
    db: Arc<Mutex<Connection>>,
) -> Result<()> {
    // GUILDS + GUILD_MESSAGES only - MESSAGE_CONTENT isn't needed since
    // everything here is slash commands and modals, not raw message text.
    let intents = GatewayIntents::GUILDS | GatewayIntents::GUILD_MESSAGES;
    let mut client = Client::builder(token, intents)
        .event_handler(Handler {
            guild_id: GuildId::new(guild_id),
            db,
            timezone,
        })
        .await
        .context("failed to build Discord client")?;
    client.start().await.context("Discord client error")?;
    Ok(())
}
