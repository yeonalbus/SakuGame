# SakuGame 项目结构（PROJECT_TREE）

> 维护者：AI（`DSPlans/` 体系）。目录结构或文件职责发生变化时同步更新本文件（发布流程 §5）。
> 最近更新：2026-11-02（v1.1：两层结构 + 版本管理）

## 一、根目录

```
SakuGame/
├── AGENTS.md            # AI 编码助手的项目级协作约定
├── README.md            # 项目介绍（珱垣维护，AI 只读）
├── CHANGELOG.md         # 版本变更日志（AI 维护）
├── index.html           # Vite 入口 HTML
├── package.json         # 前端依赖与脚本（pnpm）
├── pnpm-lock.yaml       # 前端锁文件
├── vite.config.ts       # Vite 构建配置
├── tsconfig.json        # TypeScript 配置
├── tsconfig.node.json   # Node 侧 TypeScript 配置
├── LICENSE              # 许可证
├── public/              # 静态资源
├── src/                 # Vue 3 前端源码
├── src-tauri/           # Rust 后端源码（Tauri v2）
├── YeonPlans/           # 珱垣手写区：规划、笔记、协作方案（AI 只读）
├── DSPlans/             # AI 文档区：技术规格、方案、诊断、发布文档
├── dist/                # 前端构建产物（不入库）
├── node_modules/        # 依赖目录（不入库）
├── .obsidian/           # 本地 Obsidian 配置（不入库）
└── .vscode/             # 编辑器配置（仅 extensions.json 入库）
```

`DSPlans/` 内部结构见 `DSPlans/readme.md`（Specs / plans / Problems / research / VerNotes / YeonBriefs / YeonPlans-dsedit）。

## 二、前端 `src/`

| 文件 | 职责 |
| --- | --- |
| `main.ts` | 应用入口：创建 Vue 应用并挂载 |
| `App.vue` | 根组件：页面切换（主页 / 搬运 / 详情 / 设置）、全局 toast |
| `api.ts` | Tauri IPC 封装：仓库、档案、版本、可执行文件、黑名单、扫描、缓存池、搬运、启动命令 + 事件（`scan://*` / `transfer://*` / `cache://usage`） |
| `types.ts` | 与 Rust `models.rs` 对应的 TS 类型（snake_case 字段，与 serde 输出一致） |
| `styles.css` | 全局样式（含 v2 进度条、容量环、任务卡片） |
| `vite-env.d.ts` | Vite 类型声明 |
| `components/Sidebar.vue` | 左侧导航（主页 / 搬运 / 设置） |
| `components/GameCard.vue` | 游戏卡片：封面、编号角标、缺失标记、版本数角标、长标题自适应缩字号 |
| `views/HomeView.vue` | 主页卡片墙：列出档案（1 游戏 1 卡），监听扫描完成事件刷新 |
| `views/GameDetailView.vue` | 详情页：版本管理（版本名 / 备注 / 启动项）、按钮态机（搬运 / 排队 / 进度 / 暂停 / 取消 / 恢复 / 重试 / 启动 / 配置启动项 / 压缩包未就绪）、从缓存池移除、单档案重扫、删除档案与版本、名称 / 编号内联编辑、封面更换 |
| `views/TransferView.vue` | 搬运页：按状态分组（搬运中 / 排队中 / 暂停中 / 出错 / 完成），进度 / 速率 / ETA，暂停 / 恢复 / 取消 / 重试 |
| `views/SettingsView.vue` | 设置页：缓存池（路径 / 容量 / 单位 / 环形容量图 / 路径锁定）、仓库管理（盘符 / IP / 主机名三定位）、扫描控制、扫描告警明细、黑名单、关于 |
| `assets/vue.svg` | Vite 模板图标（可清理） |

## 三、后端 `src-tauri/`

