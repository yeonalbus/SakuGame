# AGENTS.md — SakuGame 项目协作约定

本文件是所有 AI 编码助手（Claude Code / Roo Code / Cline / DSH 等）的项目级工作约定，开始任何任务前先阅读。**核心要求：每次任务收尾必须「提交 + 推送」到远程，禁止只提交不推送。**

## 项目速览

- SakuGame：前端 Vue 3（TypeScript + Vite），后端 Rust（Tauri v2 + rusqlite/SQLite），详见 `README.md` 与 `PROJECT_TREE.md`
- 远程仓库：https://github.com/yeonalbus/SakuGame（默认分支 `main`）
- 工作目录（源码仓库）：`G:\SakuProjects\SakuGame`

## Git 工作流（每次任务收尾必做）

1. **验证**：改完先验证——后端 `cargo test --manifest-path src-tauri/Cargo.toml`（真实仓库验收测试默认 `#[ignore]` 跳过，需要时执行 `cargo test --manifest-path src-tauri/Cargo.toml --test real_repo_scan -- --ignored`，且本机须有可用仓库路径）；前端 `pnpm exec vue-tsc --noEmit`（或 `pnpm build`，含类型检查与 `vite build` 构建）。文档类改动无需代码验证，但需检查改动范围。
2. **检查改动**：`git status` + `git diff --stat` 确认范围与是否混入无关文件。
3. **安全检查**：
   - 确认没有应被忽略的运行时数据 / 构建产物 / 测试素材被暂存（见下方「不可入库」清单，禁止 `git add -f` 强加）。
   - 改动不得包含账号凭据 / 密钥 / 敏感值。
   - 发现敏感文件已被跟踪：`git rm --cached <文件>` 移出 + 追加 `.gitignore` 后继续。
4. **提交**：`git add -A` → `git commit`。Commit Message 遵循 Conventional Commits 中文语义化：`<type>(<scope>): <中文描述>`，type ∈ feat / fix / refactor / style / docs / test / chore。一条 commit 一件事，不夹带无关文件；改动大时按模块拆分。**`YeonPlans/` 是珱垣独占目录，不进入 AI 的提交**（未经显式授权的改动留给珱垣自行处理）。
5. **推送**：`git push`；远程有新提交则先 `git pull --rebase` 再推。
6. **失败处理**：push 失败（网络 / 认证 / 冲突）→ **保留本地提交**，向用户明确报告原因与可重试命令，不静默、不删提交。

## 不可入库清单（示例，按项目补充并落到 `.gitignore`）

- 运行时数据：数据库文件（`*.db*`）、运行时生成的数据目录、缓存目录、日志目录
- 本地资产：测试素材 / 参考源码 / 本机实例 / 图标备份等本地专用目录
- 账号凭据：任何测试账号文档、`.env`、密钥文件
- 构建产物：构建输出目录、打包产物（`.exe` / `.zip` 等）、`*.tsbuildinfo`
- 工具目录：AI 工具链本地配置目录（如 `.dsh/`）
- 调试输出：临时脚本输出目录

## 文档职责边界

- **`README.md` 由珱垣本人维护**：AI 只读、不写、不改。发版时只在 `DSPlans/YeonBriefs/BRIEF_vX.Y.Z.md` 的「README 待改点」里列出需要改的位置与要点，不代写正文。
- **`YeonPlans/` 是珱垣独占目录**：AI **只读**。仅当珱垣在对话中显式点名授权某个文件时，AI 才可修改；未授权一律不碰。
- **AI 维护的技术文档**：`CHANGELOG.md`、`DSPlans/VerNotes/`（详实 Release Notes、`wiki.md`、发布流程）、`DSPlans/YeonBriefs/`（给珱垣的版本简报）、`PROJECT_TREE.md`。
- **宣传与展示口径归珱垣**：GitHub Release 正文、贴文、预览图，AI 不代写宣传稿。
- 发布流程见 `DSPlans/VerNotes/RELEASE_PROCESS.md`；双方职责见 `YeonPlans/协作方案/职责分配.md`（只读参照）。
- 目录归属与规则见 `DSPlans/readme.md` 与 `YeonPlans/注意.md`。

## 红线

- 禁止提交 `.env`、密钥、测试账号凭据；线上账号勿改密、勿删库
- 推送前不确定的文件先问用户，不擅自决定
- 破坏性操作（删库、批量删除文件、强推远程分支）必须先取得珱垣明确同意
