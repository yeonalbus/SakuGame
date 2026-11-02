# MVP v1.1 技术规格

> **依据**：[MVPv1.1设计规划1.0-dsedit.md](YeonPlans-dsedit/MVPv1.1设计规划1.0-dsedit.md)（产品口径）+ [MVPv1扫描器规格.md](MVPv1扫描器规格.md)（v1 规则，仅保留仍有效部分）+ [Z-Game实测探测报告.md](Z-Game实测探测报告.md)（实测约束）
> **维护者**：AI。**状态**：待施工（珱垣已确认"先不施工，只落档"）。
> **本文与产品文档的分工**：产品语言与决策归 dsedit 规划文档；本文只承载**可实现化细节**（数据结构、算法、DDL、保护矩阵、IPC、测试矩阵）。两者冲突时以规划文档的「珱垣确认」项为准。

---

## 1. 本版对 v1 规则的作废清单

> ⚠️ 施工前必读：v1 的递归判定模型整块退役。

| v1 章节 / 决策 | v1.1 处置 |
| --- | --- |
| 规格 §3.3「候选 exe 打分（相似度）」 | **早已作废**（实测零命中），v1.1 不实现，保持作废 |
| 规格 §3.4「深度与下探」+ D5（最大深度 3） | **作废**。改为固定两层，无深度参数 |
| 规格 §4「扫描算法伪代码」（classify / 容器下探 / Skip） | **作废**，由本文 §4 替代 |
| 规格 §3.7「判定粒度与多版本处理」（D15 每版本一条档案） | **作废**，由两层结构替代 |
| 规格 §6「SQLite 表结构」 | **作废**，由本文 §5 替代（`SCHEMA_VERSION = 2`） |
| 规格 §11.1 测试 T3（深度上限）、T9（压缩不穿透子目录）、T32（多版本各自建档） | **T3 / T9 作废**（深度与穿透概念消失）；**T32 改造**为 ACC-1.1-02 |
| Q-B「同目录多压缩组各建一条档案」 | **作废**，改为合并为 1 个版本（`group_key` 降级为版本内分卷组键） |
| D19「exe 候选不落库」 | **修订**：版本内候选 exe 落库；`game_code` / `cover_path` 仍**禁止**自动填写 |
| D20「档案无版本字段」 | **修订**：两层结构 |

**仍有效、必须保留的 v1 规则**：

- exe 排除名单与压缩白名单（`.bin` / `.cue` 不属压缩包）
- 分卷聚合正则（`.partN.rar` / `.rNN` / `.zNN` / `7z.001`）
- 不跟随符号链接 / junction；长路径 `\\?\` 前缀；中文路径
- 压缩包判定**不穿透**目录（本来就不穿透，v1.1 更严格：只认直接子文件）
- 幂等重扫：用户字段绝不覆盖；本次未扫到的标 `missing` 而不删除
- 断点续扫（`scan_checkpoints`）
- 扫描槽单任务（D9）、暂停 / 取消语义（I5）

---

## 2. 名词与结构

| 术语 | 定义 | 举例 |
| --- | --- | --- |
| 仓库根 `root` | 仓库配置指向的路径 | `Z:\Game` |
| 游戏标识层 | 仓库根的**直接子目录**；一层 = 一条档案 | `Z:\Game\RJ01034814` |
| 版本标识层 | 游戏标识层的直接子目录，且其**直接文件**含候选 exe 或压缩包 | `RJ01034814\Ver1.0` |
| `layout` | `compact`（紧凑，游戏标识层自身含 exe / 压缩包）｜`standard`（标准） | — |
| 版本 | 一个可启动单元；紧凑型固定 1 个 | — |
| 候选可执行文件 | 版本目录**直接文件**中、未命中排除名单的 `.exe` | `game.exe` |
| 版本内压缩组 | 版本目录直接文件中的压缩包，按分卷正则聚合为组 | `game.part1.rar` + `game.part2.rar` |

**下探固定为两层**（游戏标识层 → 版本标识层），版本标识层以下**不下探**——因此「容器目录继续下探」「深度上限」这两个概念在本版**不存在**。

---

## 3. 数据结构（Rust 侧）

```rust
pub enum GameLayout { Compact, Standard }

pub enum VersionSource { Scan, Manual }

