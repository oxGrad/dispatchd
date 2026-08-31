# Installing dispatchd

```sh
curl -fsSL https://dispatchd.graditya.com | sh
```

This downloads the right prebuilt binary for your machine from the
[latest GitHub Release](https://github.com/oxGrad/dispatchd/releases),
verifies its SHA-256 checksum, and installs it to `$HOME/.local/bin` (no
`sudo` needed). It never compiles anything - no Rust toolchain required,
including on a Raspberry Pi, where compiling this project's dependency
tree isn't practical. It does **not** run `dispatchd init` or touch any
config for you - see "Next steps" below.

## Supported platforms

- Linux x86_64, aarch64, armv7 (`musl` builds - fully static, no glibc
  version to worry about)
- macOS, Apple Silicon only (Intel Mac isn't currently built)

Raspberry Pi OS is aarch64 on a 64-bit install, armv7 on a 32-bit one -
the script detects this automatically via `uname`.

## Options

- **`DISPATCHD_VERSION`** - pin a specific release instead of installing
  latest, e.g.:
  ```sh
  curl -fsSL https://dispatchd.graditya.com | DISPATCHD_VERSION=v0.2.0 sh
  ```
- **`INSTALL_DIR`** - install somewhere other than `$HOME/.local/bin`,
  e.g. system-wide:
  ```sh
  curl -fsSL https://dispatchd.graditya.com | sudo INSTALL_DIR=/usr/local/bin sh
  ```
  The script never escalates privileges on its own - if `INSTALL_DIR`
  isn't writable, it fails with a clear message rather than silently
  invoking `sudo`.

## Next steps

Once installed:

```sh
dispatchd init
```

writes `config.toml`/`members.toml` templates to their resolved XDG
locations. From there, follow `docs/discord-setup.md` to create the
Discord application, get a bot token, and wire up `discord_guild_id`/
`discord_standup_channel_id`. `docs/user-guide.md` covers day-to-day
usage once it's running.

## Hosting the installer at `dispatchd.graditya.com` (Cloudflare)

`install.sh` is committed at the repo root, so
`curl -fsSL https://raw.githubusercontent.com/oxGrad/dispatchd/main/install.sh | sh`
already works with no extra setup. Serving it at the bare custom domain
(no path) needs a small proxy in front, since GitHub's raw content isn't
served from that domain - `cloudflare/worker.js` in this repo is exactly
that proxy. To wire it up (needs your own Cloudflare account - this is a
one-time setup step, not something dispatchd's CI does for you):

1. **Create the Worker.** Cloudflare dashboard → **Workers & Pages** →
   **Create** → paste in `cloudflare/worker.js`'s contents. (Or, if you
   prefer the CLI: `cd cloudflare && wrangler deploy`, using the
   `wrangler.toml` already in that directory.)
2. **Bind the Custom Domain.** On that Worker's **Settings → Domains &
   Routes → Add → Custom Domain**, enter `dispatchd.graditya.com`.
   Custom Domains (not the older Routes mechanism) provision the DNS
   record and TLS certificate automatically, and bind the *entire*
   domain directly to the Worker's `fetch` handler - so `/` (what a bare
   `curl dispatchd.graditya.com` requests) is answered by the script,
   with no extra path configuration needed.
3. That's it - `curl -fsSL https://dispatchd.graditya.com | sh` now
   works directly, no `-L` needed (the Worker serves the script itself
   rather than redirecting to GitHub).

The Worker proxies `install.sh` from the `main` branch on every request
(cached at Cloudflare's edge for 5 minutes), so it always mirrors
whatever's actually in the repo - merging a change to `install.sh` is
the only step needed to update what people get from the one-liner; the
Worker itself never needs redeploying for that.
