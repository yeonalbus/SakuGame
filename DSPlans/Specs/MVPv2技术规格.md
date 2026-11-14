# MVP v2 技术规格

> **依据**：[MVPv2设计规划_方案3.0.md](../../YeonPlans/版本规划/MVP%20v2/MVPv2设计规划_方案3.0.md)（产品行为）+ [MVPv2设计规划_定义3.0.md](../../YeonPlans/版本规划/MVP%20v2/MVPv2设计规划_定义3.0.md)（对象定义）+ 现有 v1.1 实现（`src-tauri/src/`）
> **状态**：待施工（产品口径已收口；本文件只定"怎么做"，不改产品行为）
> **一句话**：在 v1.1 扫描建档之上加三层——**缓存池（配置 + 容量记账）→ 搬运机（队列 + staging + 校验）→ 启动（池内拉起）**；压缩包形态降级为只提示不搬运。

---

## 1. 本版范围与不做

### 1.1 做

| 模块 | 内容 |
| --- | --- |
| 缓存池配置 | 路径（单池）、容量（整数 MB/GB，默认 10 GiB）、环形容量图数据 |
| 容量记账 | 已用/预留/配额/安全线；搬运预检、扩缩容校验 |
| 搬运机 | 全局 32 通道队列、按文件数自动定档、staging 写入、暂停/恢复/取消、L1 校验、原子 rename、失败重试、崩溃恢复 |
| 缓存记录 | 记录 + 对账（外部删除 → 标失效） |
| 清理 | 从缓存池移除（删池内目录 + 清记录 + 容量归还） |
| 启动 | 从池内路径拉起 exe；不依赖仓库可达 |
| 仓库 | 仓库编号分层（`R01`）；删除仓库时连带清理池内文件 |
| UI | 详情页按钮态机、搬运页、设置页缓存管理 |

### 1.2 不做（v2 边界）

- **不做压缩包解压**：`game_type = archive` 的版本在详情页显示「未就绪，请解压后重扫」，搬运按钮禁用
- **不做变更组**：不注入补丁、不注入存档；不写回仓库
- **不做反向搬运**：「本地裸放游戏 → 仓库」不在 v2 范围
- **不做自动淘汰/自动释放空间**（v3）
- **不做进程监控**：只负责拉起，不负责关闭
- **不做多缓存池**：仅一个

---

## 2. 名词与结构

### 2.1 三层路径映射

```
仓库（源）                              缓存池（目标）
<仓库根>/<游戏标识层>/<版本标识层>  →    <池根>/R{id:02}/<游戏标识层>/<版本标识层>
<仓库根>/<游戏标识层>（紧凑型）      →    <池根>/R{id:02}/<游戏标识层>/SakuCompact
```

- **仓库根**：v1.1 已有的 `locator` 解析结果（`resolve_repository`）
- **仓库编号**：`R` + 数据库自增 id 补零两位（`R01`…`R99`，id ≥ 100 时为 `R100` 起，**不循环**）
- **搬运对象**：标准型 = 版本标识层目录；紧凑型 = 游戏标识层目录的内容（落到 `SakuCompact` 下）
- **SakuCompact**：紧凑型的版本层占位符，固定字符串，不参与业务语义

### 2.2 术语

| 术语 | 含义 |
| --- | --- |
| 配额 `quota` | 缓存池逻辑容量上限（不预分配磁盘），默认 10 GiB |
| 已用 `used` | 已完成缓存的字节和 + 在途任务（排队/搬运/暂停）的**总量字节和** |
| 预留 | 任务通过预检那一刻起，其 `total_bytes` 计入已用；取消/失败即归还 |
| 安全线 `safety` | `池所在卷总容量 × 10%`，任何判盘都不得越过 |
| 可用空间 | `卷剩余 − 安全线` |
| 任务身份 | `(仓库编号, 游戏标识层, 版本标识层)` —— 三者唯一确定一次搬运 |
| staging | `<池根>/.staging/<task_id>/`，任务独占的临时区，完成后 rename 到最终路径 |

