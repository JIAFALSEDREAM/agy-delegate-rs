# 安装与配置

简体中文 | [English](INSTALL.en.md)

## 前提

- 从源码安装需要 Rust 1.88 或更新版本，以及本平台的 C 编译工具链（Linux 常用 GCC/Clang，macOS 使用 Xcode Command Line Tools，Windows 按所选 Rust 工具链安装相应构建工具）；本项目编译 SQLite。npm 和 GitHub Release 预编译包无需这些构建工具。
- 已安装能提供 `agy` 命令、stream-json 接口和 `--gemini_dir` 数据目录参数的 Antigravity CLI，且用户已有的登录或 API 配置能正常调用模型；本项目不安装或代管 agy。
- 支持命名权限配置且沙箱可正常运行的 Codex CLI，本机验证版本为 0.160.0。本工具直接调用 `codex sandbox`，不单独调用或检查系统中的 bubblewrap；具体运行条件由 Codex 的沙箱后端决定，`install` / `doctor` 会实际验证读写边界。

安装 Codex CLI 可使用 Node.js/npm：

```sh
npm install -g @openai/codex@0.160.0
```

Linux 的沙箱运行条件（如用户命名空间权限）需满足当前 Codex 后端要求；macOS 由 Codex 使用系统 Seatbelt，Windows 使用 Codex 的 unelevated 后端。Linux、macOS、Windows 的 x86_64 与 ARM64 共六种原生 CI 环境均已通过测试，并验证项目读取、写入被拒及临时目录可写。

当前模型验收使用用户已有的 API/转换服务，原生 OAuth 登录、系统钥匙串访问和令牌刷新尚未实测。三端都依赖本机 Codex CLI，本项目通过它运行操作系统沙箱。

## 构建与初始化

可以选择以下安装方式：

- GitHub Releases：下载匹配架构的 `.tar.gz` 或 `.zip`，对照 `SHA256SUMS` 校验，解压后将二进制放入 PATH。
- npm：`npm install -g agy-delegate-rs`，需要 Node.js 22.14+，不需要 Rust；入口包通过可选依赖自动安装匹配系统与架构的平台包，只下载一个二进制，不使用安装脚本。不要禁用 optional dependencies；若缺少平台包，使用 `npm install -g agy-delegate-rs --include=optional` 重装。
- crates.io：`cargo install agy-delegate-rs --locked`，需要前述 Rust 和 C 编译工具链。

预编译包提供 Linux x64/arm64（musl 静态链接）、macOS x64/arm64、Windows x64/arm64，共六种组合。三种方式都不会安装 agy 或 Codex CLI；首次集成执行 `agy-delegate-rs install`，成功后新开 Codex 会话。模型认证仍需真实调查验证。

从内置所有二进制的旧 npm 版（0.1.3 及更早）升级到按平台拆包版本时，原生程序路径会变化。**已经执行过 `install` 或 `register` 的用户，应在 npm 升级前先用旧版执行 `uninstall`，升级后再执行 `install`**，并沿用注册时的配置参数：

```sh
agy-delegate-rs uninstall
npm install -g agy-delegate-rs@latest
agy-delegate-rs install
```

如果已经升级且提示存在不属于当前程序的 MCP 注册，请在原 npm 安装前缀中暂时安装 `agy-delegate-rs@0.1.3`，恢复旧路径后按上述顺序迁移。无需删除配置或任务历史；`uninstall` 会先关闭该状态目录中的任务。未注册过 MCP 的用户可以直接安装新版，后续平台包升级保持相同路径。

在仓库目录执行：

```sh
cargo install --path . --locked
agy-delegate-rs init
agy-delegate-rs doctor
```

`cargo install` 把程序放入 Cargo 的 bin 目录，请确保该目录在 PATH 中。`init` 输出配置文件路径，默认使用系统用户配置目录中的 `agy-delegate-rs/config.toml`。Linux 默认是 `~/.config/agy-delegate-rs/config.toml`，受 XDG_CONFIG_HOME 影响。

也可以指定配置文件：

