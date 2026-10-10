# Installation and configuration

[简体中文](INSTALL.md) | English

## Prerequisites

- Source installation requires Rust 1.88+ and a C toolchain for your platform: typically GCC/Clang on Linux, Xcode Command Line Tools on macOS, or build tools matching your Rust toolchain on Windows. The project compiles SQLite. npm and GitHub Release binaries do not require these build tools.
- Antigravity CLI must provide `agy`, stream-json, and `--gemini_dir`. Your existing login or API configuration must already have working model access. This project does not install or manage agy.
- Codex CLI must support named permission profiles and have a working sandbox. The locally verified version is `0.160.0`. The tool calls `codex sandbox`; it does not separately invoke or check bubblewrap. `install` and `doctor` check actual read/write boundaries.

Install Codex CLI with Node.js/npm:

```sh
npm install -g @openai/codex@0.160.0
```

Linux must satisfy the current Codex sandbox backend's requirements, including user namespace permissions where needed. Codex uses Seatbelt on macOS and its unelevated backend on Windows. Tests have passed in all six native CI environments across Linux, macOS, and Windows on x86_64 and ARM64, including project reads, rejected project writes, and writable temporary directories.

Current model verification uses an existing API/protocol conversion service. Native OAuth login, system keychain access, and token refresh remain untested. All three operating systems require a local Codex CLI for sandbox execution.

## Build and initialize

Choose an installation method:

- **GitHub Releases**: download the `.tar.gz` or `.zip` for your architecture, verify it against `SHA256SUMS`, extract it, and put the binary on PATH.
- **npm**: run `npm install -g agy-delegate-rs`. Node.js 22.14+ is required; Rust is not. Optional dependencies select one native binary for your system and architecture, without installation scripts. Keep optional dependencies enabled. If the platform package is missing, reinstall with `npm install -g agy-delegate-rs --include=optional`.
- **crates.io**: run `cargo install agy-delegate-rs --locked`, with the Rust and C toolchains described above.

Prebuilt packages cover Linux x64/arm64 with static musl linking, macOS x64/arm64, and Windows x64/arm64. None of these methods installs agy or Codex CLI. Run `agy-delegate-rs install` for initial integration, then start a new Codex session. Verify model authentication with a real investigation.

Upgrading from npm `0.1.3` or earlier changes the native executable path because newer versions use separate platform packages. **If you previously ran `install` or `register`, run `uninstall` with the old version before upgrading, then `install` with the new version.** Use the same configuration options as the original registration:

```sh
agy-delegate-rs uninstall
npm install -g agy-delegate-rs@latest
agy-delegate-rs install
```

If you have already upgraded and get an MCP ownership error, temporarily install `agy-delegate-rs@0.1.3` in the original npm prefix to restore its executable path, then follow the migration above. Configuration and history do not need to be deleted. `uninstall` first stops tasks in the selected state directory. Users without an MCP registration can install the new version directly; later platform package upgrades keep the same path.

For installation from the repository:

```sh
cargo install --path . --locked
agy-delegate-rs init
agy-delegate-rs doctor
```

Ensure Cargo's bin directory is on PATH. `init` prints the configuration path, normally `agy-delegate-rs/config.toml` under your system's user configuration directory. On Linux this is usually `~/.config/agy-delegate-rs/config.toml`, subject to `XDG_CONFIG_HOME`.

You can select a configuration file explicitly:

```sh
agy-delegate-rs init --config /absolute/path/config.toml
agy-delegate-rs --config /absolute/path/config.toml doctor
```

`init` never overwrites an existing file or generates/copies credentials. Edit existing configuration directly. `doctor` returns JSON and exits unsuccessfully if a check fails. It checks configuration, environment files, program versions, public headless agy options, state storage, and sandbox boundaries. It does not call models or verify login, API connectivity, token refresh, or the actual data location used by `--gemini_dir`. Run a real investigation after initialization to verify your agy version and authentication.

## Configuration

Default template:

```toml
codex = "codex"
agy = "agy"
agy_args = []
# Always passed explicitly; agy's interactive model selection does not affect tasks.
model = "gemini-3.8-flash-high"
log_bytes = 4000000
profile_bytes = 128000000
timeout = 600
report_chars = 4000
retention_days = 14
max_history_jobs = 1000
# agy_env_file = "/absolute/path/api.env"
# effort = "high"
# state_dir = "/absolute/path/state"
```

