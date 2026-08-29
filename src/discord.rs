use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use chrono_tz::Tz;
use rusqlite::Connection;
use serenity::all::{
    ActionRowComponent, Client, Context as SerenityContext, CreateActionRow, CreateCommand,
    CreateInputText, CreateInteractionResponse, CreateInteractionResponseMessage, CreateModal,
    EventHandler, GatewayIntents, GuildId, InputTextStyle, Interaction, ModalInteraction, Ready,
};
use serenity::async_trait;

use crate::entries;

const TODO_MODAL_ID: &str = "todo_modal";
const TODO_TASK_INPUT_ID: &str = "task";
const TODO_NOTES_INPUT_ID: &str = "notes";

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
            CreateCommand::new("todo").description("Submit today's todo"),
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
                "todo" => {
                    let modal =
                        CreateModal::new(TODO_MODAL_ID, "Submit today's todo").components(vec![
                            CreateActionRow::InputText(
                                CreateInputText::new(
                                    InputTextStyle::Short,
                                    "Task",
                                    TODO_TASK_INPUT_ID,
                                )
                                .required(true),
                            ),
                            CreateActionRow::InputText(
                                CreateInputText::new(
                                    InputTextStyle::Paragraph,
                                    "Notes (optional)",
                                    TODO_NOTES_INPUT_ID,
                                )
                                .required(false),
                            ),
                        ]);
                    if let Err(e) = command
                        .create_response(&ctx.http, CreateInteractionResponse::Modal(modal))
                        .await
                    {
                        eprintln!("failed to open /todo modal: {e}");
                    }
                }
                _ => {}
            },
            Interaction::Modal(modal) if modal.data.custom_id == TODO_MODAL_ID => {
                self.handle_todo_submission(&ctx, &modal).await;
            }
            _ => {}
        }
    }
}

impl Handler {
    async fn handle_todo_submission(&self, ctx: &SerenityContext, modal: &ModalInteraction) {
        let task = modal_value(modal, TODO_TASK_INPUT_ID).unwrap_or_default();
        let notes = modal_value(modal, TODO_NOTES_INPUT_ID).filter(|s| !s.trim().is_empty());
        let discord_user_id = modal.user.id.to_string();
        let date = entries::today_in(&self.timezone);

        let insert_result = {
            let conn = self.db.lock().expect("db mutex poisoned");
            entries::insert_todo(&conn, &discord_user_id, &date, &task, notes.as_deref())
        };

        let reply_text = match insert_result {
            Ok(_) => format!("✅ Todo saved: {task}"),
            Err(e) => {
                eprintln!("failed to insert todo: {e}");
                "⚠️ Something went wrong saving your todo - please try again.".to_string()
            }
        };

        let reply = CreateInteractionResponseMessage::new()
            .content(reply_text)
            .ephemeral(true);
        if let Err(e) = modal
            .create_response(&ctx.http, CreateInteractionResponse::Message(reply))
            .await
        {
            eprintln!("failed to respond to /todo modal submission: {e}");
        }
    }
}

/// Extracts a text input's value from a submitted modal by its custom_id.
fn modal_value(modal: &ModalInteraction, custom_id: &str) -> Option<String> {
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
