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

For a systemd deployment (Linux only - this is the supported path; see
below for local/dev use), first install the unit:

```sh
sudo dispatchd service install
```

This requires systemd ≥ 250 (no plaintext fallback for older systemd) and
generates a unit that loads the token via `LoadCredentialEncrypted=`, so
it never sits on disk unencrypted. It won't start the service yet - there's
no token configured until the next step.

Then log the bot in. This is also where the token gets encrypted - unlike
a typical "paste your token here" prompt, `dispatchd` runs the encryption
itself:

```sh
sudo dispatchd discord login
```

It prompts for the token (input is hidden, like a password prompt),
confirms it's valid by asking Discord who it belongs to, then pipes it
straight into `systemd-creds encrypt --with-key=host` and writes the
result to `/etc/dispatchd/discord_token.cred` - the plaintext token is
never written to disk itself, only to that pipe. `--with-key=host` is
used because Raspberry Pi boards (Zero 2 W, 3B, etc.) have no TPM2 - this
protects an offline copy of the SD card (e.g. a stolen or improperly
wiped one), but not an attacker who already has root on the live running
Pi, since systemd itself can decrypt the credential there for legitimate
service starts. Needs root (`sudo`), since it writes under `/etc`.
Re-running it overwrites the existing credential - that's how you rotate
the token. `sudo dispatchd discord logout` removes it (idempotent - fine
to run even if nothing's stored); a running `dispatchd.service` keeps the
old token in memory until it's restarted or stopped.

Then start the bot:

```sh
sudo systemctl start dispatchd
```

`DISPATCHD_DISCORD_TOKEN` still works as a fallback (checked only if no
encrypted credential is found) for local/dev use, i.e. running `dispatchd`
directly instead of via `service install` - convenient when you don't
need or want the systemd-creds machinery.

## 5. Run it

Under systemd, `sudo systemctl start dispatchd` (from step 4) starts it.
Running `dispatchd` directly (local/dev, no systemd) does the same thing
in the foreground. Either way, if everything's wired up correctly you'll
see:

```
dispatchd connected to Discord as <your bot's name>
```

and `/ping`, `/todo`, `/update`, and `/team-status` will show up in your
server within seconds (guild-scoped commands take effect immediately,
unlike global ones). Run `/ping` in the server — dispatchd should reply
"pong! dispatchd is alive."

If `discord_guild_id` isn't set, or no token was saved via `discord login`
or `DISPATCHD_DISCORD_TOKEN`, dispatchd still runs its config/database
checks and prints "Discord not configured"
instead of connecting — that's expected until you complete the steps above.

To check on it afterwards without watching the logs, `dispatchd status`
reports the systemd side (systemd version, whether the unit is
installed/enabled/active, whether an encrypted token is on disk) and the
Discord side (resolves a token exactly like the bot itself would, then
pings Discord's API and reports the round-trip latency):

```
$ dispatchd status
systemd:
  version:              252 (>= 250, ok)
  dispatchd.service:    installed, enabled=enabled, active=active
  discord token:        encrypted credential present (/etc/dispatchd/discord_token.cred)
discord:
  ping:                 ok - logged in as dispatchd (123456789012345678), 84ms
```

## 6. `/team-status` permissions

`/team-status` is meant for the tech lead only. The bot always checks this
itself (looking up the caller in `members.toml`'s `is_lead` flag) no matter
what — that check can't be bypassed from Discord's side. On top of that,
the command is registered with Discord's `Manage Server` permission
requirement, so it's hidden from the command picker for anyone without
that permission by default.

If you want it restricted to a specific "Tech Lead" role instead of
everyone with `Manage Server`, that's a one-time manual step: **Server
Settings → Integrations → dispatchd → team-status**, then set it to only
the role(s) you want. This is optional — the bot-side check is the real
gate either way.