---

## 3. 数据结构（Rust 侧）

新增模块：`cache.rs`（配置/容量/记录）、`transfer.rs`（搬运机）。`models.rs` 追加 DTO。

```rust
/// 缓存池配置（持久化在 settings 表：cache_pool_root / cache_pool_quota_bytes）
pub struct CacheConfig {
    pub root: Option<String>,   // 绝对路径；None = 未配置
    pub quota_bytes: i64,       // 逻辑配额
}

/// 容量快照：前端展示 + 预检计算共用
pub struct CacheUsage {
    pub quota_bytes: i64,
    pub used_bytes: i64,        // 已完成 + 在途（含预留）
    pub free_bytes: i64,        // quota - used
    pub disk_total_bytes: i64,  // 池所在卷总容量
    pub disk_free_bytes: i64,   // 池所在卷剩余
    pub safety_bytes: i64,      // disk_total * 10%
    pub available_bytes: i64,   // disk_free - safety（可为负）
}

pub enum TransferStatus { Queued, Running, Paused, Error, Done }

/// 预检结果（前端拿 reason 显示提示框）
pub struct TransferPreflight {
    pub ok: bool,
    pub reason: Option<PreflightReason>, // PoolNotConfigured / QuotaExceeded / DiskExceeded / ArchiveUnsupported / InvalidVersion / AlreadyCached
    pub required_bytes: i64,
    pub available_quota_bytes: i64,
    pub available_disk_bytes: i64,
}

/// 任务视图（IPC 返回）
pub struct TransferTaskView {
    pub task_id: i64,
    pub repository_id: i64,
    pub game_id: i64,
    pub version_id: i64,
    pub repo_no: String,          // "R01"（建任务时快照，仓库改名不影响）
    pub game_name: String,
    pub version_label: String,
    pub total_bytes: i64,
    pub written_bytes: i64,
    pub total_files: u32,
    pub written_files: u32,
    pub status: TransferStatus,
    pub speed_bps: u64,           // 最近 10s 窗口
    pub eta_secs: Option<u64>,
    pub error: Option<String>,
    pub failed_files: Vec<String>,// 搬运完成但失败的文件（相对路径）
}

/// 事件负载（节流 200ms）
pub struct TransferProgressEvent {
    pub task_id: i64,
    pub written_bytes: i64,
    pub total_bytes: i64,
    pub speed_bps: u64,
    pub eta_secs: Option<u64>,
    pub status: TransferStatus,
}
```

**错误新增**（`error.rs`）：`CacheNotConfigured`、`QuotaExceeded { required, available }`、`DiskExceeded { required, available }`、`TransferNotFound`、`TransferBusy`、`TransferNotResumable`、`CacheRemoveBusy`、`ArchiveUnsupported`、`PoolPathInsideRepository`、`CachePathLocked`（有缓存/任务时改路径）、`LaunchNoExecutable`、`LaunchFailed(String)`。

---

## 4. SQLite 表结构（`SCHEMA_VERSION = 3`）

沿用 v1.1 的追加式迁移：`db.rs` 中 `SCHEMA_VERSION` 改 3，`SCHEMA` 常量追加以下表；`settings.schema_version` 更新为 3。**v1.1 的 `DROP_V1_BUSINESS_TABLES` 逻辑保留不动**，v2 不做破坏性迁移（只加表）。

