# ADR 0001 — 用 GPUI Kit 桌面应用替换 Web 前端

## 状态

已采纳（2026-10-02）。来自一次 grill 会话（模式 B），第 2 轮盘问定下目的地。后续各轮的决策与代价记在文末「后续决定」（第 3–33 轮），其中 cadence spike 已于第 33 轮收尾（结论与该 spike 的重做方法见该轮），wasm 交付形态仍是 spike 待定；怎么执行见 [`../desktop-app-migration.md`](../desktop-app-migration.md)。

## 背景

当前前端是 `frontend/`：React 19 + TypeScript + Vite + Tailwind 4 + zustand，手写约 18.6k 行，
另加约 3.3k 行 Node 测试脚本与 15 条 npm 契约测试。产物由 `rust-embed` 编译进二进制
（`crates/astrcode-server/src/http/static_assets.rs`），由 `astrcode server` 在
`127.0.0.1:3847` 上以浏览器访问；契约类型由 `astrcode-protocol` 经 ts-rs 生成（109 个 DTO 文件）。

这带来长期双语言维护：Rust 侧的协议、事件流、工具渲染语义，需要在 TS 侧再实现一遍，
并且每次协议变更都要同步生成物与契约测试。

备选方案是 `.consult/gpui-kit`（longbridge/gpui-kit，clone 于 `v0.7.0-36-g0790ad38`）：
Rust 原生桌面 UI 框架，75+ 组件，含 Markdown/HTML 渲染、DataTable、VirtualList、Dock、
Command palette、`MessageScroller` 等，与我们现有界面需求高度重合。

## 决策

**硬替换**：桌面 App 全面取代 Web UI。终点状态是删除 `frontend/`、`static_assets.rs`
及其 npm 契约测试，前端能力全部由 Rust + gpui-kit 实现，与后端共享 `astrcode-protocol`。

## 权衡

考虑过的替代方案：

- **并存**（保留 Web UI 冻结维护，桌面 App 作本地主力入口）：改动最小，但达不成「统一到单语言」
  这个真实目标——而单语言正是本次替换的动机。
- **只做可行性验证**（只试最难的 Chat 流式 + 工具卡片 + Kanban + Settings 三块，跑完再决策）：
  成本最低，但用户已认定方向，不需要再验证方向本身。
- **伪替换**（gpui 开原生窗口 + webview 包现有 Web 前端）：UI 零重写，但 React 维护负担原样保留。
  注：gpui-kit 自带 `crates/webview`（gpui-wry），这条路的成本比预想更低，故保留为退路。

已知代价与未决点：

- 浏览器入口的去留取决于 **wasm 目标**。gpui-kit 支持 `wasm32-unknown-unknown`，官方有可运行的
  gallery（`crates/story-web`）与 showcase（`crates/base/examples/wasm`），本仓库工具链已是
  nightly，构建链（`wasm-bindgen-cli` 0.2.121 + Vite 托管）可行。但官方文档明确写：
  WebAssembly 路径「have not been validated here as a full application distribution path」，
  且 canvas 渲染意味着无 DOM 语义（无原生文本选择、无屏幕阅读器、移动端体验差），
  Web 字体库初始为空、需内嵌字体（官方子集做法不覆盖访客新输入的字符），
  web 平台为单 canvas 单顶层窗口。
- 网络侧：官方文档只描述 web 平台附加的 fetch HTTP client，**未提 SSE / 流式响应**。
  本项目的聊天界面依赖 SSE 增量推送，这是必须先验证的命门。
- HTTP 服务端当前无鉴权（`http/auth.rs`），浏览器入口暴露面问题在替换前已存在，不因本次改动新增。

## 后续决定