/// 扫描判定出的一个版本
pub struct VersionEntry {
    /// 相对仓库根（紧凑型 = 游戏标识层路径；标准型 = <游戏路径>/<版本目录名>）
    pub relative_path: String,
    /// 目录原名（用户可改显示名，此为默认值）
    pub version_label: String,
    pub game_type: GameType,           // folder / archive（沿用 v1，exe 优先）
    /// 本层文件大小之和（不含子目录）
    pub size_bytes: u64,
    /// 直接文件中的候选 exe（未命中排除名单）
    pub executables: Vec<ExeCandidate>,
    /// 直接文件中的压缩包，按分卷正则聚合
    pub archive_groups: Vec<ArchiveGroup>,
}

pub struct ExeCandidate {
    pub relative_path: String,         // 相对仓库根
    pub file_name: String,
    pub size_bytes: u64,
}

/// 扫描判定出的一个游戏档案
pub struct GameEntry {
    pub relative_path: String,         // 游戏标识层，相对仓库根
    pub layout: GameLayout,
    pub display_name: String,          // 默认 = 目录名
    pub size_bytes: u64,               // = 各版本 size 之和
    pub versions: Vec<VersionEntry>,   // 可为空（反例结构）
}

/// 扫描结果（含可见性告警）
pub struct ScanReport {
    pub games: Vec<GameEntry>,
    pub warnings: ScanWarnings,
}

#[derive(Default)]
pub struct ScanWarnings {
    /// 根下散落文件数（`root/xxx.zip`：不建档）
    pub root_loose_files: u32,
    /// 因自身含 exe/压缩包而被截断、且其子目录也含 exe/压缩包的目录数
    pub truncated_with_version_dirs: u32,
    /// 目录枚举失败数（权限 / 长路径 / 网络失败）
    pub enum_failed: u32,
}
```

`ArchiveGroup` / `ArchiveFile` 沿用 v1 定义（[scanner.rs](../src-tauri/src/scanner.rs)），但**只作为版本内部结构**，不再产生独立档案。

---

## 4. 扫描算法

### 4.1 伪代码

```text
fn scan_repository(root, config, control, start_index) -> ScanReport:
    entries = list_dir(root)                       // 顶层条目，构成进度分母 M
    for (i, de) in entries:
        if i < start_index: continue               // 断点续扫
        if !control.wait_until_ready(): return Cancelled
        if de 不是目录:
            if is_archive(de.name): warnings.root_loose_files += 1
            continue                               // 根下散落文件不建档
        if de.is_reparse: continue                 // 不跟随 symlink / junction
        if hit_root_exclude(de.name, config): continue   // #recycle / @eaDir / ...
        game = classify_game(de, config, &mut warnings)
        games.push(game)
        on_progress(done = i+1, total = M, found = games.len(), current = de.name)

fn classify_game(dir, config, warnings) -> GameEntry:
    files = list_dir(dir)                          // 只列一层，不递归
    direct_exes     = files.where(is_file && is_exe && !excluded(...))
    direct_archives = files.where(is_file && is_archive(config白名单))

    // ---- 紧凑型：游戏标识层自身直接含 exe / 压缩包 ----
    if direct_exes 非空 or direct_archives 非空:
        layout = Compact
        version = VersionEntry {
            relative_path = rel(dir),
            version_label = dir.name,
            game_type     = direct_exes 非空 ? Folder : Archive,
            size_bytes    = sum(files 中文件的 size),
            executables   = direct_exes,
            archive_groups= group_by_part(direct_archives),
        }
        // 截断可见性：子目录还藏有 exe / 压缩包 → 计一条 warning（判定不变）
        for sub in files.where(is_dir):
            if sub.is_reparse: continue
            if list_dir(sub) 含 exe 或压缩包: warnings.truncated_with_version_dirs += 1; break
        return GameEntry{ rel(dir), Compact, versions = [version] }

    // ---- 标准型：逐个直接子目录判定是否为版本 ----
    versions = []
    for sub in files.where(is_dir):
        if sub.is_reparse: continue
        sub_files = list_dir(sub)                  // 一层，不递归
        s_exes  = sub_files.where(is_file && is_exe && !excluded(...))
        s_arch  = sub_files.where(is_file && is_archive(config白名单))
        if s_exes 空 and s_arch 空:
            continue                               // 不构成版本（对象定义）
        versions.push(VersionEntry{ rel(sub), sub.name,
            game_type = s_exes 非空 ? Folder : Archive,
            size_bytes = sum(sub_files 中文件 size),
            executables = s_exes, archive_groups = group_by_part(s_arch) })
    // 版本数可为 0（反例结构）——仍建档（强断言）
    return GameEntry{ rel(dir), Standard, versions }

