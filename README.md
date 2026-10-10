# agy-delegate-rs

面向 Linux、macOS 和 Windows 的只读 agy 调查工具。调用用户已有的 agy，三端统一通过 Codex 的操作系统沙箱限制写入。当前为预览版：Linux 已完成原生运行验收，macOS、Windows 已通过类型检查，仍待实机验收。

安装、配置、MCP 注册、环境诊断与历史清理见 [安装指南](INSTALL.md)。

发版后支持 GitHub Release 预编译包、`npm install -g agy-delegate-rs` 和 `cargo install agy-delegate-rs --locked`。GitHub Release 和 npm 包覆盖 Linux、macOS、Windows 的 x86_64 与 ARM64，共六种组合；npm 包内附所有二进制，无需 Rust 或安装时下载。仍需自行安装 agy、Codex CLI 和对应沙箱依赖。当前从源码安装：

```sh
cargo install --path . --locked
agy-delegate-rs init
agy-delegate-rs doctor
agy-delegate-rs cleanup --dry-run
```

## 前提

- 从源码安装需要 Rust 1.88+ 和本平台的 C 编译工具链；三端都需要 agy 和支持命名权限配置的 Codex CLI（本机验证为 0.160.0）。
- agy 须支持 stream-json 和 `--gemini_dir` 数据目录参数。
- agy 已能正常调用模型：原生登录或用户自行配置的兼容接口均可。
- 自定义接口若需要协议转换，转换服务须独立运行；本项目不包含代理。

```sh
cargo build --release
./target/release/agy-delegate-rs run --cwd /path/to/repo --task '梳理任务调度入口，附文件行号'
```

认证环境默认继承。已有环境文件可在子命令前通过 `--agy-env-file /path/to/api.env` 加载；已有环境变量优先，环境文件内容不写入任务记录。`--model`、`--agy`、`--codex` 等配置覆盖参数也写在子命令前。

真实模型调用、重连、续聊及清理已通过现有 API/转换服务验证；原生 OAuth 登录和刷新尚未实测。`doctor` 不调用模型，安装后仍需运行一条实际调查确认认证可用。

## 沙箱边界

- 项目及普通宿主文件只读；新任务只允许写入本工具管理的独立会话目录和单个任务临时目录。
- 允许联网调用模型；这是防止项目被修改的工具，不是网络或凭据隔离工具。
- 同一会话的追问复用独立目录。首次使用时从用户默认的 `~/.gemini/antigravity-cli` 导入已有 `settings.json` 和 `antigravity-oauth-token`，后续保留私有目录中的刷新结果，原文件不回写；系统钥匙串的登录复用仍需实机验收。
- CLI 权限自动放行仅在操作系统沙箱内部使用；沙箱启动失败直接失败，不回退到无沙箱执行。
- 工作目录与可写目录重叠时拒绝启动。
- Linux 使用 Codex 的 Linux 沙箱（本机使用 bubblewrap/seccomp）；macOS 使用系统 Seatbelt；Windows 指定 Codex 的 `unelevated` 模式。
- Linux 已验证真实模型读取、写入被拒和返回报告。macOS、Windows 运行能力尚待实机验证。

状态默认保存在用户数据目录的 `agy-delegate-rs` 下，可用 `--state-dir` 指定。缓存、报告可能含私有代码，不应提交到版本库。

独立会话目录位于状态目录的 `conversations/<root_id>`。`profile_bytes` 默认 128000000 字节，监控整个会话目录内的文件总大小，超限终止任务；这是定时监控而非文件系统硬配额。自动或手动清理最后一条历史引用时才删除该目录，仍有记录可供追问时保留。私有目录中的登录文件和会话数据也不应提交到版本库。

升级前已使用共享 profile 的旧任务继续沿用其原有数据；清理不会删除用户的共享 `~/.gemini`。

## 验证

```sh
cargo test
cargo test --test sandbox native_sandbox_preserves_fixture -- --ignored
```

MIT 许可证仅覆盖本项目代码。当前调用外部安装的 Codex CLI，不捆绑其二进制或 bubblewrap。