```sh
agy-delegate-rs init --config /absolute/path/config.toml
agy-delegate-rs --config /absolute/path/config.toml doctor
```

`init` 不覆盖已有文件，不生成或复制密钥。需要修改时直接编辑配置。`doctor` 输出 JSON，检查配置、环境文件、程序版本、agy 的公开无界面参数、状态目录及实际沙箱读写边界；有失败项时退出码非零。它不调用模型，也不验证登录、API 连通性、令牌刷新或 `--gemini_dir` 的实际数据写入路径。初始化后用下文的 `run` 命令执行一次实际调查，确认当前 agy 版本和认证可用。

## 配置

默认模板：

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

命令行参数优先于文件设置。配置覆盖参数放在子命令之前，例如 `agy-delegate-rs --model your-model --agy-env-file /absolute/path/api.env run --cwd /path/to/repo --task '梳理入口'`；`--config` 和 `--timeout` 也可以放在子命令之后。文件中的相对路径以配置文件所在目录为基准；`agy`、`codex` 的单独程序名从 PATH 查找，指定相对程序路径时写成 `./bin/agy`。命令行路径以启动目录为基准。不支持自动展开 `~` 或路径中的环境变量，请写完整路径。

`model` 默认是 agy 内置的 `gemini-3.8-flash-high`，即 Gemini 3.8 Flash (High)。新调查及续聊均显式传入该值，不沿用用户在 agy 中选中的模型。没有配置文件、配置中省略 `model`、旧快照中模型为 `null` 时，也使用此默认值；已有的非空模型配置保持优先。模型不可用时任务失败，不自动降级或切换。

`effort` 是可选的思考等级，支持 `low`、`medium`、`high`、`xhigh`、`max`，具体可用等级由 agy 和所选模型决定。默认模型名已经包含 High，无需另设 `effort`。agy 1.3.2 支持裸名与等级分开指定：`model = "gemini-3.8-flash"` 配合 `effort = "high"`，或命令行 `--model gemini-3.8-flash --effort high`；若模型名已有等级后缀，单独设置的等级必须与后缀一致。自定义接口或协议转换服务应按 `agy models` 和接口实际接受的模型标识覆盖配置，本工具不自动改写模型名。

`timeout` 为任务执行期限，范围 1–7200 秒；`report_chars` 限制后台任务查询返回的报告长度，范围 256–6000；`retention_days` 范围 1–365；`max_history_jobs` 范围 10–10000。后两项用于手动清理和 worker 保存任务结果后的自动清理。`agy_args` 放在托管的 agy 参数之前，例如 `agy = "node"` 配合 `agy_args = ["/absolute/path/agy.js"]`；也可重复传入 `--agy-arg`。参数中不要放凭据。`log_bytes` 范围 4096–32000000，限制单个 `agy.log` 及保留的 stderr；协议输出仍有独立的 4 MB 上限。`profile_bytes` 范围 1000000–1000000000，默认 128000000 字节，限制每个独立会话目录内的文件总大小，也可在子命令前传 `--profile-bytes`。

Rust 版不自动导入原 Python 项目的 `config.json` 或任务数据库；并行使用时保留各自独立的状态目录。需手动迁移已有程序路径、模型和环境文件路径，不要复制密钥到 TOML 配置中。

认证环境默认继承，环境变量优先于 `agy_env_file` 中的值。环境文件只引用用户已有的配置，不写入项目配置中的密钥字段；未知配置项会报错。凭据环境值及持久任务路径须为 UTF-8。自定义 API 若需要协议转换，应独立运行转换服务，本项目不提供它。

新任务通过 `--gemini_dir` 使用 `state_dir/conversations/<root_id>`，同一会话的后续任务复用该目录。首次需要时，仅从默认 `~/.gemini/antigravity-cli` 复制已有的 `settings.json` 和 `antigravity-oauth-token`；不会复制宿主会话、插件或单独的 MCP 配置文件。已存在的私有登录文件不会被覆盖，刷新结果留在私有目录，原文件不回写。环境文件中的凭据通过环境传入，程序不会自动导入宿主的 `api.env`。独立目录中的登录文件、模型缓存与调查历史按私有数据保存，请勿提交到版本库。