fn scan_single(root, game_relative_path, ...) -> GameEntry:
    // 单档案重扫：只跑 classify_game 一次，其余逻辑与全量一致
```

### 4.2 硬性约束

1. **不递归**：`classify_game` 只对「游戏标识层」调用一次 `list_dir`，对每个候选子目录再调用一次 `list_dir`。**禁止**再往下。
2. **不读 exe 内容**（性能红线，沿用 v1）。
3. **单次列目录取全部元数据**（Dirent 元数据同时用于 exe 筛选、压缩包识别、大小累加），禁止重复 stat。
4. **候选过滤**：exe 须过排除名单（沿用 v1 名单 + 实测补充项）；压缩包须过白名单（`.zip .7z .rar .tar .gz .bz2 .xz .zst .iso`，**不含** `.bin` / `.cue`）。
5. **`game_code` / `cover_path` 禁止自动填写**（D19 边界，v1 禁令不变）。
6. **进度口径不变**（D8）：分母 = 仓库根顶层条目数；旁附"已发现 N 个 / 当前：XXX"。

### 4.3 目录排除名单（仓库根级）

```text
默认值（可配置，存 settings.root_excluded_dirs，JSON 数组）：
  #recycle           群晖回收站
  @eaDir             群晖缩略图 / 索引
  #snapshot          群晖快照
  $RECYCLE.BIN       Windows 回收站
  System Volume Information
  lost+found         ext4
  .Trash*            部分 NAS
  .DS_Store
```

匹配规则：**仅对仓库根的直接子目录名**做不区分大小写的匹配（`*` 支持简单通配）。与 v1 的 `exclude_path_segments`（作用于 exe 路径段）是**两套独立配置**，不得混用。

---

## 5. SQLite 表结构（`SCHEMA_VERSION = 2`）

> **v1 数据不迁移**〔珱垣确认〕：检测到 `schema_version = 1` 时，**清空并重建**（`DROP TABLE` 旧表 → 建新表 → 写 `schema_version = 2`），并在 UI 提示「升级到 v1.1 后既有档案已清空，请重新扫描仓库」。MVP 阶段数据仅测试用。

```sql
-- 游戏档案（= 游戏标识层，一层一条）
CREATE TABLE IF NOT EXISTS games (
    id                  INTEGER PRIMARY KEY AUTOINCREMENT,
    repository_id       INTEGER NOT NULL REFERENCES repositories(id) ON DELETE CASCADE,
    relative_path       TEXT    NOT NULL,           -- 游戏标识层，相对仓库根
    layout              TEXT    NOT NULL CHECK (layout IN ('compact','standard')),
    display_name        TEXT    NOT NULL,
    game_code           TEXT,
    cover_path          TEXT,
    size_bytes          INTEGER NOT NULL DEFAULT 0, -- = 各版本 size 之和（冗余汇总，扫描重算）
    selected_version_id INTEGER,                    -- 启动按钮当前选中版本；无外键约束，应用层维护
    status              TEXT    NOT NULL DEFAULT 'ok' CHECK (status IN ('ok','missing')),
    created_at          TEXT    NOT NULL,
    updated_at          TEXT    NOT NULL,
    UNIQUE (repository_id, relative_path)
);

-- 版本（= 版本标识层；紧凑型为游戏标识层自身）
CREATE TABLE IF NOT EXISTS game_versions (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    game_id       INTEGER NOT NULL REFERENCES games(id) ON DELETE CASCADE,
    relative_path TEXT    NOT NULL,                 -- 相对仓库根
    version_label TEXT    NOT NULL,                 -- 目录原名（扫描刷新）
    display_name  TEXT    NOT NULL,                 -- 用户可改，默认 = version_label
    note          TEXT,                             -- 用户备注
    game_type     TEXT    NOT NULL CHECK (game_type IN ('folder','archive')),
    size_bytes    INTEGER NOT NULL DEFAULT 0,
    source        TEXT    NOT NULL DEFAULT 'scan' CHECK (source IN ('scan','manual')),
    status        TEXT    NOT NULL DEFAULT 'ok' CHECK (status IN ('ok','missing')),
    created_at    TEXT    NOT NULL,
    updated_at    TEXT    NOT NULL,
    UNIQUE (game_id, relative_path)
);

