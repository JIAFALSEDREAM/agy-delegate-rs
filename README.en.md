# agy-delegate-rs

[简体中文](README.md) | English

[![npm version](https://img.shields.io/npm/v/agy-delegate-rs?style=flat-square&logo=npm)](https://www.npmjs.com/package/agy-delegate-rs)
[![crates.io version](https://img.shields.io/crates/v/agy-delegate-rs?style=flat-square&logo=rust)](https://crates.io/crates/agy-delegate-rs)
[![GitHub release](https://img.shields.io/github/v/release/JIAFALSEDREAM/agy-delegate-rs?style=flat-square&logo=github)](https://github.com/JIAFALSEDREAM/agy-delegate-rs/releases/latest)
[![CI status](https://img.shields.io/github/actions/workflow/status/JIAFALSEDREAM/agy-delegate-rs/ci.yml?branch=main&style=flat-square&logo=githubactions&label=CI)](https://github.com/JIAFALSEDREAM/agy-delegate-rs/actions/workflows/ci.yml)
[![Rust 1.88+](https://img.shields.io/badge/Rust-1.88%2B-orange?style=flat-square&logo=rust)](Cargo.toml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue?style=flat-square)](LICENSE)

Delegate read-only repository investigations to Antigravity CLI (`agy`), with project files protected by the Codex operating system sandbox. Run investigations from the command line, or use the Codex MCP server to submit background tasks, retrieve results, and ask follow-up questions.

Useful for tracing entry points and call chains, locating configuration, counting files, and comparing implementations. The tool calls your existing agy installation. Model hosting and API protocol conversion are managed separately.

The default model is **Gemini 3.8 Flash (High)** (`gemini-3.8-flash-high`). Every investigation and follow-up passes the model explicitly, so switching models in agy does not affect this tool. Override it through this tool's configuration or command-line options.

## Quick start

### 1. Prepare dependencies

- **Antigravity CLI**: an `agy` command supporting stream-json and `--gemini_dir`, with working model access.
- **Codex CLI**: support for named permission profiles. The locally verified version is `0.160.0`.
- **A working Codex sandbox**: the tool calls `codex sandbox`; Codex manages the platform backend. `install` checks the actual read/write boundaries.
- **Node.js 22.14+**: required for the npm installation below.

See the [installation guide](INSTALL.en.md#prerequisites) for prerequisites. This package does not install agy or Codex CLI.

### 2. Install and connect to Codex

```sh
npm install -g agy-delegate-rs
agy-delegate-rs install
```

npm selects the binary for your platform; Rust is not required. `install` checks dependencies and the sandbox, registers the MCP server, and adds managed AGENTS rules so Codex prefers MCP for read-only investigations. Start a new Codex session after installation to use `agy_submit` and the other tools.

You can also download a binary from [GitHub Releases](https://github.com/JIAFALSEDREAM/agy-delegate-rs/releases), or run `cargo install agy-delegate-rs --locked`. Prebuilt archives and npm packages cover x86_64 and ARM64 on Linux, macOS, and Windows. Source installation requires Rust 1.88+ and a C toolchain for your platform.

**Upgrading from older npm packages**: if you registered MCP with `0.1.3` or earlier, run `uninstall` with the old version before upgrading, then run `install` with the new version. See the [migration steps](INSTALL.en.md#build-and-initialize).

### 3. Verify a real investigation

```sh
agy-delegate-rs run --cwd /absolute/path/to/repo --task 'Trace task scheduling entry points and call chains, with file and line references'
```

Installation checks and `doctor` do not call a model. This step verifies agy authentication and model access.

## Everyday use

### Command line

`run` waits for completion and returns the result. For background tasks, use `submit`, then query or follow up with the returned job ID:

```sh
agy-delegate-rs submit --cwd /absolute/path/to/repo --task 'Locate configuration loading, with file and line references'
agy-delegate-rs wait JOB_ID --seconds 50
agy-delegate-rs followup JOB_ID 'Check the configuration override order'
```

A follow-up creates a new job ID and reuses the original conversation. Background tasks run in independent workers and continue after the MCP server closes. A `wait` timeout does not cancel the task.

For longer instructions, use `--task-file /absolute/path/task.txt` instead of `--task`. Export a complete report with:

```sh
agy-delegate-rs report JOB_ID --output /absolute/path/new-report.txt
```

Exports never overwrite an existing file. See [running and cleanup](INSTALL.en.md#run-and-clean-up) for more commands.

### Codex MCP

| Tool | Purpose |
| --- | --- |
| `agy_submit` | Submit a read-only investigation and immediately return a job ID |
| `agy_status` | Query task status and results |
| `agy_wait` | Wait for a result; the task continues if the wait times out |
| `agy_followup` | Continue the original conversation with a new job |
| `agy_cancel` | Request termination of the task's local process tree |
| `agy_list` | List recent tasks and recover job IDs |

Provide a precise investigation scope and an absolute repository path. Always check the returned status: failed, incomplete, or timed-out investigations must not be treated as completed. Retrieve detailed results with `agy_status` or `agy_wait`.

## Configuration and authentication

Authentication environment variables are inherited by default. You can also load an existing environment file:

```sh
agy-delegate-rs --agy-env-file /absolute/path/api.env run --cwd /absolute/path/to/repo --task 'Locate the configuration entry point'
```

Existing environment variables take precedence over values in the file. Environment file contents are not written to task records. Place override options such as `--model`, `--agy`, `--codex`, and `--state-dir` before the subcommand.

To create a configuration file, run `agy-delegate-rs init` and edit the path it prints. The default is in your system's user configuration directory, usually `~/.config/agy-delegate-rs/config.toml` on Linux. Run `register` again after configuration changes to update the snapshot used by MCP.

The default model configuration is:

```toml
model = "gemini-3.8-flash-high"
```

This default also applies when there is no configuration file or `model` is omitted. `gemini-3.8-flash-high` is an agy built-in model ID that includes High reasoning effort. agy 1.3.2 also accepts the model and effort separately; choose either form:

```toml
model = "gemini-3.8-flash"
effort = "high"
```

The corresponding command-line options are `--model gemini-3.8-flash --effort high`. In agy, the bare Gemini 3.8 Flash model name requires an effort. If a model ID already has an effort suffix, a separate `effort` must agree with it. Custom APIs and protocol converters must use model IDs accepted by the installed agy and endpoint; check `agy models`. agy manages login authentication and custom API settings. This tool does not translate model IDs or authentication protocols. If the model is unavailable, the task fails rather than switching models automatically.

An explicitly configured model from an older version remains in effect. `null` model values in old JSON task snapshots fall back to this tool's default. To apply the new default to an existing MCP registration, check this tool's `model` configuration and run `agy-delegate-rs register` again.

Use `agy-delegate-rs doctor` to check configuration, dependencies, and actual sandbox read/write boundaries. Verify model authentication with a real investigation.

See the [installation guide](INSTALL.en.md) for all settings, custom Codex directories, and manual MCP registration. Run any API protocol conversion service separately.

## Sandbox and data boundaries

- **Read-only access**: project files and ordinary host files are read-only. New tasks may write only to their managed conversation directory and temporary task directory. A working directory overlapping a writable directory is rejected.
- **Network and credentials**: network access for model calls is allowed. The sandbox restricts file writes; it does not isolate networks or credentials.
- **Startup**: automatic approval of agy tool permissions is used inside the operating system sandbox. If sandbox startup fails, the task fails.
- **Conversation data**: follow-ups reuse `state_dir/conversations/<root_id>`. On first use, existing `settings.json` and `antigravity-oauth-token` files are imported from the default `~/.gemini/antigravity-cli`. Subsequent refreshes stay in the private directory; original files are not updated.
- **Storage limits**: managed conversation directories default to a 128,000,000-byte limit (`profile_bytes`). Periodic monitoring terminates tasks that exceed it; this is not a filesystem quota.

State is stored in your system's user data directory under `agy-delegate-rs`, or the directory selected with `--state-dir`. Reports, caches, and private login files may contain sensitive data; keep them out of version control.

Preview cleanup before removing history:

```sh
agy-delegate-rs cleanup --dry-run
agy-delegate-rs cleanup
```

Deleted tasks cannot be queried or followed up. Removing the last history reference to a conversation also removes its managed directory. Older tasks using a shared profile continue to use their original data; cleanup preserves the user's shared `~/.gemini`. See [cleanup](INSTALL.en.md#run-and-clean-up) and [uninstallation](INSTALL.en.md#sandbox-scope-and-uninstall).

## Support and validation

This is a preview release. Tests and sandbox read/write checks have passed on all six native CI environments: x86_64 and ARM64 on Linux, macOS, and Windows.

Real model calls, reconnection, follow-ups, and cleanup have been verified on Linux. Real model calls on macOS and Windows still need verification on actual machines. Native OAuth login, token refresh, and system keychain reuse have not been tested.

Run tests from the source repository:

```sh
cargo test
cargo test --test sandbox native_sandbox_preserves_fixture -- --ignored
```

## License

The [MIT license](LICENSE) covers this project's code. The tool calls an externally installed Codex CLI and does not bundle its binary or bubblewrap.
