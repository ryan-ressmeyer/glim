# Installation and operations

## Source installation

Glimse supports Linux on `x86_64-unknown-linux-gnu`. A source build requires Rust 1.93.1, Node.js 22.12 through 24.x, npm, a C/C++ toolchain, and the native build tools required by Rust dependencies. Full repository checks also exercise the root Pi package, which requires Node.js 22.19 or newer. The repository selects Rust in `rust-toolchain.toml`; `.nvmrc`, CI, and release builds pin Node.js 22.22.0 for release reproducibility.

Install the CLI and daemon from a reviewed source checkout. Cargo copies the checked frontend source and lockfile into an isolated build directory under `OUT_DIR`, runs `npm ci` and the frontend build there, then embeds those generated files in the binary. An existing ignored `web/dist` is never used as a Cargo build input.

```bash
cargo install --path . --locked
```

Developers who will run the repository checks should install both locked JavaScript dependency trees first.

```bash
npm ci
cd web
npm ci
cd ..
make check
```

The crate is marked `publish = false`. This repository does not claim a crates.io release.

The Pi integration is a separate package. Review the selected revision, then install an immutable tag or commit rather than an unpinned branch.

```bash
pi install git:github.com/ryan-ressmeyer/glim@<tag-or-commit>
```

The Pi package requires the `glim` binary on `PATH` and a separately configured daemon.

## Release checksum verification

Download the archive and its matching `.sha256` file from the same GitHub release. The checksum file names the complete versioned archive, not the binary inside it. From the directory containing both files, run:

```bash
sha256sum --check glim-v0.1.0-x86_64-unknown-linux-gnu.tar.gz.sha256
```

Replace `v0.1.0` with the release tag. Continue only if `sha256sum` reports `OK` for the expected archive. The release page records the tag, commit, and target used by CI.

## Upgrades and compatibility

Stop the service and make a consistent backup before replacing the binary.

```bash
glim service stop
```

Verify the new archive checksum, replace or reinstall the binary, then start the service. Move the Pi package to an explicitly selected tag or commit with another pinned install command.

```bash
pi install git:github.com/ryan-ressmeyer/glim@<new-tag-or-commit>
glim service start
glim status
```

The v1 HTTP paths and v1 CLI JSON schemas follow the boundaries in [`compatibility.md`](compatibility.md). A new binary may add compatible fields or endpoints within v1. Clients must ignore unknown output fields. Breaking interface changes require a new major version. Pi git refs remain pinned during ordinary package updates, so selecting a new Glimse ref is an explicit operation.

Downgrade compatibility is not provided. Restore the pre-upgrade backup if an upgrade must be reversed.

## SQLite migrations and startup failures

The store database is `metadata.sqlite3` under the selected store root. The current code uses numbered forward migrations through schema version 6. Startup reads `PRAGMA user_version`, rejects a database newer than supported before applying migrations, and applies each pending migration in a SQLite transaction. A failed migration rolls back that migration and startup returns an error. Existing tests cover rollback, newer-schema rejection, and preservation for the checked legacy fixtures; they do not establish compatibility with future releases or arbitrary modified databases.

Do not open an upgraded store with an older binary. Preserve the backup until the upgraded daemon starts and its status is acceptable.

## Consistent backups

Stop Glimse before copying any state. Copying a running SQLite database separately from its WAL or copying blobs while publication is active can produce an inconsistent backup.

```bash
glim service stop
```

Back up each boundary that applies to the deployment.

- **Configuration:** the file selected by `GLIM_CONFIG`, otherwise `$XDG_CONFIG_HOME/glim/config.json` or `$HOME/.config/glim/config.json`.
- **Store:** the complete directory selected by `GLIM_STORE_ROOT` or `store_root`, otherwise `$XDG_DATA_HOME/glim` or `$HOME/.local/share/glim`. Keep `metadata.sqlite3`, any SQLite sidecars, finalized blobs, and staging state together.
- **Token:** the exact `access.token_file` or `GLIM_TOKEN_FILE` path. The common example uses `$HOME/.config/glim/access-token`.
- **TLS:** the exact certificate and private-key paths selected by `access.tls_certificate`, `access.tls_private_key`, `GLIM_TLS_CERTIFICATE`, and `GLIM_TLS_PRIVATE_KEY`. Glimse does not provision these files.

Trusted-proxy deployments must also preserve the proxy's configuration and credentials through the proxy's own backup process. Those files are outside the Glimse store.

## Complete removal

**DESTRUCTIVE:** These steps permanently delete publications, configuration, credentials, and local integration state. Confirm the active configuration and store paths before removing anything. Keep a tested backup if the data may be needed again.

Stop and remove the managed service first. Service uninstall leaves all other files intact.

```bash
glim service stop
glim service uninstall
```

Remove the pinned Pi package by repository identity.

```bash
pi remove git:github.com/ryan-ressmeyer/glim
```

For the default Cargo installation, remove the binary with Cargo. If a custom Cargo root was used, pass that same root.

```bash
cargo uninstall glim
```

If the release binary was copied manually, inspect `command -v glim` and remove that exact file. Do not remove a directory or a path inferred from untrusted output.

For a deployment using only the documented default paths, remove the exact files and store directory below after checking that they are the intended paths.

```bash
rm -f -- "$HOME/.config/glim/config.json"
rm -f -- "$HOME/.config/glim/access-token"
rm -rf -- "$HOME/.local/share/glim"
```

A custom deployment is not fully removed until the exact `GLIM_CONFIG`, `GLIM_STORE_ROOT`, token, certificate, and private-key paths selected by its configuration or environment are removed. Delete those paths individually after inspection. TLS files may be shared with other services; do not delete shared material. Project-local Pi installation uses `.pi/settings.json` and `.pi/git/` rather than user state, so use `pi remove -l git:github.com/ryan-ressmeyer/glim` from that trusted project when applicable.
