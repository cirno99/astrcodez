# AstrCode / dsh / Pi 设计理念对比报告

> 调研时间：2026-10（沙箱内可观测到的最新发布物）
> 版本快照：AstrCode `astrcodez` 工作区 `dev` 分支 @ `3972a948`；dsh `@deepseek-ai/dsh@0.2.0-rc.2`（`next` 标签，另有 `alpha` 0.2.1-alpha.2）；pi `@mariozechner/pi-coding-agent@0.73.1`
> 取证方式：本仓库源码/文档（file:line）+ npm 发布物（package tarball 内的 README/`.d.ts`/`package.json`）+ 作者一手博客
> 环境限制：本沙箱内 `github.com` / `raw.githubusercontent.com` 不可达（curl 超时），因此三个项目的**仓库层文档**（如 dsh 的 `docs/subsystems/core.md`）无法直接读取。凡标注「npm 发布物」的引用，均为各项目官方发布的包内文档，属一手来源，但只是发布时点的快照。所有此类限制在 §6 汇总。

---

## 0. 指代确认

三个名称都存在歧义，以下是检索后确定的指代、依据与取舍理由。

### 0.1 AstrCode（本报告简称 astrcode）

| 项 | 内容 |
|---|---|
| 确切标识 | 本地工作区 `/home/cirno99/Code/Rust/astrcodez`；`git remote -v` 记录 `origin = git@github.com:cirno99/astrcodez.git`，`upstream = https://github.com/whatevertogo/astrcodey` |
| 形态 | Rust workspace，`crates/` 下 33 个 crate（`ls -1 crates \| wc -l` = 33，与 README 自述一致），AGPL-3.0 |
| 依据 | 仓库根 `README.md:1-14`（"AstrCode … A Rust-built AI coding agent platform"）、`README.md:310-312`、`git remote -v` 输出 |

**歧义与取舍**：仓库名有两个变体（`astrcodey` 上游 / `astrcodez` 本地、`astrcodey-extensions` 扩展工作区），同一项目的不同副本。本报告以**当前工作区的 `dev` 分支**为准（这也是唯一可完整取证的一手副本），并在需要时用上游 remote 名说明来源差异。

### 0.2 dsh（本报告简称 dsh）= DeepSeek Harness

| 项 | 内容 |
|---|---|
| 确切标识 | npm `@deepseek-ai/dsh`（bin `dsh`，latest/next `0.2.0-rc.2`）；`package.json` 的 `repository` 字段指向 `git+https://github.com/deepseek-ai/deepseek-harness.git`（`directory: apps/cli`）；MIT |
| 作者/组织 | `@deepseek-ai` scope（维护者含 `tianyicui-deepseek`，见 registry 元数据） |
| 依据 | registry 元数据 `https://registry.npmjs.org/@deepseek-ai/dsh`（HTTP 200，`dist-tags`/`repository`/`maintainers` 字段）；包内 `package.json`、`README.md` |

**歧义与取舍**：`dsh` 这个名字在 npm 上另有一个**完全无关**的包 —— `https://registry.npmjs.org/dsh`，描述为 "A shell written in JavaScript"，作者 Robert Eisele，v1.0.1。排除依据是上下文一致性：本工作区 `crates/astrcode-session/src/repetition_guard.rs:12` 提到「dsh-loop-guard（DSH 的思考循环守护）」，`docs/provider-request-rewrite-chain-design.md:11` 提到 dsh 的 waterfall 机制位于 `vendor/cordis/src/events.ts`，而 `@deepseek-ai/dsh` 的依赖里确实存在 `@deepseek-ai/cordis`（`vendor/cordis` 目录），两处证据互相咬合。另一处提及 `github.com/sunjiefeng/deepseek-harness`（第三方插件 README 的链接），非官方仓库，不采用。

### 0.3 Pi（本报告简称 pi）= pi-coding-agent

| 项 | 内容 |
|---|---|
| 确切标识 | npm `@mariozechner/pi-coding-agent`（bin `pi`，v0.73.1）；仓库 `git+https://github.com/badlogic/pi-mono.git`；官网 `pi.dev`；MIT |
| 作者 | Mario Zechner（`badlogic`），设计说明见其博客 `mariozechner.at` |
| 依据 | registry 元数据 + 包内 `package.json:91`（`"license": "MIT"`）、README 首段（"Pi is a minimal terminal coding harness"）、`docs/compaction.md` 内的源码链接（`packages/coding-agent/...`） |

**歧义与取舍**：`pi` 至少有四层含义，都在同一 monorepo 或同一作者名下：

1. **`@mariozechner/pi`**（v0.70.6）：同样是作者的项目，但 README 描述是「Deploy and manage LLMs on GPU pods … vLLM」，是 GPU pod 部署工具（也带一个 `pi agent` 子命令）。**不采用**。
2. **`@mariozechner/pi-coding-agent`**：终端 coding agent，即本报告的 pi。**采用**。
3. `pi-ai` / `pi-agent` / `pi-tui`：pi-mono 内的下层包，作为 pi 的架构组件讨论。
4. 本仓库 README 的 "BE PI OR BETTER THAN PI" 与 `RALPH-LOOP.md` 里对 pi 插件的引用（`@tmustier/pi-ralph-wiggum`、`cirno99/pi-backup` 的 `sleep-continue`）都指向第 2 项（pi 的 extension/agent 生态），进一步确认取舍。

**无法唯一确定项：无。** 三者均已定位到可验证的发布物；未解决的是**仓库层文档**（见 §6 的取证缺口），不影响概念/设计对比的结论，但会影响部分细节深度。

---

## 1. 概念层对比

### 1.1 核心定位与解决的问题

