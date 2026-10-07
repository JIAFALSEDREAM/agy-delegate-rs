# agy-delegate-rs

跨平台的只读 agy 调查工具。调用用户已有的 agy，通过 Codex 的操作系统沙箱限制写入。

## 前提

- 安装 Rust 1.88+、agy 和支持命名权限配置的 Codex CLI（本机验证为 0.160.0）。
- agy 已能正常调用模型：原生登录或用户自行配置的兼容接口均可。
- 自定义接口若需要协议转换，转换服务须独立运行；本项目不包含代理。

```sh
cargo build --release
./target/release/agy-delegate-rs run --cwd /path/to/repo --task '梳理任务调度入口，附文件行号'
```

认证环境默认继承。已有环境文件可以通过全局参数 `--agy-env-file /path/to/api.env` 加载；已有环境变量优先，不把密钥复制到任务记录。凭据环境值与持久任务路径须为 UTF-8。用 `--model`、`--agy`、`--codex` 覆盖对应程序或模型。

## 沙箱边界

- 项目及普通宿主文件只读；用户现有 `~/.gemini` 与单个任务临时目录可写。
- 允许联网调用模型；这是防止项目被修改的工具，不是网络或凭据隔离工具。
- 共享 agy 的认证、设置、缓存与会话。不会创建独立账号配置。
- CLI 权限自动放行仅在操作系统沙箱内部使用；沙箱启动失败直接失败，不回退到无沙箱执行。
- 工作目录与可写目录重叠时拒绝启动。
- Linux 由 Codex 使用 bubblewrap/seccomp；macOS 使用系统 Seatbelt；Windows 使用无需 UAC 的受限令牌后端。
- Linux 已验证真实模型读取、写入被拒和返回报告。macOS、Windows 运行能力尚待实机验证。

状态默认保存在用户数据目录的 `agy-delegate-rs` 下，可用 `--state-dir` 指定。缓存、报告可能含私有代码，不应提交到版本库。

后台任务命令为 `submit --cwd PATH --task TEXT`、`status ID`、`wait ID --seconds 50`、`followup ID MESSAGE`、`cancel ID`、`list`。任务保存在 SQLite 中，由独立 worker 执行，调用程序退出后仍继续。`wait` 只等待，不取消；任务期限由 `--timeout` 控制。心跳过期且确认 worker 与沙箱已停止后返回 `orphaned`，不自动重跑；仍持有任务时保持运行状态。追问沿用 agy 会话，禁止同一会话并发运行。当前尚无自动历史清理。

## MCP

运行 `agy-delegate-rs mcp` 启动 stdio 服务，提供 `agy_submit`、`agy_wait`、`agy_status`、`agy_followup`、`agy_cancel`、`agy_list`。任务失败通过 `status` 返回，调用方必须检查状态，不能把失败任务当作已完成调查。

在 Codex 的 `config.toml` 添加（替换程序绝对路径）：

```toml
[mcp_servers.agy_delegate]
command = "/absolute/path/to/agy-delegate-rs"
args = ["mcp"]
```

需要环境文件时，`args = ["--agy-env-file", "/path/to/api.env", "mcp"]`。凭据只进入子进程环境，不写进 Git 或任务记录。

目前是第一阶段：复用用户安装的 Codex CLI，尚未提取或捆绑其底层沙箱；认证模式由 agy 自己处理。官方登录尚未实测，本机验收使用的是已有外部转换服务。Unix worker 异常退出后，可核实身份的残留沙箱仍可取消；若沙箱组长也已退出且身份无法核实，取消会报错并禁止追问，需人工清理。尚未实现独立监督进程。

## 验证

```sh
cargo test
cargo test --test sandbox native_sandbox_preserves_fixture -- --ignored
```

本机验收范围见 [VALIDATION.md](VALIDATION.md)。

MIT 许可证仅覆盖本项目代码。当前调用外部安装的 Codex CLI，不捆绑其二进制或 bubblewrap。
