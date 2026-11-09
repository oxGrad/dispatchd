mod admin;
mod help;
mod missed;
mod progress;
mod recap;
mod team;
mod ticker;
mod todo;

use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use chrono::NaiveTime;
use chrono_tz::Tz;
use rusqlite::Connection;
use serenity::all::{
    ActionRowComponent, ChannelId, Client, CommandDataOption, CommandDataOptionValue,
    Context as SerenityContext, CreateCommand, CreateInteractionResponse,
    CreateInteractionResponseMessage, Error as SerenityError, EventHandler, GatewayIntents,
    GuildId, HttpError, Interaction, ModalInteraction, Ready,
};
use serenity::async_trait;

use crate::config::{Config, GithubConfig};
use crate::members;

pub struct Handler {
    guild_id: GuildId,
    db: Arc<Mutex<Connection>>,
    timezone: Tz,
    carryover_lookback_days: u32,
    day_summary_time: NaiveTime,
    run_on_weekends: bool,
    github: Option<GithubConfig>,
}

#[async_trait]
impl EventHandler for Handler {
    async fn ready(&self, ctx: SerenityContext, ready: Ready) {
        println!("dispatchd connected to Discord as {}", ready.user.name);
        let commands = vec![
            CreateCommand::new("ping").description("Check that dispatchd is alive"),
            help::command(),
            todo::command(),
            progress::command(),
            team::command(),
            admin::command(),
            recap::command(),
            recap::biweekly_command(),
            missed::command(),
        ];
        if let Err(e) = self.guild_id.set_commands(&ctx.http, commands).await {
            eprintln!("failed to register guild commands: {e}");
        }

        admin::post_upgrade_confirmation(&ctx).await;
    }

    async fn interaction_create(&self, ctx: SerenityContext, interaction: Interaction) {
        match interaction {
            Interaction::Command(command) => match command.data.name.as_str() {
                "ping" => {
                    let reply = CreateInteractionResponseMessage::new()
                        .content("pong! dispatchd is alive.")
                        .ephemeral(true);
                    if let Err(e) = command
                        .create_response(&ctx.http, CreateInteractionResponse::Message(reply))
                        .await
                    {
                        eprintln!("failed to respond to /ping: {e}");
                    }
                }
                "help" => help::handle_command(&ctx, &command).await,
                // Both write entries (synced into the shared thread), so
                // they're roster-only. Gating the command is enough - their
                // modals can only be opened from here.
                "todo" | "progress" if !self.is_member(&command.user.id.to_string()) => {
                    let reply = CreateInteractionResponseMessage::new()
                        .content("⛔ This command is for team members listed in members.toml.")
                        .ephemeral(true);
                    if let Err(e) = command
                        .create_response(&ctx.http, CreateInteractionResponse::Message(reply))
                        .await
                    {
                        eprintln!(
                            "failed to respond to non-member /{}: {e}",
                            command.data.name
                        );
                    }
                }
                "todo" => match todo::subcommand(&command.data.options) {
                    Some(("add", _)) => todo::handle_add(&ctx, &command).await,
                    Some(("edit", opts)) => {
                        todo::handle_edit(&ctx, &command, opts, &self.db, &self.timezone).await
                    }
                    Some(("delete", opts)) => {
                        todo::handle_delete(&ctx, &command, opts, &self.db, &self.timezone).await
                    }
                    Some(("list", _)) => {
                        todo::handle_list(
                            &ctx,
                            &command,
                            &self.db,
                            &self.timezone,
                            self.carryover_lookback_days,
                        )
                        .await
                    }
                    Some(("help", _)) => todo::handle_help(&ctx, &command).await,
                    _ => {}
                },
                "progress" => match progress::subcommand(&command.data.options) {
                    Some(("add", opts)) => progress::handle_add(&ctx, &command, opts).await,
                    Some(("edit", opts)) => {
                        progress::handle_edit(&ctx, &command, opts, &self.db, &self.timezone).await
                    }
                    Some(("list", _)) => {
                        progress::handle_list(&ctx, &command, &self.db, &self.timezone).await
                    }
                    Some(("help", _)) => progress::handle_help(&ctx, &command).await,
                    _ => {}
                },
                // Three subcommands, so `match` rather than `if let`.
                "team" => match team::subcommand(&command.data.options) {
                    Some(("status", _)) => {
                        team::handle_status(
                            &ctx,
                            &command,
                            &self.db,
                            &self.timezone,
                            self.carryover_lookback_days,
                        )
                        .await
                    }
                    Some(("report", _)) => {
                        team::handle_report(
                            &ctx,
                            &command,
                            &self.db,
                            &self.timezone,
                            self.carryover_lookback_days,
                        )
                        .await
                    }
                    Some(("remind", opts)) => {
                        team::handle_remind(&ctx, &command, opts, &self.db, &self.timezone).await
                    }
                    Some(("skip-meeting", _)) => {
                        team::handle_skip_meeting(&ctx, &command, &self.db, &self.timezone).await
                    }
                    _ => {}
                },
                "admin" => match admin::subcommand(&command.data.options) {
                    Some(("status", _)) => {
                        admin::handle_status(&ctx, &command, &self.db, self.github.as_ref()).await
                    }
                    Some(("upgrade", opts)) => {
                        admin::handle_upgrade(&ctx, &command, opts, &self.db).await
                    }
                    Some(("help", _)) => admin::handle_help(&ctx, &command, &self.db).await,
                    _ => {}
                },
                "recap" => recap::handle(&ctx, &command, &self.db, &self.timezone).await,
                "recap-biweekly" => {
                    recap::handle_biweekly(&ctx, &command, &self.db, &self.timezone).await
                }
                "missed" => {
                    missed::handle(
                        &ctx,
                        &command,
                        &self.db,
                        &self.timezone,
                        self.day_summary_time,
                        self.run_on_weekends,
                    )
                    .await
                }
                _ => {}
            },
            Interaction::Autocomplete(autocomplete) => match autocomplete.data.name.as_str() {
                "todo" => {
                    todo::handle_autocomplete(&ctx, &autocomplete, &self.db, &self.timezone).await
                }
                "progress" => {
                    progress::handle_autocomplete(
                        &ctx,
                        &autocomplete,
                        &self.db,
                        &self.timezone,
                        self.carryover_lookback_days,
                    )
                    .await
                }
                "team" => team::handle_autocomplete(&ctx, &autocomplete, &self.db).await,
                _ => {}
            },
            Interaction::Modal(modal) if modal.data.custom_id == todo::ADD_MODAL_ID => {
                todo::handle_modal_submission(&ctx, &modal, &self.db, &self.timezone).await;
            }
            Interaction::Modal(modal)
                if modal.data.custom_id.starts_with(todo::EDIT_MODAL_PREFIX) =>
            {
                todo::handle_edit_modal_submission(&ctx, &modal, &self.db, &self.timezone).await;
            }
            Interaction::Modal(modal)
                if modal
                    .data
                    .custom_id
                    .starts_with(progress::EDIT_MODAL_PREFIX) =>
            {
                progress::handle_edit_modal_submission(&ctx, &modal, &self.db, &self.timezone)
                    .await;
            }
            Interaction::Modal(modal)
                if modal.data.custom_id.starts_with(progress::MODAL_PREFIX) =>
            {
                progress::handle_modal_submission(&ctx, &modal, &self.db, &self.timezone).await;
            }
            _ => {}
        }
    }
}

