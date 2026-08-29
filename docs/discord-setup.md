# Discord setup

`dispatchd` needs a Discord bot application, invited to your server, plus
two IDs and a token wired into its config. This walks through all of it.

## 1. Create the application and bot user

1. Go to the [Discord Developer Portal](https://discord.com/developers/applications)
   and click **New Application**. Name it (e.g. "dispatchd").
2. In the sidebar, open **Bot**. Click **Reset Token** (or **Copy** if a
   token already shows) and save it somewhere safe — this is the value for
   `DISPATCHD_DISCORD_TOKEN` below. Treat it like a password: anyone with
   this token can control the bot.
3. Still on the **Bot** page, under **Privileged Gateway Intents**: you do
   **not** need to enable Message Content Intent — dispatchd only uses
   slash commands and modals, never raw message text. Leave it off.

## 2. Invite the bot to your server

1. In the sidebar, open **OAuth2 → URL Generator**.
2. Under **Scopes**, check `bot` and `applications.commands`.
3. Under **Bot Permissions**, check at minimum: `View Channel`,
   `Send Messages`, `Create Public Threads`, `Send Messages in Threads`.
4. Copy the generated URL at the bottom, open it in a browser, and add the
   bot to your server.

## 3. Get the guild (server) and channel IDs

1. In your Discord client: **User Settings → Advanced → Developer Mode**,
   turn it on.
2. Right-click your server's icon → **Copy Server ID**. This is
   `discord_guild_id`.
3. Right-click the channel you want the daily standup ritual to happen in
   → **Copy Channel ID**. This is `discord_standup_channel_id` (not yet
   used by anything — it's reserved for the daily-thread/reminder feature,
   still to be built).

## 4. Configure dispatchd

Run `dispatchd init` if you haven't already, to create
`~/.config/dispatchd/config.toml`. Uncomment and fill in:

```toml
discord_guild_id = 123456789012345678
discord_standup_channel_id = 123456789012345679
```

Then set the bot token as an environment variable (never put it in
`config.toml` — it's a secret, and that file may end up in backups or
version control):

```sh
export DISPATCHD_DISCORD_TOKEN="the token you copied in step 1"
```

For a systemd deployment, set it via the unit's `Environment=` directive or
an `EnvironmentFile=`, not by exporting it in a shell.

## 5. Run it

```sh
dispatchd
```

If everything's wired up correctly, you'll see:

```
dispatchd connected to Discord as <your bot's name>
```

and the `/ping` command will show up in your server within seconds
(guild-scoped commands take effect immediately, unlike global ones). Run
`/ping` in the server — dispatchd should reply "pong! dispatchd is alive."

If `discord_guild_id` or `DISPATCHD_DISCORD_TOKEN` aren't set, dispatchd
still runs its config/database checks and prints "Discord not configured"
instead of connecting — that's expected until you complete the steps above.