Explicit command-line options override file settings. Put configuration overrides before the subcommand, for example `agy-delegate-rs --model your-model --agy-env-file /absolute/path/api.env run --cwd /path/to/repo --task 'Trace entry points'`. `--config` and `--timeout` also work after the subcommand. File-relative paths resolve against the configuration directory. Plain `agy` and `codex` names are looked up on PATH; write relative executable paths as `./bin/agy`. Command-line paths resolve against the launch directory. `~` and environment variables in paths are not expanded; use full paths.

`model` defaults to agy's built-in `gemini-3.8-flash-high`, meaning Gemini 3.8 Flash (High). Every new investigation and follow-up passes it explicitly, independently of the model selected in agy. The default also applies when there is no configuration file, `model` is omitted, or an old JSON snapshot contains `model: null`. An existing explicit model remains in effect. An unavailable model causes task failure; there is no automatic fallback.

`effort` is optional and accepts `low`, `medium`, `high`, `xhigh`, or `max`; the installed agy and selected model determine which levels are supported. The default model ID already includes High, so no separate effort is needed. agy 1.3.2 also supports `model = "gemini-3.8-flash"` with `effort = "high"`, or command-line options `--model gemini-3.8-flash --effort high`. A separate effort must agree with any effort suffix in the model ID. For custom APIs or protocol converters, use the IDs accepted by `agy models` and your endpoint. This tool does not rewrite model names.

Other settings:

| Setting | Default | Purpose and range |
| --- | --- | --- |
| `timeout` | `600` | Task execution deadline, 1–7200 seconds |
| `report_chars` | `4000` | Report length returned by task queries, 256–6000 characters |
| `retention_days` | `14` | History retention, 1–365 days |
| `max_history_jobs` | `1000` | History count limit, 10–10000 |
| `log_bytes` | `4000000` | Limit for an individual agy log and retained stderr, 4096–32000000 bytes |
| `profile_bytes` | `128000000` | Total file bytes in each managed conversation directory, 1000000–1000000000 |

Retention settings apply to manual cleanup and automatic cleanup after workers save results. `agy_args` precede managed agy options: for example, `agy = "node"` with `agy_args = ["/absolute/path/agy.js"]`. You can also repeat `--agy-arg`. Keep credentials out of arguments. Protocol output has a separate 4 MB limit. `--profile-bytes` can override the conversation limit before the subcommand.

The Rust version does not import the original Python project's `config.json` or task database automatically. Keep separate state directories when running both versions. Migrate executable paths, model IDs, and environment file paths manually; keep credentials out of TOML.

Authentication environment variables are inherited; existing variables override `agy_env_file` values. Environment files reference your existing configuration, without adding credential fields to project configuration. Unknown configuration keys are rejected. Credential environment values and persistent task paths must be UTF-8. Run any protocol conversion service separately.

New tasks use `state_dir/conversations/<root_id>` through `--gemini_dir`; follow-ups reuse it. On first use, only existing `settings.json` and `antigravity-oauth-token` files are copied from the default `~/.gemini/antigravity-cli`. Host conversations, plugins, and separate MCP configuration files are not copied. Existing private login files are preserved; refreshed credentials stay private and original files are not updated. Environment file credentials are passed through the environment; the host's `api.env` is not imported automatically. Keep private login data, model caches, and investigation history out of version control.

## Register with Codex MCP

After initialization:

```sh
agy-delegate-rs install
# Register MCP without adding delegation rules:
agy-delegate-rs register
# Select a Codex configuration directory, also useful for isolated verification:
agy-delegate-rs install --codex-home /absolute/path/codex-home
```

`install` first runs offline doctor checks, then registers MCP and adds managed AGENTS rules. It refuses to overwrite an existing Python registration or policy. After verifying the Rust version, remove the old integration manually. Existing external protocol conversion services can remain in use.

Registration saves a snapshot of the effective configuration, including command-line overrides. Run `register` again after changes. The installer manages configuration and MCP integration; use npm, Cargo, or GitHub Releases to manage the executable itself.

Ownership checks verify both the current executable path and the snapshot for the selected Codex directory. Upgrades retaining the same path can register again. Before moving the executable or changing installation channels, run `uninstall` from the original path. The tool does not take over registrations owned by another executable path.

Install, register, and uninstall operations are mutually exclusive and recheck ownership after stopping tasks. Codex configuration writes have no shared lock or conditional update interface. Avoid simultaneous `codex mcp add/remove` calls or manual edits to the same file; external concurrent writes can still overwrite each other.