## 注册 Codex MCP

初始化后可以使用安装助手：

```sh
agy-delegate-rs install
# 只注册 MCP，不添加策略：
agy-delegate-rs register
# 指定 Codex 配置目录（也用于隔离验收）：
agy-delegate-rs install --codex-home /absolute/path/codex-home
```

`install` 先运行离线 doctor，通过后注册 `agy-delegate-rs` MCP 并写入受管理的 AGENTS 策略。已有 Python 注册或策略时会拒绝覆盖；确认 Rust 版可用后，再手动移除旧的注册与受管理策略。外部协议转换服务可以继续保留。

注册会保存当前有效配置（包括命令行覆盖）的快照，后续修改配置需要重新执行 `register`。安装助手管理配置与 MCP 集成；程序文件通过所选的 npm、Cargo 或 GitHub Release 安装渠道管理。

注册归属同时核对当前二进制路径和本 Codex 目录的配置快照。沿用固定安装路径的升级可以重新注册；要移动二进制或切换安装渠道，请先从原路径执行 `uninstall`，移动后再安装集成。程序不会自动接管来自其它二进制路径的注册。

安装、注册和卸载会互斥本工具的操作，并在关闭任务后重新核对注册归属。Codex 自身的配置写入没有共享锁或条件更新接口；操作期间请勿同时运行其它 `codex mcp add/remove` 或手动编辑同一配置。外部并发写入仍可能互相覆盖。

规则备份和恢复使用文件同步与原子替换；Unix 还同步父目录，按备份、规则、备份删除的顺序持久化。Windows 支持进程中断后的恢复，但不承诺断电时的目录写入持久性。

也可以手动在 Codex 的 `config.toml` 添加：

```toml
[mcp_servers.agy-delegate-rs]
command = "/absolute/path/agy-delegate-rs"
args = ["--config", "/absolute/path/config.toml", "mcp"]
```

Windows 路径可以使用 TOML 单引号。使用默认配置时 `args = ["mcp"]` 即可。重启或重新加载 Codex 的 MCP 服务后生效。

提供 `agy_submit`、`agy_status`、`agy_wait`、`agy_followup`、`agy_cancel`、`agy_list`。列表只返回简短状态；详细结果通过 status/wait 查询。调用方必须检查状态，`incomplete` 或失败不能当作完成。助手卸载只删除匹配本程序与配置快照的注册，并校验受管理策略未被编辑，保留其它设置和规则。

## 运行和清理

```sh
agy-delegate-rs run --cwd /path/to/repo --task '梳理入口，附文件行号'
agy-delegate-rs submit --cwd /path/to/repo --task '梳理入口，附文件行号'
agy-delegate-rs wait JOB_ID --seconds 50
agy-delegate-rs followup JOB_ID '继续核查调用链'
agy-delegate-rs submit --cwd /path/to/repo --task-file /path/to/task.txt
agy-delegate-rs followup JOB_ID --task-file /path/to/followup.txt
agy-delegate-rs report JOB_ID --output /path/to/new-report.txt
agy-delegate-rs cleanup --dry-run
agy-delegate-rs cleanup
```

`--task` 与 `--task-file` 二选一；`--task-file -` 从 stdin 读取 UTF-8。文件最多 96000 字节、任务最多 24000 个字符。`report` 读取完整存储结果，不受查询报告长度限制，导出时不覆盖已有文件。

后台任务由独立 worker 执行，MCP 服务关闭后仍继续；`wait` 超时不取消任务。状态默认在系统用户数据目录中，可用 `state_dir` 配置或 `--state-dir` 指定。

清理按任务最后更新时间判断过期，并保留最近的历史条数；任一条件达到即可选中已结束任务。`--dry-run` 只预览，检查 `selected_ids` 后再执行实际清理。运行中、孤儿任务，以及同一会话仍有活跃或未确认停止任务的历史都会保留；持有所有权锁或仍有沙箱进程的任务也跳过。

