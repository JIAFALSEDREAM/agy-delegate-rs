# agy-delegate-rs

[![npm version](https://img.shields.io/npm/v/agy-delegate-rs?style=flat-square&logo=npm)](https://www.npmjs.com/package/agy-delegate-rs)
[![crates.io version](https://img.shields.io/crates/v/agy-delegate-rs?style=flat-square&logo=rust)](https://crates.io/crates/agy-delegate-rs)
[![GitHub release](https://img.shields.io/github/v/release/JIAFALSEDREAM/agy-delegate-rs?style=flat-square&logo=github)](https://github.com/JIAFALSEDREAM/agy-delegate-rs/releases/latest)
[![CI status](https://img.shields.io/github/actions/workflow/status/JIAFALSEDREAM/agy-delegate-rs/ci.yml?branch=main&style=flat-square&logo=githubactions&label=CI)](https://github.com/JIAFALSEDREAM/agy-delegate-rs/actions/workflows/ci.yml)
[![Rust 1.88+](https://img.shields.io/badge/Rust-1.88%2B-orange?style=flat-square&logo=rust)](Cargo.toml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue?style=flat-square)](LICENSE)

把代码库的只读调查交给 Antigravity CLI（`agy`），通过 Codex 的操作系统沙箱保护项目文件。支持直接在命令行运行，也支持作为 Codex MCP 服务提交后台任务、查询结果和继续追问。

适合梳理入口与调用链、查找配置、统计文件、比较实现等调查任务。项目调用你已安装并配置好的 agy，不包含模型服务或 API 协议转换代理。

## 快速开始

### 1. 准备依赖

- **Antigravity CLI**：提供 `agy` 命令，支持 stream-json 和 `--gemini_dir`，且已能正常调用模型。
- **Codex CLI**：支持命名权限配置，本机验证版本为 `0.160.0`。
- **Codex 沙箱可用**：本工具直接调用 `codex sandbox`，由 Codex 管理平台沙箱后端；`install` 会实际验证读写边界。
- **Node.js 22.14+**：使用下方 npm 安装方式时需要。

依赖安装方法见[安装指南](INSTALL.md#前提)。本工具的安装包不会安装 agy 或 Codex CLI。

### 2. 安装并接入 Codex

```sh
npm install -g agy-delegate-rs
agy-delegate-rs install
```

npm 自动选择当前平台的二进制，无需 Rust。`install` 会先检查依赖和沙箱，再注册 MCP 服务并写入受管理的 AGENTS 策略，让 Codex 优先通过 MCP 委派只读调查。成功后新开 Codex 会话，即可使用 `agy_submit` 等工具。

也可通过 [GitHub Releases](https://github.com/JIAFALSEDREAM/agy-delegate-rs/releases) 下载预编译包，或使用 `cargo install agy-delegate-rs --locked`。预编译包与 npm 包覆盖 Linux、macOS、Windows 的 x86_64 和 ARM64；从源码安装需要 Rust 1.88+ 和本平台的 C 编译工具链。

**旧版 npm 用户**：如果已用 `0.1.3` 或更早版本注册 MCP，升级前需先用旧版执行 `uninstall`，升级后再执行 `install`。完整迁移步骤见[安装指南](INSTALL.md#构建与初始化)。

### 3. 验证一次真实调查

```sh
agy-delegate-rs run --cwd /absolute/path/to/repo --task '梳理任务调度入口和调用链，附文件行号'
```

安装检查和 `doctor` 都不调用模型。这一步用于确认 agy 的认证和模型调用可用。

## 日常使用

### 命令行

`run` 等待调查结束并返回结果。需要后台执行时，使用 `submit`，再用返回的任务 ID 查询或追问：

```sh
agy-delegate-rs submit --cwd /absolute/path/to/repo --task '查找配置加载位置，附文件行号'
agy-delegate-rs wait JOB_ID --seconds 50
agy-delegate-rs followup JOB_ID '继续核查配置覆盖顺序'
```

追问会产生新的任务 ID，并复用原会话。后台任务由独立 worker 执行，MCP 服务关闭后仍继续；`wait` 等待超时不会取消任务。

长任务说明可以用 `--task-file /absolute/path/task.txt` 代替 `--task`。导出完整报告：

```sh
agy-delegate-rs report JOB_ID --output /absolute/path/new-report.txt
```

导出不会覆盖已有文件。更多命令见[运行和清理](INSTALL.md#运行和清理)。

### Codex MCP

| 工具 | 用途 |
| --- | --- |
| `agy_submit` | 提交只读调查，立即返回任务 ID |
| `agy_status` | 查询任务状态和结果 |
| `agy_wait` | 等待任务结果；等待超时后任务继续运行 |
| `agy_followup` | 沿用原会话继续追问，生成新任务 |
| `agy_cancel` | 请求停止任务的本地进程树 |
| `agy_list` | 列出近期任务的简短状态，恢复任务 ID |

提交时应给出明确的调查范围和仓库绝对路径。取得结果后必须检查任务状态：失败、未完成或超时的调查不能当作已完成。详细结果通过 `agy_status` 或 `agy_wait` 获取。

## 配置与认证

认证环境默认继承，也可以加载已有的环境文件：

```sh
agy-delegate-rs --agy-env-file /absolute/path/api.env run --cwd /absolute/path/to/repo --task '查找配置入口'
```

已有环境变量优先于环境文件中的值，环境文件内容不写入任务记录。`--model`、`--agy`、`--codex`、`--state-dir` 等覆盖参数放在子命令之前。

需要配置文件时，先执行 `agy-delegate-rs init`，再编辑它输出的文件路径。默认位于系统用户配置目录下；Linux 通常为 `~/.config/agy-delegate-rs/config.toml`。配置修改后需重新执行 `register`，更新 MCP 使用的配置快照。

遇到环境或沙箱问题时，可运行 `agy-delegate-rs doctor` 检查配置、依赖和实际沙箱读写边界；模型认证仍需通过真实调查验证。

完整配置项、自定义 Codex 目录和手动 MCP 注册方法见[安装指南](INSTALL.md)。自定义 API 若需要协议转换，请单独运行转换服务。

## 沙箱与数据边界

- **只读范围**：项目及普通宿主文件只读。新任务仅允许写入本工具管理的独立会话目录和任务临时目录；工作目录与可写目录重叠时拒绝启动。
- **联网与凭据**：允许联网调用模型。沙箱用于限制文件写入，不提供网络或凭据隔离。
- **启动保障**：agy 的权限自动放行仅在操作系统沙箱内部使用；沙箱启动失败时任务直接失败。
- **会话数据**：同一会话的追问复用 `state_dir/conversations/<root_id>`。首次使用时从默认 `~/.gemini/antigravity-cli` 导入已有的 `settings.json` 和 `antigravity-oauth-token`；后续刷新留在私有目录，原文件不回写。
- **存储限制**：会话目录默认限制为 128,000,000 字节（`profile_bytes`）。这是定时监控，超限会终止任务，并非文件系统硬配额。

状态默认保存在系统用户数据目录的 `agy-delegate-rs` 下，可通过 `--state-dir` 指定。报告、缓存和私有登录文件可能包含敏感数据，请勿提交到版本库。

清理历史前可先预览：

```sh
agy-delegate-rs cleanup --dry-run
agy-delegate-rs cleanup
```

清理后的任务不能再查询或追问；删除会话的最后一条历史引用时，也会删除对应的独立会话目录。旧版使用共享 profile 的任务继续沿用原有数据，清理不会删除用户的共享 `~/.gemini`。清理条件与卸载步骤见[安装指南](INSTALL.md#运行和清理)及[沙箱范围与卸载](INSTALL.md#沙箱范围与卸载)。

## 支持与验证

当前为预览版。Linux、macOS、Windows 的 x86_64 与 ARM64 六种原生 CI 环境均已通过测试和沙箱读写检查。

Linux 已验证真实模型调用、重连、续聊和清理；macOS、Windows 的真实模型调用仍待实机验收。原生 OAuth 登录、令牌刷新和系统钥匙串复用尚未实测。

在源码仓库运行测试：

```sh
cargo test
cargo test --test sandbox native_sandbox_preserves_fixture -- --ignored
```

## 许可证

[MIT](LICENSE) 许可证仅覆盖本项目代码。本项目调用外部安装的 Codex CLI，不捆绑其二进制或 bubblewrap。