| 维度 | AstrCode | dsh | Pi |
|---|---|---|---|
| 自述定位 | "A Rust-built AI coding agent platform"；tagline "BE PI OR BETTER THAN PI"（`README.md:3-6`） | "The `dsh` command is the sole supported Node application launcher: profiles are ordered stacks of plugin-bundle patch layers under the user's own overrides. SDK and ACP are profiles, not separate public bins."（包内 `README.md` 首段） | "Pi is a minimal terminal coding harness. Adapt pi to your workflows, not the other way around, without having to fork and modify pi internals."（README 首段） |
| 要解决的问题 | 用 Rust 原生实现一套**多前端**的 agent 平台：桌面（GPUI）、浏览器（wasm）、CLI、HTTP/SSE、ACP 共用同一 agent 内核与会话事实层 | 把「一个 agent 产品」拆成**可组合的 profile**：同一份 harness 通过 bundle + patch 层组合出 web / headless / sdk / acp / desktop 等不同交付形态 | 把「上下文与工作流的控制权」还给用户：核心极小、可替换，让用户用扩展/技能/包适配自己的工作流，而不是被 harness 决定 |
| 反面目标（明确不做/不主张的） | 未在 README 中列「不做清单」；但 AGENTS.md「最小改动」原则与 provider 改写链设计文档体现出「先补类型化原语，再谈通用 waterfall」的取向 | 不提供单一「大而全」的默认应用：SDK/ACP/Web 都是 profile，不是独立 bin；桌面由 Electron carrier 拥有保留名 `desktop` | 明确写死不做的清单：**无 MCP、无子代理、无权限弹窗、无 plan mode、无内置 to-do、无后台 bash**（README "Philosophy" 段，博客同节展开理由） |

**关键结构差异（事实）**：dsh 与 pi 都把「一个可执行文件 + 一套默认行为」视为**可替换的组合结果**，但替换的层级不同 —— dsh 替换的是**进程级启动配置**（profile/patch/bundle），pi 替换的是**核心行为**（extension 可以在运行时替换内置工具、压缩策略、UI）。AstrCode 则相反：把内核（事件日志、turn 循环、工具管线）**固化在宿主**，替换点集中在**显式的 hook 与扩展契约**上。

### 1.2 目标用户与使用场景

| 维度 | AstrCode | dsh | Pi |
|---|---|---|---|
| 首要用户 | 需要「单二进制 + 桌面/Web 多前端 + 可审计事件流」的工程团队；扩展作者（Rust bundled 或跨进程 s5r） | 部署者/平台方与插件作者：需要按环境定制交付形态（Web 服务、SDK 嵌入、ACP、headless、桌面） | 个人资深开发者：要求完全可观测、可控上下文、愿意自己搭工作流 |
| 扩展作者门槛 | 高：bundled 扩展写 Rust（`astrcode-extension-sdk`），磁盘扩展写独立二进制（s5r 协议，stdio 长度前缀帧） | 中：写 TypeScript 插件、声明 peer 版本；用 `dsh plugin --profile X add` 装到 profile 的 `node_modules` | 低：写一个 TypeScript 模块（默认导出函数），放进 `~/.pi/agent/extensions/` 即可热重载 |
| 典型场景 | 桌面/IDE/浏览器共用同一会话；需要 fork/replay 审计；需要沙箱与能力授权的治理场景 | 以 DeepSeek 模型为中心的交付：Web 应用、SDK 嵌入、ACP 接入、无头批处理、Electron 桌面 | 终端内日常编码；容器内 YOLO 运行；需要 RPC/SDK 把 agent 嵌进自己的程序（README 举例 `openclaw/openclaw`） |

### 1.3 关键术语对照

| 概念 | AstrCode | dsh | Pi |
|---|---|---|---|
| 持久事实层 | **EventLog**（append-only JSONL，`seq` 单调，工具结果超限落 artifact） | **session log**（append-only typed events，`SessionSeq`，surfaceOp=append/replace） | **session file**（JSONL，**树**：每 entry 有 `id`/`parentId`，leaf 为活跃位置） |
| 读模型/投影 | **SessionReadModel**（storage 持唯一实例，从事件重放） | `session.deriveMessages()`（增量、带缓存）+ 插件注册的 `@messageProjection` | `SessionManager`（会话树 + 活跃分支内存态；`/tree` 就地移动 leaf） |
| 恢复 | 恢复 = 从最近快照 + 重放事件；fork = 从某 `seq` 开始重放 | 恢复 = `ctx.sessions.resume()` 经 persistence 后端打开日志 + `interruptedTurnClosers` 修补崩溃 turn；fork = 复制精确前缀 | 恢复 = 重新读 JSONL 树；`/fork` 生成新文件，`/clone` 复制当前分支 |
| 执行单元 | **TurnRunner**（无状态，处理完一个 turn 即丢弃） | **Agent + agent-loop**（`ReactLoopAgent`：inbox → turn → step 状态机） | **agent loop**（`pi-agent-core`，循环直到模型不再调工具，**不提供 max steps**） |
| 扩展单元 | **Extension**（bundled 进程内 trait / disk s5r 子进程） | **plugin**（cordis plugin + fiber，可注册 service/tool/prompt/command） | **extension**（TypeScript 模块，`ExtensionAPI`） |
| 组合清单 | `astrcode-bundled-extensions`（编译期注册）+ 磁盘 `extension.json` | profile 的 `package.json`（`dsh.profile.bundles` 有序列表）+ `cordis.patch.yml` | `settings.json` 的 `packages` / `extensions`，或 `pi` manifest |
| 会话级能力差异 | 工具表快照 + 扩展会话状态（`extension_data/<id>/`） | **agent preset**（每个会话挑一个 preset，preset = 一组子插件）+ `ctx.tools.restrict()` 掩码 | `--tools read,grep,find,ls` 之类启动参数 + 扩展自行限制 |
| 权限/安全 | capability 声明 + `HostRouter` 授权 + 审批状态 + 沙箱 | 沙箱模式 + `user-approval` + PTC 的 `sandbox_permissions` + `justification` + 插件版本兼容检查 | **无内置护栏**：YOLO by default，安全交给容器/用户（README + 博客明确说明） |
| 提示词 | 固定九段 pipeline（Identity→…→Additional），稳定段前置以吃 KV-cache 前缀（`README.md:460-469`） | `ctx.systemPrompt` 注册表 + 作用域贡献（agent 级覆盖全局）+ 固定 opener/runtime context/persona/工具顺序 | 极简固定 prompt（含工具说明 <1000 token），只额外注入 AGENTS.md；可用 `.pi/SYSTEM.md` 整体替换 |

