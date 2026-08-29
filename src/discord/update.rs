use std::sync::{Arc, Mutex};

use chrono_tz::Tz;
use rusqlite::Connection;
use serenity::all::{
    AutocompleteChoice, CommandDataOption, CommandDataOptionValue, CommandInteraction,
    CommandInteraction as AutocompleteInteraction, CommandOptionType, Context as SerenityContext,
    CreateActionRow, CreateAutocompleteResponse, CreateCommand, CreateCommandOption,
    CreateInputText, CreateInteractionResponse, CreateInteractionResponseMessage, CreateModal,
    InputTextStyle, ModalInteraction,
};

use crate::entries;

use super::modal_value;

pub const MODAL_PREFIX: &str = "update_modal:";
const PROGRESS_INPUT_ID: &str = "progress";
const BLOCKER_INPUT_ID: &str = "blocker";
const TASK_ENCODE_MAX_LEN: usize = 60;

pub fn command() -> CreateCommand {
    CreateCommand::new("update")
        .description("Submit an update against today's todos")
        .add_option(
            CreateCommandOption::new(
                CommandOptionType::String,
                "task",
                "Which todo? (or type a new one for unplanned work)",
            )
            .required(true)
            .max_length(100)
            .set_autocomplete(true),
        )
        .add_option(
            CreateCommandOption::new(CommandOptionType::String, "status", "Status")
                .required(true)
                .add_string_choice("Done", "done")
                .add_string_choice("In Progress", "in_progress")
                .add_string_choice("Blocked", "blocked"),
        )
}

fn get_option_string(options: &[CommandDataOption], name: &str) -> Option<String> {
    options
        .iter()
        .find(|o| o.name == name)
        .and_then(|o| match &o.value {
            CommandDataOptionValue::String(s) => Some(s.clone()),
            CommandDataOptionValue::Autocomplete { value, .. } => Some(value.clone()),
            _ => None,
        })
}

/// "done" -> "D", etc. Single letters to keep the modal custom_id short.
fn status_code(status: &str) -> &'static str {
    match status {
        "done" => "D",
        "blocked" => "B",
        _ => "P", // in_progress, and any unrecognized value
    }
}

fn decode_status_code(code: &str) -> Option<&'static str> {
    match code {
        "D" => Some("done"),
        "P" => Some("in_progress"),
        "B" => Some("blocked"),
        _ => None,
    }
}

fn status_label(status: &str) -> &'static str {
    match status {
        "done" => "Done",
        "blocked" => "Blocked",
        _ => "In Progress",
    }
}

/// Encodes a resolved `task` option value for embedding in the modal's
/// custom_id. An `"id:<n>"` reference (already short) passes through
/// unchanged; free text longer than `TASK_ENCODE_MAX_LEN` is truncated.
/// Returns `(encoded, was_truncated)`.
fn encode_task_for_modal(task_value: &str) -> (String, bool) {
    if task_value.starts_with("id:") {
        return (task_value.to_string(), false);
    }
    let char_count = task_value.chars().count();
    if char_count <= TASK_ENCODE_MAX_LEN {
        (task_value.to_string(), false)
    } else {
        (task_value.chars().take(TASK_ENCODE_MAX_LEN).collect(), true)
    }
}

/// Parses `"update_modal:<status_code>:<task_encoded>"` into
/// `(status_code, task_encoded)`. `None` if malformed.
fn parse_custom_id(custom_id: &str) -> Option<(&str, &str)> {
    let rest = custom_id.strip_prefix(MODAL_PREFIX)?;
    rest.split_once(':')
}

pub async fn handle_autocomplete(
    ctx: &SerenityContext,
    autocomplete: &AutocompleteInteraction,
    db: &Arc<Mutex<Connection>>,
    timezone: &Tz,
) {
    let partial = get_option_string(&autocomplete.data.options, "task").unwrap_or_default();
    let discord_user_id = autocomplete.user.id.to_string();
    let date = entries::today_in(timezone);

    let choices = {
        let conn = db.lock().expect("db mutex poisoned");
        entries::list_open_todos(&conn, &discord_user_id, &date, &partial)
    };

    let response = match choices {
        Ok(rows) => {
            let choices = rows
                .into_iter()
                .map(|(id, task)| AutocompleteChoice::new(task, format!("id:{id}")))
                .collect();
            CreateAutocompleteResponse::new().set_choices(choices)
        }
        Err(e) => {
            eprintln!("failed to list open todos for autocomplete: {e}");
            CreateAutocompleteResponse::new()
        }
    };

    if let Err(e) = autocomplete
        .create_response(&ctx.http, CreateInteractionResponse::Autocomplete(response))
        .await
    {
        eprintln!("failed to respond to /update autocomplete: {e}");
    }
}