实际清理删除本工具的任务数据库记录和日志目录。清理后的任务 ID 不能再查询或追问；同一会话还有其它历史记录时，独立目录仍保留，可以通过未删除的记录追问。删除最后一条历史引用时一并删除对应的受管会话目录，结果通过 `deleted_conversation_ids` 返回；用户的共享 `~/.gemini` 和原登录文件保留。

同步 `run` 也使用任务记录和 worker，参与相同清理；Ctrl-C 请求取消任务，清理在已保存结果的 worker 中进行，不阻塞提交。同步结果中的 `cleanup_status` 可能仍为 `pending`；最终清理结果可通过 `status JOB_ID` 查询。旧版本未纳入任务记录的 `run-*` 目录仍保留，需确认无进程使用后自行处理。升级前使用共享 profile 的旧任务继续沿用共享数据，清理只删除其任务记录和日志，不删除共享 agy 会话。没有历史记录可确认归属的独立目录也会保留。

清理先将任务标记为 `purging`，再删除日志和可清理的独立会话目录；在此期间不能追问同一会话。文件删除失败会返回失败项并保留任务记录，处理原因后可重试。如果会话目录删除失败，即使已删掉一部分数据也保留 `purging`，阻止恢复损坏的会话。清理进程中断后可再次运行 `cleanup` 完成恢复。过期和条数限制只作用于可安全清理的历史；被锁定或会话仍活跃的记录不占保留条数。

同一状态目录只允许一个实际清理进程；同时启动的第二个进程返回 `cleanup_active` 跳过结果。日志根目录中的 `.cleanup.lock` 会保留以维持进程间锁，请勿在工具运行期间手动删除它。检测到日志或会话目录被替换、无法核验安全删除时，会保留任务记录并报告跳过或错误。

## 沙箱范围与卸载

新任务中的项目与普通宿主文件只读；本工具管理的独立会话目录和任务临时目录可写，并允许联网。旧版共享 profile 的任务仍允许写入宿主 `~/.gemini`。当前方案用于保护项目写入，不提供账号、凭据或网络隔离。工作目录与可写目录重叠时拒绝启动；沙箱启动失败时拒绝执行，不回退为无沙箱运行。

Unix worker 与沙箱组长均已退出、但仍有子进程存活且身份无法核实时，取消会报错并阻止追问，需要人工清理；当前没有独立监督进程。

先运行 `agy-delegate-rs uninstall`，取消并确认本状态目录的任务停止、撤销匹配的 MCP 注册并恢复受管理策略，再卸载程序：npm 安装使用 `npm uninstall -g agy-delegate-rs`，Cargo 安装使用 `cargo uninstall agy-delegate-rs`，GitHub Release 安装则删除自行放置的二进制。无法确认任务停止或策略已编辑时，助手拒绝继续删除集成。该操作不会自动删除配置、任务历史或用户 agy 配置；数据清理由用户单独决定。

任务运行期间每 100 毫秒检查应用日志大小；超限即终止任务，结束后保留的日志会截断并脱敏。独立会话目录在启动前、运行中每轮约 2 秒及停止后检查文件总大小，超出 `profile_bytes` 会终止整个沙箱进程组。扫描不跟随符号链接，无法安全扫描时任务失败。取消、超时或已有执行错误会保留原状态与诊断，并附加结束检查的错误。

这些监控不是文件系统硬配额，检查间隔内可能暂时超过限制。`profile_bytes` 只监控受管独立会话目录，不限制共享 agy 账号目录或其它任务的磁盘总占用。

操作系统不能在期限内确认整个沙箱进程组结束时，任务会失败，tmp/UNVERIFIED_PROCESS 标记会说明原始日志仍保留在私有目录中；此时日志不能视为已脱敏。先确认并停止相关进程，再处理日志和清理。卸载会拒绝无法核验的任务目录。