---

## 2. 设计层对比

### 2.1 核心抽象模型与心智模型

- **AstrCode：事件溯源 + 无状态运行时。** 架构文档写死了一句判断：「**EventLog 是事实，SessionReadModel 是投影，Agent 是无状态运行时**」（`docs/architecture.md:5`）。`TurnRunner` 从一个 turn 结束即丢弃，崩溃不影响会话，重新投影即可继续（`docs/architecture.md:26-31`）。心智模型接近「数据库 + 查询」：写入是追加事实，读取是投影，fork 是重放。
- **dsh：依赖容器 + 作用域。** 根抽象是 cordis 的 `Context`/plugin/`Fiber`：`ctx.plugin()` 启动插件并返回 fiber，`inject` 声明依赖，fiber dispose 时其副作用（事件监听、service、工具注册）一并回收（`@deepseek-ai/cordis` README）。在此之上叠加**作用域**：`agent.ctx` 上的注册只对该 agent 生效并在销毁时回卷（`dsh-agent` README "Scope registrations to one agent"）。心智模型接近「可动态重组的操作系统」：能力是 service，产品是 profile 组合出来的。
- **Pi：极简 harness + 用户态全权扩展。** 核心只有「一个循环 + 四个工具 + 一段小 prompt」，扩展是任意 TypeScript 模块，可以替换内置工具、压缩策略、UI 组件（README "Extensions"、"Philosophy"）。心智模型接近「库 + 约定」：pi 不定你的工作流，你写代码定 pi。

**同一问题的三种解法示例（上下文压缩）**：
- AstrCode：唯一同步状态机（`astrcode-session::compaction::pipeline`）服务 manual/auto/reactive 三入口，严格 XML contract（`<analysis>` + `<summary>` 固定段），LLM 失败降级到确定性模板，带熔断器与 CAS 持久化，并支持增量合并（`docs/architecture.md:126-149`、`README.md:401-412`）。
- dsh：压缩是**可替换插件**（`dsh-compaction-basic`、`dsh-compaction-tool-result-pruner`），且日志层面「压缩隐藏被取代的条目而不删除」（`dsh-session` README Summary）。
- Pi：压缩是核心内建能力，但**阈值与保留量是配置**（`contextTokens > contextWindow - reserveTokens`，默认 `reserveTokens=16384`、`keepRecentTokens=20k`），并额外提供「分支摘要」用于 `/tree` 切分支（`docs/compaction.md`）。

### 2.2 架构与关键组件划分

| | AstrCode | dsh | Pi |
|---|---|---|---|
| 语言/运行时 | Rust workspace，33 crate 分 7 层（Layer 0 契约 → Layer 6 评测）（`README.md:310-378`） | Node/TypeScript，按能力域切成大量包；CLI 包自身声明 **82 个 runtime 依赖，其中 79 个 `@deepseek-ai/*`** | monorepo 四层：`pi-ai`（LLM 统一层）→ `pi-agent-core`（循环）→ `pi-tui`（终端 UI）→ `pi-coding-agent`（CLI 组装） |
| 分层原则 | 契约（core/protocol/paths/projection）与实现（ai/storage/context）分离；扩展层独立（sdk / s5r-runtime / worker / extensions / bundled-extensions） | 一切皆插件：session、llm、tools、system-prompt、agent、agent-loop、presets、sandbox、web server…各自一个包；`dsh-base` bundle 是「每个 profile 的第一层 patch」 | 层与层之间是纯库依赖，终端 UI 与 agent 核心解耦，SDK/RPC 暴露核心 |
| 前端 | GPUI 桌面 + wasm Web（共享 `astrcode-ui`）+ CLI + HTTP/SSE + ACP | profile 即前端：web / tui / headless / sdk / sdk-minimal / acp / desktop（Electron 保留名） | TUI / print+JSON / RPC（stdin/stdout JSONL）/ SDK |
| 组合/装载 | 编译期（bundled）+ 运行期（s5r 子进程、MCP 进程池） | `--dump-default-config` / `--dump-config` / `--dump-config-schema` 检查组合后的树；HMR 监听 profile manifest 与 patch 文件并「一次串行 reload」（CLI README "Profiles"） | 约定目录自动发现 + `settings.json` 显式路径 + pi package manifest |

补充事实：dsh 的 cordis 是**vendored 分支**（`@deepseek-ai/cordis` 的 `package.json` 里 `repository.directory = vendor/cordis`，`author = Shigma <shigma10826@gmail.com>`，MIT），即 DeepSeek 官方 fork 并改名发布了 Meta-Framework cordis。**（推断）** 这解释了为什么 astrcode 文档引用 dsh 的中间件链位置写作 `vendor/cordis/src/events.ts`。

