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

use crate::entries::{self, UpdateForEdit, UpdateRow};

use super::{get_option_string, modal_value};

/// Custom_id prefix for the `/progress add` modal:
/// `progress_modal:<status_code>:<task_encoded>`.
pub const MODAL_PREFIX: &str = "progress_modal:";
/// Custom_id prefix for the `/progress edit` modal:
/// `progress_edit_modal:<status_code>:<update_id>`.
pub const EDIT_MODAL_PREFIX: &str = "progress_edit_modal:";
const PROGRESS_INPUT_ID: &str = "progress";
const BLOCKER_INPUT_ID: &str = "blocker";
const TASK_ENCODE_MAX_LEN: usize = 60;
/// How much of a progress/blocker writeup `/progress list` shows per line.
const LIST_SUMMARY_MAX_LEN: usize = 100;

const PROGRESS_HELP_TEXT: &str = "\
**/progress subcommands**
`/progress add task:<...> status:<...>` - report progress against a todo (or type a new task for unplanned work)
`/progress edit report:<...>` - revise one of today's progress reports; omit `status` to keep it (autocomplete over today's)
`/progress list` - list today's progress reports with their ids
`/progress help` - show this message";

fn status_option(required: bool) -> CreateCommandOption {
    CreateCommandOption::new(CommandOptionType::String, "status", "Status")
        .required(required)
        .add_string_choice("Done", "done")
        .add_string_choice("In Progress", "in_progress")
        .add_string_choice("Blocked", "blocked")
}

pub fn command() -> CreateCommand {
    CreateCommand::new("progress")
        .description("Report and revise progress against today's todos")
        .add_option(
            CreateCommandOption::new(
                CommandOptionType::SubCommand,
                "add",
                "Report progress against a todo",
            )
            .add_sub_option(
                CreateCommandOption::new(
                    CommandOptionType::String,
                    "task",
                    "Which todo? (or type a new one for unplanned work)",
                )
                .required(true)
                .max_length(100)
                .set_autocomplete(true),
            )
            .add_sub_option(status_option(true)),
        )
        .add_option(
            CreateCommandOption::new(
                CommandOptionType::SubCommand,
                "edit",
                "Revise one of today's progress reports",
            )
            .add_sub_option(
                CreateCommandOption::new(CommandOptionType::String, "report", "Which report?")
                    .required(true)
                    .set_autocomplete(true),
            )
            .add_sub_option(status_option(false)),
        )
        .add_option(CreateCommandOption::new(
            CommandOptionType::SubCommand,
            "list",
            "List today's progress reports with their ids",
        ))
        .add_option(CreateCommandOption::new(
            CommandOptionType::SubCommand,
            "help",
            "Show /progress's subcommands",
        ))
}

/// Discord nests a subcommand invocation's own options one level under a
/// single top-level entry named after the subcommand - this pulls out
/// `(subcommand_name, its_nested_options)`. Same shape as `todo::subcommand`.
pub fn subcommand(options: &[CommandDataOption]) -> Option<(&str, &[CommandDataOption])> {
    let top = options.first()?;
    match &top.value {
        CommandDataOptionValue::SubCommand(nested) => Some((top.name.as_str(), nested)),
        _ => None,
    }
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

/// Glyph for a status value in the `/progress list` index. `•` for
/// anything unrecognized (shouldn't happen - the command only writes the
/// three known values).
fn list_status_glyph(status: &str) -> &'static str {
    match status {
        "done" => "✅",
        "blocked" => "⛔",
        "in_progress" => "⏳",
        _ => "•",
    }
}

/// First line of `text`, trimmed, truncated to `max` chars with a `…` when
/// it's longer. Keeps `/progress list` lines to one scannable row each.
fn summarize(text: &str, max: usize) -> String {
    let first_line = text.lines().next().unwrap_or("").trim();
    if first_line.chars().count() <= max {
        first_line.to_string()
    } else {
        let mut out: String = first_line.chars().take(max).collect();
        out.push('…');
        out
    }
}