```sql
-- 搬运任务（持久化，支撑崩溃恢复与队列重建）
CREATE TABLE IF NOT EXISTS transfer_tasks (
    id             INTEGER PRIMARY KEY AUTOINCREMENT,
    repository_id  INTEGER NOT NULL REFERENCES repositories(id) ON DELETE CASCADE,
    game_id        INTEGER NOT NULL REFERENCES games(id) ON DELETE CASCADE,
    version_id     INTEGER NOT NULL REFERENCES game_versions(id) ON DELETE CASCADE,
    repo_no        TEXT    NOT NULL,              -- R01（快照）
    source_rel     TEXT    NOT NULL,              -- 版本目录相对仓库根
    dest_rel       TEXT    NOT NULL,              -- 池内相对路径（不含池根，如 R01/RJ123/RJV1）
    total_bytes    INTEGER NOT NULL,
    total_files    INTEGER NOT NULL,
    written_bytes  INTEGER NOT NULL DEFAULT 0,
    written_files  INTEGER NOT NULL DEFAULT 0,
    concurrency    INTEGER NOT NULL,              -- 档位 4/8/16/32
    status         TEXT    NOT NULL DEFAULT 'queued'
                   CHECK (status IN ('queued','running','paused','error','done','cancelled')),
    error_message  TEXT,
    failed_files   TEXT,                          -- JSON 数组：相对路径
    created_at     TEXT    NOT NULL,
    started_at     TEXT,
    finished_at    TEXT,
    updated_at     TEXT    NOT NULL,
    UNIQUE (version_id)                           -- 一个版本同时只有一个任务
    -- 部分索引保证：同一版本不允许同时存在两个未结束任务
);
CREATE UNIQUE INDEX IF NOT EXISTS uq_transfer_active
    ON transfer_tasks(version_id) WHERE status IN ('queued','running','paused');

-- 缓存记录（已完成搬运的落点，对账用）
CREATE TABLE IF NOT EXISTS cache_entries (
    id             INTEGER PRIMARY KEY AUTOINCREMENT,
    repository_id  INTEGER NOT NULL REFERENCES repositories(id) ON DELETE CASCADE,
    game_id        INTEGER NOT NULL REFERENCES games(id) ON DELETE CASCADE,
    version_id     INTEGER NOT NULL REFERENCES game_versions(id) ON DELETE CASCADE,
    repo_no        TEXT    NOT NULL,
    dest_rel       TEXT    NOT NULL,              -- 池内相对路径
    size_bytes     INTEGER NOT NULL,
    file_count     INTEGER NOT NULL,
    status         TEXT    NOT NULL DEFAULT 'ok'
                   CHECK (status IN ('ok','missing')),
    created_at     TEXT    NOT NULL,
    last_verified_at TEXT,
    UNIQUE (version_id)
);

CREATE INDEX IF NOT EXISTS idx_cache_repo   ON cache_entries(repository_id, status);
CREATE INDEX IF NOT EXISTS idx_transfer_state ON transfer_tasks(status);
```

**settings 新增键**（沿用 `db.rs` 的 `get_setting/set_setting`）：

| key | 默认 | 说明 |
| --- | --- | --- |
| `cache_pool_root` | 无 | 缓存池绝对路径；缺失 = 未配置 |
| `cache_pool_quota_bytes` | `10737418240`（10 GiB） | 逻辑配额 |

> 注：`UNIQUE(version_id)` 与部分唯一索引并存时，删任务行前必须先落 `cancelled`。实现上：取消 = 直接 `DELETE` 该行（staging 已删），保持表精简；状态机不需保留历史。若需留痕，另建 `transfer_log`，v2 不做。

---

## 5. 缓存池配置与容量记账

### 5.1 配置项校验（`set_cache_config`）

按顺序：

1. **路径非空**、可创建（不存在则尝试创建，失败报错）
2. **不得设在任何仓库根之内**：对每个 `repositories` 解析出的绝对路径做前缀比较（大小写不敏感、去尾部分隔符）；命中 → `PoolPathInsideRepository`
3. **路径不可变判定**：若 `cache_entries` 存在 `status='ok'` 或 `transfer_tasks` 存在 `status IN ('queued','running','paused')` → 拒绝改路径（`CachePathLocked`）。改容量不受此限
4. **配额**：正整数；单位换算是纯前端展示，落库一律字节
5. **缩容**：`new_quota >= used`，否则 `QuotaExceeded`
6. **扩容**：`new_quota - used <= available`（`available = disk_free - safety`），否则 `DiskExceeded`