### 2.3 执行/运行模型

| | AstrCode | dsh | Pi |
|---|---|---|---|
| 循环骨架 | 阶段化 pipeline：prepare context → build provider request → stream LLM → execute tools → loop or return（`README.md:381-391`） | turn/step 状态机：turn 边界打开 durable turn 并原子认领「next-step 输入 + 一条排队 prompt」；step 边界只认领 next-step（`dsh-agent-loop` README "Turn and step flow"） | 单一循环：处理用户消息 → 执行工具 → 回喂 → 直到模型不再调工具；**不提供 max steps**（博客「Minimal agent scaffold」） |
| 输入语义 | `TurnScheduler::deliver_input` 三策略：`InjectIfRunningElseStart`（steer）/ `QueueIfRunningElseStart`（HTTP 连发）/ `StartNew`（`docs/architecture.md:94-106`） | inbox 两种待办列表 `nextTurn` / `nextStep`；`followup()`（排队下一 turn）/ `steer()`（下一步输入）/ `inject()`（只注入上下文、不唤醒）（`dsh-agent` README） | 消息队列（每 turn 后询问排队消息并注入）；`pi-agent-core` 提供 one-at-a-time / all-at-once 两种模式（博客） |
| 工具并发 | 并行批（≤5），工具声明 `ExecutionMode`：读类 Parallel、写类 Sequential（`README.md:414-422`） | 每 step 默认最多 10 个并行安全调用（`DEFAULT_MAX_PARALLEL_TOOL_CALLS = 10`）；独占调用构成 barrier | 无显式并发编排描述；`bash` 工具**同步执行**，长任务交 tmux（博客「No background bash」） |
| 崩溃/中断 | 事件已落盘，重投影即续；工具结果超限转 artifact | `resume()` 读取物理有效日志，并为「turn 中途崩溃」追加 `interruptedTurnClosers`（语义修补属 agent 层职责）；取消是协作式，工具收到 `exec.signal`（`ABORTED_BEFORE_DISPATCH` / `ABORTED`） | 会话文件即状态；被取消的流会追加 `interrupted: true` 锚点？——**未验证**（该表述来自 astrcode 之外的 dsh 文档，pi 侧只确认 JSONL 中记录 model/thinking 变更、compaction、branch summary 等条目） |
| KV-cache 意识 | 稳定 prompt 段前置（`README.md:468-469`）；provider 改写链设计明确「`prompt_build` 保持 append-only，因 `is_stable()` 前缀承诺」（`docs/provider-request-rewrite-chain-design.md:42`） | 为 KV-cache 显式记账：完整 header 只在首次/变更/新 series/替换/恢复时写，未变步骤继承上一次 header；`request/context` 仅在 provider/model/contextWindow/systemPromptUpdate 变化时记录（`dsh-agent-loop` README "Request headers and adapter defaults"） | 未在 docs 中看到同等机制；作者主张的替代方案是「小 prompt + 用户自己控制上下文」（博客）——**（推断）** 这也是一种 cache 策略，但无显式 header 记账 |

### 2.4 扩展机制与集成方式

| | AstrCode | dsh | Pi |
|---|---|---|---|
| 扩展形态 | ① bundled 进程内 Rust（`Extension` trait：`manifest()`/`register()`/`start()`/`stop()`/`health()`）；② 磁盘 s5r 子进程（独立二进制，stdio 长度前缀帧 + JSON `WireMessage`）；③ MCP 工具（`astrcode-extension-mcp`，明确**不**实现 `Extension` trait）（`docs/extension-system.md:8-16`） | cordis 插件：注册 service/tool/prompt section/command/事件监听；工具注册即自动进入 prompt 组装（`dsh-tools` README） | TypeScript 模块：`pi.registerTool()` / `registerCommand()` / `on(event)` / `appendEntry()` / `ctx.ui.*`；可替换内置工具、自定义渲染（`docs/extensions.md:2-16`） |
| 扩展点风格 | **显式 hook 点数得清**：Session*/Turn*/Pre|PostToolUse/BeforeProviderRequest/AfterProviderResponse/Pre|PostCompact/PromptBuild/UserPromptSubmit，且每种有 `Blocking`/`NonBlocking`/`Advisory` 模式（`README.md:432-437`） | **waterfall 中间件链（veto-able）**：`tools/pre-execute`（allow/deny/ask，可重排）→ 单调 guard（`ctx.tools.guard`，返回理由即拒，后续监听器无法翻案）→ `tools/execute`（可包裹 timeout/retry/metrics）→ `tools/post-execute` → `finalizeContent` → 只读 `tools/result`（`dsh-tools` README "Extension points"） | 事件订阅 + 注册：`tool_call` 返回 `{block:true, reason}` 即可拦截；生命周期事件覆盖 resources/session/agent/turn/message/tool/model 各家族（`docs/extensions.md` 目录） |
| 能力/权限模型 | 扩展声明 capability；运行时经 `HostRouter` 授权 `astrcode.*` invoke；磁盘扩展会话状态命名空间隔离（`README.md:430-431`） | 插件声明 DSH peer 范围，启动/安装时校验，不兼容需显式 `version-exemptions`；工具调用经 sandbox 模式 + 审批（`danger-full-access` 或逐次审批）（plugin-manager / tools README） | 无能力模型：扩展 = 全系统权限的任意代码（docs 明确警告），安全靠容器（README "Pi Packages" 安全提示 + 博客「YOLO by default」） |
| 组合/复用单元 | crate 层：sdk + worker + s5r-runtime；磁盘扩展工作区 `astrcodey-extensions`（每个扩展一个独立二进制 + `extension.json`） | bundle（`dsh.profile.bundles` 有序 patch 层）+ agent preset（会话级子插件组合）+ npm 包 | pi package（npm/git，`package.json` 的 `pi` 键声明 extensions/skills/prompts/themes；无 manifest 时按约定目录发现） |
| 热更新 | 未在 README 中看到运行时热重载的声明（有 `on_config_changed()` 热配置与 `POST /api/extensions/reload`） | 有：`dsh-hmr` 监听 profile manifest 与两处 patch 文件，串行重组合；YAML 里启用 HMR 时「改动即生效」 | 有：自动发现目录中的扩展可用 `/reload` 热重载；主题热重载（README） |
| MCP 立场 | **支持**：持久进程池、预热、健康检查、stdio/HTTP 两种传输（`README.md:108-152`） | **支持**：`dsh-mcp-client` + `dsh-mcp-resources`，MCP 工具按 server schema 注册进同一 registry（`dsh-tools` README） | **明确不支持**：理由是 MCP server 的工具描述常驻上下文（Playwright MCP 21 工具 13.7k token 等），替代方案是「CLI 工具 + README，按需读取」（README"Philosophy" + 博客「No MCP support」） |
| 子代理 | 支持：`astrcode-extension-agent-tools`（子代理委托与发现，Claude Code 兼容） | 支持：`dsh-tool-subagent` + `dsh-subagent-fork-in-process` / `spawn-in-process` | **明确不支持**内置子代理工具；建议 `bash` 里再起一个 pi 实例（可用 tmux 提升可观测性）（博客「No sub-agents」） |

