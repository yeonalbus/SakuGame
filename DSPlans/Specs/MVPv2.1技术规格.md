# MVP v2.1 技术规格

> 依据：`YeonPlans/版本规划/MVP v2.1_alter/` 下 `MVPv2.1设计规划_方案1.0.md`、`MVPv2.1设计规划_定义1.0.md`、`MVPv2.1设计规划_UI草案1.0.md`、两个 HTML 样板。
> 本文档只补方案未落到字段/状态机层的部分，**设计意图以方案为准**；本文档与方案冲突时以方案为准并回改本文档。
> 施工前已与珱垣确认的 5 项决策：①DB 改动按本规格 §4；②mtime 口径为「变动文件」（新增 + 修改，删除单列）；③抢占复用 `paused`；④不做「关闭进程后等 5 分钟」自动检测；⑤紧凑型不做特殊分叉，锚点检查统一放版本粒度。

---

## 1. 本版范围与不做

### 1.1 做

- `.SakuAnchor.json` 指纹：生成、校验、重建、移动后重定位
- 变更组（`SakuAlter`）三层能力：**注入**（搬运完成后覆盖本体/用户目录）、**检测**（移出缓存前 hash + mtime 取并集）、**存储**（提取到 `.waiting` → 删缓存池 → 回写 HDD 的 `SakuAlter`）
- 轻量 mtime 基线（游戏本体 + 用户路径全文件）
- 用户路径（`user_path`）配置与注入/检测联动
- 回运与搬运共用 32 并发额度的统一调度，回运优先，可阻塞（抢占）搬运
- 详情页 / 搬运页按 HTML 样板改造

### 1.2 不做（v2.1 边界）

- 不做游戏文件夹外的存档自动探测、不按引擎/后缀匹配存档
- 不做多变更组备份（单变更组；队列先进先出留待后续）
- 不做游戏进程跟踪与「退出后延迟 5 分钟自动检测」（触发点只有「用户点移出缓存」）
- 不做压缩包版本（沿用 v2 的 `ArchiveUnsupported`）
- 不引入调度公平性算法（大文件饿死问题留待后续版本）
- 不改 README

---

## 2. 名词与结构

| 术语 | 定义 |
| --- | --- |
| 版本层根 | 标准型 `<仓库根>/<游戏标识层>/<版本标识层>`；紧凑型 `<仓库根>/<游戏标识层>`（即 `game_versions.relative_path` 的绝对形式） |
| `SakuAlter` | 版本层根下的维护目录，**唯一被写入的容器**：`BG/`（游戏本体变更组）、`UD/`（用户目录变更组）、`.SakuAnchor.json` |
| 变更组文件 | `BG`/`UD` 下的文件；`BG` 的 `rel_path` 相对版本层根，`UD` 的 `rel_path` 相对 `user_path` |
| 锚点 | `.SakuAnchor.json`，版本层的身份指纹（`version_uuid`）+ 生成留痕 |
| 基线 | 一次注入后对「版本层根全文件（排除 `SakuAlter`）+ 用户路径全文件」的 mtime/size 快照 |
| 变动文件 | 相对基线 mtime 或 size 变化的文件（新增、修改）；删除的文件单独记为 `deleted`，不可勾选 |
| 回运（回写） | 把缓存池内变更组搬回 HDD `SakuAlter`，与「搬运」（HDD→池）方向相反 |

**统一路径公式**（两种布局通用，实现不分叉）：

```
SakuAlter 目录   = <仓库根>/<version.relative_path>/SakuAlter
锚点文件         = 上述 + /.SakuAnchor.json
BG 覆盖目标      = <版本层根>/<rel_path>
UD 覆盖目标      = <user_path>/<rel_path>
缓存池版本层根   = <池根>/<cache_entries.dest_rel>
```

紧凑型 `version.relative_path == game.relative_path`，公式自动成立；缓存池落点为 `SakuCompact`，但注入/基线/检测全部在「缓存池版本层根」内进行，与布局无关。

---

## 3. 数据结构（Rust 侧）