### 5.2 已用计算（`used`）

```
used = Σ cache_entries(status='ok').size_bytes
     + Σ transfer_tasks(status IN ('queued','running','paused')).total_bytes
```

- 在途任务按 **total_bytes** 计（保守）：暂停中的任务已写入部分也占着盘，且恢复后还要继续写完
- 记账入口统一走同一个 `Mutex<CapacityLedger>`（内存值 + DB 兜底），**判定与预留必须在同一把锁内完成**，否则 32 并发下两个任务会同时通过预检

### 5.3 预检（`preflight_transfer`，纯读，不预留）

按顺序返回第一个命中的 reason：

| 顺序 | 条件 | reason |
| --- | --- | --- |
| 1 | 池未配置 | `PoolNotConfigured` |
| 2 | 版本不存在 / 状态 missing | `InvalidVersion` |
| 3 | `game_type='archive'` | `ArchiveUnsupported` |
| 4 | 已有 `cache_entries(ok)` | `AlreadyCached` |
| 5 | `required > quota - used` | `QuotaExceeded` |
| 6 | `required > available` | `DiskExceeded` |

`required` = 本次要搬的字节。首次搬 = 版本 `size_bytes`（**已改为递归求和**，含子目录，与搬运量一致；见 §6.1）。恢复 = `total_bytes - written_bytes`。

### 5.4 扩容/缩容判定（前端实时反馈 + 后端二次校验）

```rust
// 扩容
new_quota - used <= disk_free - disk_total * 10%
// 缩容
new_quota >= used
```

---

## 6. 搬运机

### 6.1 任务创建（`start_transfer`）

`preflight_transfer` 通过后，在同一把容量锁内：

1. 遍历源版本目录，**递归**累计 `total_bytes` / `total_files`（`game_versions.size_bytes` 已同步改为递归求和，二者口径一致；任务创建时仍独立求和一次，防源目录在扫描后变化）
2. 检查磁盘/配额（用 §5.3 条件，此时 `used` 已含本任务，故判 `required` 相对更新后的余量）
3. 插入 `transfer_tasks(status='queued')` → **任务字节即刻计入已用（预留）**
4. 唤醒调度器；返回 `task_id`

失败（预检不通过 / 插入冲突）→ 返回明确错误，不占配额。

### 6.2 调度

- 单后台线程 + `Condvar`/`mpsc` 唤醒；无任务时阻塞
- **全局通道 32**：某任务占用其档位 `concurrency`，`Σ running.concurrency <= 32`
- 队列按 `created_at` 升序；每次唤醒按序尝试调入，直到通道不足
- 调入成功：`status='running'`，起工作线程池（档位大小的线程数）

### 6.3 并发定档

按 `total_files` 自动定档，**不给用户调**：

| 文件数 | 档位 |
| --- | --- |
| ≤ 200 | 4 |
| 201–2000 | 8 |
| 2001–10000 | 16 |
| > 10000 | 32 |

### 6.4 写入流程

1. **staging 创建**：`<池根>/.staging/<task_id>/`；任务开始前若已存在（崩溃残留），保留并按"已存在文件跳过"续传
2. **文件枚举**：递归遍历源目录，产出相对路径列表（相对版本目录）
3. **复制**：按档位线程池分发；单个文件 `read→write`，块 1 MiB，复用缓冲区；目标路径 = `staging/<相对路径>`
4. **进度**：`written_bytes` 原子累加；每 200ms 从工作线程推一次 `transfer://progress`（读快照，不阻塞复制）
5. **ETA**：10 秒滑动窗口（`VecDeque<(Instant, bytes)>`），`speed = Δbytes/Δt`，`eta = (total - written)/speed`
6. **失败**：单文件重试 2 次（间隔 200/500ms）；仍失败 → 记入 `failed_files`，**继续搬其余文件**
7. **结束判定**：全部文件处理完（成功 + 失败）→ 进入校验