### 2.5 依赖与生态

| | AstrCode | dsh | Pi |
|---|---|---|---|
| 运行时依赖 | Rust 生态；宿主依赖不发布到 crates.io，扩展以本地路径依赖宿主（`astrcodey-extensions/README.md`） | Node ≥ 20 + pnpm；插件从 npm 装进 profile 的 `node_modules`（`dsh plugin --profile <name> <pnpm args>`） | Node ≥ 18；包从 npm/git 装（`pi install npm:@x/y`、`git:github.com/u/r@v1`），支持 `-l` 项目本地安装 |
| 生态规模（发布物可数） | 33 crate 在同仓库；扩展工作区另有 8 个 crate（7 个磁盘扩展 + 1 个共用工具 `astrcode-ext-common`，见 `astrcodey-extensions/README.md` 布局表） | 官方 CLI 包直接依赖 79 个 `@deepseek-ai/*` 包（外部依赖仅 3 个：commander/js-yaml/node-addon-require-builtin） | monorepo 多层包；生态靠第三方 `pi-package` 关键词包 |
| 生态交叉 | 扩展工作区已移植 dsh 与 pi 的插件（`astrcodey-extensions/README.md:116`、`:464-466`） | `dsh-llm-pi-ai` 依赖 `@earendil-works/pi-ai@^1.0.2`（描述与 pi-ai 完全一致，版本号与 pi-mono 同轨，如 `legacy-node20: 0.74.2`）——**（推断）** 即 pi 的 LLM 统一层的改名/迁移发行版 | 未发现对 dsh/astrcode 的依赖 |
| 版本/兼容策略 | 扩展 manifest 与宿主 SDK 版本；磁盘扩展协议带 `protocol.s5r` 与 `command` 字段 | 最严格：DSH peer 范围校验 + 精确版本豁免（`compatibility.json`），升级不继承豁免 | 包管理器语义（semver 区间、可 pin 版本、`pi update` 跳过 pinned） |

**（推断）** 三者形成一个有趣的三角：AstrCode 明确把 pi 当作「要超越的基线」（tagline），同时把 dsh 的 waterfall 当作「要吸收的设计参照」（provider 改写链设计文档）；dsh 反过来在自己的 LLM seam 里用了 pi 的 LLM 库；AstrCode 的扩展工作区则同时移植了 dsh 与 pi 的插件（we-need、hashline-edit）。

### 2.6 明确的设计取舍与代价

| | 取舍（有意为之） | 代价 |
|---|---|---|
| AstrCode | 内核固化：事件日志、turn 循环、工具管线都是宿主特权；扩展只能通过显式 hook 与 capability 参与 | ① 扩展门槛高（Rust 或跨进程协议）；② 新交互面需要「补 hook 点」——其设计文档正是在做完缺口分析后决定把 `before_provider_request` 升级为类型化链式原语（`docs/provider-request-rewrite-chain-design.md:9-31`）；③ hook 面窄导致 dsh 那种「一个 waterfall 顺带解决换模型/改 prompt/改工具/重试/流包裹」的能力需要显式补齐 |
| AstrCode | 强制类型化与可静态检查的决策语义（如「工具改写只能收窄」`SessionToolSelection::restrict`、hook 结果用封闭枚举） | 组合语义显式但表达力受限；写扩展的作者要理解宿主类型约束 |
| dsh | 极致可组合：profile = 有序 patch 层栈，插件/工具/压缩/权限/前端全部可换；HMR 改动即生效 | ① 组合复杂度高（层优先级：bundles 顺序 → profile patch → home patch → `--patch`）；② 依赖矩阵管理成本（peer 范围校验 + 豁免 + `pnpm` 构建脚本审批）；③ 包数量巨大、启动链路长；④ 「能力面 = 已装载插件面」意味着没有插件就什么都做不了（agent 服务在 driver 注册前是 inert 的，`dsh-agent` README） |
| dsh | session 默认内存态，持久化作为插件叠加（`dsh-session` README）；请求对象 deep-frozen，扩展不能改写 | 灵活但需要消费者理解「durability 由后端决定」；frozen 请求把改写需求推向更早的 hook（对应 astrcode 文档所说的「waterfall 各阶段」） |
| dsh | 工具呈现可切换（`native` / `ptc` / `both`，PTC 只暴露 `run_code` + 生成的 TS/Python SDK） | 需要 `ctx.ptcRuntime` 与语言 SDK renderer 才能启用；模型侧语义从「调工具」变成「写程序调工具」，可观测性与训练分布都要重新适配 |
| pi | 核心最小化：4 个工具、<1000 token 的 prompt+工具定义、不做 MCP/子代理/plan mode/to-do/后台 bash/权限弹窗 | ① 功能缺口由生态填（用户要自己装包/写扩展）；② YOLO 默认把安全责任外推给容器，误用风险高；③ 只有终端一个一等前端（作者明说「先 TUI，GUI 以后再说」）；④ 扩展是全权限任意代码，供应链风险显著 |
| pi | 会话以「用户可后处理的 JSONL 树」为核心模型 | 无服务端投影层，多客户端/服务化场景需要自建（RPC/SDK 是进程内或 stdio 形态） |

