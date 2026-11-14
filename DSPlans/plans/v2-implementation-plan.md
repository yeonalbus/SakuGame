# MVP v2 施工方案

> **依据**：[MVPv2设计规划_方案3.0.md](../../YeonPlans/版本规划/MVP%20v2/MVPv2设计规划_方案3.0.md)（产品口径）、[MVPv2技术规格.md](../Specs/MVPv2技术规格.md)（数据结构／DDL／算法／IPC／测试）
> **状态**：**已完成**（P0–P7 全部落地；`cargo test` 42 项通过，`vue-tsc --noEmit` 通过）。本文件保留为施工路径记录。
> **总原则**：v1.1 的结构不动（扫描／档案／版本／黑名单保持原样），v2 是在其上**追加**三层——缓存池 → 搬运机 → 启动。不做破坏性迁移，只追加表。

---

## 1. 现状与改动面

### 1.1 现有代码地图

| 文件 | 现状 | v2 处置 |
| --- | --- | --- |
| `src-tauri/src/db.rs` | schema v2，8 张表 | **追加**：`SCHEMA_VERSION=3`，加 `transfer_tasks` / `cache_entries` 两表；`DROP_V1_BUSINESS_TABLES` 保留不动 |
| `src-tauri/src/models.rs` | v1.1 DTO | **追加**：`CacheConfig` / `CacheUsage` / `TransferStatus` / `TransferTaskView` / `TransferPreflight` / `TransferProgressEvent` |
| `src-tauri/src/error.rs` | 13 个变体 | **追加** 11 个变体（见技术规格 §3） |
| `src-tauri/src/cache.rs` | — | **新建**：配置读写与校验、容量记账（`CapacityLedger`）、预检、对账、清理 |
| `src-tauri/src/transfer.rs` | — | **新建**：任务创建、调度器、工作线程池、复制／进度／校验／rename、暂停／恢复／取消 |
| `src-tauri/src/state.rs` | `AppState { db, scan }` | **扩展**：加 `transfer: Arc<TransferSlot>` |
| `src-tauri/src/commands.rs` | 24 个命令 | **追加** 12 个命令；**增强** `delete_repository` |
| `src-tauri/src/lib.rs` | 注册 24 命令 | 注册全部新命令 |
| `src/types.ts` / `src/api.ts` | v1.1 封装 | **追加** v2 类型与命令封装、3 个事件监听 |
| `src/App.vue` | 页面切换（主页/详情/设置） | 加「搬运」页路由与导航 |
| `src/components/Sidebar.vue` | 主页/设置 | 加「搬运」入口，变为 主页/搬运/设置 |
| `src/views/TransferView.vue` | — | **新建**：按状态分组的任务队列页 |
| `src/views/GameDetailView.vue` | 版本管理 + 启动占位 | 按钮态机（搬运/进度/暂停/恢复/重试/启动/配置启动项/未就绪）、版本条目「从缓存池移除」 |
| `src/views/SettingsView.vue` | 仓库管理 | 缓存池配置区（路径/容量/单位/环形图） |
| `src/styles.css` | 现有 | 追加进度条／容量环／任务卡片样式 |

### 1.2 不在本次范围

- 压缩包解压（版本 `game_type='archive'` 只提示不搬运）
- 变更组／补丁注入／存档注入／回写
- 反向搬运（本地 → 仓库）
- 自动淘汰、进程监控
- 多缓存池

---

## 2. 阶段划分

> 每阶段 `cargo test`（后端）或 `pnpm exec vue-tsc --noEmit`（前端）通过 + 一条 commit。P0–P3 后端连续推进（中途编译不过属正常，P3 结束整体可用）。

### P0 地基（schema v3 + 配置读写的校验）

1. `db.rs`：`SCHEMA_VERSION=3`；追加 `transfer_tasks` / `cache_entries` DDL 与索引（见技术规格 §4）
2. `error.rs`：11 个新变体 + `kind` 映射
3. `cache.rs`：`CacheConfig` 读写（settings 键）、`set_cache_config` 的 6 步校验（路径可建 / 不在仓库根内 / 路径锁定 / 配额正整数 / 缩容 / 扩容）
4. `commands.rs`：`get_cache_config` / `set_cache_config`；`lib.rs` 注册