### 6.5 校验与落地

```
L1 校验：staging 内实际文件数 == total_files 成功数
          实际总字节 == 已成功搬运字节和
          逐文件 size 与源端一致（源端 stat 结果在复制时记录到内存表，不重复 stat 源）
```

- 校验通过 → `fs::rename(staging, <池根>/<dest_rel>)`（同卷原子）
  - **目标已存在**（外部残留）：`rename` 前检测，存在则 → `status='error'`，提示「目标路径已存在，请先清理缓存池」，**不覆盖、不删除**
- 校验失败 → `status='error'`，staging 保留（供排查），不 rename
- 校验通过后：删 staging 残留 → 写 `cache_entries(ok)` → `status='done'` → 推 `transfer://state` + `cache://usage`

### 6.6 暂停 / 取消 / 恢复

| 操作 | 语义 |
| --- | --- |
| 暂停 | 置 `pause_flag`；**在文件边界生效**（当前文件写完才停）；已写部分保留；`status='paused'`；释放档位通道 |
| 恢复 | 重跑 §5.3 预检 → 通过则 `status='queued'` 重新入队；已存在且 size 一致的文件跳过，不一致重写 |
| 取消 | 置 `cancel_flag` → 工作线程收尾 → **删除整个 staging** → `DELETE` 任务行（配额归还）→ 推状态 |

暂停/取消都用 `Arc<AtomicBool>`；工作线程在每个文件开始前检查（`pause` 检查 → 等待；`cancel` 检查 → 退出）。

### 6.7 失败与不可达

- 单文件失败重试 2 次后记入 `failed_files`，继续
- 结束后若有失败项 → `status='error'`，`error_message` 汇总，任务页列出失败清单，可「重试」（重试 = 重建任务，只搬失败文件？**v2 简化：整任务重来，已存在文件跳过**）
- 源仓库不可达（SMB 断）：文件操作返回 IO 错误 → 走单文件重试 → 仍失败 → 任务 `error`，**保留 staging 与现场**，不自动无限重试；提示「源仓库不可达」

### 6.8 崩溃恢复

启动时：

```
UPDATE transfer_tasks SET status='paused', updated_at=now WHERE status='running'
```

staging 保留；用户在任务页点「恢复」→ 走 §6.6。

### 6.9 长路径

- 全链路（遍历 / 复制 / 校验 / rename / 删除）使用 Windows verbatim 前缀（`\\?\`），Rust 侧封装 `to_verbatim(path)` helper
- staging 目录名固定 `.staging/<id>/`，不额外加长
- 预检时计算 `池根 + dest_rel + 最长相对路径` 长度，超 240 → 前端提示「路径较长，建议选择更短的缓存池路径」（**不阻止**，程序能处理）
- 启动前检查 exe 绝对路径 > 260 → 提示「路径超过系统限制，游戏可能无法启动」

---

## 7. 缓存记录与对账

### 7.1 写入时机

只在 **L1 校验通过 + rename 成功后**写 `cache_entries`，且与任务状态更新放在同一事务。

### 7.2 对账时机

| 时机 | 动作 |
| --- | --- |
| 程序启动 | ① running → paused；② 遍历 `cache_entries`，`dest_rel` 目录不存在 → `status='missing'`；③ 修正 `used`（扫描 `cache_entries` 实际大小？**不递归**，只信任记录值，避免每次启动扫池） |
| 打开详情页 / 列表 | 读取该版本记录，`TestPath` 校验；不存在 → `missing`（顺便落库） |
| 点启动前 | 再次校验；`missing` → 提示「缓存已失效，请重新搬运」 |

### 7.3 一致性口径

- **记录值优先**：`cache_entries.size_bytes` 由搬运时写入，不随外部改动刷新
- 外部删除池内文件 → 记录变 `missing`（不自动清记录，用户手动「从缓存池移除」或重新搬运覆盖）
- `missing` 记录不占 `used`（对账时从求和里排除），但记录保留供 UI 显示「已失效」

---

## 8. 清理（从缓存池移除）

`remove_cache(version_id)`：

1. 该版本有在途任务（queued/running/paused）→ `CacheRemoveBusy`（前端提示先取消）
2. 取 `cache_entries.dest_rel` → 删除 `<池根>/<dest_rel>` 整个目录（verbatim 递归）
3. 删除 `cache_entries` 行
4. 推 `cache://usage`；前端按钮回「搬运：XXX」