-- 版本内候选 / 手填可执行文件
CREATE TABLE IF NOT EXISTS game_version_executables (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    version_id    INTEGER NOT NULL REFERENCES game_versions(id) ON DELETE CASCADE,
    relative_path TEXT    NOT NULL,                 -- 相对仓库根（可能超出版本目录）
    file_name     TEXT    NOT NULL,
    size_bytes    INTEGER NOT NULL DEFAULT 0,
    is_selected   INTEGER NOT NULL DEFAULT 0,       -- 该版本的启动项
    source        TEXT    NOT NULL CHECK (source IN ('scan','manual')),
    invalid       INTEGER NOT NULL DEFAULT 0,       -- 1 = 上次重扫已找不到该文件
    created_at    TEXT    NOT NULL,
    UNIQUE (version_id, relative_path)
);

-- 版本内压缩包文件（v2 搬运依据；group_key 仅表示分卷组）
CREATE TABLE IF NOT EXISTS game_version_files (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    version_id INTEGER NOT NULL REFERENCES game_versions(id) ON DELETE CASCADE,
    group_key  TEXT    NOT NULL,                    -- 分卷组键（= 去后缀主体名）
    file_name  TEXT    NOT NULL,
    part_index INTEGER,
    size_bytes INTEGER NOT NULL,
    UNIQUE (version_id, file_name)
);

-- 黑名单（删除的档案 / 版本）
CREATE TABLE IF NOT EXISTS blocklist (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    repository_id INTEGER NOT NULL REFERENCES repositories(id) ON DELETE CASCADE,
    kind          TEXT    NOT NULL CHECK (kind IN ('game','version')),
    relative_path TEXT    NOT NULL,                 -- 档案：游戏标识层；版本：<游戏路径>/<版本目录名>
    created_at    TEXT    NOT NULL,
    UNIQUE (repository_id, kind, relative_path)
);

CREATE INDEX IF NOT EXISTS idx_games_repo_status      ON games(repository_id, status);
CREATE INDEX IF NOT EXISTS idx_games_display_name     ON games(display_name);
CREATE INDEX IF NOT EXISTS idx_versions_game          ON game_versions(game_id);
CREATE INDEX IF NOT EXISTS idx_exes_version           ON game_version_executables(version_id);
CREATE INDEX IF NOT EXISTS idx_vfiles_version         ON game_version_files(version_id);
CREATE INDEX IF NOT EXISTS idx_blocklist_repo         ON blocklist(repository_id, kind);

