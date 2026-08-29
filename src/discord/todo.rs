use std::sync::{Arc, Mutex};

use chrono_tz::Tz;
use rusqlite::Connection;
use serenity::all::{
    CommandInteraction, Context as SerenityContext, CreateActionRow, CreateCommand,
    CreateInputText, CreateInteractionResponse, CreateInteractionResponseMessage, CreateModal,
    InputTextStyle, ModalInteraction,
};

use crate::entries;

use super::modal_value;

pub const MODAL_ID: &str = "todo_modal";
const TASK_INPUT_ID: &str = "task";
const NOTES_INPUT_ID: &str = "notes";

pub fn command() -> CreateCommand {
    CreateCommand::new("todo").description("Submit today's todo")
}

pub async fn handle_command(ctx: &SerenityContext, command: &CommandInteraction) {
    let modal = CreateModal::new(MODAL_ID, "Submit today's todo").components(vec![
        CreateActionRow::InputText(
            CreateInputText::new(InputTextStyle::Short, "Task", TASK_INPUT_ID).required(true),
        ),
        CreateActionRow::InputText(
            CreateInputText::new(
                InputTextStyle::Paragraph,
                "Notes (optional)",
                NOTES_INPUT_ID,
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

pub async fn handle_modal_submission(
    ctx: &SerenityContext,
    modal: &ModalInteraction,
    db: &Arc<Mutex<Connection>>,
    timezone: &Tz,
) {
    let task = modal_value(modal, TASK_INPUT_ID).unwrap_or_default();
    let notes = modal_value(modal, NOTES_INPUT_ID).filter(|s| !s.trim().is_empty());
    let discord_user_id = modal.user.id.to_string();
    let date = entries::today_in(timezone);

    let insert_result = {
        let conn = db.lock().expect("db mutex poisoned");
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
