use serenity::all::{
    CommandInteraction, Context as SerenityContext, CreateCommand, CreateInteractionResponse,
    CreateInteractionResponseMessage,
};

const HELP_TEXT: &str = "\
**dispatchd commands**
`/todo create` - submit a new todo for today
`/todo edit` - edit one of today's todos
`/todo delete` - delete one of today's todos
`/todo list` - list today's todos with their ids
`/todo help` - todo-specific help
`/update` - submit an update against one of today's todos (or free-typed unplanned work)
`/team-status` - (tech lead only) see who's updated today
`/ping` - check that dispatchd is alive";

pub fn command() -> CreateCommand {
    CreateCommand::new("help").description("Show dispatchd's commands")
}

pub async fn handle_command(ctx: &SerenityContext, command: &CommandInteraction) {
    let reply = CreateInteractionResponseMessage::new()
        .content(HELP_TEXT)
        .ephemeral(true);
    if let Err(e) = command
        .create_response(&ctx.http, CreateInteractionResponse::Message(reply))
        .await
    {
        eprintln!("failed to respond to /help: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn help_text_mentions_every_command() {
        for needle in [
            "/todo create",
            "/todo edit",
            "/todo delete",
            "/todo list",
            "/todo help",
            "/update",
            "/team-status",
            "/ping",
        ] {
            assert!(HELP_TEXT.contains(needle), "missing {needle}");
        }
    }
}