```rust
// models.rs 追加
pub struct SakuAnchor {           // 只用于读写 .SakuAnchor.json
    pub schema_version: String,   // 生成时的应用版本（env!("CARGO_PKG_VERSION")）
    pub game_uuid: String,        // UUID v4
    pub version_uuid: String,     // UUID v4
    pub created_at: i64,          // Unix 秒；DB 侧仍用 RFC3339
}

pub enum AlterScope { Bg, Ud }    // serde: "bg" / "ud"

pub struct AlterFileRow {         // alter_files 视图
    pub scope: String, pub rel_path: String,
    pub size_bytes: i64, pub hash: String,
}

pub struct CandidateFile {        // 勾选树叶子
    pub scope: String,            // bg / ud
    pub rel_path: String,
    pub size_bytes: i64,
    pub mtime_ms: i64,
    pub reason: Vec<String>,      // ["hash"] / ["mtime"] / ["hash","mtime"]
    pub required: bool,           // true = 已有变更组文件的 hash 失配，默认必回写、不可取消勾选
}

pub struct WritebackCandidates {
    pub version_id: i64,
    pub game_name: String,
    pub version_label: String,
    pub files: Vec<CandidateFile>,
    pub deleted: Vec<String>,     // 相对基线的删除项（仅展示，不参与回写）
    pub user_path: Option<String>,
    pub user_path_state: String,  // ok / unset / unreachable
    pub baseline_ready: bool,     // false = 无基线（v2.1 之前的旧缓存），mtime 部分跳过
    pub alter_ready: bool,        // false = 无变更组记录，hash 部分跳过
}

// TransferTaskView 追加
pub direction: String,            // pull / writeback
pub stage: String,                // 见 §10.2
```

---

## 4. SQLite 表结构（`SCHEMA_VERSION = 5`）

### 4.1 迁移（v4 → v5）

现有 `init_schema` 的补列模式继续用；`transfer_tasks.status` 的 `CHECK` 约束无法 `ALTER`，因此**重建该表**（保留行，无子表引用它）：

```sql
DROP INDEX IF EXISTS uq_transfer_active;
DROP INDEX IF EXISTS idx_transfer_state;
ALTER TABLE transfer_tasks RENAME TO _transfer_tasks_v4;
CREATE TABLE transfer_tasks ( ...新定义见 4.2... );
INSERT INTO transfer_tasks(id, repository_id, game_id, version_id, repo_no, source_rel, dest_rel,
    total_bytes, total_files, written_bytes, written_files, concurrency, direction, stage,
    status, error_message, failed_files, created_at, started_at, finished_at, updated_at)
SELECT id, repository_id, game_id, version_id, repo_no, source_rel, dest_rel,
    total_bytes, total_files, written_bytes, written_files, concurrency, 'pull', NULL,
    status, error_message, failed_files, created_at, started_at, finished_at, updated_at
FROM _transfer_tasks_v4;
DROP TABLE _transfer_tasks_v4;
-- 索引重建（uq 覆盖 waiting_confirm）
```

`game_uuid` / `version_uuid` 补列后**就地回填**（对已有行生成 UUID v4）；`anchor_hash` 留空，等首次扫描/搬运时建立。

### 4.2 表定义

