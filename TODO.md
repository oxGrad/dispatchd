# TODO

## Encrypt the Discord token at rest with `systemd-creds`

Currently `dispatchd service install` writes the token as plaintext in
`/etc/dispatchd/dispatchd.env` (mode `0600`, root-owned) - protected by
file permissions only, not encryption.

Deferred design (confirmed direction, not yet implemented):
- Requires systemd ≥250 (`systemd-creds`) - no silent fallback to
  plaintext if it's unavailable.
- Replace `/etc/dispatchd/dispatchd.env` with a `systemd-creds`-encrypted
  `/etc/dispatchd/discord_token.cred`, referenced from the unit via
  `LoadCredentialEncrypted=discord_token:/etc/dispatchd/discord_token.cred`
  instead of `EnvironmentFile=`.
- `dispatchd service install` never touches the raw token itself - it
  prints the exact `sudo systemd-creds encrypt - /etc/dispatchd/discord_token.cred`
  command for the operator to run themselves.
- `dispatchd` needs a small app-level change to read the token from
  `$CREDENTIALS_DIRECTORY/discord_token` (set automatically by systemd
  for units using `LoadCredentialEncrypted=`) in preference to the
  existing `DISPATCHD_DISCORD_TOKEN` env var, which stays as the
  fallback for local/dev use.
- Honest caveat to keep in any docs: the default `--with-key=host` mode
  (used since a Pi Zero 2 W has no TPM2) protects a stolen/copied SD card
  read on another machine offline, but *not* an attacker who already has
  root on the live running Pi (systemd itself can decrypt it there for
  legitimate service starts). A `--with-key=host+tpm2` upgrade is
  possible if a TPM2 module is ever added.
