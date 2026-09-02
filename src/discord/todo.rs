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

use crate::entries::{self, DeleteTodoOutcome, TodoForEdit};

use super::{get_option_string, modal_value};

pub const CREATE_MODAL_ID: &str = "todo_modal";
pub const EDIT_MODAL_PREFIX: &str = "todo_edit_modal:";
const TASK_INPUT_ID: &str = "task";
const NOTES_INPUT_ID: &str = "notes";
const SOW_REF_INPUT_ID: &str = "sow_ref";
const SOW_REF_MAX_LEN: u16 = 30;

const TODO_HELP_TEXT: &str = "\
**/todo subcommands**
`/todo create` - submit a new todo for today (optionally tag it with an SOW ref like M1D2)
`/todo edit id:<...>` - edit one of today's todos, SOW ref included (autocomplete over today's)
`/todo delete id:<...>` - delete one of today's todos
`/todo list` - list today's todos with their ids
`/todo help` - show this message";

pub fn command() -> CreateCommand {
    CreateCommand::new("todo")
        .description("Manage your todos")
        .add_option(CreateCommandOption::new(
            CommandOptionType::SubCommand,
            "create",
            "Submit a new todo for today",
        ))
        .add_option(
            CreateCommandOption::new(
                CommandOptionType::SubCommand,
                "edit",
                "Edit one of today's todos",
            )
            .add_sub_option(
                CreateCommandOption::new(CommandOptionType::String, "id", "Which todo?")
                    .required(true)
                    .set_autocomplete(true),
            ),
        )
        .add_option(
            CreateCommandOption::new(
                CommandOptionType::SubCommand,
                "delete",
                "Delete one of today's todos",
            )
            .add_sub_option(
                CreateCommandOption::new(CommandOptionType::String, "id", "Which todo?")
                    .required(true)
                    .set_autocomplete(true),
            ),
        )
        .add_option(CreateCommandOption::new(
            CommandOptionType::SubCommand,
            "list",
            "List today's todos with their ids",
        ))
        .add_option(CreateCommandOption::new(
            CommandOptionType::SubCommand,
            "help",
            "Show /todo's subcommands",
        ))
}

/// Discord nests a subcommand invocation's own options one level under a
/// single top-level entry named after the subcommand - this pulls out
/// `(subcommand_name, its_nested_options)`.
pub fn subcommand(options: &[CommandDataOption]) -> Option<(&str, &[CommandDataOption])> {
    let top = options.first()?;
    match &top.value {
        CommandDataOptionValue::SubCommand(nested) => Some((top.name.as_str(), nested)),
        _ => None,
    }
}

async fn reply_ephemeral(
    ctx: &SerenityContext,
    command: &CommandInteraction,
    content: impl Into<String>,
    context_label: &str,
) {
    let reply = CreateInteractionResponseMessage::new()
        .content(content.into())
        .ephemeral(true);
    if let Err(e) = command
        .create_response(&ctx.http, CreateInteractionResponse::Message(reply))
        .await
    {
        eprintln!("failed to respond to {context_label}: {e}");
    }
}

