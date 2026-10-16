# GitHub setup

The GitHub integration is **optional**. With it, dispatchd reads your
organization's issues, pull requests and reviews and folds them into the
standup reports, next to what people submit with `/todo` and `/progress`
(the full design is in
[`docs/superpowers/specs/2026-10-15-github-activity-design.md`](superpowers/specs/2026-10-15-github-activity-design.md)).
Without it, everything works exactly as before.

> **Status:** dispatchd connects (`dispatchd github login`) and, while
> the bot runs, syncs the org's PRs, issues and reviews into its database
> every `sync_interval_minutes` (`/admin status` shows the last sync).
> Showing that activity in the daily summary and `/recap` comes in the
> next release.

dispatchd authenticates as a **GitHub App** that you create in your
organization. An App isn't tied to any one person's account, can be
limited to read-only access, and its access is chosen per repository by
the org.

## 1. Create the GitHub App

You need to be an **owner** of the organization.

1. On GitHub, go to your organization's **Settings → Developer settings →
   GitHub Apps** and click **New GitHub App**.
2. Fill in:
   - **GitHub App name**: anything unique on GitHub, e.g.
     `dispatchd-<your-org>`.
   - **Homepage URL**: required by the form; any URL works (e.g. your org's
     page or `https://github.com/oxgrad/dispatchd`).
   - **Webhook**: **uncheck Active**. dispatchd polls GitHub; it doesn't
     receive webhooks (it usually runs on a box with no public address).
3. Under **Permissions → Repository permissions**, set exactly these and
   leave everything else at **No access**:
   - **Issues**: Read-only
   - **Pull requests**: Read-only
   - **Metadata**: Read-only (GitHub selects this one automatically)

   No organization or account permissions are needed. dispatchd never
   writes anything to GitHub.
4. **Where can this GitHub App be installed?**: **Only on this account**.
5. Click **Create GitHub App**.
6. On the App's settings page that opens, note the **App ID** (a number
   near the top). That's `app_id` below.
7. Scroll down to **Private keys** and click **Generate a private key**.
   Your browser downloads a `.pem` file. **This is the secret**: anyone
   with it can read whatever the App can. Don't commit it, paste it into
   chat, or put it in `config.toml`.

## 2. Install the App on your organization

1. On the App's settings page, open **Install App** in the sidebar and
   click **Install** next to your organization.
2. Choose which repositories the App can read:
   - **All repositories**, or
   - **Only select repositories**, then pick them.

   This choice *is* the bot's repo scope: there's no repo list in
   dispatchd's config. To leave a repo (infra, docs, ...) out of the
   reports, don't select it here. You can change it any time from the
   org's **Settings → GitHub Apps → Configure**.

## 3. Configure dispatchd

In `config.toml` (see `config.example.toml` for the commented template),
uncomment the `[github]` section and set both values:

```toml
[github]
app_id = 123456
org = "your-org"
```

In `members.toml`, add each member's GitHub username:

```toml
[[members]]
discord_user_id = "000000000000000003"
name = "Budi"
role = "senior"
github_user = "budi-dev"
```

Leave `github_user` out for anyone who doesn't work on GitHub (a designer,
a `viewer`); they're simply not shown in GitHub sections. Each GitHub
account can belong to only one member.

Check both files:

```sh
dispatchd validate
```

It reports whether GitHub is configured, how many members have a
`github_user`, and warns if one side is set up without the other.

## 4. Store the private key

Copy the `.pem` to the machine running dispatchd (e.g. with `scp`), then:

```sh
sudo dispatchd github login --key ./dispatchd-your-org.2026-10-15.private-key.pem
```

Run it from the account that owns dispatchd's `config.toml` (the one the
service runs as). Under `sudo` it reads that account's
`~/.config/dispatchd/config.toml`; for a config somewhere else, use
`sudo DISPATCHD_CONFIG_PATH=/path/to/config.toml dispatchd github login ...`.

This:

1. checks the key end to end: it signs in as the App, finds its
   installation on `org`, gets an access token and counts the repositories
   it can see, and prints a summary such as
   `GitHub App "dispatchd-your-org" (123456) is installed on your-org (all repositories), 14 repos visible.`
2. encrypts the key with `systemd-creds` (the same way `discord login`
   stores the bot token) to `/etc/dispatchd/github_app_key.cred`;
3. adds the key to `dispatchd.service` if the service is already
   installed (otherwise `sudo dispatchd service install` picks it up
   later).

Then restart the bot and delete the plaintext `.pem` from the machine;
dispatchd only uses the encrypted copy:

```sh
sudo systemctl restart dispatchd
rm ./dispatchd-your-org.2026-10-15.private-key.pem
```

To rotate the key, generate a new one on the App's settings page, run
`github login` again with it, then delete the old key on GitHub. To turn
the integration off, run `sudo dispatchd github logout` and remove the
`[github]` section.

For local runs outside systemd, point `DISPATCHD_GITHUB_APP_KEY_PATH` at
the `.pem` instead.

## 5. Check it

```sh
sudo dispatchd status
```

ends with a GitHub section:

```
github:
  app:                  123456 on your-org
  api:                  ok - "dispatchd-your-org" installed (all repositories), 14 repos visible, 182ms
```

`/admin status` in Discord shows the same section, plus the last sync:

```
  sync:                 last ok 2026-10-16T02:00:00Z - 42 PRs, 30 issues, 77 reviews stored
```

To see what the sync would pick up without waiting for it (or without
the bot running at all):

```sh
sudo dispatchd github preview --days 3
```

It runs the sync's own queries against your org and prints totals plus a
per-GitHub-user breakdown, without writing anything.

## Troubleshooting

| Message | Fix |
|---|---|
| `GitHub rejected the App JWT (401)` | `app_id` doesn't match the App the key belongs to, or this machine's clock is off. Check `timedatectl`. |
| `the App is not installed on the "x" org (404)` | Install it (step 2), or fix `org`. |
| `GitHub refused to issue an installation token` | The installation is suspended. Unsuspend it in the org's **Settings → GitHub Apps**. |
| `the installation token can't list repositories (403)` | The App is missing **Metadata: Read-only**. Fix the permissions, then accept the updated permissions in the org's installation settings. |
| `key: not found` | Run `sudo dispatchd github login --key <file.pem>` (needs root, because the credential is root-only). |
| `0 repos visible` | The installation has no repositories selected (step 2). |
| `GitHub sync disabled - ... no App key was found` (bot log) | The bot started without the key: run `github login`, then restart dispatchd. |
| `github_user "x" ... doesn't exist on GitHub` (bot log) | That member renamed their GitHub account; update `github_user` in `members.toml`. |