- **wasm 定位（2026-10-02，第 3 轮）**：先 spike 再定。只验证两件命门事——浏览器内能否消费现有 SSE 增量、全量中文输入是否可用；实测结果决定浏览器入口去留。spike 出结论前，架构不引入 wasm 可移植性约束。
- **后端连接方式（2026-10-02，第 4 轮）**：桌面 App 与 Web UI 同构，统一走本地 HTTP + SSE 消费 `astrcode-protocol`，不采用进程内订阅、不采用 stdio JSON-RPC。得到一条 IO 路径；代价是 SSE 成为架构硬依赖，第 3 轮 spike 的结论可以直接推翻本决策。
- **server 获取方式（2026-10-02，第 5 轮）**：桌面 App 进程内 `bootstrap_with` 出 runtime，绑 `127.0.0.1:0`（动态端口），UI 作为它自己的第一个 HTTP+SSE 客户端，App 内提供「在浏览器打开」的入口 URL。不采用「必须已有 `astrcode server`」、不采用端口探测复用、不采用固定 3847。已知代价：与 CLI 的 `astrcode server` 并存时是两条独立事件流，CLI 里正在跑的 turn 不会实时流进 App，只有落盘的会话列表一致。
- **第一片切片（2026-10-02，第 6 轮）**：最小端到端 Chat——连自己的 server、会话列表、发一条提示、流式 markdown、一个只读工具卡（选 diff）、一次审批往返。刻意做薄：不含虚拟滚动、不含视觉打磨、只实现一个工具渲染器。理由：它是唯一能验证第 4 轮 HTTP+SSE 同构假设的切片，把架构风险最早暴露。已知代价：拿最硬的骨头当第一次 gpui-kit 练习，故「做薄」是该切片成立的前提。
- **验收基线（2026-10-02，第 7 轮）**：把 `frontend/scripts/` 的 12 个测试脚本按「领域不变式 / Web 运行时权宜」分类，不变式类必须有一对一的 Rust 测试，权宜类明确宣布不重建、由新实现重新推导。不采用全部一对一映射（会把 React 时代的权宜逻辑固化进 Rust），不采用只留协议契约测试（会让行为不变式静默消失）。逐项分类的最终判定权在用户，初判表见会话记录。
- **工具渲染机制（2026-10-02，第 8 轮）**：照搬现有双层注册表到 Rust——intent 层直接读 `ToolResult.metadata["presentation"]`（`astrcode-core::tool::PRESENTATION_METADATA_KEY`，协议零变更，并保留「未知值按未声明处理」的向前兼容语义），名字回退层照搬现有匹配。第一片只实现 `diff` 一条 intent 路径。不采用「把名字回退上移为更细 intent」（要动 `astrcode-core` 与扩展 SDK，规模独立于本次替换），不采用「只做三种简化卡片」（属静默降级）。副作用：`PRESENTATION_METADATA_KEY` 的注释写死「前端按此键拾取 intent」，迁移后须同步更新。
- **仓库与产物边界（2026-10-02，第 9 轮）**：新增独立 crate `crates/astrcode-gui` 与独立二进制；CLI 二进制不链入 GUI 依赖，以保住 headless / docker / eval 路径；首版发布形态是各平台 tar.gz / zip 内的 GUI 二进制，不做签名与公证。不采用「CLI 同 binary 加 `gui` 子命令」，不采用 dmg / msi / AppImage + 签名公证，不采用「暂不定发布形态」。连带工作：删除 `frontend/` 时须同步移除 `scripts/bump-release-version.sh` 里 `frontend/package.json` 与 lockfile 的版本同步项；`docs/release.md` 称 workflow「构建 CLI、桌面包、npm 包」，而 `.github/workflows/release.yml` 目前没有任何桌面产物 job，这处漂移一并修正。
- **看板迁移（2026-10-02，第 10 轮）**：看板排在最后一片、照搬现有交互，用 gpui-kit 原语自绘拖拽；删 `frontend/` 前必须打平。若自绘拖拽不可接受，预授权降级为「下拉选列 + 键盘移动」。不采用扩展驱动的声明式通用视图（要动扩展系统，规模独立于本次替换），不采用局部 webview。前提：先做 DnD spike 证明 gpui 里跨容器拖拽可行。事实依据：`docs/kanban-extension-design.md` 已声明「前端只是看板数据的读写界面」，六列状态机与 `Card` 模型在 `astrcode-extension-kanban` 内。
- **设置页迁移（2026-10-02，第 11 轮）**：设置页独立成一片，照搬现有四个分区（`models` / `providers` / `permissions` / `appearance`，含 provider 预设管理，不做后置），复用现有 `/api/config*` 与 `/api/models*` 路由（协议零变更），顺序排在第一片 Chat 骨架之后。理由：CLI 子命令只有 `Exec` / `Server` / `Acp` / `Eval`，**没有 `config` 子命令**，设置页是今天唯一的图形化 provider 配置入口，把它排到最后等于让新用户上手即手改 TOML。不采用「排在最后」「只做只读 + 打开 config.toml」「改设计成命令面板」。已知例外：`appearance` 分区在桌面端的主题语义与 Web 不同，不应照搬。
- **本地 UI 偏好的归宿（2026-10-02，第 12 轮）**：上移到服务端——侧栏宽度、折叠的项目、主题偏好、项目路径历史等改为服务端持有，Web 与桌面共享同一份。不采用「桌面 App 自持本地偏好文件」「不持久化」「复用 `config.toml` 新增 `[ui]` 段」。已知代价，逐条记录以免日后误读：(1) 这打破了第 8、11 轮维持的「协议零变更」，需要新增服务端字段或路由；(2) 服务端当前**没有鉴权、没有用户模型**（`http/auth.rs`），偏好因此是「该 server 实例全局」而非「每用户」，多客户端或远程连接时会互相覆盖；(3) 换机器连接同一 server 时偏好跟随，含他人偏好；(4) 迁移时需决定是否沿用旧键（主题键拼写为 `astrocode-theme`，与其余 4 个 `astrcode:` 前缀不一致）。
- **状态层迁移（2026-10-02，第 13 轮）**：按性质拆开重建——流生命周期（`sessionStreamController`）、增量应用（`applyDelta` + `blockHelpers` 的字段回退规则）作为领域逻辑在 Rust 重建并配测试；`frameBuffer` 的按帧冲刷删除，`coalesce` 一并归入该层——读实现后确认它只是相邻同目标 delta 的批内拼接（`store/delta/coalesce.ts:25`），不是独立领域语义；改由 gpui 的实体通知节奏驱动，但保留其内存上限（1024 个 delta / 256KiB 文本）；`conversationHistory` 的窗口上限（8 页 / 1300 blocks）先照搬，待列表虚拟化方案定后再评估。不采用一比一移植（会把无对应物的帧语义固化进 Rust），不采用退化为快照 + 分页（先崩手感）。结构上倾向与 UI 解耦的模块，使这些不再依赖窗口与 GPU 即可测试。前提：**需先验证 gpui 的重渲染节奏能否直接承接高频 delta**，这是第 13 轮引入的第三个前置 spike。（**此前提已于第 33 轮解除**：结论是「不能按 delta 逐条承接，但可以承接」——前提是合并、只渲可见块、切点避开围栏三条同时成立；见第 23–33 轮。）
- **盘问范围（2026-10-02，第 14 轮）**：验收基线只对四个存疑脚本（`delta-coalesce`、`streaming-cache`、`conversation-history`、`project-path-history`）逐个盘问，其余 8 个沿用第 7 轮的文件级判定，不逐文件重问。不采用「12 个脚本逐个过一遍」（共 3,347 行，其中一半在第 7 轮已判为 Web 运行时权宜或随前端删除，重问等于重做已定的事）。已知代价：非存疑脚本的判定只到文件级；第 7 轮「逐项分类最终判定权在用户」因此一直悬空，而后续写迁移计划时又发现三处文件级判定需要修正（`assistant-run-model`、`settings-model-options`、`kanban-selection` 实为领域不变式），印证了文件级判定不可靠。
- **会话历史窗口（2026-10-02，第 15 轮）**：语义照搬、数字重新推导——保留向后分页加载、`detachedFromLatest` 与「回到最新」交互、流式更新不打断正在阅读的用户；`MAX_TIMELINE_PAGES = 8` 与 `MAX_TIMELINE_BLOCKS = 1300` 改由 gpui 侧实测内存决定，不照搬。不采用「数字与语义全部照搬」，不采用「交给虚拟列表、去掉客户端上界」（虚拟列表解决渲染代价，不解决客户端持有十万级 block 对象），不采用「窗口化下移服务端」。必须保留的领域语义：游标按数值比较（`earliestConversationCursor`，测试守 `'9' < '10'`）、`mergeById` 的按 id 覆盖、越界时丢弃最旧页。迁移注意：`MAX_TIMELINE_BLOCKS` 的越界检查发生在 `requestAnimationFrame` 驱动的 `flushPending` 内，不要把这个耦合一并带走。
- **流式 markdown（2026-10-02，第 16 轮）**：用 gpui-kit 的框架能力承载——`TextViewState::markdown` + `push_str` 逐块追加，**不重建前端的「安全提交点」算法**（`markdownStreaming.ts` 的 18 字段增量扫描态、fence 计数、`STREAMING_SPLIT_CACHE_LIMIT`）。不采用「在 Rust 重建该算法求逐字节等价」，不采用「先写对照实现」。两个 spike 观察点：半截围栏被渲染成什么形态、长回答尾段的每块重解析成本。若重解析成本不可接受，**被授权的退路是混合方案 C**：渲染仍交给框架，提交边界由我们算。依据：gpui-kit 官方 `examples/stream-markdown` 的做法就是 push 整段重解析（`main.rs` 注释「it will reparse and re-render automatically」），且 `text` 模块内无任何与未闭合围栏对应的机制。
- **看板日历分桶（2026-10-02，第 17 轮）**：在 Rust 完整重建日期语义——本地时区换算（RFC3339 UTC → 本地日）、补零 `YYYY-MM-DD` 键与「字符串比较即时间先后」、`UNSCHEDULED_BUCKET_KEY` 兜底、`day/week/month/year` 四种刻度的桶边界与「年刻度只给数量」，`kanban-calendar.test.mjs` 的 336 行断言迁为 Rust 测试。不采用「只重建按日分组的骨架」，不采用「把分桶下移到服务端」，不采用「随看板后置再定」。理由：日键格式与后端 `Card.date` 的归一化一一对应，时区换算是用户可见的正确性（东八区凌晨的卡片会落错天）。**本轮未回答的新问题**：原生 App 长期驻留、跨时区或跨夏令时运行时，「今天」的求值时机（每次渲染重算 vs 缓存）需要单独定。
- **验收基线修订（2026-10-02，第 17 轮附带）**：第 7 轮的分类表按**断言级**而非文件级重做。`delta-coalesce.test.mjs`（678 行，全项目最大）横跨两层：`coalesceDeltas` 的相邻拼接属帧层可弃用，但它同时守着 `applyCoalescedDeltas` 的孤儿 patch 不变式（「must not manufacture blocks without a start or durable request」）与 `reduceConversationDeltas` 的重试语义，这些必须重建。
- **项目路径历史（2026-10-02，第 18 轮）**：归入第 12 轮的服务端偏好，语义 1:1 上移——`history` 为 MRU 上限 10（命中提到最前、溢出丢最久未用）、`ignored` 上限 50、`rememberProjectPath` 同时撤销该路径的忽略、`forgetProjectPath` 清 history 并把它记入 ignored、`mergeProjectPathCandidates` 的「history 顺序优先 + trim + 去重 + 剔除 ignored」合并规则；134 行断言迁为 Rust 测试。不采用「给这题开例外、留在客户端本地」（那正是第 12 轮否掉的「两份互不同步」，过渡期 Web 与桌面候选还会不一致），不采用「第一遍不迁移」（丢掉 MRU 便利与「删不掉」这个已修 bug），不采用「只存 ignored、history 改由服务端会话列表推导」（`CreateCardModal.tsx:71` 是建卡**成功后**才记路径，卡片的工作目录不一定已有对应会话，推导不总成立）。理由：这个输入框今天已是「服务端主机上的路径选择器」——`defaultWorkingDir` 与会话目录本就来自服务端机器，只有这两份列表是客户端本地的，上移是向既有语义收敛而非新引入共享状态。事实依据：它不是看板专属，`Sidebar/NewProjectModal.tsx:40` 同样调用 `rememberProjectPath`。已知代价：`ignored` 属个人口味却被共享，一个客户端删掉的候选会对同一 server 上的其它客户端一起消失，第 12 轮「服务端无用户模型」这条在此最突出。**遗留未决**：第 12 轮遗留项 (4)「是否用旧键值作一次性种子迁移」本轮只是并入处理范围，方式并未确定。
- **ts-rs 生成链（2026-10-02，第 19 轮）**：随 `frontend/` 一起删干净——`typescript` feature（`dep:ts-rs`）、example `generate-typescript`、4 个源文件的 `ts_rs::TS` derive（`http.rs` 一处 84 个）、workspace 的 `ts-rs` 依赖、109 个生成物、`generate:protocol` 与 `check:protocol` 两条 npm 脚本、`scripts/protocol-contract.test.mjs`。不采用「保留 feature 与 example、只删生成物与脚本」（example 把输出目录写死为 `frontend/src/services/generated`，前端一删它就成了必须同步修改的死代码），不采用「留到第 3 轮 wasm spike 有结论再定」，不采用「先归档最后一次生成物再删」（git 历史已足够，归档属仪式）。关键理由：wasm 形态是 gpui-kit 编到 `wasm32-unknown-unknown`、以 Rust crate 身份直接依赖 `astrcode-protocol`，与 TypeScript 无关；需要 ts-rs 的只有「保留 TypeScript 前端」这一个世界，而那正是本 ADR 要删掉的东西。已知代价：今天 `npm run check` 会拦住「Rust wire 类型改了、TS 绑定未重生成」的漂移，删除后这条守卫消失——但它服务的正是被删的前端，单语言下不存在这个漂移面。可逆性：恢复是机械的（一个 feature、一个 example、4 处 derive），且都在 git 历史里，所以这是低成本可逆的删除。附带澄清：npm 分发不是幽灵——`npm/astrcode` 与 `release.yml` 的 `publish-npm` job 真实存在，但只发 CLI 二进制（`files: ["bin/", "install.js", "README.md"]`，`prepare-npm-packages.sh` 只拷二进制），不含前端产物或 TS 类型，因此不存在 npm 侧的类型消费者；第 9 轮记的幽灵仅指「桌面包」那一项。
- **迁移期 Web UI 纪律（2026-10-02，第 20 轮）**：在 main 上双轨推进，`frontend/` 立即冻结为「只修阻塞级 bug、不接新功能」——判定线是「打不开、发不出消息、审批卡死、会话列表读不出」，这类才修；新能力只做在桌面端；看板打平那天整目录删除；后端新增能力在 Web 侧允许静默降级（前端本来就有名字回退层兜底）。不采用「不冻结、新功能双向落地」（等于把本 ADR 要消灭的 TypeScript 税付满整个迁移期），不采用「长期分支 `feat/desktop-app`、main 只维护 Web UI」（把分叉开在唯一持续变动的部分——后端协议、扩展系统、看板——合并成本最高且 CI 要维护两套），不采用「先删再加、把删除前置」（制造一段无可用界面的断档）。已知代价两条：(1) 迁移期 Web 入口功能落后于桌面端，形成用户可见的不对称；(2) 冻结不等于可以不管——后端 wire 类型一变、不重生成 TS 绑定，Web UI 就会静默错位，而这条关卡今天只由本地 `npm run check` 执行（CI 的 4 个 node job 是 `frontend-lint` / `frontend-typecheck` / `frontend-format` / `contract-test`，都不跑 `check:protocol`）。**附带项**：把 `check:protocol` 加进 CI 的 `contract-test` job（一行 `run:`）；此附带项在回复中已声明需要点头，本次按「选 A 即默认同意」记入，若不愿扩大范围须明确撤回。
- **三个 spike 的挂钩（2026-10-02，第 21 轮）**：不设统一门禁，分三档挂钩子——cadence spike 可与第一片的 UI 骨架并行，但必须先于状态层落地；DnD spike 挂在看板片之前；wasm spike 只设截止线（**删除日**）、不设起点。不采用「三个串行先行、全部出结论再动第一片」（两个不影响第一片的验证会挡住全部开工），不采用「只做 cadence、另两个并入各切片时再说」（把「删除日必须已有结论」的验证降级成可选前置），不采用「不做独立 spike、并入对应切片」。理由：三者受影响面不同，只有 cadence 位于第一片的近关键路径上；且第 3 轮已定「spike 出结论前不引入 wasm 可移植性约束」，过早开工与之自相矛盾，而删除日一到，浏览器入口的存亡只能靠它。已知代价：删除日之前须为 wasm 的结论与（若可行）落地预留时间窗；若结论为「不可行」，浏览器入口随删除一并消失这一用户可见后果必须提前写进发布说明——属沟通义务，不是技术风险。
- **文档形态（2026-10-02，第 22 轮）**：新增 [`../desktop-app-migration.md`](../desktop-app-migration.md) 记「怎么做」——打平清单、切片顺序、三档 spike 挂钩、按断言级重做的验收基线、删除日作业清单、未决项；ADR 0001 保持为决策记录、不拆分。不采用「把计划段追加进 ADR」（会让同一篇文档既记决策又记计划），不采用「先逐条读完 12 个脚本再写文档」（其中一半已判为权宜或随前端删除），不采用「只更新 `CONTEXT.md` 收尾」。**撤回第 21 轮回复中的一项提议**：曾提「把 ADR 收敛为单条硬替换决策」，本轮撤回——那会拆掉决策日志的可追溯性，却省不下东西。附带产出：`CONTEXT.md` 增补「打平」「删除日」「冻结」「三档 spike」四个术语。附带发现：`MainView` 有四个视图，其中 `plugins` 视图（275 行）在 21 轮盘问中从未被提及，已作为未决项记入迁移计划 §6。
- **cadence spike 结论（第 23–33 轮，第 33 轮收尾）**：第 13 轮引入的前置 spike 到此结束。**答案**：推入 delta 几乎免费（release 0.0001 ms/块），成本全在帧上；帧成本是「每帧被渲染的字节数」的函数（约 0.4 ms/KiB 直到 16 KB），与持有的历史量（第 24 轮）、构建方式（第 26/29 轮）、切块大小（第 30 轮）均无关。故 gpui 的重渲染节奏**不能按 delta 逐条承接，但可以承接**，需三条同时成立：**合并**（第 27 轮：62 KiB 文档上追加 60 字节仍要 35 ms，故合并是必需而非优化）、**只渲染可见块**（第 28 轮：7→118 KiB 全程 1.35 ms，同份 118 KiB 全渲染 142–146 ms）、**切点避开围栏内部**（第 31 轮）。
- **尺寸曲线与视口（第 23/25/26/30/32 轮）**：生产条件（单窗口、BLOCK 形态、一次 `markdown()` 构建）热帧在 8/16/32/64/128/256 KiB 为 2.55/5.39/14.27/35.45/160.36/626.73 ms——16 ms 帧预算约在 32–40 KiB 被突破。首帧≈热帧，无跨帧缓存（第 23 轮，直到 256 KiB 仍成立）。内容形态影响 6.9 倍（同为 59 KiB：BLOCK 30.24 ms vs CHUNK 208.17 ms）。第 30 轮标定：切块大小对帧成本基本无影响（可见字节相同则读数一致），唯一驱动量是可见字节数——944 B→0.50、1.9 KB→0.77–0.84、3.8 KB→1.34–1.62、8.0 KB→3.11、15–16 KB→6.21–6.26、32 KB→16.85、64 KB→36.09 ms。第 32 轮实测 900×700 视口可容约 1.5 KiB（0.443 px/字节），对应稳态单帧约 0.6 ms，取代此前 2–4 KB 的估算。**唯一上界约束**：虚拟化粒度就是块、单块无法只渲一半，故无上限增长的末块是唯一能打破预算的形态。
- **测量可信度与已作废读数（第 25/26/29 轮）**：全部绝对值只在「单窗口 + 同一次运行内」可比——窗口数会污染读数（同一条件 3 窗口 94.6 ms vs 单窗口 30.2 ms，第 26 轮）；换配置或换模式后须弃掉前两帧再计时（一个 4.23 ms 的假读数就来自紧随模式切换的那一帧，第 29 轮）；不要在文档长大后再排空大量待处理 revision（O(n²)，会让测试挂死）。已作废三处：第 23 轮扫描组的 n^2.0 指数与「16 ms 预算在 16 KiB 处被突破」、第 25 轮「构建历史 3.3 倍」（单窗口下 30.24 vs 31.18 ms）、第 24 轮「流式消息无法被虚拟化掉」——分别以第 26 轮曲线、第 26 轮复核、第 28 轮实测为准。
- **半截标记的渲染形态（第 31 轮）**：完整围栏与半截围栏渲染完全相同；半截行内代码与半截强调无害（标记按字面保留）；危险在后半块——开头无围栏的代码续片会被当 markdown 解析，`#` 与 `-` 被消费，即代码行被施加 markdown 格式。这给第 16 轮「不重建前端的『安全提交点』算法」逼出下界：完整算法仍不必重建，但「不在围栏内切」是渲染正确性的必要条件。
- **证据位置与重做配方（第 33 轮）**：脚本在 `.consult/gpui-kit/crates/kit/tests/cadence_spike.rs`（`.consult/` 被 gitignore，不入库；待 `astrcode-gui` 落地后随首个状态层测试一并搬入）。可信的是以 `consolidated_` / `streaming_` / `tail_append_` / `chunk_size_` / `half_open_fence_` / `viewport_capacity_` 开头的那批；`cadence_spike`、`virtualization_spike`、`construction_spike`、`size_curve_spike` 四个每个条件各开新窗口，绝对值偏高 1.9–3 倍，只可用同一次运行内的比值。运行方式：`cargo test --release -p gpui-kit --features test-support --test cadence_spike --locked -- --nocapture <测试名>`。遗留两项：切点规则（「不在围栏内切」）尚未实现，属状态层首个待实现项；全部绝对值待有显示/GPU 的机器重标。
- **wasm spike 结论（2026-10-03，第 34 轮）**：**浏览器入口可行**，第 3 轮留下的两个命门都已在无头 chromium 上跑通——流式消费 SSE 增量（帧逐条到达，非整包缓冲）与 gpui-kit `Input` 的中文 IME 组合输入；进程内 server 与真实 `astrcode server` 两种上游都测过。落地形态据此定型：抽出宿主无关的 `crates/astrcode-ui`（协议客户端 + 会话状态 + gpui-kit 视图），原生宿主 `astrcode-gui` 与 wasm 宿主 `crates/astrcode-webui` 共用同一份。**传输抽象点是 gpui 的 `HttpClient`**——原生注入 `reqwest_client`（自带 tokio 运行时），web 注入 `platform.fetch_http_client()`，因此共享层既不依赖 tokio 也不需要运行时句柄，第 5 轮为「把 runtime 句柄交给 UI」留的机制随之删除。宿主只注入三项：`base_url`（web 留空，请求按相对引用落到页面自身 origin——页面由 server 同源托管）、`working_dir`（web 交空串，服务端按自己的启动目录解析，与 `active_session_working_dir` 无 focus 会话时的取值一致）、`HttpClient`。**产品页复核（同日）**：`/api/sessions` → 建会话 → 订阅 `/api/sessions/{id}/stream` 全通，且观察窗口内无 panic——`cx.spawn` 里的 `this.update(cx, _)` 在 web 后端可用；spike 期那次 `AsyncApp::update_entity` 撞已借出 `AppCell` 的 panic 未复现，成因仍未定位（当时是探针在绘制路径附近更新，产品代码是在任务唤醒时更新），故这条不算已解释，只算不再阻塞。**新发现的两条硬约束**：(1) web 平台不提供任何字体，而 `WebTextSystem` 把 `.SystemUIFont` 落在族名 `IBM Plex Sans` 上——随包必须有一份占住该族名的字体，否则首帧之后的文本测量直接 panic；(2) CJK 子集必须按**自己的文案**生成，照搬别家（如 gpui-kit story-web）的子集会出现整片缺字（实测界面自有汉字无一有兜底）。不采用「保留 reqwest 让 UI 层持有 tokio」，不采用「web 侧硬编码 `location.origin`」（相对引用即可，省掉 web-sys 依赖），不采用「UI 层自己读进程 cwd」。已知代价：debug 产物 34 MB（release 未测）；`.consult/` 里的 spike 工程不入库。**内嵌托管（同日落地）**：产物由 `astrcode-server` 内嵌（`rust-embed`，`http/webui_assets.rs`）挂在 `/app` 下，与 `/` 上内嵌的旧前端并存，删除日把 `/` 换到这里；响应发 `Cross-Origin-Opener-Policy: same-origin` 与 `Cross-Origin-Embedder-Policy: require-corp`——gpui 的 web 平台依赖 `SharedArrayBuffer`，而它只在 cross-origin isolated 文档里可用。无头 chromium 对着真实 server 复核 7 项全通：`crossOriginIsolated === true`、页内 `/api` 落在页面自身 origin（无反代）、进会话订阅 SSE、观察窗口无 panic。`www/wasm/` 不入库，故该模块不假设产物存在，缺失时落 JSON 404 并在启动时告警。
- **看板打平批次与浏览器验收（2026-10-03，第 35 轮）**：数据层（`crates/astrcode-ui/src/kanban/`：线缆镜像、日历分桶、路径历史、列/槽位/落点词汇）与视图层（`views/kanban.rs` 两区六列、空桶收缩、手风琴三态、入场门控、跟随可见性的轮询）连同侧栏入口与外壳切换一并落地，并做完浏览器验收。**拖拽高亮改用 `drag_over` 的样式，不再自存悬停状态**：事实依据是本版 gpui 的 `on_drag_move` 在这些落点上根本不触发（用它写高亮时验收量到的变化像素为 0），而 `drag_over` 与真正生效的 `on_drop` 共用同一条命中判定（`hitbox.is_hovered`），gpui-component 自己的 dock 落点也是这个写法。不采用「保留 `on_drag_move` + 自存 `drop_target`」（既不触发，状态式高亮还得自己负责清残影），不采用「放宽验收而不是修高亮」。**布局修复**：日历区那条横向滚动条会把内容的 min-content 当成父格的自动最小宽度顶上来，把该格撑到一屏只放得下一个时间桶——给日历区与滚动条都补 `min_w_0` 显式允许收缩；「滚动容器顶宽父格」在 gpui 的自动最小尺寸下是通用坑，不是看板专属。**验收结论**：无头 chromium 对着真实 server + 真看板扩展跑 20 项全通（三条落点解析路径、空操作不发 PATCH、无落点松手不写不留残影、悬停高亮与离开后清除、点卡片跳会话、停用扩展入口消失、无 panic）。**未覆盖**：月/年刻度、运行中的两列（扩展独占、无落点）、批量拖拽（前端「拖一张就是拖一组」）、原生 `astrcode-gui` 宿主。已知代价/遗留：卡片编辑弹窗、框选多选、卡片上的列下拉框与折叠操作行仍未做，属下一批。证据位置：`.consult/webui-spike/e2e/kanban-board.mjs` 与驱动 `/tmp/astrcode-kanban-e2e/run.sh`（`.consult/` 被 gitignore，不入库）；验收用隔离 home 且**不开** `automationEnabled`——否则自动化会真的领卡片、开模型回合花用户的钱。附带修掉验收侧一处量法错误：格子描边落在面板色的上一行，原断言量的是面板色首行，永远量不到描边。
