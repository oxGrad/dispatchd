use std::sync::{Arc, Mutex};

use jiff::tz::TimeZone;
use rusqlite::Connection;
use serenity::all::{
    AutocompleteChoice, CommandDataOption, CommandDataOptionValue, CommandInteraction,
    CommandInteraction as AutocompleteInteraction, CommandOptionType, Context as SerenityContext,
    CreateActionRow, CreateAutocompleteResponse, CreateCommand, CreateCommandOption,
    CreateInputText, CreateInteractionResponse, CreateInteractionResponseFollowup,
    CreateInteractionResponseMessage, CreateModal, InputTextStyle, ModalInteraction,
};

use crate::entries::{self, DeleteTodoOutcome, TodoForEdit};

use super::{get_option_string, modal_value};

pub const ADD_MODAL_ID: &str = "todo_modal";
pub const EDIT_MODAL_PREFIX: &str = "todo_edit_modal:";
pub const CARRY_MODAL_PREFIX: &str = "todo_carry_modal:";
const TASK_INPUT_ID: &str = "task";
const NOTES_INPUT_ID: &str = "notes";
const SOW_REF_INPUT_ID: &str = "sow_ref";
const SOW_REF_MAX_LEN: u16 = 30;

const TODO_HELP_TEXT: &str = "\
**/todo subcommands**
`/todo add [carryover:<...>]` - submit a new todo for today (optionally tag it with an SOW ref like M1D2), or carry over an unfinished one (prefilled)
`/todo edit id:<...>` - edit one of today's todos, SOW ref included (autocomplete over today's)
`/todo delete id:<...>` - delete one of today's todos
`/todo list` - list today's todos with their ids
`/todo help` - show this message";