-- settings 新增键：root_excluded_dirs（JSON 数组）
-- settings 作废键：max_depth（v1.1 无深度概念，读到时忽略）
-- v1 的 game_files 表不再创建（被 game_version_files 取代）
```

**保留不变的表**：`repositories`、`repository_locators`、`scan_jobs`、`scan_checkpoints`、`settings`。

---

## 6. 重扫保护矩阵

> 原则：**扫描只刷新"客观事实"，用户的每一次裁决永久保留。**

| 字段 | 全量重扫 | 单档案重扫 | 备注 |
| --- | --- | --- | --- |
| `games.relative_path` | 匹配键 | 匹配键 | 不修改 |
| `games.layout` | **覆盖** | 覆盖 | 目录结构变了就该变 |
| `games.size_bytes` | **覆盖**（版本和） | 覆盖 | — |
| `games.display_name` | **不覆盖** | 不覆盖 | 用户字段 |
| `games.game_code` | **不覆盖** | 不覆盖 | 用户字段 |
| `games.cover_path` | **不覆盖** | 不覆盖 | 用户字段 |
| `games.selected_version_id` | **不覆盖**；指向的版本消失则置空 | 同左 | 见 §7.3 |
| `games.status` | 覆盖（`ok` ↔ `missing`） | 覆盖 | 命中黑名单则整条跳过 |
| `game_versions.relative_path` | 匹配键 | 匹配键 | 不修改 |
| `game_versions.version_label` | **覆盖**（目录原名） | 覆盖 | 目录改名即反映 |
| `game_versions.display_name` | **不覆盖** | 不覆盖 | 用户可改 |
| `game_versions.note` | **不覆盖** | 不覆盖 | 用户备注 |
| `game_versions.game_type` | **覆盖** | 覆盖 | — |
| `game_versions.size_bytes` | **覆盖** | 覆盖 | — |
| `game_versions.status` | 覆盖 | 覆盖 | 本次未扫到 → `missing`，**不删除** |
| `game_versions.source = manual` | **永不删除、永不删除版本本身** | 同左 | 仅刷新 `status` |
| `game_version_executables`（`source=scan`） | **全量替换** | 全量替换 | 扫描发现的候选清单 |
| `game_version_executables`（`source=manual`） | **保留** | 保留 | 手填的 |
| `is_selected` | **保留**；被选文件在新列表中不存在 → 该行 `invalid = 1`，**不自动改选** | 同左 | UI 显示「启动项失效」 |
| `game_version_files` | **全量替换** | 全量替换 | 压缩包清单 |

**删除语义**：扫描**从不删除**行（除 schema 重建）。本次未出现的版本 → `status='missing'`（UI 灰显）。用户想彻底移除 → 走「删除 + 黑名单」（§7）。

---

## 7. 行为细则

### 7.1 黑名单

- **优先级最高**：重扫时，`blocklist` 中命中的 `game` → 整条跳过（不建、不刷新、不产生 missing）；命中的 `version` → 该版本跳过（不建、不刷新、不产生 missing）。
- **级联**：删除档案时，同时写入 1 条 `kind='game'` 记录 + 其下**所有已存在版本**的 `kind='version'` 记录（防止后续手动新建同名版本时误恢复）。
- **取消拉黑**：删除黑名单记录 → 下次扫描按常规逻辑重新判定（手填数据不恢复）。
- **改名失效**：黑名单按相对路径匹配，目录改名后旧记录失效（会重新扫出来）——设置页需给知情说明。

### 7.2 手动版本

- 创建：`create_manual_version(game_id, relative_path, executable_relative_path)`。
- 约束：
  - `relative_path` 必须是该游戏目录下的**直接子目录**（或等于游戏目录自身，用于"把紧凑型当标准型"的场景）；
  - 目录必须真实存在，否则拒绝并报错；
  - `executable_relative_path` 必须位于该版本目录之下（**允许更深层**，如 `RJ004/Game/data/game.exe`），且文件存在；
  - 两者的 `source` 均写 `manual`。
- 行为：手动版本与扫描版本在 UI 上同等对待，加「手动」标记；重扫从不删除。

### 7.3 选中版本与选中启动项

- `games.selected_version_id` 落库（〔AI 决策·珱垣默许〕）。
- 版本被删除（用户删除 / 黑名单）时：若它是当前选中版本 → 自动切换到该游戏剩余版本中的第一个（按 `version_label` 排序）；无剩余 → 置 `NULL`，启动按钮显示「无可用版本」并禁用。
- `is_selected` 在**每个版本内**唯一（应用层保证：设置新选中时把**同版本**其他行置 0）；**不同版本的启动项配置各自独立**，切换版本不会清掉其他版本的配置。
- **启动列表的显隐**〔珱垣确认〕：启动按钮下拉列表**只列出「已选择启动项」的版本**；未配置启动项的版本**不出现**在列表中（在版本管理里为其选定启动项后才会出现）。当前版本未配置启动项时，按钮显示「未配置启动项」并禁用。
- 选中的 exe 在新一轮扫描中消失 → `invalid = 1`，UI 显示「启动项失效（文件已不存在）」，**绝不自动改选**。

### 7.4 0 版本档案

- `layout = standard` 且 `versions` 为空 → 档案照建，`size_bytes = 0`。
- UI：版本区显示「未识别到版本」+「手动新建版本」入口；启动按钮「无可用版本」禁用；卡片墙仍显示该档案（用户可删除 / 拉黑）。

### 7.5 单档案重扫

- 命令：`scan_single_game(game_id)`。
- **占用全局扫描槽**（D9）〔珱垣确认〕：与全量扫描互斥；扫描期间设置页的「开始」按钮同样变为不可用。
- 只对目标游戏执行一次 `classify_game`，落库规则与全量一致（走 §6 同一套 `apply_versions`）。
- 目录已不存在 → 档案与版本标 `missing`，返回明确错误信息（不静默）。

---

## 8. IPC 协议

### 8.1 命令（前端 → Rust）

| 命令 | 入参 | 说明 |
| --- | --- | --- |
| `list_games` | `repository_id?` | **1 游戏 = 1 行**（卡片墙用）；返回含 `layout` / `size_bytes` / `version_count` / `selected_version_label?` |
| `get_game` | `game_id` | 返回档案 + **版本列表**（含每版本的 exe 候选与选中项）+ `absolute_path` |
| `update_game` | `game_id, display_name, game_code?, cover_path?` | 沿用 v1 |
| `update_version` | `version_id, display_name, note?` | 改版本名 / 备注 |
| `set_selected_version` | `game_id, version_id` | 启动按钮选中版本 |
| `set_selected_executable` | `version_id, executable_id` | 设置该版本启动项 |
| `list_executable_candidates` | `relative_path, max_depth?` | 手动配置用：列出指定目录下（可递归 `max_depth` 层）的 exe 候选 |
| `create_manual_version` | `game_id, relative_path, executable_relative_path, display_name?` | 手动新建版本 |
| `add_manual_executable` | `version_id, relative_path` | 给已有版本补充启动项 |
| `delete_version` | `version_id` | 删除 + 写黑名单 |
| `delete_game` | `game_id` | 删除 + 写黑名单（级联版本） |
| `list_blocklist` | — | 设置页黑名单 |
| `unblock` | `blocklist_id` | 取消拉黑 |
| `scan_single_game` | `game_id` | 单档案重扫（占扫描槽） |
| `start_scan` / `pause_scan` / `resume_scan` / `cancel_scan` / `scan_status` | 沿用 v1 | — |

### 8.2 事件（Rust → 前端）

| 事件 | 载荷 | 说明 |
| --- | --- | --- |
| `scan://progress` | 沿用 v1 + `scope: 'full' \| 'single'` | 单档案重扫也推此类进度（`total` 恒为 1） |
| `scan://finished` | `ScanSummary` + `warnings{root_loose_files, truncated_with_version_dirs, enum_failed}` | **新增 warnings 明细**，扫描结束在设置页展示 |
| `scan://failed` | 沿用 v1 | — |