Policy backup and restoration use file synchronization and atomic replacement. Unix also synchronizes the parent directory, persisting the backup, policy, and backup removal in order. Windows supports recovery after process interruption, but does not guarantee directory durability after power loss.

For manual registration, add this to Codex's `config.toml`:

```toml
[mcp_servers.agy-delegate-rs]
command = "/absolute/path/agy-delegate-rs"
args = ["--config", "/absolute/path/config.toml", "mcp"]
```

TOML single-quoted strings are convenient for Windows paths. With default configuration, use `args = ["mcp"]`. Restart or reload Codex's MCP server to apply it.

Available tools are `agy_submit`, `agy_status`, `agy_wait`, `agy_followup`, `agy_cancel`, and `agy_list`. Lists return brief status; use status/wait for details. Check task status and do not treat `incomplete` or failed tasks as completed. Uninstallation removes only matching registrations and verifies that managed rules have not been edited, preserving other settings and rules.

## Run and clean up

```sh
agy-delegate-rs run --cwd /path/to/repo --task 'Trace entry points with file and line references'
agy-delegate-rs submit --cwd /path/to/repo --task 'Trace entry points with file and line references'
agy-delegate-rs wait JOB_ID --seconds 50
agy-delegate-rs followup JOB_ID 'Continue checking the call chain'
agy-delegate-rs submit --cwd /path/to/repo --task-file /path/to/task.txt
agy-delegate-rs followup JOB_ID --task-file /path/to/followup.txt
agy-delegate-rs report JOB_ID --output /path/to/new-report.txt
agy-delegate-rs cleanup --dry-run
agy-delegate-rs cleanup
```

Choose either `--task` or `--task-file`. `--task-file -` reads UTF-8 from stdin. Files are limited to 96000 bytes and tasks to 24000 characters. `report` returns the complete stored result regardless of query report limits; exports preserve existing files.

Independent background workers continue after the MCP server closes. A wait timeout does not cancel work. State defaults to the system user data directory, configurable with `state_dir` or `--state-dir`.

Cleanup selects finished tasks by last update time or by the retained history count. Preview `selected_ids` with `--dry-run`. Running tasks, orphaned tasks, and history belonging to conversations with active or unverified tasks are retained. Tasks with an ownership lock or a surviving sandbox process are also skipped.

Cleanup removes task database records and log directories. Deleted job IDs cannot be queried or followed up. A conversation directory remains while other history references exist, allowing follow-ups through those records. Removing the last reference deletes the managed directory and returns its ID in `deleted_conversation_ids`. Shared `~/.gemini` data and original login files are preserved.

Synchronous `run` also uses task records and workers, with the same cleanup rules. Ctrl-C requests cancellation. Cleanup runs in workers after saving results, without blocking submission. A synchronous result may show `cleanup_status: pending`; query `status JOB_ID` for the final outcome. Older unrecorded `run-*` directories are retained; remove them manually only after confirming no process uses them. Older shared-profile tasks continue using shared data, and cleanup removes only their records/logs. Managed directories without history proving ownership are also retained.

Cleanup marks tasks `purging` before deleting logs and eligible conversation directories. Follow-ups in the same conversation are blocked during removal. Failed file deletion preserves records and returns failure details for retry. Partially deleted conversation directories remain `purging` to prevent resuming damaged conversations. Run `cleanup` again to recover after interruption. Retention limits count only history that is safe to remove; locked records and active conversations do not consume the retained count.

Only one actual cleanup process may run per state directory. A second process returns `cleanup_active` and skips removal. The log directory's `.cleanup.lock` is retained for process locking; do not delete it while the tool is running. Replaced directories or unverifiable deletion boundaries cause records to be retained with a skip/error report.

## Sandbox scope and uninstall

New tasks can read project and ordinary host files, write managed conversation and task temporary directories, and access the network. Older shared-profile tasks can still write the host's `~/.gemini`. The sandbox protects project writes; it does not isolate accounts, credentials, or networks. Overlapping working/writable directories are rejected. Sandbox startup failure stops execution without an unsandboxed fallback.

On Unix, cancellation fails and blocks follow-ups if the worker and sandbox group leader have exited but surviving child processes cannot be identified reliably. Manual process cleanup is required; there is currently no separate supervisor.