失败（目录被占用 / 权限）→ 报错，**记录与目录都不动**（不留半删状态）。

---

## 9. 启动游戏

`launch_game(game_id)`：

1. 取 `selected_version_id`；无 → `LaunchNoExecutable`
2. 取该版本 `is_selected` 的 exe；无 → `LaunchNoExecutable`（前端显示「配置启动项」）
3. 取 `cache_entries`；无 / `missing` → 提示「缓存已失效，请重新搬运」
4. **路径映射**：
   ```
   池内 exe = <池根>/<dest_rel> + (exe.relative_path − version.relative_path)
   ```
   例：`dest_rel=R01/RJ123456/RJV1`，`exe.relative_path=RJ123456/RJV1/game.exe`，`version.relative_path=RJ123456/RJV1` → `R01/RJ123456/RJV1/game.exe`
5. `Command::new(池内 exe).current_dir(exe 所在目录)` 拉起
6. **全程不访问仓库**（不探测 locator、不 check_repository）；NAS 离线也必须能启动
7. 启动失败（文件被删 / 权限 / 路径超限）→ `LaunchFailed`，提示具体原因

---

## 10. 仓库编号与仓库删除

### 10.1 编号生成

```rust
fn repo_no(id: i64) -> String { format!("R{id:02}") }
```

- id 来自 `repositories.id`（自增，永不复用）
- `id >= 100` → `R100` 起，不循环、不回收
- **不落库**：每次由 id 即时计算（id 稳定 ⇒ 编号稳定）；`transfer_tasks.repo_no` / `cache_entries.repo_no` 存快照，供仓库已删时的历史展示

### 10.2 删除仓库（增强 `delete_repository`）

前置检查（新命令 `get_repository_delete_preview` 供前端弹确认）：

```
{ cache_count, cache_bytes, active_task_count }
```

执行顺序：

1. 有在途任务 → 拒绝（提示先取消任务）
2. 删除 `<池根>/R{id:02>` 整个目录（存在才删）
3. `DELETE FROM repositories`（级联删 games/versions/executables/files/blocklist/transfer_tasks/cache_entries）

**删库再加回**：新 id → 新编号 → 旧缓存路径不再匹配（旧目录已在步骤 2 删除），符合产品口径。

---

## 11. IPC 协议

### 11.1 命令（前端 → Rust）

| 命令 | 入参 | 说明 |
| --- | --- | --- |
| `get_cache_config` | – | `CacheConfig` + `CacheUsage` |
| `set_cache_config` | `root?: string`, `quota_bytes: i64` | 按 §5.1 校验后保存 |
| `get_cache_usage` | – | 仅容量快照（轮询/事件兜底） |
| `preflight_transfer` | `version_id: i64` | 返回 `TransferPreflight`（纯读） |
| `start_transfer` | `version_id: i64` | 预检 + 预留 + 入队 |
| `list_transfers` | – | 全部任务（搬运页） |
| `get_version_transfer` | `version_id: i64` | 详情页按钮态用 |
| `pause_transfer` / `resume_transfer` / `cancel_transfer` / `retry_transfer` | `task_id: i64` | 状态机见 §6.6 |
| `remove_cache` | `version_id: i64` | §8 |
| `launch_game` | `game_id: i64` | §9 |
| `get_repository_delete_preview` | `repository_id: i64` | 删除确认弹窗用 |
| `delete_repository` | `repository_id: i64` | 已有命令，按 §10.2 增强 |