pub async fn handle_command(ctx: &SerenityContext, command: &CommandInteraction) {
    let task_value = get_option_string(&command.data.options, "task").unwrap_or_default();
    let status_value = get_option_string(&command.data.options, "status").unwrap_or_default();

    let (task_encoded, _) = encode_task_for_modal(&task_value);
    let custom_id = format!(
        "{MODAL_PREFIX}{}:{task_encoded}",
        status_code(&status_value)
    );

    let modal = CreateModal::new(custom_id, "Submit update").components(vec![
        CreateActionRow::InputText(
            CreateInputText::new(InputTextStyle::Paragraph, "Progress", PROGRESS_INPUT_ID)
                .required(true),
        ),
        CreateActionRow::InputText(
            CreateInputText::new(
                InputTextStyle::Short,
                "Blocker (if blocked)",
                BLOCKER_INPUT_ID,
            )
            .required(false),
        ),
    ]);

    if let Err(e) = command
        .create_response(&ctx.http, CreateInteractionResponse::Modal(modal))
        .await
    {
        eprintln!("failed to open /update modal: {e}");
    }
}

pub async fn handle_modal_submission(
    ctx: &SerenityContext,
    modal: &ModalInteraction,
    db: &Arc<Mutex<Connection>>,
    timezone: &Tz,
) {
    let discord_user_id = modal.user.id.to_string();

    let Some((status_code_str, task_encoded)) = parse_custom_id(&modal.data.custom_id) else {
        eprintln!(
            "malformed /update modal custom_id: {}",
            modal.data.custom_id
        );
        return;
    };
    let status = decode_status_code(status_code_str).unwrap_or("in_progress");

    let progress = modal_value(modal, PROGRESS_INPUT_ID).unwrap_or_default();
    let blocker = modal_value(modal, BLOCKER_INPUT_ID).filter(|s| !s.trim().is_empty());
    let date = entries::today_in(timezone);

    let insert_result = {
        let conn = db.lock().expect("db mutex poisoned");

        let (todo_id, task) = if let Some(id_str) = task_encoded.strip_prefix("id:") {
            match id_str.parse::<i64>() {
                Ok(id) => match entries::todo_task(&conn, id, &discord_user_id) {
                    Ok(Some(task)) => (Some(id), task),
                    Ok(None) => (None, task_encoded.to_string()),
                    Err(e) => {
                        eprintln!("failed to resolve todo task for update: {e}");
                        (None, task_encoded.to_string())
                    }
                },
                Err(_) => (None, task_encoded.to_string()),
            }
        } else {
            (None, task_encoded.to_string())
        };

        entries::insert_update(
            &conn,
            &discord_user_id,
            &date,
            &task,
            todo_id,
            status,
            &progress,
            blocker.as_deref(),
        )
        .map(|_| task)
    };

    let reply_text = match insert_result {
        Ok(task) => format!("✅ Update saved: {task} — {}", status_label(status)),
        Err(e) => {
            eprintln!("failed to insert update: {e}");
            "⚠️ Something went wrong saving your update - please try again.".to_string()
        }
    };

    let reply = CreateInteractionResponseMessage::new()
        .content(reply_text)
        .ephemeral(true);
    if let Err(e) = modal
        .create_response(&ctx.http, CreateInteractionResponse::Message(reply))
        .await
    {
        eprintln!("failed to respond to /update modal submission: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_code_and_decode_round_trip() {
        for status in ["done", "in_progress", "blocked"] {
            let code = status_code(status);
            assert_eq!(decode_status_code(code), Some(status));
        }
    }

    #[test]
    fn encode_task_passes_short_text_through_unchanged() {
        let (encoded, truncated) = encode_task_for_modal("Fix the bug");
        assert_eq!(encoded, "Fix the bug");
        assert!(!truncated);
    }

    #[test]
    fn encode_task_truncates_long_free_text() {
        let long_task = "x".repeat(80);
        let (encoded, truncated) = encode_task_for_modal(&long_task);
        assert_eq!(encoded.chars().count(), TASK_ENCODE_MAX_LEN);
        assert!(truncated);
    }

    #[test]
    fn encode_task_passes_id_refs_through_untouched_regardless_of_length() {
        let (encoded, truncated) = encode_task_for_modal("id:123456789012345");
        assert_eq!(encoded, "id:123456789012345");
        assert!(!truncated);
    }

    #[test]
    fn parse_custom_id_splits_valid_input() {
        assert_eq!(
            parse_custom_id("update_modal:D:id:42"),
            Some(("D", "id:42"))
        );
    }

    #[test]
    fn parse_custom_id_rejects_malformed_input() {
        assert_eq!(parse_custom_id("something_else:D:id:42"), None);
        assert_eq!(parse_custom_id("update_modal:D"), None);
    }
}
