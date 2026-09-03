# Installing dispatchd

```sh
curl -fsSL https://dispatchd.graditya.com | sudo sh
```

This downloads the right prebuilt binary for your machine from the
[latest GitHub Release](https://github.com/oxGrad/dispatchd/releases),
verifies its SHA-256 checksum, and installs it to `/usr/local/bin`. It
never compiles anything - no Rust toolchain required, including on a
Raspberry Pi, where compiling this project's dependency tree isn't
practical. It does **not** run `dispatchd init` or touch any config for
you - see "Next steps" below.

`sudo` is used because dispatchd runs as a systemd service: the binary
has to live somewhere `sudo dispatchd ...` (which has a sanitized
`PATH`) and the systemd unit can both find it. `/usr/local/bin` is that
place; `$HOME/.local/bin` is not. If you only want to try it locally
without the service, see `INSTALL_DIR` under "Options".

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
  curl -fsSL https://dispatchd.graditya.com | sudo DISPATCHD_VERSION=v0.2.0 sh
  ```
- **`INSTALL_DIR`** - install somewhere other than `/usr/local/bin`. The
  common case is a local install with no `sudo`, into a directory you
  own (handy for trying dispatchd outside systemd, with
  `DISPATCHD_DISCORD_TOKEN` set directly):
  ```sh
  curl -fsSL https://dispatchd.graditya.com | INSTALL_DIR="$HOME/.local/bin" sh
  ```
  Note that a binary under `$HOME` can't be used for the systemd
  deployment - `sudo` and systemd won't find it (and on SELinux systems
  can't execute it). Use the default `/usr/local/bin` for that.
  The script never escalates privileges on its own - if `INSTALL_DIR`
  isn't writable it fails with a clear message rather than silently
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

## Upgrading

Re-run the installer - it overwrites the binary in place:

```sh
curl -fsSL https://dispatchd.graditya.com | sudo sh
```

systemd keeps running the old binary until the service is restarted (the
replaced file's inode stays open). If `dispatchd.service` is active when
the installer finishes, it detects that and prompts:

```text
dispatchd.service is running. Restart it now to run v0.4.0? [y/N]
```

Answer `y` and it runs the restart for you. When there's no terminal to
prompt on (piped in CI, another script), it never restarts on its own -
it just prints the command to run:

```sh
sudo systemctl restart dispatchd
```

Afterwards, confirm what's running:

```sh
dispatchd --version   # the version now on disk
dispatchd status      # unit installed/enabled/active, Discord reachable
```

That's the whole upgrade. You do **not** need to re-run
`dispatchd service install`, remove and re-add the unit, or
`daemon-reload` - the unit's `ExecStart` is a fixed
`/usr/local/bin/dispatchd` path with no version in it. Database schema
migrations, if any ship in the new version, run automatically on the next
start.

The one exception: if a release note says the **systemd unit itself**
changed, run

```sh
sudo dispatchd service install   # idempotent: rewrites the unit + daemon-reload, does not restart
sudo systemctl restart dispatchd
```

To move between specific versions (including downgrades), pin with
`DISPATCHD_VERSION` (see "Options") and restart the same way.

## Running on a cloud VM (Google Cloud free tier)

dispatchd is a good fit for a tiny always-on VM: one static binary,
~20-50 MB RAM, negligible CPU, a few MB of SQLite on disk, and only
**outbound** network (the Discord gateway). No inbound ports, so there is
**no firewall or load-balancer setup** - a plain VM on the default VPC is
all it needs.

GCP's [free tier](https://cloud.google.com/free/docs/free-cloud-features#compute)
includes one `e2-micro` that runs $0 for compute as long as you stay
inside these limits:

- **Region:** `us-west1`, `us-central1`, or `us-east1` (only these
  qualify).
- **Machine type:** exactly `e2-micro`.
- **Boot disk:** 30 GB or less, **Standard persistent disk**
  (`pd-standard`) - not SSD or balanced.
- **Image:** Debian 12 or Ubuntu 24.04. Both ship systemd >= 250, which
  `dispatchd discord login` requires. **Ubuntu 22.04 ships systemd 249
  and will not work** with the systemd install path.
- **Egress:** 1 GB/month is free; a standup bot for a handful of people
  uses a tiny fraction of that.

One caveat: GCP now bills external IPv4 addresses at roughly
**$0.004/hr (about $3/month)** even on a free-tier VM. dispatchd needs
outbound internet, and removing the external IP would force Cloud NAT
(not free), so budget ~$3/month for the address unless Google's pricing
changes. Everything else is genuinely free.

Create the VM (console: **Compute Engine -> Create instance**, or CLI):

```sh
gcloud compute instances create dispatchd \
  --zone=us-central1-a \
  --machine-type=e2-micro \
  --image-family=debian-12 --image-project=debian-cloud \
  --boot-disk-size=10GB --boot-disk-type=pd-standard
```

SSH in and install as usual:

```sh
gcloud compute ssh dispatchd --zone=us-central1-a

# on the VM:
curl -fsSL https://dispatchd.graditya.com | sudo sh
sudo timedatectl set-timezone Asia/Jakarta   # optional - match your team
dispatchd init
# edit ~/.config/dispatchd/config.toml (discord_guild_id,
# discord_standup_channel_id, timezone) and ~/.config/dispatchd/members.toml
sudo dispatchd service install
sudo dispatchd discord login
sudo systemctl start dispatchd
dispatchd status
```

The systemd unit restarts the bot on failure and on VM reboot, and the
maintenance timer (installed alongside it) handles the weekly prune. The
SQLite DB lives on the boot disk, which persists across reboots - back it
up (`~/.local/share/dispatchd/`, or wherever `DISPATCHD_DB_PATH` points)
if the biweekly-recap history matters to you.

The same recipe works on any small always-on Linux VM (Oracle Cloud's
always-free Ampere instances, a Raspberry Pi, etc.) - only the
provisioning command changes.

## Hosting the installer at `dispatchd.graditya.com` (Cloudflare)

`install.sh` is committed at the repo root, so
`curl -fsSL https://raw.githubusercontent.com/oxGrad/dispatchd/main/install.sh | sudo sh`
already works with no extra setup. Serving it at the bare custom domain
(no path) needs a small proxy in front, since GitHub's raw content isn't
served from that domain - `cloudflare/worker.js` in this repo is exactly
that proxy. It routes:

| Path | Serves |
| --- | --- |
| `/` and `/install.sh` | `install.sh` (so `curl … \| sh` works) |
| `/tos` | `cloudflare/tos.html` - the bot's Terms of Service |
| `/privacy-policy` | `cloudflare/privacy-policy.html` - the bot's Privacy Policy |
| anything else | `404` |

Every route is fetched from the repo's `main` branch (cached 5 minutes at
Cloudflare's edge), so editing one of those files in the repo is all it
takes to update what the domain serves. The `/tos` and `/privacy-policy`
URLs are what you put in the Discord Developer Portal (**App → General
Information → Terms of Service URL / Privacy Policy URL**); Discord asks
for them once a bot is in enough servers to need verification. **Fill in
the `[effective date]` and `[operator contact email]` placeholders in
both HTML files before publishing** - and have a lawyer look them over if
anything real is riding on them; they are a plain-English starting point,
not legal advice.

To wire up the Worker (needs your own Cloudflare account - this is a
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
3. That's it - `curl -fsSL https://dispatchd.graditya.com | sudo sh` now
   works directly, no `-L` needed (the Worker serves the script itself
   rather than redirecting to GitHub), and `/tos` / `/privacy-policy`
   serve the two HTML pages.

### Deploying via a Git-connected build (Workers Builds)

If instead of pasting/`wrangler deploy` you connect the Worker to this
GitHub repo (Worker → **Settings → Builds**), two settings matter, because
`wrangler.toml` lives in `cloudflare/`, not the repo root:

- **Root directory:** set it to `cloudflare`. Without this the build fails
  with *"Missing entry-point to Worker script or to assets directory"* -
  Wrangler is looking for a config file at the repo root and there isn't
  one. (Alternatively, leave the root at `/` and set the deploy command to
  `npx wrangler deploy --config cloudflare/wrangler.toml`.)
- **Worker name:** `name` in `cloudflare/wrangler.toml` must equal the
  connected Worker's name. If they differ, Cloudflare flags it after each
  build and (Wrangler ≥ 3.109.0) opens a PR to rewrite the file to match -
  so pick the name when you create the Worker and keep the file in sync.

The Worker fetches each route's file from the `main` branch on every
request (cached at Cloudflare's edge for 5 minutes), so it always mirrors
whatever's actually in the repo - merging a change to `install.sh`,
`cloudflare/tos.html`, or `cloudflare/privacy-policy.html` is the only
step needed to update what the domain serves; the Worker itself only
needs redeploying if you change its routing in `worker.js`.