/// The ephemeral confirmation shown after `/progress add` or `/progress
/// edit`. `verb` is "saved" or "updated". `task` is the todo/task text on
/// `add`; `None` on `edit`, where the header is just "Progress updated"
/// (the edit path doesn't re-fetch the task). One field per line; the
/// `Blocker:` line is always present (`none` when empty) since this is a
/// read-back of exactly what was recorded.
fn format_confirmation(
    verb: &str,
    task: Option<&str>,
    status: &str,
    progress: &str,
    blocker: Option<&str>,
) -> String {
    let progress = progress.trim();
    let blocker = blocker.map(str::trim).filter(|s| !s.is_empty());
    let header = match task {
        Some(t) => format!("✅ Progress {verb}: {t}"),
        None => format!("✅ Progress {verb}"),
    };
    format!(
        "{header}\nStatus: {}\nProgress: {progress}\nBlocker: {}",
        status_label(status),
        blocker.unwrap_or("none"),
    )
}

/// The `/progress list` body. Caller handles the empty case.
fn format_progress_list(rows: &[UpdateRow]) -> String {
    let mut out = String::from("**Today's progress reports:**");
    for r in rows {
        out.push('\n');
        out.push_str(&format!(
            "`{}` {} {} — {}",
            r.id,
            list_status_glyph(&r.status),
            r.task,
            summarize(&r.progress, LIST_SUMMARY_MAX_LEN),
        ));
        if let Some(blocker) = &r.blocker {
            out.push_str(&format!(
                " (blocker: {})",
                summarize(blocker, LIST_SUMMARY_MAX_LEN)
            ));
        }
    }
    out
}

/// Encodes a resolved `task` option value for embedding in the `add`
/// modal's custom_id. An `"id:<n>"` reference (already short) passes
/// through unchanged; free text longer than `TASK_ENCODE_MAX_LEN` is
/// truncated. Returns `(encoded, was_truncated)`.
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

/// Parses `"progress_modal:<status_code>:<task_encoded>"` into
/// `(status_code, task_encoded)`. `None` if malformed.
fn parse_custom_id(custom_id: &str) -> Option<(&str, &str)> {
    let rest = custom_id.strip_prefix(MODAL_PREFIX)?;
    rest.split_once(':')
}