卸载的取消范围是选中的 state_dir，包括 CLI 任务，即使当前 Codex 配置目录没有 Rust 注册也会停止该任务池。多个 Codex 配置目录共用状态目录时共用任务池和清理策略；需要隔离任务时，请分别配置 state_dir。程序会在关闭检查期间阻止新提交；遇到正在提交或启动的任务时需稍后重试卸载。

## 维护者发版

`.github/workflows/release.yml` 仅由推送 `v*.*.*` tag 触发，进一步限制为稳定版本 `vMAJOR.MINOR.PATCH`。tag 指向的提交必须在仓库默认主分支历史中，且 tag、`Cargo.toml` 和 `npm/package.json` 版本一致；不符合时终止，不发布。

首次发版前完成一次性设置：

1. 将仓库推到 `JIAFALSEDREAM/agy-delegate-rs`，默认主分支设为 `main`，启用 Actions。
2. 在仓库 Actions Secrets 中添加 `CARGO_REGISTRY_TOKEN`，需有发布 `agy-delegate-rs` 的权限。npm 不使用这个 token，也无需 `NPM_TOKEN`。
3. npm 的入口包 `agy-delegate-rs` 和六个平台包 `agy-delegate-rs-{linux,darwin,win32}-{x64,arm64}` 必须由你的账号持有。不存在的平台包需首次手动发布：从 GitHub Release 下载对应的 `PACKAGE-VERSION.tgz`，本地登录 npm 后执行 `npm publish ./PACKAGE-VERSION.tgz --access public`。先发六个平台包，再发入口包；已有包无需重新引导。此时 workflow 的 npm job 可能因尚未配置可信发布失败，完成配置后重跑该 job。
4. 为每个包分别配置 Trusted Publisher：在 npm 包设置中选择 GitHub Actions，填写 owner `JIAFALSEDREAM`、仓库名 `agy-delegate-rs`、workflow 文件名 `release.yml`，不填 Environment。允许直接 `npm publish`；新配置默认的 staged publishing 权限不代替此项。当前 npm 文档要求首次成功使用新配置在 2 天内完成，超时需重建配置。

npm 11.15+ 也可在首次发布后通过 CLI 配置可信发布（按提示在浏览器完成账号 2FA）。下面以入口包为例，六个平台包分别替换包名执行：

```sh
npm trust github agy-delegate-rs --repo JIAFALSEDREAM/agy-delegate-rs --file release.yml --allow-publish --yes
```

Cargo/npm 元数据中的仓库地址必须与 Actions 的实际仓库一致，发版前会核对。版本与源代码保持 tag 内容，crate 打包要求工作区干净。本地未提交的验收文档不会进入 crate 或 npm 包。

每次发版修改 Cargo/npm 两处版本、入口包 `optionalDependencies` 中的六个平台依赖版本，并更新 `Cargo.lock` 中的本包版本。六个平台包的元数据由 CI 自动生成，无需分别维护。提交到主分支后打 tag 并推送，例如版本均为 `0.1.3` 时：

```sh
git tag -a v0.1.3 -m 'Release v0.1.3'
git push origin v0.1.3
```

流程先验证，并在 Linux、macOS、Windows 的 x86_64 与 ARM64 六种原生环境构建、执行测试与原生沙箱检查，生成六个二进制压缩包、七个 npm 安装包和 SHA-256 清单；全部构建通过后发布 GitHub Release，再分别发布 npm 和 crate。六个平台包由脚本自动生成、统一版本，npm job 先逐一发布并验证平台包，全部成功后才发布入口包；CI 负责维护，无需手工编辑七套元数据。跨平台的模型认证仍需实机验收，CI 不会调用模型。

三个渠道不是原子事务。部分渠道失败时，在 Actions 重跑失败的 job；npm 和 crate 若已有相同版本，会比较现有包的完整性摘要，相同才跳过，不会覆盖不同内容。已公开的 GitHub Release 会校验并复用原资产，npm 内容不匹配时拒绝继续；只允许替换尚未公开的草稿资产。首次 npm 手动发布后也应重跑同一 job，以复用原安装包。不要删除已发布版本的 tag 后重复发不同内容；修复并提升版本再发。