| 路径 | 职责 |
| --- | --- |
| `Cargo.toml` / `Cargo.lock` | Rust 依赖与锁文件 |
| `build.rs` | 构建脚本（tauri-build） |
| `tauri.conf.json` | Tauri 应用配置：窗口、构建命令、打包图标 |
| `capabilities/default.json` | 主窗口权限清单（core / opener / dialog） |
| `icons/` | 应用图标（打包用） |
| `src/main.rs` | 程序入口 |
| `src/lib.rs` | 模块声明 + Tauri Builder 装配、命令注册 |
| `src/commands.rs` | Tauri 命令层：仓库 CRUD（含删除预览）、档案 / 版本读写、可执行文件配置、黑名单、扫描控制、缓存池配置、预检、搬运控制、从缓存池移除、启动、版本运行时状态 |
| `src/scanner.rs` | 扫描器：两层枚举（游戏标识层 → 版本标识层）、压缩白名单与分卷聚合、排除名单、根级目录排除、可见性告警 |
| `src/store.rs` | 两层结构幂等落库：游戏 + 版本 + 可执行文件，重扫保护矩阵，黑名单跳过 |
| `src/db.rs` | SQLite 持久化层（WAL、外键、schema v3：v1.1 八表 + `transfer_tasks` / `cache_entries`；检测到 v1 旧库则清空业务表重建） |
| `src/cache.rs` | 缓存池：配置读写与校验（路径不得在仓库根内、有缓存时锁路径）、容量记账（已用 / 预留 / 安全线）、预检、池内路径约定（仓库编号 `R{id}`）、对账（`missing` 标记）、从缓存池移除 |
| `src/transfer.rs` | 搬运机：任务创建与预留、调度线程（全局 32 通道 + 按文件数定档 4/8/16/32）、staging 复制与进度、L1 校验、原子 rename、暂停 / 恢复 / 取消、失败重试、崩溃恢复 |
| `src/models.rs` | 数据模型：两层结构 DTO（`GameRow` / `GameVersionRow` / `ExecutableRow` / `BlocklistRow`）+ 扫描器内部结构 + v2 缓存池 / 搬运 DTO |
| `src/locator.rs` | 仓库定位：盘符 → IP → 主机名可达性探测 |
| `src/state.rs` | 全局状态：数据库句柄、扫描槽、搬运机引擎 |
| `src/error.rs` | 统一错误类型 `AppError`（含 v2 缓存池 / 搬运 / 启动变体，Tauri 命令统一返回 `Result<T, AppError>`） |
| `tests/real_repo_scan.rs` | 真实仓库扫描验收测试（默认 `#[ignore]`，手动触发） |
| `gen/schemas/` | Tauri 生成的权限 schema（不入库） |
| `target/` | Cargo 构建产物（不入库） |

## 四、功能定位索引