```sql
-- games 追加
ALTER TABLE games         ADD COLUMN game_uuid       TEXT;
-- game_versions 追加
ALTER TABLE game_versions ADD COLUMN version_uuid    TEXT;
ALTER TABLE game_versions ADD COLUMN last_known_path TEXT;   -- 最近一次确认可达的版本层绝对路径
ALTER TABLE game_versions ADD COLUMN anchor_hash     TEXT;   -- .SakuAnchor.json 字节的 blake3（防重复写入）
ALTER TABLE game_versions ADD COLUMN user_path       TEXT;   -- 用户目录绝对路径；NULL = 未配置

-- 任务表（重建）
CREATE TABLE transfer_tasks (
    id             INTEGER PRIMARY KEY AUTOINCREMENT,
    repository_id  INTEGER NOT NULL REFERENCES repositories(id) ON DELETE CASCADE,
    game_id        INTEGER NOT NULL REFERENCES games(id) ON DELETE CASCADE,
    version_id     INTEGER NOT NULL REFERENCES game_versions(id) ON DELETE CASCADE,
    repo_no        TEXT    NOT NULL,
    source_rel     TEXT    NOT NULL,
    dest_rel       TEXT    NOT NULL,
    total_bytes    INTEGER NOT NULL,
    total_files    INTEGER NOT NULL,
    written_bytes  INTEGER NOT NULL DEFAULT 0,
    written_files  INTEGER NOT NULL DEFAULT 0,
    concurrency    INTEGER NOT NULL,
    direction      TEXT    NOT NULL DEFAULT 'pull' CHECK (direction IN ('pull','writeback')),
    stage          TEXT,   -- §10.2；NULL = 尚未进入阶段
    status         TEXT    NOT NULL DEFAULT 'queued'
                   CHECK (status IN ('queued','running','paused','waiting_confirm','error','done','cancelled')),
    error_message  TEXT,
    failed_files   TEXT,
    created_at     TEXT NOT NULL,
    started_at     TEXT,
    finished_at    TEXT,
    updated_at     TEXT NOT NULL
);

-- 变更组文件（回写后更新 hash；BG/UD 各自相对根）
CREATE TABLE IF NOT EXISTS alter_files (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    version_id INTEGER NOT NULL REFERENCES game_versions(id) ON DELETE CASCADE,
    scope      TEXT    NOT NULL CHECK (scope IN ('bg','ud')),
    rel_path   TEXT    NOT NULL,
    size_bytes INTEGER NOT NULL DEFAULT 0,
    hash       TEXT    NOT NULL,
    updated_at TEXT    NOT NULL,
    UNIQUE (version_id, scope, rel_path)
);

-- mtime 基线（每次注入后整体覆盖写）
CREATE TABLE IF NOT EXISTS mtime_baseline (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    version_id INTEGER NOT NULL REFERENCES game_versions(id) ON DELETE CASCADE,
    scope      TEXT    NOT NULL CHECK (scope IN ('bg','ud')),
    rel_path   TEXT    NOT NULL,
    size_bytes INTEGER NOT NULL DEFAULT 0,
    mtime_ms   INTEGER NOT NULL,
    UNIQUE (version_id, scope, rel_path)
);
CREATE INDEX IF NOT EXISTS idx_baseline_version ON mtime_baseline(version_id, scope);

-- 回写包（.waiting 的 DB 镜像，崩溃恢复依据）
CREATE TABLE IF NOT EXISTS writeback_packages (
    task_id    INTEGER PRIMARY KEY REFERENCES transfer_tasks(id) ON DELETE CASCADE,
    version_id INTEGER NOT NULL REFERENCES game_versions(id) ON DELETE CASCADE,
    stage      TEXT    NOT NULL,          -- extracted / cleaning / storing
    manifest   TEXT    NOT NULL,          -- JSON：.waiting/<task_id>/manifest.json 的副本
    created_at TEXT    NOT NULL,
    updated_at TEXT    NOT NULL
);
```

索引：`uq_transfer_active ON transfer_tasks(version_id) WHERE status IN ('queued','running','paused','waiting_confirm')`；`idx_transfer_state(status)`；`idx_alter_version(version_id, scope)`。

> 注：`uq_transfer_active` 覆盖 `waiting_confirm`，保证一个版本同时只有一个回写包。

---

## 5. `.SakuAnchor.json`

### 5.1 生成时机（惰性）

| 时机 | 行为 |
| --- | --- |
| 扫描建档（含单游戏重扫） | 版本层根**已有** `SakuAlter` → 校验/补写锚点；**没有** → 不创建 `SakuAlter`，不生成（避免污染用户仓库） |
| 搬运完成后注入 | `SakuAlter` 存在且锚点缺失 → 生成；存在且 `version_uuid` 不符 → 重写为 DB 值（视为用户从别处拷贝来的旧锚点） |
| 回写存储完成 | `SakuAlter` 为本次创建 → 生成锚点 |
| 用户手删锚点 | `last_known_path` 可达 → 用 DB 的 `game_uuid`/`version_uuid` 重建，`created_at` 刷新；不可达 → 该版本悬空，搬运时走 §5.3 兜底或报错 |