---

## 3. 三者的根本分歧点

**分歧一：策略归属 —— 谁来决定 agent 的行为？**
- pi：**外置给用户**。核心不做决定（连权限与安全都不做），一切通过 TypeScript 扩展与文件约定（AGENTS.md/TODO.md/PLAN.md）实现；作者的理由是「可观测性」与「上下文工程」优先。
- dsh：**上收进容器但保持可替换**。策略是插件，运行时可通过 patch 层栈与 preset 重组；连工具对模型的呈现方式（native/ptc）都是配置。
- AstrCode：**写入宿主类型系统**。策略以显式 hook + 封闭决策枚举表达（Blocking/NonBlocking/Advisory、`ProviderResult::{Allow,Block,ReplaceMessages,AppendMessages}`），扩展只能在既定语义格内组合。
→ 这直接决定了三者的扩展成本曲线：pi 最低、dsh 中等、AstrCode 最高；反过来也在「组合可预测性」上呈相反顺序。

**分歧二：组合维度 —— 静态装配 vs 动态瀑布 vs 用户态全权。**
- AstrCode：hook 订阅在扩展注册期确定，宿主负责 fold 与优先级（设计文档明确要「host 维护链状态逐 handler fold，效果是封闭枚举」）。
- dsh：运行时 waterfall + fiber 作用域 + HMR 重组，中间件可 veto、可包裹、可重排，且组合结果可用 `--dump-config` 检查。
- pi：扩展是普通函数，能改的东西不设边界（`pi.registerTool` 可直接替换内置工具）。
→ 表达力与实际风险同向：dsh 用「可重排但受类型约束的中间件」换取灵活性，AstrCode 用「显式但有限」换可审计性，pi 用「无限但自负后果」换开发速度。

**分歧三：事实层的归属 —— 会话历史是宿主核心还是可选叠加。**
- AstrCode 与 dsh 都事件溯源，但 astrcode 把 EventLog 作为**宿主唯一事实源**（storage 持唯一 `SessionReadModel`，JSONL + 快照 + CAS 防并发写坏），dsh 把 session 做成内存事件日志 + **可选持久化插件**（`session/event` feed 订阅式落盘，`flush()` 才形成 durability barrier）。
- pi 把历史直接等同于**用户文件**（JSONL 树，可按 cwd 组织、可 fork 出新文件、可 HTML 导出/分享），没有服务端投影。
→ 影响「恢复/审计/多客户端同步」的能力边界：astrcode 最强（重放即恢复、fork 即重放），dsh 次之（内存真值 + 后端决定持久），pi 最弱但最透明（文件即事实，用户可读可改）。

**分歧四：安全模型 —— 治理 vs 逃生。**
- AstrCode：capability 声明 + 授权路由 + 审批 + 沙箱（治理内建于宿主）。
- dsh：沙箱模式 + 逐次审批 + PTC 程序的 `justification` 要求 + 插件版本兼容闸门（治理内建于组合层）。
- pi：明确放弃护栏（YOLO by default，文档与博客都写明「跑容器里」）。
→ 三者对「agent 是否可信」给出的答案不同：astrcode/dsh 假设需要边界与审批；pi 假设边界由环境（容器）提供。

**分歧五：上下文哲学 —— 固定结构化 vs 程序化组装 vs 极简。**
- AstrCode：九段固定 pipeline + 稳定段前置（KV-cache 前缀优先），扩展可通过 `PromptBuild` 追加但保持前缀承诺。
- dsh：section 注册表 + 变量 + 工具顺序配置；把「header 何时变化」当作 first-class 记账对象。
- pi：**<1000 token 的 prompt 与工具定义**，理由是前沿模型已被 RL 训练到「天生懂 coding agent」；上下文交给用户与 AGENTS.md。
→ 这是最能体现三家世界观差异的一项：astrcode 相信「结构化 + 稳定前缀」，dsh 相信「可组合 + 可观测的变化记账」，pi 相信「模型自足 + 用户判断」。

---

## 4. 结论：各自更适合的场景