pub fn command() -> CreateCommand {
    CreateCommand::new("todo")
        .description("Manage your todos")
        .add_option(
            CreateCommandOption::new(
                CommandOptionType::SubCommand,
                "add",
                "Submit a new todo for today",
            )
            .add_sub_option(
                CreateCommandOption::new(
                    CommandOptionType::String,
                    "carryover",
                    "Unfinished todo from an earlier day to carry over",
                )
                .required(false)
                .set_autocomplete(true),
            ),
        )
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

/// The add/carry modal: same three inputs as `/todo edit`, optionally prefilled.
fn todo_modal(custom_id: String, title: &str, prefill: Option<TodoForEdit>) -> CreateModal {
    let TodoForEdit {
        task,
        notes,
        sow_ref,
    } = prefill.unwrap_or(TodoForEdit {
        task: String::new(),
        notes: None,
        sow_ref: None,
    });
    CreateModal::new(custom_id, title).components(vec![
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
    ])
}

pub async fn handle_add(
    ctx: &SerenityContext,
    command: &CommandInteraction,
    options: &[CommandDataOption],
    db: &Arc<Mutex<Connection>>,
    timezone: &TimeZone,
    carryover_lookback_days: u32,
) {
    let modal = match get_option_string(options, "carryover") {
        None => todo_modal(ADD_MODAL_ID.to_string(), "Submit today's todo", None),
        Some(raw) => {
            let origin = match raw.parse::<i64>() {
                Ok(id) => {
                    let conn = db.lock().expect("db mutex poisoned");
                    entries::carryover_origin(
                        &conn,
                        &command.user.id.to_string(),
                        &entries::today_in(timezone),
                        carryover_lookback_days as i64,
                        id,
                    )
                    .map(|o| o.map(|o| (id, o)))
                }
                Err(_) => Ok(None),
            };
            match origin {
                Ok(Some((id, prefill))) => todo_modal(
                    format!("{CARRY_MODAL_PREFIX}{id}"),
                    "Carry over todo",
                    Some(prefill),
                ),
                Ok(None) => {
                    reply_ephemeral(
                        ctx,
                        command,
                        "⚠️ Please pick an unfinished todo from the suggestions.",
                        "/todo add",
                    )
                    .await;
                    return;
                }
                Err(e) => {
                    eprintln!("failed to look up carry-over origin: {e}");
                    reply_ephemeral(
                        ctx,
                        command,
                        "⚠️ Something went wrong - please try again.",
                        "/todo add",
                    )
                    .await;
                    return;
                }
            }
        }
    };
    if let Err(e) = command
        .create_response(&ctx.http, CreateInteractionResponse::Modal(modal))
        .await
    {
        eprintln!("failed to open /todo add modal: {e}");
    }
}

pub async fn handle_carry_modal_submission(
    ctx: &SerenityContext,
    modal: &ModalInteraction,
    db: &Arc<Mutex<Connection>>,
    timezone: &TimeZone,
    carryover_lookback_days: u32,
) {
    let Some(origin_id) = modal
        .data
        .custom_id
        .strip_prefix(CARRY_MODAL_PREFIX)
        .and_then(|s| s.parse::<i64>().ok())
    else {
        eprintln!(
            "malformed /todo carry modal custom_id: {}",
            modal.data.custom_id
        );
        return;
    };
    let task = modal_value(modal, TASK_INPUT_ID).unwrap_or_default();
    let notes = modal_value(modal, NOTES_INPUT_ID).filter(|s| !s.trim().is_empty());
    let sow_ref = normalize_sow_ref(modal_value(modal, SOW_REF_INPUT_ID));

    let result = {
        let conn = db.lock().expect("db mutex poisoned");
        entries::carry_over_todo(
            &conn,
            &modal.user.id.to_string(),
            &entries::today_in(timezone),
            carryover_lookback_days as i64,
            origin_id,
            &task,
            notes.as_deref(),
            sow_ref.as_deref(),
        )
    };
    let reply_text = match result {
        Ok(Some(_)) => format!("✅ Todo carried over: {task}"),
        Ok(None) => {
            "⚠️ That todo can't be carried over anymore (already carried, finished, or too old)."
                .to_string()
        }
        Err(e) => {
            eprintln!("failed to carry over todo: {e}");
            "⚠️ Something went wrong - please try again.".to_string()
        }
    };
    let reply = CreateInteractionResponseMessage::new()
        .content(reply_text)
        .ephemeral(true);
    if let Err(e) = modal
        .create_response(&ctx.http, CreateInteractionResponse::Message(reply))
        .await
    {
        eprintln!("failed to respond to /todo carry modal submission: {e}");
    }
}

pub async fn handle_modal_submission(
    ctx: &SerenityContext,
    modal: &ModalInteraction,
    db: &Arc<Mutex<Connection>>,
    timezone: &TimeZone,
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
        eprintln!("failed to respond to /todo add modal submission: {e}");
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
    timezone: &TimeZone,
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

    let modal = todo_modal(
        format!("{EDIT_MODAL_PREFIX}{id}"),
        "Edit todo",
        Some(TodoForEdit {
            task,
            notes,
            sow_ref,
        }),
    );
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
    timezone: &TimeZone,
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
    timezone: &TimeZone,
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

/// Builds the `/todo list` reply: today's todos with ids, then a
/// read-only "Carried over" section for still-open todos from recent
/// past days. Pure.
fn format_todo_list(
    today: &[(i64, String, Option<String>)],
    carried: &[entries::CarryoverTodo],
) -> String {
    let mut sections: Vec<String> = Vec::new();
    if today.is_empty() {
        sections.push("You haven't submitted a todo today yet - use `/todo add`.".to_string());
    } else {
        let lines: Vec<String> = today
            .iter()
            .map(|(id, task, sow_ref)| match sow_ref {
                Some(r) => format!("`{id}` {task} [{r}]"),
                None => format!("`{id}` {task}"),
            })
            .collect();
        sections.push(format!("**Today's todos:**\n{}", lines.join("\n")));
    }
    if !carried.is_empty() {
        let lines: Vec<String> = carried
            .iter()
            .map(|c| match &c.sow_ref {
                Some(r) => format!(
                    "{} [{r}] — from {}",
                    c.task,
                    entries::short_date(&c.origin_date)
                ),
                None => format!("{} — from {}", c.task, entries::short_date(&c.origin_date)),
            })
            .collect();
        sections.push(format!(
            "**Not carried over yet** (use `/todo add carryover:`):\n{}",
            lines.join("\n")
        ));
    }
    sections.join("\n\n")
}

pub async fn handle_list(
    ctx: &SerenityContext,
    command: &CommandInteraction,
    db: &Arc<Mutex<Connection>>,
    timezone: &TimeZone,
    carryover_lookback_days: u32,
) {
    let discord_user_id = command.user.id.to_string();
    let date = entries::today_in(timezone);

    let result = {
        let conn = db.lock().expect("db mutex poisoned");
        let today = entries::list_todos(&conn, &discord_user_id, &date, "");
        let carried = entries::carryover_todos(
            &conn,
            &discord_user_id,
            &date,
            carryover_lookback_days as i64,
            "",
        );
        today.and_then(|t| carried.map(|c| (t, c)))
    };

    let reply_text = match result {
        Ok((today, carried)) => format_todo_list(&today, &carried),
        Err(e) => {
            eprintln!("failed to list todos: {e}");
            "⚠️ Something went wrong - please try again.".to_string()
        }
    };

    // A long carried-over list can push the reply past Discord's 2000-char
    // message cap - chunk it the same way `/team report` does (split on the
    // section blank line, 1900 for emoji headroom), first chunk in the
    // response, the rest as ephemeral follow-ups.
    let mut chunks = crate::status::split_into_messages(&reply_text, 1900).into_iter();
    let first = chunks.next().unwrap_or(reply_text);
    reply_ephemeral(ctx, command, first, "/todo list").await;
    for chunk in chunks {
        if let Err(e) = command
            .create_followup(
                &ctx.http,
                CreateInteractionResponseFollowup::new()
                    .content(chunk)
                    .ephemeral(true),
            )
            .await
        {
            eprintln!("failed to send /todo list follow-up: {e}");
        }
    }
}

pub async fn handle_help(ctx: &SerenityContext, command: &CommandInteraction) {
    reply_ephemeral(ctx, command, TODO_HELP_TEXT, "/todo help").await;
}

/// Discord rejects an autocomplete response outright if any choice's name
/// exceeds this - and the task modal input has no length cap, unlike
/// `SOW_REF_INPUT_ID`.
const AUTOCOMPLETE_NAME_MAX_CHARS: usize = 100;

/// Autocomplete suggestion label for `/todo edit`|`delete` - shows the id
/// the option value carries so the member knows what they're picking,
/// plus the SOW ref if any (the `id` help text promises both). Truncates
/// the task so the whole label always fits Discord's 100-char choice
/// name limit - one oversized choice would otherwise fail the entire
/// autocomplete response, hiding every suggestion.
fn format_todo_choice_label(id: i64, task: &str, sow_ref: &Option<String>) -> String {
    let prefix = format!("#{id} ");
    let suffix = sow_ref
        .as_deref()
        .map(|r| format!(" [{r}]"))
        .unwrap_or_default();
    let task_budget =
        AUTOCOMPLETE_NAME_MAX_CHARS.saturating_sub(prefix.chars().count() + suffix.chars().count());
    format!("{prefix}{}{suffix}", truncate_chars(task, task_budget))
}

/// Autocomplete label for a carry-over candidate: `"{task} · from Aug 27"`,
/// capped at Discord's 100-char choice-name limit.
fn carryover_choice_label(task: &str, origin_date: &str) -> String {
    truncate_chars(
        &format!("{task} · from {}", entries::short_date(origin_date)),
        100,
    )
}

fn truncate_chars(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    let keep: String = s.chars().take(max_chars.saturating_sub(1)).collect();
    format!("{keep}…")
}

pub async fn handle_autocomplete(
    ctx: &SerenityContext,
    autocomplete: &AutocompleteInteraction,
    db: &Arc<Mutex<Connection>>,
    timezone: &TimeZone,
    carryover_lookback_days: u32,
) {
    let Some((sub, options)) = subcommand(&autocomplete.data.options) else {
        return;
    };
    let discord_user_id = autocomplete.user.id.to_string();
    let date = entries::today_in(timezone);

    if sub == "add" {
        let partial = get_option_string(options, "carryover").unwrap_or_default();
        let rows = {
            let conn = db.lock().expect("db mutex poisoned");
            entries::carryover_todos(
                &conn,
                &discord_user_id,
                &date,
                carryover_lookback_days as i64,
                &partial,
            )
        };
        let response = match rows {
            Ok(rows) => CreateAutocompleteResponse::new().set_choices(
                rows.into_iter()
                    .map(|c| {
                        AutocompleteChoice::new(
                            carryover_choice_label(&c.task, &c.origin_date),
                            c.id.to_string(),
                        )
                    })
                    .collect(),
            ),
            Err(e) => {
                eprintln!("failed to list carry-over todos for autocomplete: {e}");
                CreateAutocompleteResponse::new()
            }
        };
        if let Err(e) = autocomplete
            .create_response(&ctx.http, CreateInteractionResponse::Autocomplete(response))
            .await
        {
            eprintln!("failed to respond to /todo add autocomplete: {e}");
        }
        return;
    }
    let partial = get_option_string(options, "id").unwrap_or_default();

    let choices = {
        let conn = db.lock().expect("db mutex poisoned");
        entries::list_todos(&conn, &discord_user_id, &date, &partial)
    };

    let response = match choices {
        Ok(rows) => {
            let choices = rows
                .into_iter()
                .map(|(id, task, sow_ref)| {
                    AutocompleteChoice::new(
                        format_todo_choice_label(id, &task, &sow_ref),
                        id.to_string(),
                    )
                })
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
            "/todo add",
            "carryover:",
            "/todo edit",
            "/todo delete",
            "/todo list",
            "/todo help",
        ] {
            assert!(TODO_HELP_TEXT.contains(needle), "missing {needle}");
        }
    }

    #[test]
    fn carryover_choice_label_shows_origin_date_and_fits_discord() {
        assert_eq!(
            carryover_choice_label("Fix login", "2026-08-27"),
            "Fix login · from Aug 27"
        );
        let long = carryover_choice_label(&"x".repeat(300), "2026-08-27");
        assert_eq!(long.chars().count(), 100);
        assert!(long.ends_with('…'));
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

    #[test]
    fn format_todo_choice_label_includes_id_and_sow_ref() {
        assert_eq!(
            format_todo_choice_label(12, "Write tests", &Some("M1D2".to_string())),
            "#12 Write tests [M1D2]"
        );
        assert_eq!(
            format_todo_choice_label(13, "Ship the release", &None),
            "#13 Ship the release"
        );
    }

    #[test]
    fn format_todo_choice_label_truncates_to_discords_100_char_choice_limit() {
        // The task input has no length cap - an oversized choice name would
        // otherwise make Discord reject the *entire* autocomplete response,
        // hiding every suggestion, not just this one.
        let long_task = "a".repeat(200);
        let label = format_todo_choice_label(12, &long_task, &Some("M1D2".to_string()));
        assert!(label.chars().count() <= 100, "label was {label:?}");
        assert!(label.starts_with("#12 "));
        assert!(label.ends_with("[M1D2]"));
    }

    #[test]
    fn format_todo_list_today_only() {
        let today = vec![
            (12, "Write tests".to_string(), Some("M1D2".to_string())),
            (13, "Ship the release".to_string(), None),
        ];
        assert_eq!(
            format_todo_list(&today, &[]),
            "**Today's todos:**\n`12` Write tests [M1D2]\n`13` Ship the release"
        );
    }

    #[test]
    fn format_todo_list_appends_carried_over_section() {
        let today = vec![(13, "Ship the release".to_string(), None)];
        let carried = vec![
            crate::entries::CarryoverTodo {
                id: 1,
                task: "Refactor auth".to_string(),
                sow_ref: Some("M2".to_string()),
                origin_date: "2026-08-27".to_string(),
            },
            crate::entries::CarryoverTodo {
                id: 2,
                task: "Write migration".to_string(),
                sow_ref: None,
                origin_date: "2026-08-25".to_string(),
            },
        ];
        assert_eq!(
            format_todo_list(&today, &carried),
            "**Today's todos:**\n`13` Ship the release\n\n\
             **Not carried over yet** (use `/todo add carryover:`):\n\
             Refactor auth [M2] — from Aug 27\n\
             Write migration — from Aug 25"
        );
    }

    #[test]
    fn format_todo_list_no_todos_today_but_carried() {
        let carried = vec![crate::entries::CarryoverTodo {
            id: 1,
            task: "Refactor auth".to_string(),
            sow_ref: None,
            origin_date: "2026-08-27".to_string(),
        }];
        assert_eq!(
            format_todo_list(&[], &carried),
            "You haven't submitted a todo today yet - use `/todo add`.\n\n\
             **Not carried over yet** (use `/todo add carryover:`):\n\
             Refactor auth — from Aug 27"
        );
    }

    #[test]
    fn format_todo_list_completely_empty() {
        assert_eq!(
            format_todo_list(&[], &[]),
            "You haven't submitted a todo today yet - use `/todo add`."
        );
    }

    #[test]
    fn a_huge_todo_list_chunks_under_discords_message_cap() {
        // 400 carried-over todos would blow past 2000 chars in one message;
        // `/todo list` chunks it the same way `/team report` does.
        let today = vec![(1, "today".to_string(), None)];
        let carried: Vec<_> = (0..400)
            .map(|i| crate::entries::CarryoverTodo {
                id: i,
                task: format!("Task number {i} with a reasonably wordy description"),
                sow_ref: None,
                origin_date: "2026-08-27".to_string(),
            })
            .collect();
        let full = format_todo_list(&today, &carried);
        assert!(
            full.chars().count() > 1900,
            "test setup should exceed the cap"
        );

        // `/todo list`'s reply is chunked the way `/team report` is; the
        // content-preservation guarantee is covered by split_into_messages'
        // own tests - here we just confirm a big list actually triggers the
        // split and every chunk fits Discord's message cap.
        let chunks = crate::status::split_into_messages(&full, 1900);
        assert!(chunks.len() > 1, "expected the list to be split");
        for chunk in &chunks {
            assert!(chunk.chars().count() <= 1900);
        }
    }

    // `subcommand()` isn't unit-tested here: `CommandDataOption` is
    // `#[non_exhaustive]` with no public constructor, so a real value
    // can't be built outside serenity itself - same "can't exercise real
    // Discord types without a live connection" limitation as the rest of
    // this codebase's Discord-facing code (see e.g. `ticker.rs`'s
    // `is_unknown_channel_error`).
}
