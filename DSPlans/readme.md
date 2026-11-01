# DSPlans —— AI 文档区

本目录用于存放**由 AI 编写**的各类文档。
**只能由 AI 编写与维护，珱垣不直接改动本目录内容**；与之相对，珱垣手写区见根目录 `YeonPlans/`。

## 一、目录层级

```
DSPlans/
├── plans/            # 施工方案：功能开发 plan、路线图、早期规划（Round 系列方案全部在此）
├── Problems/         # 问题记录：反馈问题的诊断、修复说明、答疑话术
├── VerNotes/         # 发布文档：RELEASE_NOTES（详实版）、发布流程、用户 FAQ
├── YeonBriefs/       # 版本简报：给珱垣写 README / Release 正文的精炼素材
└── readme.md         # 本规则文件
```

| 子目录 | 收纳内容 | 典型文件 |
| --- | --- | --- |
| `plans/` | 开工前的施工方案与规划 | `round49-bookmark-redesign-plan.md`、`初版目录树.md` |
| `Problems/` | 问题反馈的处理过程与结论 | `Q1-handling.md` |
| `VerNotes/` | 面向发布的详实说明与流程 | `RELEASE_NOTES_vX.Y.Z.md`、`wiki.md`、`RELEASE_PROCESS.md` |
| `YeonBriefs/` | 给珱垣的精炼版本简报 | `BRIEF_vX.Y.Z.md` |

## 二、命名约定

- 施工方案：`roundN-<主题>-plan.md`（N 为递增轮次），或 `<主题>-plan.md`（无轮次体系时）
- 单版本详述：`RELEASE_NOTES_vX.Y.Z.md`
- 版本简报：`BRIEF_vX.Y.Z.md`（与 Release Notes 同版本后缀）
- 问题记录：`Q<序号>-<主题>.md` 或 `<主题>-diagnostic-report.md`

## 三、与 YeonPlans 的分工

- `YeonPlans/`：珱垣本人落笔的决策、思路与学习笔记，**AI 禁止改动其内容**。
- `DSPlans/`：AI 产出的方案、诊断、发布文档，**珱垣不直接改动**。
- 双方沟通产物：AI 若需要给珱垣「可读的结论」，写进 `DSPlans/YeonBriefs/`，不改 `YeonPlans/`。

新增文档时按上表归入对应子目录；暂无法归类的临时内容可先放本目录根，稳定后再归位。

## 四、维护约定

- 本目录是**长期档案**：已完成的方案不删除，作为后续追溯依据；结论性内容同步进 `CHANGELOG.md` 或对应版本文档。
- 其中的「实施记录」应可复现（写清时间、提交范围、验证方式），不写猜测。
- 敏感内容（账号、密钥、内网地址）一律不入库，必要时放本地 `_local/` 并在文档中标注。
