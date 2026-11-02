# SakuGame 发布流程（Release Process）

> 每次发版必须按此清单执行。本文件是**AI 侧执行清单**——珱垣侧职责见 `YeonPlans/协作方案/职责分配.md`，两边不重复。
>
> 最近一次执行：2026-11-02 v0.1.0（仅打 tag 留档，**未发 GitHub Release**；首个对外 Pre-release 计划随 MVP v2 发出）

---

## 0. 流程总览

```
敲定版本号 → 更新版本号 → 整理改动清单 → 组装版本简报 → 同步构建产物
    → 维护 CHANGELOG → 生成 Release Notes（详实版） → 验证（测试/构建）
    → 打包冒烟 → git 提交 + 打 tag + 推送 → 通知珱垣「可以发版了」（附简报）
    → 珱垣写 Release 正文并发布 → （如有流水线）确认构建状态 → 发布后收尾
```

---

## 1. 敲定版本号

按[语义化版本（SemVer）](https://semver.org/lang/zh-CN/)选择：

| 类型 | 示例 | 适用场景 |
| --- | --- | --- |
| MAJOR | `3.0.0` | 破坏性变更 / 重大架构重构 / 里程碑式发布 |
| MINOR | `2.3.0` | 向后兼容的新功能 + 修复 + 优化（最常用） |
| PATCH | `2.3.1` | 仅修 Bug，无新功能 |

**决策依据**：是否引入破坏性变更（数据无法平滑迁移、配置格式不兼容、API 不向后兼容）。若仅新增功能 + 修复且数据可平滑迁移，选 MINOR。

**版本号由珱垣拍板**，AI 给建议。

> ⚠️ **未发布版本号吸收规则**：已被写进版本文件 / `CHANGELOG` 但**未打 tag 发版**的版本号，其内容一律并入下一个正式版本，不单独补发；收尾时删掉或改写那个"待发布"区块。

---

## 2. 更新版本号

版本号来源（三处必须一致）：`package.json`、`src-tauri/Cargo.toml`、`src-tauri/tauri.conf.json`。

修改后**必须同步**：

| 位置 | 说明 |
| --- | --- |
| `package.json` / `src-tauri/Cargo.toml` / `src-tauri/tauri.conf.json` | **版本号三处必须一致** |
| `pnpm-lock.yaml` | 锁文件（仅随依赖变更更新，不改版本号） |
| `src-tauri/tauri.conf.json` 的 `version` | 打包时嵌入前端「关于」页，改后必须重新构建 |
| `DSPlans/VerNotes/RELEASE_NOTES_vX.Y.Z.md` | 标题与本次版本号一致 |
| `DSPlans/YeonBriefs/BRIEF_vX.Y.Z.md` | 文件名与「版本信息」 |
| `CHANGELOG.md` | 顶部版本区块（见第 6 节） |

> ⚠️ 版本号变更后**必须重新构建产物**——「关于」页版本号通常是构建时嵌进前端的，不重建会残留旧版本号。

---

## 3. 整理改动清单

基于 git 历史核对本轮内容：

```bash
git tag -l                                       # 已有 tag（命名规范：vX.Y.Z）
git log <上一tag>..HEAD --oneline --no-decorate   # 本轮全部提交
git diff --stat <上一tag>..HEAD                  # 改动规模
```

按提交 type 归类：`feat` → 新增功能、`fix` → Bug 修复、`refactor/perf` → 优化改进。
**重点核对版本窗口内是否有"未写进简报"的 Round**——简报靠日常增量，缺口从这里补。

---

## 4. 组装版本简报（给珱垣）

在 `DSPlans/YeonBriefs/` 下维护 `BRIEF_vX.Y.Z.md`（模板见 `BRIEF_TEMPLATE.md`）：

- [ ] 日常各 Round 的增量节是否齐全（对应第 3 节的提交范围）
- [ ] 组装定稿：版本信息 → 主题（**珱垣已确认**）→ 亮点分组 → 升级注意 → 已知问题 → README 待改点 → 机械清单
- [ ] 「已知问题」如实写，不因不利于宣传而省略
- [ ] 「README 待改点」只指位置与要点，**不代写正文**
- [ ] 完成后**主动通知珱垣**：「可以发版了」，并给出简报路径

> 口径：简报只陈述事实，不写宣传话术。主题句与卖点措辞由珱垣决定。

---

## 5. 维护项目结构文档 / 技术文档（按项目取舍）

- [ ] `PROJECT_TREE.md`（如有）：版本头、新增目录与文件、功能定位索引
- [ ] 用户手册 / FAQ（`DSPlans/VerNotes/wiki.md`）：新增设置项与常见问题
- [ ] 接口文档 / 配置说明（如有）

---

## 6. 维护 CHANGELOG.md

`CHANGELOG.md` 是**版本日志文档**，作为「项目更新状态」的唯一归档处：README 只写当前版本（归珱垣），历史版本一律记在这里。

- [ ] 顶部追加本次版本区块（倒序）：版本号 · 日期 + 本期总结 + 分点亮点（新增功能 / 优化改进 / Bug 修复 / 升级说明 / 已知问题）
- [ ] 若存在未发布的"待发布区块"，按第 1 节规则**并入本版**后删除旧区块
- [ ] 已发布版本的既有文字不改写（勘误除外）
- [ ] 与 Release Notes 的分工：CHANGELOG 是**跨版本一览**（每版几段），`RELEASE_NOTES_vX.Y.Z.md` 是**单版本详述**

---

## 7. 生成 Release Notes（详实技术版）

在 `DSPlans/VerNotes/` 下新建 `RELEASE_NOTES_vX.Y.Z.md`（模板见 `RELEASE_NOTES_TEMPLATE.md`），面向**开发者与愿意看细节的读者**：

- 引言：自上一版本以来做了什么、遗留问题解决情况、数据迁移情况
- `✨ 新增功能` / `🛠️ 优化改进` / `🐛 Bug 修复`：按技术口径分组，可含实现说明
- `⚠️ 已知问题`：如实列出
- `📖 使用说明`：部署 / 运行 / 升级说明（技术性内容；卖点与宣传话术不写在这里）

---

## 8. 验证

发布前必须通过：

```bash
cargo test --manifest-path src-tauri/Cargo.toml
pnpm exec vue-tsc --noEmit
```

> 失败则不发布：先修复，再重新验证。已知的偶发失败（如顺序性单测）需在 Release Notes「已知问题」标注。

---

## 9. 构建产物同步 + 打包冒烟

**本地一键打包（推荐日常测试）**——双击项目根目录的 `build-release.bat`，或命令行：

```bat
build-release.bat             :: 构建绿色版 exe -> release\SakuGame.exe
build-release.bat --run       :: 构建完直接启动
build-release.bat --bundle    :: 额外生成安装包（msi + nsis）
```

手动全量构建（发版正式产物，含安装包）：

```bash
pnpm tauri build
```

- [ ] 构建产物目录已包含**本轮全部前端改动**（提交前用 `git status` 确认产物文件有更新）
- [ ] 打包产物生成，脚本标题 / 版本号正确
- [ ] 冒烟：启动 → 界面正常 → 「关于」页版本号与目标一致
- [ ] （如有）无界面 / headless 模式可正常启动

---

## 10. git 提交 + 打 tag

```bash
git status && git diff --stat     # 确认改动范围
git add <本次预期文件>             # 逐项暂存，避免夹带无关文件
git commit -m "chore(release): vX.Y.Z 发布准备（版本号/产物/CHANGELOG/VerNotes/简报）"
git tag vX.Y.Z
git push origin main
git push origin vX.Y.Z
```

tag 命名必须与历史一致。

> ⚠️ **不夹带珱垣的目录**：`YeonPlans/`（只读区）与根目录零散文件（截图等）不进入发版提交；无关改动留给珱垣自行处理。

---

## 11. 发布（由珱垣执行）

- [ ] AI 通知「可以发版了」，附简报路径与本轮机械清单
- [ ] 珱垣撰写 Release 正文（基于简报自行组织）
- [ ] 珱垣在网页创建 Release：标题 `vX.Y.Z`，tag 选对应 tag
- [ ] 附件上传构建产物
- [ ] 点发布
- [ ] 如有 CI / 镜像流水线：由 tag 或手动触发，到对应平台确认状态

---

## 12. 发布后收尾

- [ ] 确认 tag、Release、（流水线产物）三项状态回传给珱垣
- [ ] 将 `BRIEF_vX.Y.Z.md` 定格（不再追加）；新版本另建新简报文件
- [ ] 宣传 / 贴文 / 预览图 / wiki 条目：**由珱垣决定与执行**，AI 只按需补写 wiki 内容
- [ ] 若有遗留的临时脚本 / 调试产物 / 截图，清理或归入 `_local/`

---

## 附录 A：版本简报结构模板

见 `DSPlans/YeonBriefs/BRIEF_TEMPLATE.md`。

## 附录 B：发布前自查清单

- [ ] `git status` 仅含预期改动，无临时脚本 / 调试产物 / 珱垣的目录
- [ ] 无敏感信息随发布（密钥、内网地址、账号凭据）
- [ ] 版本号一致：来源文件 / 锁文件 / 后端常量 / Release Notes 标题 / 简报文件名 / `CHANGELOG.md` 顶部区块
- [ ] 未发布的旧版本号区块已按规则并入本版
- [ ] `CHANGELOG.md` 已追加本次版本区块（倒序），已发布版本文字未被改写
- [ ] `VerNotes` 详实版与 `YeonBriefs` 简报都已就位，简报已通知珱垣
- [ ] 构建产物已同步（版本号或前端改动后必做）
- [ ] Release Notes「已知问题」如实填写
- [ ] 不代写 README 与宣传文案

## 附录 C：文档职责边界

| 文档 / 区域 | 维护者 | 说明 |
| --- | --- | --- |
| `README.md` | **珱垣** | AI 不写、不改；只在简报里列「待改点」 |
| `YeonPlans/` | **珱垣** | **AI 只读**，未经显式点名授权不得修改 |
| `CHANGELOG.md` | AI | 跨版本一览 |
| `DSPlans/VerNotes/RELEASE_NOTES_vX.Y.Z.md` | AI | 单版本详实技术版 |
| `DSPlans/YeonBriefs/BRIEF_vX.Y.Z.md` | AI | 给珱垣的精炼素材 |
| `PROJECT_TREE.md` / `wiki.md` | AI | 技术文档 |
| Release 正文 / 贴文宣传 / 预览图 | **珱垣** | 产品与展示口径 |