| 若你的首要约束是… | 选择 | 理由（对应上文证据） |
|---|---|---|
| 单一自包含二进制、桌面 + 浏览器 + CLI 多前端共享同一会话内核，且需要可审计/可重放/可 fork 的会话事实 | **AstrCode** | EventLog 唯一事实源 + 投影读模型 + 快照/重放（`docs/architecture.md:11-31,69-80`）；四前端共用一个 agent 内核（`README.md:260-308`） |
| 强治理：能力声明、沙箱、审批、扩展不能越权 | **AstrCode** | capability + `HostRouter` 授权 + hook 模式分级（`README.md:424-440`）；磁盘扩展为受协议约束的子进程，而非同进程任意代码 |
| 以 DeepSeek 模型为中心的平台化交付：Web 服务 / SDK 嵌入 / ACP / 无头 / 桌面都要，且要按客户或环境定制能力集 | **dsh** | profile = 有序 bundle + patch 层栈；`--dump-config` 可验证组合；插件管理面向终端用户与 agent 双通道（CLI README "Profiles"、plugin-manager README） |
| 需要插件生态与快速迭代（TypeScript、pnpm、HMR、版本兼容矩阵管理） | **dsh** | `dsh-hmr` 串行重组；peer 范围校验 + `compatibility.json` 豁免；`plugin_manager` 工具与 Web 侧边栏（plugin-manager README） |
| 需要把「工具调用」升维成「写程序调工具」（代码内循环、批量调用、减少往返） | **dsh** | PTC 模式：只暴露 `run_code` + 生成的 TS/Python SDK，子调用有并发池与 `sandbox_permissions` 审批（`dsh-tools` README "PTC mode"） |
| 个人终端工作流、要求完全可观测与完全可控的上下文，愿意自己写扩展/用 CLI 工具替代 MCP | **pi** | 极简 prompt/toolset + 明确的不做清单 + 扩展可替换核心行为（README Philosophy；博客全文） |
| 把 agent 作为库嵌进自己的应用（Node.js 内 SDK 或 stdio RPC） | **pi** | SDK（`createAgentSession`）与 RPC 模式（严格 LF 分隔 JSONL 帧）；官方举 `openclaw/openclaw` 为例（README "Programmatic Usage"） |
| 在容器/一次性环境里跑，安全靠环境隔离而非工具护栏 | **pi** | YOLO 是唯一且默认模式，文档与博客都主张容器化（README Philosophy；博客「YOLO by default」） |

**一句话总结**：三者都在回答「如何把 LLM 装进一个可控的循环里」，但对**控制权放在哪一层**给出了三种互不兼容的答案 —— AstrCode 放在**宿主类型系统与事件事实层**，dsh 放在**可组合的插件容器与 patch 层栈**，pi 放在**用户的终端、文件与 TypeScript 扩展**。因此它们不是「同一件事的三个实现」，而是三种可共存的工程哲学：AstrCode 适合要治理与多前端的团队，dsh 适合要平台化定制与生态迭代的交付方，pi 适合要极致可控与最小信仰的个人。

---

## 5. 来源列表

### 5.1 一手来源 —— AstrCode（本地仓库，`/home/cirno99/Code/Rust/astrcodez`）

| 内容 | 位置 |
|---|---|
| 项目自述、tagline、四前端、运行模式 | `README.md:1-14`、`README.md:260-308`、`README.md:471-478` |
| 架构总览图、crate 分层、关键设计决策（agent loop / provider / compact / tools / extension / ACP / event-sourcing / prompt） | `README.md:310-469` |
| 核心判断「EventLog 是事实，SessionReadModel 是投影，Agent 是无状态运行时」、事件流路径、fork/快照、compact 三入口、server 三层状态、`deliver_input` 三策略 | `docs/architecture.md:5`、`:11-49`、`:69-80`、`:126-149`、`:86-106` |
| 扩展系统三层（bundled / 磁盘 s5r / MCP 不实现 trait）、代码地图、hook 与 capability | `docs/extension-system.md:8-45` |
| 「dsh 用 waterfall 同时获得五种能力」的对比与「升级为类型化链式原语」的决策 | `docs/provider-request-rewrite-chain-design.md:3-45` |
| 磁盘扩展工作区布局与依赖策略、已移植的 dsh/pi 插件清单 | `/home/cirno99/Code/Rust/astrcodey-extensions/README.md:1-30`、`:116`、`:207-210`、`:464-466` |
| dsh-loop-guard 引用 | `crates/astrcode-session/src/repetition_guard.rs:12`、`:46` |
| 上游/来源 remote | `git remote -v`（`origin=cirno99/astrcodez`，`upstream=whatevertogo/astrcodey`） |

### 5.2 一手来源 —— dsh（npm 发布物，`https://registry.npmjs.org/@deepseek-ai/dsh` 及 `@deepseek-ai/*`）

| 内容 | 位置 |
|---|---|
| CLI 定位、入口模式表、profile/bundle/patch 层栈、插件管理、版本兼容与豁免 | `@deepseek-ai/dsh@0.2.0-rc.2` 包内 `README.md`（"The `dsh` command is the sole supported Node application launcher"…"Profiles"）；`package.json` |
| Agent 句柄、registry、initiator scope（AsyncLocalStorage）、inbox 语义（followup/steer/inject）、`agent/*` 事件族、作用域注册 | `@deepseek-ai/dsh-agent@0.2.0-rc.2` 包内 `README.md` |
| turn/step 流程、durable turn 与原子认领、请求 header/context 变化记账、持久化接管、失败与取消语义、`DEFAULT_MAX_PARALLEL_TOOL_CALLS = 10` | `@deepseek-ai/dsh-agent-loop@0.2.0-rc.2` 包内 `README.md`、`lib/types/constants.d.ts` |
| 工具注册与 `defineTool`、呈现模式 native/ptc/both、`restrict`/`guard`、执行管线五段、PTC 模式与 `run_code` 约束 | `@deepseek-ai/dsh-tools@0.2.0-rc.2` 包内 `README.md` |
| 事件溯源 session log、`deriveMessages()`、surfaceOp、插件消息投影、fork 种子、flush barrier | `@deepseek-ai/dsh-session@0.2.0-rc.2` 包内 `README.md` |
| system prompt 注册表与配置（opener/runtime context/persona/toolOrder） | `@deepseek-ai/dsh-system-prompt@0.2.0-rc.2` 包内 `README.md` |
| provider-neutral LLM seam、deep-frozen 请求、retry 独立成包、适配器分工（含 `dsh-llm-pi-ai` 依赖 `@earendil-works/pi-ai@^1.0.2`） | `@deepseek-ai/dsh-llm@0.2.0-rc.2` 包内 `README.md`；`@deepseek-ai/dsh-llm-pi-ai` registry 元数据 |
| 插件管理（bundle 选择/安装/回滚、registry 轮询、构建脚本审批、版本豁免） | `@deepseek-ai/dsh-plugin-manager@0.2.0-rc.2` 包内 `README.md` |
| agent preset（会话级子插件组合） | `@deepseek-ai/dsh-agent-preset@0.2.0-rc.2` 包内 `README.md` |
| cordis 容器语义（Context/plugin/Fiber/inject）、vendored 事实（`repository.directory = vendor/cordis`、author Shigma、MIT） | `@deepseek-ai/cordis@4.0.1-rc.4` 包内 `README.md`、`package.json` |
| 版本、仓库、许可、维护者 | `https://registry.npmjs.org/@deepseek-ai/dsh`（`dist-tags` / `repository` / `maintainers`） |