跳过判定：扫描时锚点存在且 `version_uuid` 与 DB 一致 → 该版本**不重扫**（不更新 `size_bytes`、不重建 exe 候选）。**仅标准型生效**：紧凑型的版本层就是游戏标识层，跳过省不下目录枚举，且 `game.size_bytes` 需刷新，故紧凑型不做跳过。`档案重扫`（单档案重扫 / 续扫）忽略此跳过。跳过发生在 `classify_game` 的标准型版本循环里（blocklist 过滤之后），入库时只刷 `status / updated_at`。

### 5.2 定位（点搬运时）

```
1. last_known_path 存在且可达
   → 读 <last_known_path>/SakuAlter/.SakuAnchor.json
      version_uuid 相符 → 用它当仓库根
      缺失/不符        → 走 2
2. 遍历所有已登记仓库根，只扫两层（不递归内容）：
   <root>/<game>/*/SakuAlter/.SakuAnchor.json   （标准型）
   <root>/<game>/SakuAlter/.SakuAnchor.json     （紧凑型）
   命中 version_uuid → 更新 last_known_path → 用它当仓库根
3. 仍未命中 → 回退 v2 现有 locator 解析（resolve_repository）
```

第 2 步只读 JSON、不列举文件，代价是每仓库游戏数的 stat 量，可接受。

### 5.3 与删除/拉黑的联动

- 删除版本 / 删除档案 / 删除仓库：只删 DB 行与锚点的「登记关系」；**不删 `SakuAlter` 目录**（存档是用户资产）。`blocklist` 里的版本不写锚点。
- 版本被拉黑 → 扫描不生成锚点；解除拉黑 → 下次扫描按 5.1 恢复。

---

## 6. 搬运（pull）流程扩展

在 v2 的 `run_task` 成功后插入两个阶段（状态机见 §10.2）：

```
transfer（v2 原流程）→ rename 落地 → 写 cache_entries
  → stage=inject   变更组注入
  → stage=baseline 初始 mtime 记录
  → status=done
```

### 6.1 注入

```
SakuAlter = <池根>/<dest_rel>/SakuAlter
若不存在 → 跳过（stage 直接进 baseline）
BG：遍历 SakuAlter/BG/**  → 覆盖到 <池根>/<dest_rel>/<rel>
UD：user_path 已配置且可达 → 遍历 SakuAlter/UD/** → 覆盖到 <user_path>/<rel>
    user_path 未配置或不可达 → 跳过 UD，任务 error_message 追加提示（不算失败）
对注入的每个文件算 blake3 → upsert alter_files（这就是「记录变更组文件当前 hash」）
SakuAlter 存在时：校验/生成/修复锚点（§5.1）
```

注入失败（权限/占用）：任务 `error`，**保留缓存池内容**（可重试，重试走 `retry_transfer` 重建 pull 任务并跳过已存在文件）。

`SakuAlter/UD` 下有文件但 `user_path` 未配置或不可达 → 视为**会丢档**，任务 `error` 并回滚（删除刚落地的缓存目录 + 清掉半成品 `alter_files` / `mtime_baseline`），错误文案提示先配用户路径。绝不静默跳过。

注入/基线整体失败的回滚口径：删除缓存池落点目录 + 清掉该版本的 `alter_files` / `mtime_baseline`，任务 `error`，回到「未搬运」状态以便重试。

### 6.2 基线

注入完毕（或跳过后）立即扫描并**整体覆盖写** `mtime_baseline`：

- `scope=bg`：`<池根>/<dest_rel>` 全文件，排除第一层 `SakuAlter/`
- `scope=ud`：`user_path` 全文件（配置且可达才扫）

`mtime_ms` = Unix 毫秒（`SystemTime` 转算，避免各平台精度差异）。基线为空的版本视为 `baseline_ready=false`。

### 6.3 阶段期间的并发与启动限制