impl Handler {
    /// Fails closed: a DB error denies rather than lets a non-member in.
    fn is_member(&self, discord_user_id: &str) -> bool {
        let conn = self.db.lock().expect("db mutex poisoned");
        members::is_member(&conn, discord_user_id).unwrap_or_else(|e| {
            eprintln!("failed to check is_member: {e}");
            false
        })
    }
}

/// Discord's JSON error code for "Unknown Channel" - returned when a
/// request targets a channel or thread that no longer exists (e.g. a
/// deleted standup thread).
const UNKNOWN_CHANNEL_ERROR_CODE: isize = 10003;

fn is_unknown_channel_code(code: isize) -> bool {
    code == UNKNOWN_CHANNEL_ERROR_CODE
}

/// True when `err` is Discord reporting that the channel/thread a request
/// targeted no longer exists, as opposed to a transient failure. Not
/// unit-tested directly - building a real `serenity::Error` needs a
/// `reqwest::Method`, which isn't a direct dependency of this crate.
pub(crate) fn is_unknown_channel_error(err: &SerenityError) -> bool {
    matches!(
        err,
        SerenityError::Http(HttpError::UnsuccessfulRequest(response))
            if is_unknown_channel_code(response.error.code)
    )
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
/// in-progress autocomplete value too. Shared by `progress.rs` (the `task`
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
            carryover_lookback_days: config.carryover_lookback_days,
            day_summary_time: config.day_summary_time,
            run_on_weekends: config.run_on_weekends,
            github: config.github.clone(),
        })
        .await
        .context("failed to build Discord client")?;

    spawn_github_sync(&config, &db);

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

/// Starts the GitHub activity sync (`github::sync::run_loop`) when `[github]`
/// is configured and the App key resolves. Independent of the standup
/// ticker: it runs even without a standup channel, and a missing or broken
/// key only disables the sync, never the bot.
fn spawn_github_sync(config: &Config, db: &Arc<Mutex<Connection>>) {
    let Some(github) = config.github.clone() else {
        return;
    };
    let key = crate::github::app_key_pem()
        .ok_or_else(|| {
            "no App key was found - run `sudo dispatchd github login --key <file.pem>` \
             (see docs/github-setup.md), then restart dispatchd"
                .to_string()
        })
        .and_then(|pem| {
            crate::github::auth::AppKey::from_pem(&pem)
                .map_err(|e| format!("the App key is unusable: {e:#}"))
        });
    match key {
        Ok(key) => {
            println!(
                "GitHub sync enabled for {} (every {} min)",
                github.org, github.sync_interval_minutes
            );
            tokio::spawn(crate::github::sync::run_loop(db.clone(), github, key));
        }
        Err(reason) => {
            eprintln!("GitHub sync disabled - {reason}");
            // Recorded so /admin status says the sync isn't running, rather
            // than showing a stale "last ok" from an earlier process.
            let conn = db.lock().expect("db mutex poisoned");
            if let Err(e) = crate::github::store::record_sync_error(
                &conn,
                &format!("sync not running - {reason}"),
            ) {
                eprintln!("failed to record the github sync status: {e}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_channel_code_matches_discords_10003() {
        assert!(is_unknown_channel_code(UNKNOWN_CHANNEL_ERROR_CODE));
        assert!(is_unknown_channel_code(10003));
        assert!(!is_unknown_channel_code(10004));
    }
}