---

## 9. 测试用例矩阵

> 与规划文档 ACC 一一对应；`ACC-1.1-xx` 为验收口径，本节为可执行测试。

| # | 构造 | 断言 |
| --- | --- | --- |
| T-1.1-01 | `RJ001/{game.exe, data/*}` | 1 档案 / compact / 1 版本 / `executables=[game.exe]`（对应 ACC-01） |
| T-1.1-02 | `RJ002/{Ver1.0/game.exe, Ver2.0/game.exe}` | 1 档案 / standard / 2 版本（ACC-02，替代 v1 T32） |
| T-1.1-03 | `RJ003/{Ver1.0/game.exe, Ver2.0/game.7z}` | folder 版本 + archive 版本各 1（ACC-03） |
| T-1.1-04 | `RJ004/{DLC,ArtBook,Game/data/game.exe}` | 1 档案 / standard / **0 版本** / 无 executables（ACC-04，反例） |
| T-1.1-05 | `root/空目录/` | 1 档案 / standard / 0 版本（ACC-05，强断言） |
| T-1.1-06 | `root/{#recycle, @eaDir, System Volume Information}/` | 均不建档（ACC-06） |
| T-1.1-07 | `root/散装.zip` | 不建档；`warnings.root_loose_files == 1`（ACC-07） |
| T-1.1-08 | `RJ005/{game.exe, Ver2.0/game.exe}` | compact + 1 版本，且 `warnings.truncated_with_version_dirs == 1`（ACC-08） |
| T-1.1-09 | `RJ006/Ver1.0/{a.zip, b.zip}` | 1 版本 / 1 archive 组（若同主体）或 2 组；`size = a+b`（ACC-09） |
| T-1.1-10 | `Ver1.0/{game.exe, data.zip}` | `game_type == folder`（ACC-10，exe 优先） |
| T-1.1-11 | 删除版本 → 重扫 | 版本不再出现；`blocklist` 有记录；取消拉黑后恢复且 exe 配置为空（ACC-11） |
| T-1.1-12 | 删除档案 → 重扫 | 档案与其版本均不出现；`blocklist` 有 1 条 game + N 条 version（ACC-12） |
| T-1.1-13 | 手填字段 → 全量重扫 | 名称/编号/封面/版本名/备注/选中版本/选中 exe 全部保留（ACC-13，核心回归） |
| T-1.1-14 | 选中的 exe 被移走 → 重扫 | 该行 `invalid == 1`，`is_selected` 保持不变（ACC-14） |
| T-1.1-15 | 对 T-1.1-04 的档案 `create_manual_version("Game", "Game/data/game.exe")` | 手动版本存在；重扫后仍在（ACC-15） |
| T-1.1-16 | `scan_single_game` | 仅目标档案刷新；扫描槽被占用期间 `start_scan` 返回忙（ACC-16） |
| T-1.1-17 | 启动按钮 | 返回「未实现」提示，无进程创建（ACC-17） |
| T-1.1-18 | 中文名 / 超 260 字符路径 | 正确建档与识别版本（ACC-18，沿用 v1 T30/T31） |
| T-1.1-19 | 0 版本档案的 DTO | `versions == []`；UI 侧入口存在（ACC-19） |
| T-1.1-20 | 同一游戏 2 版本 → `list_games` | 返回 **1 行**（ACC-20，防回归：不得按版本数出行） |
| T-1.1-21 | 排除名单过滤后无候选：`Ver1.0/{unins000.exe}` | 该子目录**不构成版本** → 0 版本 + manual 兜底可用 |
| T-1.1-22 | 分卷：`Ver1.0/{game.part1.rar, game.part2.rar}` | 1 版本 / `game_version_files` 落 2 行同 `group_key` |