### 11.2 事件（Rust → 前端）

| 事件 | 负载 | 说明 |
| --- | --- | --- |
| `transfer://progress` | `TransferProgressEvent` | 节流 200 ms；详情页 + 搬运页共用 |
| `transfer://state` | `TransferTaskView` | 状态变化（排队/开始/暂停/完成/出错/取消） |
| `cache://usage` | `CacheUsage` | 容量变化（配置、任务起止、清理） |

---

## 12. UI 对接点

### 12.1 详情页按钮态机

| 条件 | 按钮 |
| --- | --- |
| 无缓存 + 可搬 | 「搬运：XXX」 |
| 搬运中 | 「搬运：XXX」+ 下方进度条 + 暂停 / 取消 |
| 暂停中 | 同上 + 恢复 / 取消 |
| 出错 | 进度条消失 + 「重试」+ 错误摘要 |
| 完成 + 有启动项 | 「启动：XXX」 |
| 完成 + 无启动项 | 「配置启动项」 |
| 压缩包形态 | 「未就绪，请解压后重扫」+ 禁用 |

版本下拉栏条目右侧：删除按钮（未缓存低亮度 / 缓存中正常亮度，悬浮提示「从缓存池移除版本」）。

### 12.2 搬运页

左侧导航「主页 / 搬运 / 设置」。按状态分组：搬运中 / 排队中 / 暂停中 / 出错 / 完成。每行：游戏名 + 版本标识、体积、进度条、速率、ETA、操作按钮。

### 12.3 设置页

仓库管理分上下两块：

- 上：缓存池路径输入框（有缓存/任务时禁用）、容量输入框 + MB/GB 切换、环形容量图（已用/剩余）、安全线提示
- 下：现有扫描进度条 + 仓库路径配置条目

---

## 13. 测试用例矩阵

测试环境用临时目录模拟仓库与池（不依赖真实 NAS）；`#[ignore]` 的真实仓库测试沿用 v1.1 的 `real_repo_scan`。

### 13.1 容量与配置

| # | 场景 | 期望 |
| --- | --- | --- |
| T2-01 | 默认配置 | quota = 10 GiB；池未配置时按钮禁用 |
| T2-02 | 路径设在仓库根内 | 拒绝，`PoolPathInsideRepository` |
| T2-03 | 有缓存时改路径 | 拒绝，`CachePathLocked` |
| T2-04 | 缩容到 < 已用 | 拒绝 |
| T2-05 | 缩容到 = 已用 | 允许 |
| T2-06 | 扩容后可用空间不足安全线 | 拒绝 |
| T2-07 | 连点两个总量超配额的任务 | 第二个 `QuotaExceeded`（预留生效） |
| T2-08 | 取消任务后配额归还 | 可再启动同量任务 |

### 13.2 搬运

| # | 场景 | 期望 |
| --- | --- | --- |
| T2-10 | 标准型搬运 | 落点 `<池根>/R01/<game>/<ver>/`，内容与源一致 |
| T2-11 | 紧凑型搬运 | 落点 `<池根>/R01/<game>/SakuCompact/`，内容 = 游戏标识层内容 |
| T2-12 | 搬运中暂停 → 恢复 | 已存在文件跳过，最终字节一致 |
| T2-13 | 取消 | staging 删除，无残留 |
| T2-14 | 目标路径已存在 | `error`，提示清理，不覆盖 |
| T2-15 | L1 校验失败（模拟篡改） | `error`，staging 保留，不 rename |
| T2-16 | 文件数档位 | ≤200→4 / 201–2000→8 / 2001–10000→16 / >10000→32 |
| T2-17 | 全局通道 | 两个 16 档任务并行 = 32；第三个排队 |
| T2-18 | 单文件失败 2 次 | 记入 failed_files，其余继续 |
| T2-19 | 崩溃恢复 | 启动后 running → paused，可续传 |
| T2-20 | 压缩包版本 | `ArchiveUnsupported`，按钮禁用 |
| T2-21 | 长路径（>240） | 搬成功；预检返回长度提示 |