- `stage=inject` / `baseline` 期间：任务仍 `running`（占额度），**禁止启动游戏**（详情页按钮态机取 `task` 非空即禁用启动）。
- 方案要求「注入时释放游戏所占的并行额度」——v2.1 的口径是：**注入与基线的 I/O 不计入文件通道**（它们不是文件复制），但任务本身仍占其档位直到结束，避免被无限叠加。本版按此实现。

---

## 7. 检测（移出缓存前）

触发：用户点「移出缓存」→ `start_writeback(version_id)` → 建 `direction='writeback'`、`stage='detect'` 的任务。

```
1. 缓存池版本层根不存在 → error
2. 生成候选（BG）
   a. hash 严格：遍历 alter_files(scope=bg)，逐个算 blake3 → 与记录不符 → reason 含 hash
   b. mtime 宽松：遍历 <版本层根> 全文件（排除 SakuAlter），与基线比
        基线无该 rel_path → 新增；size 或 mtime_ms 不同 → 修改 → reason 含 mtime
        基线有、当前无 → deleted（不勾选）
3. 生成候选（UD）
   user_path 未配置 → user_path_state=unset，跳过
   不可达 → user_path_state=unreachable，跳过
   否则同 2（UD 变更组 hash + user_path 基线）
4. 并集去重（同 scope+rel_path 合并 reason）
5. 已有变更组文件命中 hash 失配 → required=true（默认必回写，UI 不可取消）
6. 写入磁盘 .waiting 之前不落任何暂存；候选结果暂存在任务行（由前端拉取，不落库）
7. hash 阶段算出的内容 hash 一并返回（前端确认后回写时免重算）
```

返回 `WritebackCandidates`。前端渲染勾选树（按目录前缀分组）。

**边界**：`alter_files` 为空 → `alter_ready=false`；`mtime_baseline` 为空 → `baseline_ready=false`（v2 旧缓存），两者皆空时提示「该版本无检测基线，将直接移除缓存；如需保留存档请先搬运一次 v2.1」。确认后不产生任何回写文件，直接走清理。

---

## 8. 存储（回写 + 清缓存）

### 8.1 任务状态流转

`detect` 完成 → `status=waiting_confirm`、`stage=waiting_confirm`，**释放并发额度**（任务线程结束）。

- 用户点「确认并开始回写」→ `confirm_writeback(task_id, selection)` → 校验 selection 是候选子集（`required` 必须全选）→ 回 `status=queued`、`stage=extract` → 调度器重新分配额度
- 用户点弹窗「取消/叉号」→ `abort_writeback(task_id)` → `status=paused`（任务保留、不再自动启动）、`stage=waiting_confirm`；**游戏与存档原样保留**
- 用户点任务卡片「取消」→ 复用 `cancel_transfer`（删任务行；缓存池与 SakuAlter 均不动），前端二次确认文案固定为「不会丢弃存档变动，游戏仍保留在缓存池」

### 8.2 三段执行（一个任务线程内完成 extract → clean → store）

```
extract：把选中的 BG/UD 文件复制到 <池根>/.waiting/<task_id>/staged/<scope>/<rel_path>
         写 .waiting/<task_id>/manifest.json（见 8.3）+ 写 writeback_packages
clean  ：删除 <池根>/<dest_rel> 整目录 + 删 cache_entries 行
store  ：按 manifest 逐个 rename/复制到
         <仓库根>/<version_rel>/SakuAlter/<BG|UD>/<rel_path>
         写回成功 → 更新 alter_files.hash = 提取时的 blake3
         全部完成 → 更新/补写锚点（SakuAlter 可能首次创建）
                   → 删 .waiting/<task_id> + writeback_packages
                   → 删锚点所在的空目录层级？否（保留 SakuAlter）
                   → status=done
```

- 提取阶段可取消：删 `.waiting` + 任务行，缓存池不动（回 waiting_confirm 之前的「游戏在池内」状态之前提：extract 中途取消走「中止」语义 → 状态 `paused`、`stage=waiting_confirm`，不删任务。**取消任务行的语义只在 `waiting_confirm` 与 `error` 下允许**）
- `extract` / `clean` 进行中：整卡禁用操作按钮（样板「处理中...」），不提供取消