pub async fn handle_create(ctx: &SerenityContext, command: &CommandInteraction) {
    let modal = CreateModal::new(CREATE_MODAL_ID, "Submit today's todo").components(vec![
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
        CreateActionRow::InputText(
            CreateInputText::new(
                InputTextStyle::Short,
                "SOW Ref (optional)",
                SOW_REF_INPUT_ID,
            )
            .required(false)
            .max_length(SOW_REF_MAX_LEN),
        ),
    ]);
    if let Err(e) = command
        .create_response(&ctx.http, CreateInteractionResponse::Modal(modal))
        .await
    {
        eprintln!("failed to open /todo create modal: {e}");
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
    let sow_ref = normalize_sow_ref(modal_value(modal, SOW_REF_INPUT_ID));
    let discord_user_id = modal.user.id.to_string();
    let date = entries::today_in(timezone);

    let insert_result = {
        let conn = db.lock().expect("db mutex poisoned");
        entries::insert_todo(
            &conn,
            &discord_user_id,
            &date,
            &task,
            notes.as_deref(),
            sow_ref.as_deref(),
        )
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
        eprintln!("failed to respond to /todo create modal submission: {e}");
    }
}

/// Parses the `id` option's resolved value. `None` means the user typed
/// instead of picking an autocomplete suggestion.
fn parse_id_option(options: &[CommandDataOption]) -> Option<i64> {
    get_option_string(options, "id")?.parse().ok()
}

/// Trims and upper-cases a raw SOW ref from the modal, yielding `None` when
/// nothing meaningful was entered. SOW refs are stored and displayed
/// upper-cased (e.g. `m1d2` -> `M1D2`) so tags line up across members
/// regardless of how each person typed them.
fn normalize_sow_ref(raw: Option<String>) -> Option<String> {
    raw.map(|s| s.trim().to_uppercase())
        .filter(|s| !s.is_empty())
}

pub async fn handle_edit(
    ctx: &SerenityContext,
    command: &CommandInteraction,
    options: &[CommandDataOption],
    db: &Arc<Mutex<Connection>>,
    timezone: &Tz,
) {
    let Some(id) = parse_id_option(options) else {
        reply_ephemeral(
            ctx,
            command,
            "⚠️ Please pick a todo from the suggestions.",
            "/todo edit",
        )
        .await;
        return;
    };
    let discord_user_id = command.user.id.to_string();
    let date = entries::today_in(timezone);

    let current = {
        let conn = db.lock().expect("db mutex poisoned");
        entries::todo_for_edit(&conn, id, &discord_user_id, &date)
    };

    let TodoForEdit {
        task,
        notes,
        sow_ref,
    } = match current {
        Ok(Some(todo)) => todo,
        Ok(None) => {
            reply_ephemeral(
                ctx,
                command,
                "⚠️ Couldn't find that todo (already deleted, or not from today?).",
                "/todo edit",
            )
            .await;
            return;
        }
        Err(e) => {
            eprintln!("failed to look up todo for edit: {e}");
            reply_ephemeral(
                ctx,
                command,
                "⚠️ Something went wrong - please try again.",
                "/todo edit",
            )
            .await;
            return;
        }
    };

    let modal = CreateModal::new(format!("{EDIT_MODAL_PREFIX}{id}"), "Edit todo").components(vec![
        CreateActionRow::InputText(
            CreateInputText::new(InputTextStyle::Short, "Task", TASK_INPUT_ID)
                .required(true)
                .value(task),
        ),
        CreateActionRow::InputText(
            CreateInputText::new(
                InputTextStyle::Paragraph,
                "Notes (optional)",
                NOTES_INPUT_ID,
            )
            .required(false)
            .value(notes.unwrap_or_default()),
        ),
        CreateActionRow::InputText(
            CreateInputText::new(
                InputTextStyle::Short,
                "SOW Ref (optional)",
                SOW_REF_INPUT_ID,
            )
            .required(false)
            .max_length(SOW_REF_MAX_LEN)
            .value(sow_ref.unwrap_or_default()),
        ),
    ]);
    if let Err(e) = command
        .create_response(&ctx.http, CreateInteractionResponse::Modal(modal))
        .await
    {
        eprintln!("failed to open /todo edit modal: {e}");
    }
}

pub async fn handle_edit_modal_submission(
    ctx: &SerenityContext,
    modal: &ModalInteraction,
    db: &Arc<Mutex<Connection>>,
    timezone: &Tz,
) {
    let Some(id) = modal
        .data
        .custom_id
        .strip_prefix(EDIT_MODAL_PREFIX)
        .and_then(|s| s.parse::<i64>().ok())
    else {
        eprintln!(
            "malformed /todo edit modal custom_id: {}",
            modal.data.custom_id
        );
        return;
    };

    let task = modal_value(modal, TASK_INPUT_ID).unwrap_or_default();
    let notes = modal_value(modal, NOTES_INPUT_ID).filter(|s| !s.trim().is_empty());
    let sow_ref = normalize_sow_ref(modal_value(modal, SOW_REF_INPUT_ID));
    let discord_user_id = modal.user.id.to_string();
    let date = entries::today_in(timezone);

    let updated = {
        let conn = db.lock().expect("db mutex poisoned");
        entries::update_todo(
            &conn,
            id,
            &discord_user_id,
            &date,
            &task,
            notes.as_deref(),
            sow_ref.as_deref(),
        )
    };

    let reply_text = match updated {
        Ok(true) => format!("✅ Todo updated: {task}"),
        Ok(false) => "⚠️ Couldn't find that todo anymore (already deleted?).".to_string(),
        Err(e) => {
            eprintln!("failed to update todo: {e}");
            "⚠️ Something went wrong saving your edit - please try again.".to_string()
        }
    };

    let reply = CreateInteractionResponseMessage::new()
        .content(reply_text)
        .ephemeral(true);
    if let Err(e) = modal
        .create_response(&ctx.http, CreateInteractionResponse::Message(reply))
        .await
    {
        eprintln!("failed to respond to /todo edit modal submission: {e}");
    }
}

pub async fn handle_delete(
    ctx: &SerenityContext,
    command: &CommandInteraction,
    options: &[CommandDataOption],
    db: &Arc<Mutex<Connection>>,
    timezone: &Tz,
) {
    let Some(id) = parse_id_option(options) else {
        reply_ephemeral(
            ctx,
            command,
            "⚠️ Please pick a todo from the suggestions.",
            "/todo delete",
        )
        .await;
        return;
    };
    let discord_user_id = command.user.id.to_string();
    let date = entries::today_in(timezone);

    let outcome = {
        let conn = db.lock().expect("db mutex poisoned");
        entries::delete_todo(&conn, id, &discord_user_id, &date)
    };

    let reply_text = match outcome {
        Ok(DeleteTodoOutcome::Deleted(task)) => format!("🗑️ Deleted: {task}"),
        Ok(DeleteTodoOutcome::NotFound) => {
            "⚠️ Couldn't find that todo (already deleted, or not from today?).".to_string()
        }
        Ok(DeleteTodoOutcome::StillReferenced) => {
            "⚠️ Can't delete - you've already posted a /progress report against this todo."
                .to_string()
        }
        Err(e) => {
            eprintln!("failed to delete todo: {e}");
            "⚠️ Something went wrong - please try again.".to_string()
        }
    };
    reply_ephemeral(ctx, command, reply_text, "/todo delete").await;
}

pub async fn handle_list(
    ctx: &SerenityContext,
    command: &CommandInteraction,
    db: &Arc<Mutex<Connection>>,
    timezone: &Tz,
) {
    let discord_user_id = command.user.id.to_string();
    let date = entries::today_in(timezone);

    let todos = {
        let conn = db.lock().expect("db mutex poisoned");
        entries::list_todos(&conn, &discord_user_id, &date, "")
    };

    let reply_text = match todos {
        Ok(rows) if rows.is_empty() => {
            "You haven't submitted a todo today yet - use `/todo create`.".to_string()
        }
        Ok(rows) => {
            let lines: Vec<String> = rows
                .iter()
                .map(|(id, task, sow_ref)| match sow_ref {
                    Some(r) => format!("`{id}` {task} [{r}]"),
                    None => format!("`{id}` {task}"),
                })
                .collect();
            format!("**Today's todos:**\n{}", lines.join("\n"))
        }
        Err(e) => {
            eprintln!("failed to list todos: {e}");
            "⚠️ Something went wrong - please try again.".to_string()
        }
    };
    reply_ephemeral(ctx, command, reply_text, "/todo list").await;
}

pub async fn handle_help(ctx: &SerenityContext, command: &CommandInteraction) {
    reply_ephemeral(ctx, command, TODO_HELP_TEXT, "/todo help").await;
}

pub async fn handle_autocomplete(
    ctx: &SerenityContext,
    autocomplete: &AutocompleteInteraction,
    db: &Arc<Mutex<Connection>>,
    timezone: &Tz,
) {
    let Some((_, options)) = subcommand(&autocomplete.data.options) else {
        return;
    };
    let partial = get_option_string(options, "id").unwrap_or_default();
    let discord_user_id = autocomplete.user.id.to_string();
    let date = entries::today_in(timezone);

    let choices = {
        let conn = db.lock().expect("db mutex poisoned");
        entries::list_todos(&conn, &discord_user_id, &date, &partial)
    };

    let response = match choices {
        Ok(rows) => {
            let choices = rows
                .into_iter()
                .map(|(id, task, _sow_ref)| AutocompleteChoice::new(task, id.to_string()))
                .collect();
            CreateAutocompleteResponse::new().set_choices(choices)
        }
        Err(e) => {
            eprintln!("failed to list todos for autocomplete: {e}");
            CreateAutocompleteResponse::new()
        }
    };

    if let Err(e) = autocomplete
        .create_response(&ctx.http, CreateInteractionResponse::Autocomplete(response))
        .await
    {
        eprintln!("failed to respond to /todo autocomplete: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn todo_help_text_mentions_every_subcommand() {
        for needle in [
            "/todo create",
            "/todo edit",
            "/todo delete",
            "/todo list",
            "/todo help",
        ] {
            assert!(TODO_HELP_TEXT.contains(needle), "missing {needle}");
        }
    }

    #[test]
    fn normalize_sow_ref_uppercases_and_trims() {
        assert_eq!(
            normalize_sow_ref(Some("  m1d2 ".to_string())),
            Some("M1D2".to_string())
        );
        assert_eq!(
            normalize_sow_ref(Some("M1D2".to_string())),
            Some("M1D2".to_string())
        );
    }

    #[test]
    fn normalize_sow_ref_drops_empty_and_whitespace_only() {
        assert_eq!(normalize_sow_ref(None), None);
        assert_eq!(normalize_sow_ref(Some(String::new())), None);
        assert_eq!(normalize_sow_ref(Some("   ".to_string())), None);
    }

    // `subcommand()` isn't unit-tested here: `CommandDataOption` is
    // `#[non_exhaustive]` with no public constructor, so a real value
    // can't be built outside serenity itself - same "can't exercise real
    // Discord types without a live connection" limitation as the rest of
    // this codebase's Discord-facing code (see e.g. `ticker.rs`'s
    // `is_unknown_channel_error`).
}
