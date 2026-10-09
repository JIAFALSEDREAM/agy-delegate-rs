# 安装与配置

## 前提

- Rust 1.88 或更新版本，以及本平台的 C 编译工具链（Linux 常用 GCC/Clang，macOS 使用 Xcode Command Line Tools，Windows 按所选 Rust 工具链安装相应构建工具）；本项目编译 SQLite。
- 已安装能提供 `agy` 命令、stream-json 接口和 `--gemini_dir` 数据目录参数的 Antigravity CLI，且用户已有的登录或 API 配置能正常调用模型；本项目不安装或代管 agy。
- 支持命名权限配置的 Codex CLI。本机验证版本为 0.160.0；Linux 还需要 bubblewrap 和允许创建用户命名空间的环境。

安装 Codex CLI 可使用 Node.js/npm：

```sh
npm install -g @openai/codex@0.160.0
```

Linux 的 Debian/Ubuntu 系统可安装 bubblewrap：

```sh
sudo apt-get install bubblewrap
```

macOS 由 Codex 使用系统 Seatbelt；Windows 使用 Codex 的 unelevated 后端。当前已在 Linux 验证实际沙箱，Windows/macOS 的 Rust 代码检查已通过，实际沙箱仍需在对应系统验收。

当前模型验收使用用户已有的 API/转换服务，原生 OAuth 登录、系统钥匙串访问和令牌刷新尚未实测。三端都依赖本机 Codex CLI，本项目通过它运行操作系统沙箱。

## 构建与初始化

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
log_bytes = 4000000
profile_bytes = 128000000
timeout = 600
report_chars = 4000
retention_days = 14
max_history_jobs = 1000
# agy_env_file = "/absolute/path/api.env"
# model = "your-model"
# state_dir = "/absolute/path/state"
```

命令行参数优先于文件设置。配置覆盖参数放在子命令之前，例如 `agy-delegate-rs --model your-model --agy-env-file /absolute/path/api.env run --cwd /path/to/repo --task '梳理入口'`；`--config` 和 `--timeout` 也可以放在子命令之后。文件中的相对路径以配置文件所在目录为基准；`agy`、`codex` 的单独程序名从 PATH 查找，指定相对程序路径时写成 `./bin/agy`。命令行路径以启动目录为基准。不支持自动展开 `~` 或路径中的环境变量，请写完整路径。

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

注册会保存当前有效配置（包括命令行覆盖）的快照，后续修改配置需要重新执行 `register`。安装助手不安装或卸载 Cargo 二进制；这仍由 `cargo install/uninstall` 管理。不会创建另一套启动器。

注册归属同时核对当前二进制路径和本 Codex 目录的配置快照。沿用固定 Cargo 安装路径的升级可以重新注册；要移动二进制，请先从原路径执行 `uninstall`，移动后再安装集成。程序不会自动接管来自其它二进制路径的注册。

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

先运行 `agy-delegate-rs uninstall`，取消并确认本状态目录的任务停止、撤销匹配的 MCP 注册并恢复受管理策略，再运行 `cargo uninstall agy-delegate-rs`。无法确认任务停止或策略已编辑时，助手拒绝继续删除集成。该操作不会自动删除配置、任务历史或用户 agy 配置；数据清理由用户单独决定。

任务运行期间每 100 毫秒检查应用日志大小；超限即终止任务，结束后保留的日志会截断并脱敏。独立会话目录在启动前、运行中每轮约 2 秒及停止后检查文件总大小，超出 `profile_bytes` 会终止整个沙箱进程组。扫描不跟随符号链接，无法安全扫描时任务失败。取消、超时或已有执行错误会保留原状态与诊断，并附加结束检查的错误。

这些监控不是文件系统硬配额，检查间隔内可能暂时超过限制。`profile_bytes` 只监控受管独立会话目录，不限制共享 agy 账号目录或其它任务的磁盘总占用。

操作系统不能在期限内确认整个沙箱进程组结束时，任务会失败，tmp/UNVERIFIED_PROCESS 标记会说明原始日志仍保留在私有目录中；此时日志不能视为已脱敏。先确认并停止相关进程，再处理日志和清理。卸载会拒绝无法核验的任务目录。

卸载的取消范围是选中的 state_dir，包括 CLI 任务，即使当前 Codex 配置目录没有 Rust 注册也会停止该任务池。多个 Codex 配置目录共用状态目录时共用任务池和清理策略；需要隔离任务时，请分别配置 state_dir。程序会在关闭检查期间阻止新提交；遇到正在提交或启动的任务时需稍后重试卸载。