**完成判定**：`cargo test` 通过；`get_cache_config` 返回默认 10 GiB + 未配置。

### P1 容量记账与预检

1. `cache.rs`：`CapacityLedger`（内存已用 + DB 兜底）、`CacheUsage` 计算、`used` 求和（cache_entries(ok) + 在途任务 total）
2. `preflight_transfer` 六条判定（顺序见技术规格 §5.3），纯读不预留
3. `cache://usage` 事件
4. 命令：`get_cache_usage` / `preflight_transfer`

**完成判定**：单元测试覆盖 T2-01~T2-08。

### P2 搬运核心（单任务）

1. `transfer.rs`：任务创建（递归求 total_bytes/files、预留、入队）
2. staging 创建 → 文件枚举 → 复制（1 MiB 块、进度累加）→ L1 校验 → rename → 写 `cache_entries`
3. 进度事件节流 200 ms + ETA 10 s 窗口
4. 失败重试 2 次 + `failed_files`
5. 目标已存在 → error 不覆盖

**完成判定**：单任务可完整搬运；T2-10/11/14/15 通过。

### P3 调度与生命周期

1. 调度线程 + 32 通道 + 按文件数定档（4/8/16/32）
2. 暂停（文件边界）/ 恢复（跳过已存在）/ 取消（删 staging + 删任务行）
3. 崩溃恢复：启动时 `running` → `paused`
4. 命令：`start_transfer` / `list_transfers` / `get_version_transfer` / `pause_transfer` / `resume_transfer` / `cancel_transfer` / `retry_transfer`
5. 事件：`transfer://progress` / `transfer://state`

**完成判定**：T2-12/13/16/17/18/19 通过。

### P4 对账与清理

1. 启动对账（`cache_entries` 目录存在性 → `missing`）
2. 详情页读取时校验
3. `remove_cache`：任务冲突拒绝 → 删目录 → 删记录 → 归还容量
4. 命令：`remove_cache`

**完成判定**：T2-30~T2-33 通过。

### P5 启动

1. `launch_game`：版本 → 选中 exe → 缓存记录 → 路径映射 → `Command` 拉起
2. 全程不探测仓库
3. 长路径检查（>260 提示）

**完成判定**：T2-40~T2-43 通过（离线场景用不可达 locator 模拟）。

### P6 仓库编号与删除

1. `repo_no(id)` = `R{id:02}`；`dest_rel` 生成（标准型 / `SakuCompact`）
2. `delete_repository` 增强：有在途任务拒绝 → 删 `<池根>/R{id}` → 删仓库
3. 命令：`get_repository_delete_preview`

**完成判定**：T2-44~T2-46 通过。

### P7 前端

1. `types.ts` / `api.ts`：v2 类型、命令、3 个事件
2. `App.vue` + `Sidebar.vue`：「搬运」页
3. `GameDetailView.vue`：按钮态机 + 版本条目移除按钮 + 压缩包未就绪
4. `TransferView.vue`：状态分组 + 进度／速率／ETA + 操作
5. `SettingsView.vue`：缓存池路径／容量／单位／环形图 + 路径锁定提示
6. `styles.css`：进度条、容量环、任务卡片

**完成判定**：`pnpm exec vue-tsc --noEmit` 通过；手工走查全链路。

---

## 3. 提交切分

| # | 提交 | 内容 |
| --- | --- | --- |
| 1 | `feat(cache): 缓存池配置与容量记账` | P0–P1 |
| 2 | `feat(transfer): 搬运机` | P2–P3 |
| 3 | `feat(cache): 对账、清理与启动` | P4–P5 |
| 4 | `feat(repo): 仓库编号分层与删除清理` | P6 |
| 5 | `feat(ui): 搬运页与缓存池界面` | P7 |

---

## 4. 验证方式

- 后端：`cargo test --manifest-path src-tauri/Cargo.toml`（真实仓库测试 `#[ignore]` 跳过）
- 前端：`pnpm exec vue-tsc --noEmit`
- 端到端：临时目录模拟仓库 + 缓存池，走「配置 → 搬运 → 暂停/恢复 → 取消 → 启动 → 清理 → 删仓库」