| 功能 | 前端 | 后端 |
| --- | --- | --- |
| 仓库配置（定位 / 名称 / 共享名） | `SettingsView.vue` | `commands.rs`、`locator.rs` |
| 扫描建档（开始 / 暂停 / 恢复 / 取消） | `SettingsView.vue` | `scanner.rs`、`store.rs`、`commands.rs` |
| 单档案重扫 | `GameDetailView.vue` | `commands.rs`（`scan_single_game`）、`scanner.rs`（`scan_one_game`） |
| 扫描可见性告警 | `SettingsView.vue` | `scanner.rs`（`ScanWarnings`）、`commands.rs`（`ScanSummary`） |
| 卡片墙 | `HomeView.vue`、`GameCard.vue` | `commands.rs`（`list_games`，1 游戏 1 行） |
| 档案编辑（名称 / 编号 / 封面） | `GameDetailView.vue` | `commands.rs`（`get_game` / `update_game`） |
| 版本管理（版本名 / 备注 / 启动项 / 手动新建版本） | `GameDetailView.vue` | `commands.rs`、`store.rs` |
| 档案 / 版本删除与黑名单 | `GameDetailView.vue`、`SettingsView.vue` | `commands.rs`、`db.rs`（`blocklist` 表） |
| 缓存池配置（路径 / 容量 / 单位 / 容量图） | `SettingsView.vue` | `commands.rs`（`get_cache_config` / `set_cache_config`）、`cache.rs` |
| 容量预检与记账 | `GameDetailView.vue` | `commands.rs`（`preflight_transfer` / `get_cache_usage`）、`cache.rs` |
| 搬运（排队 / 暂停 / 恢复 / 取消 / 重试） | `GameDetailView.vue`、`TransferView.vue` | `transfer.rs`、`commands.rs`、事件 `transfer://progress` / `transfer://state` |
| 从缓存池移除 | `GameDetailView.vue` | `commands.rs`（`remove_cache`）、`cache.rs` |
| 启动游戏 | `GameDetailView.vue` | `commands.rs`（`launch_game`）：池内路径映射，不依赖仓库可达 |
| 仓库删除（连带清理池内文件） | `SettingsView.vue` | `commands.rs`（`get_repository_delete_preview` / `delete_repository`）、`cache.rs` |
| 应用配置（窗口 / 权限 / 图标 / 打包） | — | `tauri.conf.json`、`capabilities/`、`icons/` |

## 五、数据流概览

设置页添加仓库 → 可达性探测（`locator.rs`）→ 开始扫描（`scanner.rs` 两层枚举产出「游戏 + 版本」）→ 幂等落库（`store.rs` → SQLite：`games` / `game_versions` / `game_version_executables` / `game_version_files`，黑名单条目跳过）→ 事件 `scan://progress` / `scan://finished`（含告警明细）→ 主页卡片墙刷新（1 游戏 1 卡）；详情页的「档案重扫」走同一扫描槽，只刷新单个档案。

设置页配置缓存池 → 详情页点「搬运」→ `preflight_transfer`（池配置 / 配额 / 安全线 / 压缩包 / 已缓存）→ `start_transfer` 递归求和 + 预留 + 入队 → 调度线程按 32 通道与文件数档位调入 → 工作线程池写入 `.staging/<task_id>/` → L1 校验 → 原子 rename 到 `池根/R{仓库id}/游戏标识层[/SakuVersion]` → 写 `cache_entries` → 事件 `transfer://progress`（200ms 节流）/ `transfer://state` → 详情页与搬运页同步；「启动」从池内路径拉起，全程不探测仓库；「从缓存池移除」删副本并归还容量。

## 六、数据模型（v2）

| 表 | 含义 |
| --- | --- |
| `games` | 游戏档案（= 仓库根的直接子目录），含形态、名称、编号、封面、当前选中版本 |
| `game_versions` | 版本（= 游戏目录的直接子目录；紧凑型为游戏目录自身），含目录原名、显示名、备注、来源（scan / manual）、状态 |
| `game_version_executables` | 版本内的候选 / 手填可执行文件，`is_selected` 为启动项，`invalid` 表示重扫后已找不到 |
| `game_version_files` | 版本内的压缩包文件（分卷按 `group_key` 聚合）；v2 不支持压缩包搬运，仅保留识别信息 |
| `blocklist` | 删除的档案与版本（`kind` = game / version），重扫时跳过 |
| `transfer_tasks` | 搬运任务（进行中与历史）：仓库编号快照、源 / 池内相对路径、总量、已写、档位、状态、失败清单 |
| `cache_entries` | 缓存记录（每版本一条）：池内相对路径、体积、文件数、`ok` / `missing`（对账标记） |

## 七、维护提示

- 新增 / 删除目录、文件职责变化时更新本文件；发版时按 `DSPlans/VerNotes/RELEASE_PROCESS.md` §5 核对。
- `gen/schemas/` 由 `pnpm tauri dev` / `pnpm tauri build` 自动生成，不纳入版本控制。