### 8.3 `.waiting` 格式

```
<池根>/.waiting/<task_id>/
├── manifest.json
└── staged/
    ├── bg/<rel_path>...
    └── ud/<rel_path>...
```

```json
{
  "task_id": 12, "version_id": 3, "schema": 1,
  "repository_id": 1, "game_rel": "RJ123456", "version_rel": "RJ123456/Ver_1.0",
  "user_path": "C:\\Users\\X\\AppData\\LocalLow\\Studio\\Name",
  "created_at": "2025-11-20T05:00:00Z",
  "files": [ { "scope": "bg", "rel_path": "save/syssave.dat", "hash": "…", "size_bytes": 1234 } ]
}
```

### 8.4 崩溃恢复（启动时，`cache::reconcile` 之后）

```
1. running 任务 → paused（沿用 v2）；同时把 stage 保留
2. waiting_confirm 任务 → 保留（弹窗状态可从任务行重建候选？否：候选结果不落库）
   → 本版处置：waiting_confirm 一律置 cancelled（游戏仍在池内，无任何写入），
     用户重新点「移出缓存」；任务页可见「已取消」记录。
3. 扫 <池根>/.waiting/*：有 manifest 的 → 恢复 store（§8.2 第三段）+ 补删缓存池残余
4. 无 manifest 的 .waiting 目录 → 直接删除
5. staging（v2 搬运残料）→ 沿用 v2（保留供续传）
```

> 与 v2 冲突点：方案 §三.5 情况 A 说搬运中断要清掉 SSD 残余；v2 已实现 staging 续传，**v2.1 保留续传**（不清），方案该句按「删未完成的 .waiting」理解。

---

## 9. 并发与调度

### 9.1 额度模型（沿用 v2：全局 32 文件通道、整任务占位）

| 任务 | 占额度 | 说明 |
| --- | --- | --- |
| pull：`transfer` 阶段 | 是，档位 = `pick_concurrency(total_files)` | v2 原样 |
| pull：`inject` / `baseline` | 是（沿用档位，不再额外叠加） | §6.3 |
| writeback：`detect` | 是，档位 = `pick_concurrency(候选文件数，最小 4)` | 检测有 I/O |
| writeback：`waiting_confirm` | **否**（线程已退出） | 方案硬要求 |
| writeback：`extract/clean/store` | 是，档位 = `pick_concurrency(选中文件数)` | 确认后重新申请 |

### 9.2 调度规则

```
每轮：
  1. 取 queued 任务：writeback 在前（created_at ASC），pull 在后（created_at ASC）
  2. 对每个 writeback：
       若 used + need > 32 → 尝试抢占：按「占用额度最大、创建最晚」顺序挑 running 的 pull 任务
             → pause 之（复用 pause_task，文件边界生效）
             → 等其线程退出、额度归还（最多等 30s，超时则本轮放弃该回写任务）
             → 重试
       满足则启动
  3. 对每个 pull：used + need <= 32 则启动
```

- 抢占后搬运任务状态为 `paused`（与用户手动暂停无法区分——可接受，符合「复用 paused」决策）；用户点「恢复」即 `queued` 重新排队并保留已复制 staging。
- 回运结束后正常唤醒，被抢占的 `paused` 任务**不自动恢复**（与 v2 暂停语义一致，需用户恢复或后续版本引入自动续跑）。
- 饿死：允许（§1.2）。

---

## 10. 状态机与 IPC

### 10.1 任务状态

```
queued ──调度──> running ──完成──> done
                  │   │
                  │   ├─(writeback detect 完成)──> waiting_confirm
                  │   ├─(用户暂停/被抢占)──> paused
                  │   └─(失败)──> error
waiting_confirm ──确认──> queued(stage=extract)
waiting_confirm ──中止──> paused
waiting_confirm ──任务页取消──> (删行)
```

### 10.2 stage 取值