First run `agy-delegate-rs uninstall` to stop and verify tasks in the selected state, remove matching MCP registration, and restore managed rules. Then remove the executable with `npm uninstall -g agy-delegate-rs`, `cargo uninstall agy-delegate-rs`, or by deleting your downloaded binary. If task termination cannot be verified or managed rules were edited, integration removal stops. Configuration, history, and agy settings are retained; data cleanup is a separate decision.

Application log sizes are checked every 100 milliseconds. Exceeding the limit terminates the task; retained logs are truncated and redacted afterward. Managed conversation file sizes are checked before startup, roughly every 2 seconds during execution, and after termination. Exceeding `profile_bytes` terminates the entire sandbox process group. Scans do not follow symbolic links and fail tasks if a safe scan is impossible. Cancellation, timeout, and existing execution failures retain their original status/diagnostics, with final scan errors added separately.

Monitoring is not a filesystem quota; limits can be exceeded between checks. `profile_bytes` covers managed conversation directories, not shared agy account directories or total disk usage across other tasks.

If the operating system cannot confirm termination of the entire sandbox process group within the deadline, the task fails. A `tmp/UNVERIFIED_PROCESS` marker indicates that original logs remain in the private directory and must not be considered redacted. Stop and verify related processes before handling logs or cleanup. Uninstall rejects task directories it cannot verify.

Uninstall cancels tasks in the selected `state_dir`, including CLI tasks, even if the current Codex directory has no Rust MCP registration. Codex directories sharing state also share the task pool and cleanup policy; use separate state directories for isolation. New submissions are blocked during shutdown checks. Retry uninstall later if a task is being submitted or started.

## Maintainer releases

`.github/workflows/release.yml` runs only on pushed `v*.*.*` tags, further restricted to stable `vMAJOR.MINOR.PATCH` versions. The tagged commit must be in the repository's default branch history, and tag, Cargo, and npm versions must match. Otherwise publication stops.

One-time setup:

1. Push the repository to `JIAFALSEDREAM/agy-delegate-rs`, set `main` as the default branch, and enable Actions.
2. Add an Actions secret named `CARGO_REGISTRY_TOKEN` with permission to publish `agy-delegate-rs`. npm uses trusted publishing and does not need `NPM_TOKEN`.
3. Your npm account must own the entry package and all six `agy-delegate-rs-{linux,darwin,win32}-{x64,arm64}` platform packages. Bootstrap missing packages by downloading `PACKAGE-VERSION.tgz` from GitHub Releases and running `npm publish ./PACKAGE-VERSION.tgz --access public` after npm login. Publish platform packages before the entry package. Existing packages do not need bootstrapping. If trusted publishing is not configured yet, the npm job may fail; rerun it after setup.
4. Configure a Trusted Publisher for each npm package: GitHub Actions, owner `JIAFALSEDREAM`, repository `agy-delegate-rs`, workflow `release.yml`, and no Environment. Allow direct `npm publish`; staged publishing permissions do not replace this. Current npm documentation requires the first successful use within 2 days of configuration, otherwise recreate it.

With npm 11.15+, you can configure trusted publishing after initial publication using the CLI and browser 2FA. For the entry package:

```sh
npm trust github agy-delegate-rs --repo JIAFALSEDREAM/agy-delegate-rs --file release.yml --allow-publish --yes
```

Repeat for each platform package. Cargo/npm repository metadata must match the actual Actions repository. Releases use tagged source, and crate packaging requires a clean working tree. Uncommitted local verification documents are excluded.

For each release, update `Cargo.toml`, the entry npm package version and its six `optionalDependencies`, and this package's version in `Cargo.lock`. CI generates platform package metadata with the same version. Commit to the default branch, then create and push the tag. For example, when all versions are `0.1.3`:

```sh
git tag -a v0.1.3 -m 'Release v0.1.3'
git push origin v0.1.3
```

The workflow validates the release, builds and tests all six native platforms, checks their sandboxes, and produces six binary archives, seven npm packages, and SHA-256 checksums. After all builds succeed, it publishes GitHub Releases, then npm and crates.io. The npm job publishes/verifies all platform packages before the entry package. CI does not call models; cross-platform authentication still requires verification on actual machines.

Publication across the three channels is not atomic. Rerun failed jobs in Actions. If a version already exists on npm or crates.io, its integrity checksum must match before skipping it; different contents are rejected. Public GitHub Release assets are verified and reused. Only draft assets may be replaced. After manually bootstrapping npm packages, rerun the same job to reuse the original archives. Do not delete a published tag and republish different contents; fix the issue and release a new version.