### 13.3 缓存与清理

| # | 场景 | 期望 |
| --- | --- | --- |
| T2-30 | 外部删除池内目录 | 对账后 `missing`，按钮回「搬运」 |
| T2-31 | 清理进行中版本 | `CacheRemoveBusy` |
| T2-32 | 清理后容量归还 | used 减少，按钮回「搬运」 |
| T2-33 | 清理时目录被占用 | 报错，记录与目录均不变 |

### 13.4 启动与仓库

| # | 场景 | 期望 |
| --- | --- | --- |
| T2-40 | 正常启动 | 从池内路径拉起，cwd = exe 目录 |
| T2-41 | 仓库离线启动 | 正常拉起（不探测仓库） |
| T2-42 | 未配启动项 | `LaunchNoExecutable`，前端显示「配置启动项」 |
| T2-43 | 缓存 missing 时启动 | 提示重新搬运 |
| T2-44 | 删除仓库 | 池内 `R{id}` 目录被删；档案级联删除 |
| T2-45 | 有在途任务时删仓库 | 拒绝，提示先取消 |
| T2-46 | 删库再加回 | 新编号，旧缓存不匹配 |

---

## 14. 实现顺序建议

1. **Phase 0 · 地基**：`SCHEMA_VERSION=3` + 两张新表 + settings 键；`cache.rs` 配置读写与校验；`get_cache_config` / `set_cache_config`
2. **Phase 1 · 记账**：`CacheUsage` 计算、`preflight_transfer`、扩缩容校验、`cache://usage` 事件
3. **Phase 2 · 搬运（串行）**：单任务 staging 复制 + 进度 + L1 + rename + 记录（先不做并发/暂停）
4. **Phase 3 · 调度**：队列、32 通道、档位、暂停/恢复/取消、崩溃恢复
5. **Phase 4 · 对账与清理**：启动对账、`remove_cache`、`missing` 状态
6. **Phase 5 · 启动**：`launch_game` + 路径映射 + 不依赖仓库
7. **Phase 6 · 仓库**：编号分层、删除预览与清理
8. **Phase 7 · UI**：详情页态机、搬运页、设置页环形图

每阶段跑 `cargo test` + `pnpm exec vue-tsc --noEmit`。

---

## 15. 风险与未决

| # | 风险 | 处置 |
| --- | --- | --- |
| R-1 | 递归求和的性能（大游戏 42k 文件） | 只 stat 不读内容；与复制共用一次遍历 |
| R-2 | 32 通道下进度事件风暴 | 全局节流 200ms，合并同任务 |
| R-3 | 容量记账竞态 | 判定与预留同锁；DB 事务兜底 |
| R-4 | 长路径 | verbatim 前缀全链路；预检提示 |
| R-5 | SMB 断连卡住工作线程 | 文件级超时（读操作设 30s 超时后按失败处理）；出错保留现场 |
| R-6 | 池内目录被外部改名 | 对账标 missing；不自动修复 |
| R-7 | 版本大小统计口径 | 已统一为**递归求和**（扫描与搬运一致）；大目录（42k 文件）扫描耗时会增加，只 stat 不读内容 |
| R-8 | 删除仓库误伤 | 前置预览（数量/体积/任务数），前端二次确认（产品侧已要求） |

**未决（实现时若出现需回报）**

- 搬运重试的间隔与次数是硬编码还是可配置？（v2 先硬编码 2 次）
- 缓存池放在网络盘上是否允许？（v2 不拦截，但预检时提示"缓存池建议使用本地 SSD"）
