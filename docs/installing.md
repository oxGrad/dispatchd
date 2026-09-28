# Installing dispatchd

```sh
curl -fsSL https://get.graditya.com/dispatchd | sudo sh
```

This downloads the right prebuilt binary for your machine from the
[latest GitHub Release](https://github.com/oxGrad/dispatchd/releases),
verifies its SHA-256 checksum, and installs it to `/usr/local/bin`. It
never compiles anything - no Rust toolchain required, including on a
Raspberry Pi, where compiling this project's dependency tree isn't
practical. It does **not** run `dispatchd init` or touch any config for
you - see "Next steps" below.

`get.graditya.com` is [`oxGrad/get`](https://github.com/oxGrad/get), a
shared Cloudflare Pages app that hosts installers (and, for dispatchd,
the Terms of Service / Privacy Policy pages below) for oxGrad's tools -
see "Where the installer actually lives" further down. This repo itself
has no Cloudflare setup.

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
  curl -fsSL https://get.graditya.com/dispatchd | sudo DISPATCHD_VERSION=v0.2.0 sh
  ```
- **`INSTALL_DIR`** - install somewhere other than `/usr/local/bin`. The
  common case is a local install with no `sudo`, into a directory you
  own (handy for trying dispatchd outside systemd, with
  `DISPATCHD_DISCORD_TOKEN` set directly):
  ```sh
  curl -fsSL https://get.graditya.com/dispatchd | INSTALL_DIR="$HOME/.local/bin" sh
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

### `dispatchd upgrade` (recommended)

```sh
sudo dispatchd upgrade                    # download latest, verify, swap, restart
dispatchd upgrade --check                 # report current vs latest, do nothing (read-only, no sudo)
sudo dispatchd upgrade --no-restart       # swap the binary, print the restart command
sudo dispatchd upgrade --version v0.4.0   # install a specific tag (pin / downgrade)
```

`dispatchd upgrade` resolves the latest GitHub release, and if it's newer
than the running binary, downloads the right prebuilt for this machine,
verifies its SHA-256 against the release `SHA256SUMS`, swaps it in place,
and runs `systemctl restart dispatchd`. `--no-restart` skips the restart
and prints the command instead; a restart that fails is reported, not
fatal. `--version` installs any tag, including an older one to downgrade.
It needs root to write `/usr/local/bin/dispatchd` and to restart the
service, hence `sudo`.

As with the installer, this covers the "binary changed" case. If a
release note says the **systemd unit itself** changed, still run
`sudo dispatchd service install` (see below).

`dispatchd upgrade` only works for a binary installed from a GitHub
release. A build-from-source install has a `DISPATCHD_TARGET` that won't
match any release asset, so there's nothing for it to download - rebuild
from source to update instead.

### Or re-run the installer

Re-run the installer - it overwrites the binary in place:

```sh
curl -fsSL https://get.graditya.com/dispatchd | sudo sh
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

The `dispatchd-upgrade.path` / `dispatchd-upgrade.service` helper units
(which back Discord's `/admin upgrade`) are also (re)written by
`service install` - a deployment that predates them needs one
`sudo dispatchd service install` followed by `sudo systemctl restart
dispatchd` after upgrading to pick them up (the restart is what creates
the bot's `/run/dispatchd` runtime directory).

To move between specific versions (including downgrades), use
`sudo dispatchd upgrade --version <tag>` (above), or pin the installer
with `DISPATCHD_VERSION` (see "Options") and restart the same way.

## Restarting the service

```sh
sudo dispatchd service restart   # equivalent to: sudo systemctl restart dispatchd
```

A thin wrapper around `systemctl restart dispatchd.service` that checks
the unit is actually installed first.

## Uninstalling the service

```sh
sudo dispatchd service uninstall   # stops, disables, and removes the systemd units
```

This removes `dispatchd.service`, the maintenance timer, and the
`/admin upgrade` helper units, then `daemon-reload`s. It leaves the
encrypted Discord token (`/etc/dispatchd/discord_token.cred`) and
config/DB files in place - run `sudo dispatchd discord logout` and
delete those yourself if you want a full teardown.

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
curl -fsSL https://get.graditya.com/dispatchd | sudo sh
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

## Where the installer (and the bot's legal pages) actually live

This repo has no Cloudflare setup of its own - no `install.sh`, no
`cloudflare/` directory. Everything is hosted by
[`oxGrad/get`](https://github.com/oxGrad/get), a Cloudflare Pages
Functions app shared across oxGrad's tools:

| URL | Serves |
| --- | --- |
| `https://get.graditya.com/dispatchd` | the install script |
| `https://get.graditya.com/dispatchd/tos` | Terms of Service |
| `https://get.graditya.com/dispatchd/privacy-policy` | Privacy Policy |

The `/tos` and `/privacy-policy` URLs are what you put in the Discord
Developer Portal (**App → General Information → Terms of Service URL /
Privacy Policy URL**); Discord asks for them once a bot is in enough
servers to need verification. Their content lives in `oxGrad/get` (not
here) - **fill in the `[effective date]` and `[operator contact email]`
placeholders there before publishing**, and have a lawyer look them
over if anything real is riding on them; they are a plain-English
starting point, not legal advice.

The install script is generated from `oxGrad/get`'s
`functions/_shared/install-script.js`, a template kept in sync with
this repo's actual release contract: the musl targets and armv7 support
above, the `SHA256SUMS` checksum file, `/usr/local/bin` as the default
install dir, and the `systemctl restart` prompt. If this repo's release
build ever changes - a new target, a renamed checksum file, a different
default install directory - update `oxGrad/get`'s `PRODUCTS.dispatchd`
entry (and, if the shape changes, `renderInstallScript`) to match;
that repo's README explains the template. A release here (see
`.github/workflows/release.yml`) just needs to keep producing the same
asset names and checksum format the template expects - there's nothing
to redeploy on this side.