/// Parses `"progress_edit_modal:<status_code>:<update_id>"` into
/// `(status_code, update_id)`. `None` if malformed or the id isn't an
/// integer.
fn parse_edit_custom_id(custom_id: &str) -> Option<(&str, i64)> {
    let rest = custom_id.strip_prefix(EDIT_MODAL_PREFIX)?;
    let (code, id) = rest.split_once(':')?;
    Some((code, id.parse().ok()?))
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

pub async fn handle_autocomplete(
    ctx: &SerenityContext,
    autocomplete: &AutocompleteInteraction,
    db: &Arc<Mutex<Connection>>,
    timezone: &Tz,
) {
    let Some((sub, options)) = subcommand(&autocomplete.data.options) else {
        return;
    };
    let discord_user_id = autocomplete.user.id.to_string();
    let date = entries::today_in(timezone);

    let response = match sub {
        "add" => {
            let partial = get_option_string(options, "task").unwrap_or_default();
            let choices = {
                let conn = db.lock().expect("db mutex poisoned");
                entries::list_open_todos(&conn, &discord_user_id, &date, &partial)
            };
            match choices {
                Ok(rows) => CreateAutocompleteResponse::new().set_choices(
                    rows.into_iter()
                        .map(|(id, task)| AutocompleteChoice::new(task, format!("id:{id}")))
                        .collect(),
                ),
                Err(e) => {
                    eprintln!("failed to list open todos for autocomplete: {e}");
                    CreateAutocompleteResponse::new()
                }
            }
        }
        "edit" => {
            let partial = get_option_string(options, "report").unwrap_or_default();
            let choices = {
                let conn = db.lock().expect("db mutex poisoned");
                entries::list_updates(&conn, &discord_user_id, &date, &partial)
            };
            match choices {
                Ok(rows) => CreateAutocompleteResponse::new().set_choices(
                    rows.into_iter()
                        .map(|r| {
                            AutocompleteChoice::new(
                                format!("{} — {}", r.task, status_label(&r.status)),
                                r.id.to_string(),
                            )
                        })
                        .collect(),
                ),
                Err(e) => {
                    eprintln!("failed to list updates for autocomplete: {e}");
                    CreateAutocompleteResponse::new()
                }
            }
        }
        _ => CreateAutocompleteResponse::new(),
    };

    if let Err(e) = autocomplete
        .create_response(&ctx.http, CreateInteractionResponse::Autocomplete(response))
        .await
    {
        eprintln!("failed to respond to /progress autocomplete: {e}");
    }
}

pub async fn handle_add(
    ctx: &SerenityContext,
    command: &CommandInteraction,
    options: &[CommandDataOption],
) {
    let task_value = get_option_string(options, "task").unwrap_or_default();
    let status_value = get_option_string(options, "status").unwrap_or_default();

    let (task_encoded, _) = encode_task_for_modal(&task_value);
    let custom_id = format!(
        "{MODAL_PREFIX}{}:{task_encoded}",
        status_code(&status_value)
    );

    let modal = CreateModal::new(custom_id, "Submit progress").components(vec![
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
        eprintln!("failed to open /progress add modal: {e}");
    }
}

pub async fn handle_edit(
    ctx: &SerenityContext,
    command: &CommandInteraction,
    options: &[CommandDataOption],
    db: &Arc<Mutex<Connection>>,
    timezone: &Tz,
) {
    let Some(update_id) = get_option_string(options, "report").and_then(|s| s.parse::<i64>().ok())
    else {
        reply_ephemeral(
            ctx,
            command,
            "⚠️ Please pick a report from the suggestions.",
            "/progress edit",
        )
        .await;
        return;
    };
    let status_override = get_option_string(options, "status");
    let discord_user_id = command.user.id.to_string();
    let date = entries::today_in(timezone);

    let current = {
        let conn = db.lock().expect("db mutex poisoned");
        entries::update_for_edit(&conn, update_id, &discord_user_id, &date)
    };

    let UpdateForEdit {
        status: current_status,
        progress,
        blocker,
    } = match current {
        Ok(Some(u)) => u,
        Ok(None) => {
            reply_ephemeral(
                ctx,
                command,
                "⚠️ Couldn't find that report (not from today, or not yours?).",
                "/progress edit",
            )
            .await;
            return;
        }
        Err(e) => {
            eprintln!("failed to look up progress report for edit: {e}");
            reply_ephemeral(
                ctx,
                command,
                "⚠️ Something went wrong - please try again.",
                "/progress edit",
            )
            .await;
            return;
        }
    };

    // `status` option omitted means "keep the current status".
    let status = status_override.as_deref().unwrap_or(&current_status);
    let custom_id = format!("{EDIT_MODAL_PREFIX}{}:{update_id}", status_code(status));

    let modal = CreateModal::new(custom_id, "Edit progress").components(vec![
        CreateActionRow::InputText(
            CreateInputText::new(InputTextStyle::Paragraph, "Progress", PROGRESS_INPUT_ID)
                .required(true)
                .value(progress),
        ),
        CreateActionRow::InputText(
            CreateInputText::new(
                InputTextStyle::Short,
                "Blocker (if blocked)",
                BLOCKER_INPUT_ID,
            )
            .required(false)
            .value(blocker.unwrap_or_default()),
        ),
    ]);

    if let Err(e) = command
        .create_response(&ctx.http, CreateInteractionResponse::Modal(modal))
        .await
    {
        eprintln!("failed to open /progress edit modal: {e}");
    }
}

pub async fn handle_list(
    ctx: &SerenityContext,
    command: &CommandInteraction,
    db: &Arc<Mutex<Connection>>,
    timezone: &Tz,
) {
    let discord_user_id = command.user.id.to_string();
    let date = entries::today_in(timezone);

    let rows = {
        let conn = db.lock().expect("db mutex poisoned");
        entries::list_updates(&conn, &discord_user_id, &date, "")
    };

    let reply_text = match rows {
        Ok(rows) if rows.is_empty() => {
            "You haven't reported any progress today yet - use `/progress add`.".to_string()
        }
        Ok(rows) => format_progress_list(&rows),
        Err(e) => {
            eprintln!("failed to list progress reports: {e}");
            "⚠️ Something went wrong - please try again.".to_string()
        }
    };
    reply_ephemeral(ctx, command, reply_text, "/progress list").await;
}

pub async fn handle_help(ctx: &SerenityContext, command: &CommandInteraction) {
    reply_ephemeral(ctx, command, PROGRESS_HELP_TEXT, "/progress help").await;
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
            "malformed /progress modal custom_id: {}",
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
        Ok(task) => {
            format_confirmation("saved", Some(&task), status, &progress, blocker.as_deref())
        }
        Err(e) => {
            eprintln!("failed to insert update: {e}");
            "⚠️ Something went wrong saving your progress - please try again.".to_string()
        }
    };

    let reply = CreateInteractionResponseMessage::new()
        .content(reply_text)
        .ephemeral(true);
    if let Err(e) = modal
        .create_response(&ctx.http, CreateInteractionResponse::Message(reply))
        .await
    {
        eprintln!("failed to respond to /progress modal submission: {e}");
    }
}

pub async fn handle_edit_modal_submission(
    ctx: &SerenityContext,
    modal: &ModalInteraction,
    db: &Arc<Mutex<Connection>>,
    timezone: &Tz,
) {
    let discord_user_id = modal.user.id.to_string();

    let Some((status_code_str, update_id)) = parse_edit_custom_id(&modal.data.custom_id) else {
        eprintln!(
            "malformed /progress edit modal custom_id: {}",
            modal.data.custom_id
        );
        return;
    };
    let status = decode_status_code(status_code_str).unwrap_or("in_progress");

    let progress = modal_value(modal, PROGRESS_INPUT_ID).unwrap_or_default();
    let blocker = modal_value(modal, BLOCKER_INPUT_ID).filter(|s| !s.trim().is_empty());
    let date = entries::today_in(timezone);

    let updated = {
        let conn = db.lock().expect("db mutex poisoned");
        entries::update_update(
            &conn,
            update_id,
            &discord_user_id,
            &date,
            status,
            &progress,
            blocker.as_deref(),
        )
    };

    let reply_text = match updated {
        Ok(true) => format_confirmation("updated", None, status, &progress, blocker.as_deref()),
        Ok(false) => {
            "⚠️ Couldn't find that report anymore (not from today, or not yours?).".to_string()
        }
        Err(e) => {
            eprintln!("failed to update progress report: {e}");
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
        eprintln!("failed to respond to /progress edit modal submission: {e}");
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
            parse_custom_id("progress_modal:D:id:42"),
            Some(("D", "id:42"))
        );
    }

    #[test]
    fn parse_custom_id_rejects_malformed_input() {
        assert_eq!(parse_custom_id("something_else:D:id:42"), None);
        assert_eq!(parse_custom_id("progress_modal:D"), None);
    }

    #[test]
    fn parse_edit_custom_id_splits_status_and_update_id() {
        assert_eq!(
            parse_edit_custom_id("progress_edit_modal:D:42"),
            Some(("D", 42))
        );
        assert_eq!(
            parse_edit_custom_id("progress_edit_modal:B:7"),
            Some(("B", 7))
        );
    }

    #[test]
    fn parse_edit_custom_id_rejects_malformed_input() {
        assert_eq!(parse_edit_custom_id("progress_modal:D:42"), None);
        assert_eq!(parse_edit_custom_id("progress_edit_modal:D"), None);
        assert_eq!(
            parse_edit_custom_id("progress_edit_modal:D:notanumber"),
            None
        );
    }

    #[test]
    fn summarize_takes_first_line_and_trims() {
        assert_eq!(
            summarize("  hello world  \nsecond line", 100),
            "hello world"
        );
    }

    #[test]
    fn summarize_truncates_long_text_with_ellipsis() {
        let long = "x".repeat(120);
        let out = summarize(&long, 20);
        assert_eq!(out.chars().count(), 21); // 20 chars + ellipsis
        assert!(out.ends_with('…'));
    }

    #[test]
    fn summarize_leaves_short_text_untouched() {
        assert_eq!(summarize("all tests green", 100), "all tests green");
    }

    #[test]
    fn list_status_glyph_covers_the_three_statuses() {
        assert_eq!(list_status_glyph("done"), "✅");
        assert_eq!(list_status_glyph("in_progress"), "⏳");
        assert_eq!(list_status_glyph("blocked"), "⛔");
        assert_eq!(list_status_glyph("weird"), "•");
    }

    #[test]
    fn format_progress_list_renders_one_line_per_report_with_blocker_suffix() {
        let rows = vec![
            crate::entries::UpdateRow {
                id: 41,
                task: "Write tests".to_string(),
                status: "done".to_string(),
                progress: "all green".to_string(),
                blocker: None,
            },
            crate::entries::UpdateRow {
                id: 42,
                task: "Fix prod outage".to_string(),
                status: "blocked".to_string(),
                progress: "rolled back the deploy".to_string(),
                blocker: Some("waiting on ops".to_string()),
            },
        ];
        assert_eq!(
            format_progress_list(&rows),
            "**Today's progress reports:**\n\
             `41` ✅ Write tests — all green\n\
             `42` ⛔ Fix prod outage — rolled back the deploy (blocker: waiting on ops)"
        );
    }

    #[test]
    fn progress_help_text_mentions_every_subcommand() {
        for needle in [
            "/progress add",
            "/progress edit",
            "/progress list",
            "/progress help",
        ] {
            assert!(PROGRESS_HELP_TEXT.contains(needle), "missing {needle}");
        }
    }

    #[test]
    fn format_confirmation_without_blocker_reads_back_none() {
        assert_eq!(
            format_confirmation(
                "saved",
                Some("Refactor auth"),
                "done",
                "all tests green",
                None
            ),
            "✅ Progress saved: Refactor auth\nStatus: Done\nProgress: all tests green\nBlocker: none"
        );
    }

    #[test]
    fn format_confirmation_with_blocker_shows_it() {
        assert_eq!(
            format_confirmation(
                "saved",
                Some("Write migration"),
                "blocked",
                "schema drafted",
                Some("DBA review")
            ),
            "✅ Progress saved: Write migration\nStatus: Blocked\nProgress: schema drafted\nBlocker: DBA review"
        );
    }

    #[test]
    fn format_confirmation_trims_and_treats_whitespace_blocker_as_none() {
        assert_eq!(
            format_confirmation(
                "saved",
                Some("Task"),
                "in_progress",
                "  did a thing  ",
                Some("   ")
            ),
            "✅ Progress saved: Task\nStatus: In Progress\nProgress: did a thing\nBlocker: none"
        );
    }

    #[test]
    fn format_confirmation_omits_the_header_task_when_none() {
        assert_eq!(
            format_confirmation("updated", None, "done", "x", None),
            "✅ Progress updated\nStatus: Done\nProgress: x\nBlocker: none"
        );
    }
}