**必须删除的 v1 测试**：`t3_depth_limit`、`t9_archive_not_through_subdir`。`t32_multi_version_each_becomes_archive` 改造为 T-1.1-02。

---

## 10. 实现顺序建议（施工时参考）

1. schema v2 + 数据层（`db.rs` / `store.rs` 重写为 `apply_report`）
2. 扫描器重写（`scanner.rs`：`classify_game` 两层枚举，删除 `walk_dir` 递归）
3. 命令层（`get_game` 带版本、`list_games` 单行、选中版本 / exe）
4. 删除与黑名单（含级联）
5. 手动版本 + 候选 exe 列举
6. 单档案重扫（复用扫描槽）
7. 前端：详情页（版本列表 / 启动按钮占位 / 删除 / 重扫）、设置页（黑名单、warning 展示）

---

## 11. 风险与未决

| # | 风险 | 应对 |
| --- | --- | --- |
| R-1 | **排除名单把唯一 exe 过滤掉** → 该子目录不构成版本，用户不知情 | 该场景下版本数变少但不报错；靠「手动新建版本」兜底；后续可考虑在详情页对 0 版本档案给出引导文案 |
| R-2 | **紧凑截断吞掉子目录版本** | 已有 `truncated_with_version_dirs` warning（§4.1） |
| R-3 | **单层枚举在 HDD/NAS 上的 IO** | 每个游戏 = 1 次根枚举 + N 次子目录枚举，总体**少于** v1 的递归遍历；建议实测 240 目录耗时并与 v1 对比 |
| R-4 | **旧库清空导致用户数据丢失** | 仅 MVP 阶段可接受（珱垣确认不迁移）；升级时必须显式提示 |
| R-5 | `selected_version_id` 无外键，可能悬空 | 应用层在删除版本、重扫、读取时统一校验并修复 |
| R-6 | 「版本目录改名」会让 `version_label` 变化 | `display_name` 不变（用户字段），但 `relative_path` 变化会导致该版本被认为"新版本 + 旧版本 missing" —— 需在 UI 提示"版本目录改名会被识别为一个新版本" |

**未决（需珱垣确认后才能定）**

- 无。当前所有待定项均已按「AI 决策·珱垣默许」落定，列在规划文档的决策清单中，可随时推翻。

---

## 12. 变更记录

| 日期 | 变更 |
| --- | --- |
| 2026-11-02 | 初版：依据 v1.1 设计规划（dsedit 版）建立完整技术规格 |