### 5.3 一手来源 —— Pi（npm 发布物 + 作者博客）

| 内容 | 位置 |
|---|---|
| 定位、四模式、扩展/技能/prompt 模板/主题、pi package manifest、SDK 与 RPC、Philosophy 清单（无 MCP/子代理/权限弹窗/plan mode/to-do/后台 bash）、context files 与 SYSTEM.md | `@mariozechner/pi-coding-agent@0.73.1` 包内 `README.md` |
| 扩展 API 与事件族、扩展位置与热重载、安全警告 | 同包 `docs/extensions.md:2-16`、`:107-135`、目录（resources/session/agent/turn/message/tool/model 事件族） |
| 会话存储（`~/.pi/agent/sessions`，JSONL 树，`id`/`parentId`）、`/tree` `/fork` `/clone` 语义、分支摘要 | 同包 `docs/sessions.md` |
| 压缩触发条件（`contextTokens > contextWindow - reserveTokens`，默认 16384；`keepRecentTokens` 默认 20k）与分支摘要机制 | 同包 `docs/compaction.md` |
| 设计哲学与取舍（极简系统提示与 4 工具、YOLO by default、no MCP/子代理/plan mode/to-do/后台 bash、Terminal-Bench 结果、pi-ai/pi-agent/pi-tui 分层动机） | `https://mariozechner.at/posts/2025-11-30-pi-coding-agent/` |
| 版本、仓库、许可 | registry 元数据 + 包内 `package.json:91`（MIT） |

### 5.4 二手/辅助来源（仅用于交叉验证，未作为论断依据）

- `https://registry.npmjs.org/@mariozechner/pi`：同作者 GPU pod 部署工具，用于排除同名歧义。
- `https://registry.npmjs.org/dsh`（Robert Eisele 的 JS shell）：用于排除 dsh 同名歧义。
- 扩展工作区 README 中转述的第三方 dsh 生态项目（`dsh-weneed`、`dsh-routing-suite` 等）：仅用于说明 dsh 生态存在，未用于设计论断。

---

## 6. 推断、未验证与取证缺口

**明确标注为推断（有证据但非直接陈述）**

1. `@earendil-works/pi-ai` 与 pi-mono 的 `pi-ai` 同源：依据是 description 字符串完全一致、版本号在同轨（`legacy-node20: 0.74.2` vs pi-coding-agent 0.73.1）且仓库为 `earendil-works/pi`。未读到迁移公告。
2. 「dsh 生态插件普遍以 dsh 的 patch/bundle 机制分发」：依据是 `dsh.bundle.patch` / `cordis.patch.yml` 的实际文件与 CLI README 的 profile 说明；未统计全生态比例。
3. astrcode 与 dsh 在 `event sourcing + 投影` 上的相似性是**各自独立收敛**的判断（astrcode 文档明确引用 dsh 的 waterfall 作对照，但未见引用 dsh 的 session 设计）。

**未验证 / 未能取证**

1. **dsh 仓库层文档**（如 `docs/subsystems/core.md`、`docs/cordis-primer.md`、`.agents/notes/**` 决策记录）：包内 README 大量引用这些路径，但本沙箱 `github.com` 不可达，未读到原文。本报告对 dsh 的所有描述均来自包内文档与 `package.json`。
2. **pi 的部分细节**：`pi-tui` 的官方 README 未取到（只读了 `@mariozechner/pi-coding-agent` 包内文档与博客中对 pi-tui 的描述）；pi 的 session 文件精确字段（`docs/session-format.md` 未逐行核对）；pi 侧「取消流会写入 interrupted 锚点」类表述未在 pi 文档中确认（该行为来自 dsh 文档）。
3. **AstrCode 的生态现状**：未核对 crates.io 是否发布、上游 `whatevertogo/astrcodey` 与本工作区 `dev` 分支的差异范围（本地只跑了 `git remote -v`、`git log -1`、`git status -sb`，未做 diff）。
4. **性能/规模数字**：三方均未在本报告范围内做基准复现；仅转述 pi 作者自报的 Terminal-Bench 结果（属作者自述，非独立复现）。
5. **版本时效**：dsh 的 `latest` 与 `next` 标签指向 0.2.0-rc.2，另有 `alpha` 0.2.1-alpha.2 已发布；本报告主体基于 `next` 0.2.0-rc.2，alpha 分支的差异未核对。