| direction | stage | 界面文案（搬运页） |
| --- | --- | --- |
| pull | `transfer` | ①传输 |
| pull | `inject` | ②注入 · 变更组注入中 |
| pull | `baseline` | ③记录 · 生成初始记录中 |
| writeback | `detect` | ①检测 · 检测变更组中 |
| writeback | `waiting_confirm` | 待确认 |
| writeback | `extract` | ②提取 · 提取变更组中 |
| writeback | `clean` | ③清理 · 从缓存池移除游戏副本 |
| writeback | `store` | ④存储 · 存储变更组 |

### 10.3 新增命令

| 命令 | 入参 | 返回 | 说明 |
| --- | --- | --- | --- |
| `start_writeback` | `version_id` | `task_id` | 建 `writeback` 任务（`stage=detect`），进入调度；检测结果落 `writeback_packages` |
| `get_writeback_plan` | `task_id` | `WritebackCandidates` | 读已落盘的检测结果（不重扫）；`waiting_confirm` 时前端用它渲染勾选树 |
| `confirm_writeback` | `task_id`, `selection: Vec<AlterRef>` | `void` | 必须包含全部 `required`（缺失时后端自动补）；按选中数重定并发档 |
| `abort_writeback` | `task_id` | `void` | waiting_confirm → paused（`stage` 保持 `waiting_confirm`；恢复时重新检测） |
| `set_version_user_path` | `version_id`, `path: Option<String>` | `void` | 校验绝对路径与可达性；空 → NULL |
| `get_alter_summary` | `version_id` | `AlterSummary` | 详情页展示：变更组文件数/字节、锚点状态、基线状态、用户路径状态 |

`AlterRef = { scope: String, rel_path: String }`；`AlterSummary = { bg_count, ud_count, bytes, anchor_state: "ok"|"missing"|"drift"|"none", baseline_ready, user_path_state, user_path }`。

`get_version_transfer` / `get_version_states` 的活跃任务过滤需包含 `waiting_confirm`。

### 10.4 事件

沿用 `transfer://progress` / `transfer://state` / `cache://usage`；`TransferTaskView` 增 `direction`、`stage`，前端据 `stage` 渲染流水线与文案。

---

## 11. UI 对接点

- **搬运页**：五页签沿用（搬运中 / 排队中 / 暂停中 / 出错 / 完成）；卡片增「方向徽章（↓搬运 / ↑回写）」「微步骤流水线」「并发胶囊」「全局并发池条（已占 X / 32）」。`waiting_confirm` 卡片操作 = `选择文件` + `暂停` + `取消`；`extract/clean` 卡片 = 禁用「处理中...」。
- **勾选弹窗**：树形（游戏本体重变动 / 用户路径变动；已有变更组文件红色标注「自动回写」，不可取消勾选）；底部「未勾选的新增文件将直接丢弃」；取消 = 中止；确认 = 开回写。
- **详情页**：删「本机详情」页签（字段留在 DB 不删）；形态胶囊（标准绿 / 紧凑橙）置于编号左侧；版本条目由抽屉改卡片；配置抽屉增「用户路径（绝对路径）」；按钮态机增「回写中」（`task.direction=writeback` 且非 `waiting_confirm` 时禁用启动与搬运）。
- 样式按项目现有 `styles.css` 手写体系实现，HTML 样板只作视觉/交互参照，不引入 Tailwind。

---

## 12. 测试矩阵

