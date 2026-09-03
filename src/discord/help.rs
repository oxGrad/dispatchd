use serenity::all::{
    CommandInteraction, Context as SerenityContext, CreateCommand, CreateInteractionResponse,
    CreateInteractionResponseMessage,
};

const HELP_TEXT: &str = "\
**dispatchd commands**
`/todo add` - submit a new todo for today
`/todo edit` - edit one of today's todos
`/todo delete` - delete one of today's todos
`/todo list` - list today's todos with their ids
`/todo help` - todo-specific help
`/progress add` - report progress against one of today's todos (or free-typed unplanned work)
`/progress edit` - revise one of today's progress reports
`/progress list` - list today's progress reports with their ids
`/progress help` - progress-specific help
`/team status` - (tech lead only) one line per member: who's updated today
`/team report` - (tech lead only) full detail of everyone's todos + progress today
`/team remind member:<name> kind:<todo|progress>` - (tech lead only) nudge a member in today's thread
`/team skip-meeting` - (tech lead only) cancel today's meeting and tell the team
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
            "/todo add",
            "/todo edit",
            "/todo delete",
            "/todo list",
            "/todo help",
            "/progress add",
            "/progress edit",
            "/progress list",
            "/progress help",
            "/team status",
            "/team report",
            "/team remind",
            "/team skip-meeting",
            "/ping",
        ] {
            assert!(HELP_TEXT.contains(needle), "missing {needle}");
        }
    }
}
