# SakuGame

把 NAS 上的游戏仓库变成可浏览的本地游戏库。

## 定位

桌面应用（**Tauri v2 + Rust 后端 + Vue 3 前端**）。核心场景：游戏仓库存放在 NAS（HDD）上，浏览与管理在本机进行，避免 HDD 随机寻道拖慢体验。

## 当前阶段：MVP v1 — 扫描建档

- 配置多个仓库路径（支持 **盘符 / IP / 主机名** 三种定位方式，按可达性依次探测）
- 扫描仓库目录，自动识别游戏并建档（文件夹型 / 压缩包型）
- 卡片墙浏览、详情页、手动配置名称 / 编号 / 封面

**不做**（留给后续版本）：启动游戏、搬运到本地、删除游戏、存储管理。

## 项目结构

```
├── YeonPlans/      产品规划（作者撰稿，AI 只读）
├── DSPlans/        工程规格、决策记录、实现文档
├── src/            Vue 3 前端
├── src-tauri/      Rust 后端（Tauri）
└── public/         静态资源
```

## 开发

前置：Node 20+ / pnpm / Rust stable (MSVC) / WebView2 Runtime

```bash
pnpm install
pnpm tauri dev      # 开发
pnpm tauri build    # 打包
```

## 文档索引

| 文档 | 说明 |
| --- | --- |
| `YeonPlans/版本规划/MVPv1设计规划2.0.md` | 产品规划与决策表 |
| `DSPlans/MVPv1扫描器规格.md` | 扫描器实现规格（判定规则、数据结构、IPC） |
| `DSPlans/MVPv1施工就绪度评估.md` | 决策记录 D1–D20 |
| `DSPlans/Z-Game实测探测报告.md` | 真实仓库实测与判定机制修正 |