| # | 场景 | 期望 |
| --- | --- | --- |
| T21-01 | 扫描已有 SakuAlter 的版本 | 生成锚点，`anchor_hash` 落库 |
| T21-02 | 扫描无 SakuAlter 的版本 | 不创建目录、不生成锚点 |
| T21-03 | 锚点 hash 未变重扫 | 跳过该版本（size/exe 不刷新） |
| T21-04 | 档案重扫 | 忽略跳过，强制重扫 |
| T21-05 | 锚点被手删 + 路径可达 | 重建锚点，`created_at` 刷新 |
| T21-06 | 版本目录被移动 + 点搬运 | last_known_path 失效 → 兜底扫描命中 → 更新路径 → 搬运成功 |
| T21-10 | 搬运完成 + SakuAlter 存在 | BG 覆盖本体、UD 覆盖 user_path、hash 落库、基线与池内一致 |
| T21-11 | 搬运完成 + 无 SakuAlter | 跳过注入，仍生成基线；无锚点 |
| T21-12 | 搬运完成 + user_path 未配置 | 跳过 UD，任务成功，error_message 含提示 |
| T21-20 | 游戏修改已有文件（mtime 变） | 检测为变动（reason=mtime），可勾选 |
| T21-21 | 新增文件 | 检测为新增（reason=mtime） |
| T21-22 | 变更组文件被改写 | reason 含 hash，required=true，不可取消 |
| T21-23 | 同一文件同时 hash + mtime 变 | 合并成一条，reason 两项 |
| T21-24 | 删除文件 | 进 deleted 列表，不可勾选 |
| T21-25 | 无基线旧缓存 | baseline_ready=false，提示，确认后直接清理 |
| T21-30 | 确认回写 | .waiting 出现 → 池目录删除 → HDD SakuAlter 写入 → hash 更新 → 包清理 |
| T21-31 | 弹窗取消 | 任务 paused，池内游戏与 SakuAlter 原样 |
| T21-32 | 任务页取消 | 删任务行，池内游戏原样 |
| T21-33 | 回写中强退（有 manifest） | 启动后自动恢复 store 并清理 |
| T21-34 | 检测中强退 | running→paused；stage 保留 |
| T21-40 | 回写排队 + 搬运占满 32 | 抢占搬运（paused），回写启动，结束后搬运可恢复 |
| T21-41 | 回写额度不足但无搬运在跑 | 本轮放弃，轮询重试，不死锁 |
| T21-42 | waiting_confirm 期间额度 | `slots_used` 不含该任务 |
| T21-43 | 紧凑型版本全流程 | 锚点在标识层/SakuAlter，注入/基线/回写路径正确 |

---

## 13. 实现顺序

1. **Phase 0 地基**：Cargo 依赖（`uuid` v4、`blake3`）、§4 schema v5 + 迁移 + 回填、DTO
2. **Phase 1 锚点**：`anchor.rs`（读写/校验/重建/兜底定位）+ 扫描集成 + 删除联动
3. **Phase 2 注入**：`alter.rs` 注入 + hash + 基线；`run_task` 接阶段
4. **Phase 3 检测**：候选生成 + 命令
5. **Phase 4 存储**：`.waiting` 提取/清理/回写 + 崩溃恢复
6. **Phase 5 调度**：writeback 任务 + 抢占
7. **Phase 6 前端**：搬运页 / 详情页 / 勾选弹窗 / 用户路径
8. **Phase 7 验证**：`cargo test` + `pnpm exec vue-tsc --noEmit` + 本矩阵回归

每阶段跑 `cargo test`；Phase 6 后跑 `vue-tsc`。

---

## 14. 风险与未决

| # | 风险 | 处置 |
| --- | --- | --- |
| R-1 | 全文件 mtime 基线在 4 万文件量级耗时 | 只 stat 不读内容；注入后一次性扫；与注入同线程串行 |
| R-2 | 变更组内混入大文件（用户勾选 archive.dat） | hash 只对变更组文件算；回写体积按实际文件 |
| R-3 | 探测 `*/*/SakuAlter` 兜底在超大仓库变慢 | 只读 JSON 不递归；先 last_known_path 快路径 |
| R-4 | 抢占导致用户手动暂停的任务被误恢复/误取消 | 抢占只复用 paused，不写任何附加状态；文档明示 |
| R-5 | 多版本共用同一 user_path 时 UD 互相覆盖 | 不做合并；按「最后搬运者覆盖」；文档明示 |
| R-6 | 游戏引擎扫描 SakuAlter 目录造成干扰 | 已知兼容性风险，不在本版处理 |
| R-7 | 版本体积含 SakuAlter → 容量记账偏大 | 保持 v2 递归口径；体积显示文案不改 |

### 未决（留待后续版本）

- 多变更组 FIFO 备份（5 个槽位）
- 进程退出检测 + 延迟自动检测
- 调度公平性（防大文件饿死）
- 回写完成后被抢占的搬运任务自动恢复
