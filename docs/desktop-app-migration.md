# 桌面 App 迁移计划

> 依据：[ADR 0001](adr/0001-replace-web-frontend-with-gpui-kit.md) 记录终点与各轮取舍（第 2–22 轮）。
> 本文只记「怎么做」：打平清单、切片顺序、spike 挂钩、验收基线、删除日作业。
> 状态：计划（2026-10-02 定稿于一次 grill 会话）。未决项见 §6。

## 1. 打平清单

「打平」= 桌面 App 覆盖 Web UI 现有功能面，是删除 `frontend/` 的判据。
维度取自 `frontend/src/App.tsx:14` 的 `MainView = 'chat' | 'plugins' | 'settings' | 'kanban'` 与外壳组件；
粒度是「视图 + 能力」，各切片开工时再展开成该切片的验收清单。**逐项勾完、且没有只存在于 `frontend/` 的能力，才进入删除日。**

| 项 | 现有规模 | 桌面端承接 | 状态 |
| --- | --- | --- | --- |
| Chat 视图 | 6855 行 | 第一片已落地（会话列表、流式 markdown、工具卡、批准往返、多行输入区）；与 `frontend/` 的差距按 §1.1 分批打平 | 进行中 |
| Settings 视图 | 1936 行 | 设置片（四分区：`models` / `providers` / `permissions` / `appearance`，复用 `/api/config*` 与 `/api/models*`） | 未开始 |
| Kanban 视图 | 2950 行 | 看板片（数据层、两区六列、自绘拖拽与入口已通；弹窗、多选与列下拉框待补，见 §1.2） | 进行中 |
| Plugins 视图 | 275 行 | 归属未定，见 §6 | 未开始 |
| 外壳：`Sidebar` | 1241 行 | 会话导航、项目分组、折叠状态、宽度拖拽、会话与项目的删除 / Fork 已落地（第十三批）、批量选择删除已落地（第十四批）、新建项目弹窗与页头「新项目 / 收起边栏」两枚小按钮、页脚身份块已落地（第十五批）；插件与设置入口待补（见 §1.1 第 5 项） | 进行中 |
| 外壳：`layout/`、`ConnectingScreen`、`ErrorBoundary`、`TransientHintDialog` | `layout/` 52 行，其余未统计 | 窗口骨架与错误兜底 | 未开始 |

Chat 视图按第 13/15/16 轮拆成三条纵向能力，它们不是独立切片，而是第一片的组成部分：

- **状态层**（第 13 轮）：`sessionStreamController` 的流生命周期与 `applyDelta` 的增量应用在 Rust 重建并配测试；`frameBuffer` 的按帧冲刷删除，改由 gpui 实体通知节奏驱动，但保留内存上限（1024 个 delta / 256KiB 文本）；`coalesce` 作为相邻同目标 delta 的批内拼接并入该层。
- **会话历史窗口**（第 15 轮）：语义照搬（向后分页、`detachedFromLatest` 与「回到最新」、流式不打断阅读），`MAX_TIMELINE_PAGES` / `MAX_TIMELINE_BLOCKS` 的数字改由 gpui 侧实测内存决定。
- **流式 markdown**（第 16 轮）：`TextViewState::markdown` + `push_str` 承载，不重建「安全提交点」算法；退路是混合方案（渲染交框架、提交边界自算）。该退路已被第 28–31 轮改写：渲染必须切成块 + 只渲可见尾部（第 28/30 轮），且切点必须避开围栏内部（第 31 轮）——故「提交边界自算」从退路变成必需，但只需弱化版（不在围栏内切）。

### 1.1 Chat 打平批次（2026-10-03）

第一片刻意做薄，与 `frontend/` 的差距按下列批次补。批次边界按「一次改动能独立验收」划，顺序按依赖排，不按行数。

已落地（第一批，`crates/astrcode-ui/src/views/`）：

- 转录列：居中阅读列（宽上限 720px），与顶栏、输入区共用同一内缩。
- 用户消息：`Bubble`（trailing）+ markdown 正文——前端 `UserMessage.tsx` 同样走 markdown。
- 助手消息：思考折叠（`Collapsible`，流式中默认展开，用户动过之后以用户的为准）+ markdown 正文。
- 输入区：多行 `Textarea`（回车提交、Shift+Enter 换行、1–8 行随内容长），发送（图标）+ 中止并排。
- 顶栏：会话显示名 + 阶段；显示名与侧边栏行共用 `views::display_title`。

已落地（第二批：图标 + 工具卡，同日）：

- 图标层 `crates/astrcode-ui/src/icons.rs`：28 枚图标逐条照搬前端 `ui/Icon.tsx` 的图形，作为字节编进共享 UI 层，用 `Icon::data` 直接渲染。**不走宿主资产源**——两端注册的资产源覆盖的目录不同（桌面宿主的 gpui-kit 默认捆绑、Web 宿主按 `{origin}/assets/icons/*.svg` 拉取），按路径寻址会让同一处界面在两端缺图（见第 2 项的历史结论）。
- 工具卡双层注册表（`src/tool_view.rs`，纯推导、无 gpui，可脱窗口测试）：名字层优先、intent 层（`metadata["presentation"]`）回退。顺序与前端一致——前端的名字渲染器 priority 100、intent 渲染器 50，取首个匹配即名字层胜出。形态覆盖 `read`/`read_tool_result`/`grep`/`find`/`write`/`edit`/`shell`/`shell_poll`/`patch` 与 `terminal`/`diff`/`search`/`read` 四个 intent。
- 工具卡结构：摘要行（图标 + 一句摘要，如 `$ cargo test -p astrcode-ui`、`read … 120/300 lines`、`edit a.rs 2 replacements +12 -3`）+ 状态 `Tag` + 耗时 + 旋转的展开箭头（整行可点）；详情面板按 ID / 参数 / 摘要 / 结果四行，结果按形态给元信息行 + 正文：diff 逐行着色、`read` 带行号、搜索/命令输出通用文本。正文默认只给开头 6000 字符或 24 行，其余收在「显示完整输出」后；展开后仍限 400 行（渲染成本护栏）。审批卡只在等待审批时顶掉结果位，其余状态不再自动展开（与前端一致）。**第三批把这张卡挪进回合、摘要行换成活动标签，详情面板不变。**

已落地（第三批：助手回合分组，同日）：

- 转录分组（`src/assistant_run.rs`，同样纯推导、无 gpui）：连续的 assistant / toolCall 合并成一次回合，回合内按「有可见正文就断开」切段——思考与工具进 `Process` 段、可见正文单独成 `Content` 段；其余块各自成行。对应前端 `assistantRunModel.ts` 的 `buildAssistantRunModel`。
- 过程段渲染成一行摘要：`处理中` / `已处理 3s`（段内工具耗时合计）+ 最近一条活动（`toolActivityFor` 那套「运行命令 cargo test」文案）+「N 项」+ 旋转箭头，整行可点；展开后是左侧竖线包住的思考（markdown）与工具行。**有待审批的工具时强制展开、点了也不收起**（`has_attention`）——否则审批按钮会被折叠区藏起来。
- 工具行改成回合内的活动行：图标 + 活动文案（文件名 / 命令行 / 搜索词；失败标红、其余用 `primary`，`accent` 在本主题里是浅色底当文字看不见）+ `+N`/`-N` 变更量 + 尾部状态（完成的报耗时），点开才是原来的详情面板。活动标签（`activity_for`）与详情里的 `summary_line` 是两套文案，各留在各自的位置。
- 思考从「块内折叠」挪进过程段：有 `reasoning_content` 就用它，否则只在完稿正文里剥「<think-block>」旧标记（Kimi 时代的线缆约定，服务端已不再产生，旧会话的正文里仍有）。
- 刻意没跟的三处：`buildMessageListItems` 的 32 块分片（React 虚拟列表的行粒度，gpui 没有对应物）、末尾的「分叉」行（分叉入口不在本片）、展开动画。
- 验证：`cargo test -p astrcode-ui` 34 项通过（新增活动标签 4 项、分组/思考/耗时 8 项）；wasm 重建后在无头 chromium 里跑真实旧会话（拷进隔离 store，避开活动实例的会话租约）：折叠态 90279 个非背景像素，点最后一行摘要后 +20739，再点一次又 +19936（第二层工具行展开），全程无异常。

已落地（第四批：askUser 问卷卡片）：

- 问卷推导（`src/ask_user.rs`，纯推导、无 gpui）：题干/页眉/选项从工具参数 `questions` 解析（选项不足两个的题整道丢弃，与前端 `parseAskUserInput` 同口径），作答结果从结果文本的 `answers` 回填；作答状态机 `Draft` 独立于窗口——单选替换、多选切换、「其他」自定义输入顶掉已选项、缺任何一题不给提交。对应前端 `Chat/tools/askUser.ts`。
- 待回答的问卷提到折叠区**外**：`ProcessSegment` 多一个 `prompts` 字段，收拢时把「名字是 `askUser` 且仍在流式」的条目摘出去（`entries` 只留思考与其余工具），折叠键在收拢那一刻定下、不随问卷被摘走而漂移。与前端 `PendingAskUserPrompts` 一致：卡片渲染在摘要行上方；`has_attention` 把问卷也算进去，此时摘要行保持展开。
- 卡片本体（`src/views/ask_user_card.rs`）：一个按工具调用 id 持有的实体，自带题号、每题草稿、「其他」输入框与提交状态——答卷期间每次状态更新只原地刷新快照，不重建实体。三种形态：问卷（页眉徽标 + 题干 + 可点选项：选中换描边、`推荐` 徽标、单选选中时铺开预览正文）、`已提交，等待继续`、服务端结果回填后的问答列表。作答完成后块回到 `entries` 当普通工具行，卡片改在详情面板的结果位渲染（前端把 `AskUserCard` 当 `approvalUi` 顶掉结果位，同一处）。
- 两个端点（`src/api.rs`）：`POST /api/extensions/astrcode-ask-user/sessions/{id}/questions/{callId}/respond|reject`——路由由扩展自己注册，共享 UI 层只管拼路径。
- 颜色映射再记一次：强调色用 `primary`。本主题的 `accent` 是一块浅色**背景**（neutral-800），当文字色或描边色用会与底色糊在一起。
- 刻意没跟的三处：跨会话的 `PendingAskUserBanner`（要接待答问卷的自定义事件与轮询，属流式策略那一档）、超时自动选择的倒计时（需要 `autoSelectAt`/`serverTime`，只有 pending 快照里才有，前端拿不到快照时同样不显示）、`frontend/` 卡片里的动画。
- 验证：`cargo fmt --check -p astrcode-ui` 与 `cargo clippy -p astrcode-ui --all-targets --no-deps -- -D warnings` 干净，`cargo test -p astrcode-ui` 41 项通过（新增问卷解析/作答 5 项、回合摘出问卷 2 项）；`cargo check -p astrcode-gui --all-targets`、`-p astrcode-webui --all-targets` 通过。**这一批没跑浏览器验收**：待回答态只在 live turn 里存在（静态会话里的流式工具调用会被投影降级成「失败」），造不出样本，端到端要等一次真实模型回合调用 `askUser`。

已落地（第五批：回合级动作）：

- 推导层（`src/assistant_run.rs`）：`TranscriptItem::Run` 从裸的段列表换成 `Run`——段 + 回合级的两个动作 `RunActions { copy_text, fork_at }`。`copy_text` 是回合内助手块的可见正文按空行连接（对应前端 `assistantRunCopyText`），`fork_at` 取收尾块的 `storage_seq`；收尾不是完稿的助手正文时整个动作都不给（对应 `assistantRunCompletedReply`）。`needs_session_fork_row` 是前端 `buildMessageListItems` 末尾那条「末项自己不能分叉就补一个会话级入口」的规则。`Run::key` 取回合首块 id，只用来让元素 id 在会话内唯一。
- 动作行（`views/chat.rs`）：完成答复下方一行——「复制」（`IconName::Copy`）与「分叉」（`IconName::Branch`，只在有持久化点时出现）。复制走 gpui 的 `write_to_clipboard`，按钮随即切成「已复制」并保持两秒（回落任务按次替换，上一次随之取消）；分叉调 `POST /api/sessions/{id}/fork`（`src/api.rs`）。
- 会话级入口：转录末尾在末项给不出分叉按钮时补一个「分叉当前会话」（对应前端 `forkRow`）。
- 换会话归外壳（`views/shell.rs`）：新增 `ChatEvent::SessionForked`，`Shell` 收到后刷新列表并**显式选中新会话**——分叉不改变列表顺序，走 `sync_sessions` 的「选中第一项」会切错；`sync_sessions` 的尾部因此抽成 `apply_session_list` 供两处复用。
- 刻意没跟的：动作行整体的 `opacity-60 → hover:100`（gpui 侧要用 `group_hover` 表达，收益只是装饰）、复制成功后的 `sr-only` live region、动画。
- 验证：`cargo fmt --check`、`cargo clippy -p astrcode-ui --all-targets --no-deps -- -D warnings` 干净，`cargo test -p astrcode-ui --lib` 44 项通过（新增回合动作 3 项）；`cargo check -p astrcode-gui --all-targets`、`-p astrcode-webui --all-targets` 通过。**浏览器验收（9/9 通过）**：重建 wasm 后在无头 chromium 上跑真实旧会话（隔离 store + `tools/dev_server.py` 反代，脚本 `e2e/turn-actions.mjs`，在 `.consult/` 里不入库）——渲染无 panic（每回合两个按钮的元素 id 不撞）、悬停扫描在 (408,666) 与 (472,666) 定位到复制与分叉、点复制后剪贴板里是与快照**逐字符相同**的回合正文（3532 字符）、点分叉发出 `POST /api/sessions/{id}/fork` 并拿到新会话 id、外壳随后请求新会话的 conversation。端点契约另用 curl 直接验过：`{}` 与 `{"storageSeq":96}` 都回 `{"sessionId":…}`。这一批的浏览器验收**用的是回放样本**，没有真跑一次模型回合（不需要：动作只看已落地的转录）。

已落地（第六批：工具卡的另外两张卡）：

- `todoWrite` 计划卡：新模块 `src/todo_list.rs`（纯推导、无 gpui）——进度项优先取结果的 `newTodos`（扩展 `astrcode-extension-todo-tool` 回填的那份落盘结果），没有才退回参数的 `todos`；`content` / `activeForm` / `status` 缺一即丢（与前端 `parseTodoItem` 同口径）；标签带执行者前缀 `[self] ` / `[agent: reviewer] `。摘要行 `todoWrite · 1 pending · 2 in-progress · 1 done` 走 `summary_line` 的名字层分支（原来会退回结果文本），卡片本体在 `views::chat::render_todo_card`：整卡一段等宽纯文本、每项一行、行首是状态词（三个状态词等宽，天然对齐），计数由卡片头的摘要行给出（初版是计数网格 + 每项一行「标签、状态、百分比、`Progress` 进度条」，后来为削减每帧重排的元素数量改掉：一张卡从 O(项数) 个元素降成 1 个），处置与 `askUser` 同处——占结果位，`has_items` 为假时退回通用正文。
- **没建 RenderSpec 渲染层**（本条修正前一批对第一项的判断）：前端是用 RenderSpec（`box` + `key_value` + `list`/`progress`）表达的，但这里真正要用到的只有「计数」与「进度行」两类，现有的元信息网格与 `Progress` 直接够；`list` 的项目符号、`markdown` / `diff` / `code` 等形态在别的工具卡里没有第二处消费者，为一张卡立一层渲染器属投机。剩下的 agent 子会话卡要的也不是渲染层，而是 `agentSessions` 状态，见待打平清单。
- `patch` 的逐文件段：`tool_view::patch_files` 解析 `metadata.files`（键与写它的地方一致，`crates/astrcode-extension-coding/src/files/patch.rs:63`：`path` / `changeType` / `applied` / `error`），渲染插在元信息网格与 diff 正文之间（与前端 `PatchToolDetails` 同序）：每行「标签 + 路径 + 失败原因」，没应用上的一律标 `failed` 并标 danger，最多 12 条，其余收成 `+N more files`。`summary` 字段刻意没读——前端也没用它。
- 刻意没跟的：`list` 的项目符号、`RenderSpecViewer` 的其余形态、`askUser` 那批遗留的跨会话 `PendingAskUserBanner`。（这条里的「进度条的按状态变色」随上面那张卡改成纯文本一并去掉。）
- 验证：`cargo fmt --check` 干净，`cargo clippy -p astrcode-ui --all-targets --no-deps -- -D warnings` 干净，`cargo test -p astrcode-ui --lib` 50 项通过（新增 6 项：`todo_list` 5、`patch_files` 1）；`cargo check -p astrcode-gui --all-targets`、`-p astrcode-webui --all-targets` 通过（webui 那三条 unused dependency 警告是既有的）。**浏览器验收 6/6 通过**，但它验到的是哪一层必须说清：手工造的样本（`/tmp/make-fixture.py`，不入库）17 行日志、6 个块、含 `todoWrite` 与 `patch` 各一个工具调用，页面加载 → 过程段展开 → 工具行点开全程无 panic，转录列非背景像素 2541 → 12762（`e2e/tool-cards.mjs` + `e2e/png.mjs`，都在 `.consult/` 里不入库）。
- **本批暴露的一件事（不是本批引入）：回放路径的快照不带工具元数据。** `conversation_to_dto` 走 `transcript_blocks`（由 LLM 消息重建，`blocks.rs:391` 的兜底块写死 `metadata: None`），只有直播路径（durable 事件投影，`blocks.rs:117`）才带。实测两处：真实旧会话 34 个块里带 metadata 的工具块 **0** 个；把日志停在未收尾处也不会走直播投影（SSE 只回 `: connected`）。后果是这两张卡、以及所有靠 metadata 的摘要 / intent / 行数字节数，**只在直播的回合里画得出来**，回放旧会话一律退回通用正文。两个前端同此，所以不是迁移引入的差异；但它让「用回放样本验收 metadata 驱动的卡片」这条路彻底走不通——卡片**内容**只有单测守得住。

已落地（第七批：agent 子会话卡）：

- 状态层（新模块 `src/agent_session.rs` + `src/conversation.rs`）：`AgentSession` = 快照链接的字段 + 两个本地字段 `phase` / `current_tool`；归并逐条照搬前端 `applyAgentSessionUpdate`——`spawned` 是唯一的建项入口而且总是整项覆盖，`completed` / `failed` / `progress` 都以当前项为准（当前项不在就整条丢掉），`progress` 只写运行中的项。`ConversationState` 多一份 `agent_sessions`：`reset` 用快照 `agent_sessions` 建基线，`AgentSessionUpdated` / `AgentSessionRemoved` 按 `child_session_id` 归并，同值时不动（阶段随每个步骤上报，卡片只画变了的那几项）。
- 卡片（`views::chat::render_agent_card`）：身份与状态一行（`子 AGENT` + 名字 + 状态药丸，运行中 primary / 完成 success / 失败 danger）、任务一行；运行中给「查看子会话」，完成给摘要（等宽、上限 192px 可滚），失败给错误。「查看子会话」发 `ChatEvent::OpenSession`，外壳按切会话处理——与点列表项同路，子会话不在当前列表时拿不到标题，顶栏留空。
- 工具行的状态文字：运行中的子会话顶掉「执行中…」，报 `子Agent · 当前工具` 或 `子Agent运行中`（与前端 `streamingStatusText` 同序）。
- **一处照搬前端的判据必须说清**：卡片只在工具调用还在 `streaming` 时出现（前端 `AgentChildSessionPanel` 的 `block.status === 'streaming'`）。子会话一收尾，派生它的那次工具调用随即收尾，卡片随之消失、退回通用正文——它是「进行中」的进度视图，不是结果卡。所以完成 / 失败两支持有摘要与错误的分支只在「子会话已收尾、工具调用尚未收尾」这个窗口里画得出来（两个前端同此）。
- **协议侧一条观察**：快照链接（`AgentSessionLinkDto`）不带 `phase` / `current_tool`，只有直播期的 `Progress` 增量才给。所以回放旧会话既没有阶段与当前工具，又因上面那条判据根本画不出卡片。
- 刻意没跟的：`uppercase tracking-wider`（中文上没有效果，字号与颜色已表达同一层级）、`TopBar` 那侧的「N 个子会话」汇总与下拉（属待打平第 5 项）。
- 验证：`cargo fmt --check` 干净，`cargo clippy -p astrcode-ui --all-targets --no-deps -- -D warnings` 干净，`cargo test -p astrcode-ui --lib` 58 项通过（新增 8 项：归并 6、工具行文字 2）；`cargo check -p astrcode-gui --all-targets`、`-p astrcode-webui --all-targets` 通过。**本批没跑浏览器验收**：卡片只在直播回合里出现（同上判据 + 第六批那条「回放快照不带元数据」），造不出样本；能测的部分（归并、行文字）都落在纯推导层的单测里，渲染本身没有自动化覆盖。

已落地（第八批：跨会话待回答问卷横幅）：

- 状态层（新模块 `src/pending_ask_user.rs`）：`PendingQuestion` = 会话 id + 工具调用 id + 解析后的题目 + 原始载荷；`PendingQuestions` 是**跨会话**的集合（键里带会话 id，`callId` 只在 turn 内唯一，跨会话会撞）。`apply_event` 归并直播事件（`ask_user.pending` / `ask_user.resolved`，只认 `astrcode-ask-user` 这两个类型），`merge_snapshot` 归并全局快照——对应前端 `store/delta/applyDelta.ts` 的 `customEvent` 两支与 `mergePendingAskUserSnapshot`。
- **判据换成事件序号**：前端判「请求期间是否变过」用的是对象身份（`pendingAtStart[key] !== question`），这里用本地事件序号：发起请求时记下 `live_seq`，合并时只保留序号更大的直播条目，并让本次请求期间落地的回答挡住旧快照。语义等价，但不依赖对象身份。
- 两条来源缺一不可，这条是设计前提：`ask_user.pending` / `ask_user.resolved` 声明为 `GlobalLive`，而服务端**只把它发给别的会话的流**（`http/stream.rs` 对同会话的全局事件直接丢弃），所以当前会话的问卷事件根本不到当前流上；流断掉时更是只剩全局快照。于是 `ChatView` 多一个 5 秒轮询任务，只在 `stream_connected == false` 时才发（与前端 `PendingAskUserPoller` 同判据），并给单次请求加了 5 秒上限（前端的 abort 是为了不让 in-flight 标记卡死轮询，这里同理：轮询是一趟一趟串起来的）。
- 横幅（`views::chat::render_pending_banner`）：顶栏之下、转录之上、**不在滚动区里**。当前会话的问卷若在块里找不到可见卡片（流断过之后 live 工具块会丢）就画一张恢复卡片——用快照里的题目拼一个 `streaming` 的 askUser 工具块交给第四批那张卡片，卡片实体按 `callId` 单独持有一份（与跟着块走的 `ask_user` 分开）；其余会话画成一行「会话「X」有问题待回答：…」，整行可点，发 `ChatEvent::OpenSession` 走外壳切会话。
- 会话标题（`ChatView::set_session_titles`）：外壳刷新列表时顺手把「会话 id → 显示名」推给会话面板，横幅里的其他会话才写得出名字；列表里没有它（如刚分叉的子会话）退回短 id，与前端同口径。
- 刻意没跟的：扩展可用性那道闸（前端要 `askUserExtensionAvailable !== false` 才轮询，桌面端没有 `/api/extensions` 那一层，端点不可用就当次失败跳过）；`autoSelectAt` / `serverTime` 的倒计时（第四批已记，横幅这一版仍不画）；`uppercase tracking-wider`。
- 验证：`cargo fmt --all --check` 干净，`cargo clippy -p astrcode-ui --all-targets --no-deps -- -D warnings` 干净，`cargo test -p astrcode-ui --lib` 70 项通过（新增 12 项：解码、跨会话键冲突、墓碑与快照合并、恢复块形状）；`cargo check -p astrcode-gui --all-targets`、`-p astrcode-webui --all-targets` 通过（webui 那三条 unused dependency 警告是既有的）。**浏览器验收 7/7 通过**，脚本在 `.consult/webui-spike/e2e/pending-ask-user.mjs`（不入库），做法：隔离 home 起一个 `astrcode server` + 一个代理（静态托管 `crates/astrcode-webui/www`、`/api` 反代、把问卷端点换成一纸桩数据），三趟无头 chromium——空桩 / 当前会话 + 另一个会话各一条 / 只有另一个会话一条。测得：轮询确实打到了问卷端点（`empty=2 both=3`）；空桩时顶栏与转录空态两条带、另一会话时中间多一条带、当前会话的卡片再加若干条带；点那一行之后外壳为新会话发了 `conversation` 请求（切过去了）；全程无 panic。
- **这一批验收测到哪一层必须说清**：桩掉的只是「全局快照」这一条路，直播事件那条路（`apply_event`）需要一次真实调 `askUser` 的 live turn 才造得出样本，仍只有单元测试覆盖；桩代理为了让轮询真正跑起来，把 `/api/sessions/*/stream` 回成一个立刻结束的 200 SSE（真实服务端在 turn 结束后同样会关流），因此「流连着时靠直播事件更新」这一支没有端到端证据。
- 已知取舍：单块渲染的帧预算问题在这批之后更值得注意——横幅是转录之外的一层，随它出现/消失，转录会整体位移。

已落地（第九批：输入区的斜杠命令与参数补全）：

- 推导层（新模块 `src/slash_command.rs`，纯函数、无 gpui）：`/` 触发检测（只认行首或空格之后的 `/`，且 `/` 到光标之间不许有空白）、`/name args` 的参数触发（命令名与参数之间必须有空白，且该命令声明了 `argument_completions`）、按名字或描述的大小写不敏感过滤、插入后的新文本与光标落点。判据与前端 `InputBar.tsx` 的 `findSlashTrigger` / `updateArgTrigger` 逐条对齐，14 项单测钉住（含跨行、多字节字符与越界光标）。
- 两块面板（`views::chat`）：命令面板（加载中 / 「没有找到匹配「x」的命令」/ 技能与插件分组小标题 / 选中项与悬停）与参数补全面板（候选 + 说明 / 「无补全建议」/ 「结果过多，已截断」）。两者都画在输入区**上方的一层浮层**里（`absolute` + `bottom_full`），不参与列布局——否则每敲一个字转录都会跟着上下位移。
- 按键接管走 `cx.intercept_keystrokes`：拦截发生在动作派发之前，命中时 `stop_propagation`，于是上下键不再移动光标、Tab 不再跳焦点、回车不再提交。面板没有可选项时**不接管**，回车照旧归输入区（与前端一致——它过滤为空时也不拦按键）；Shift+Enter 一律归输入区换行。
- 触发重算挂在 `InputEvent::Change` 上。插入走 `set_value` + `set_selected_range`：`set_value` 会把多行输入的选择重置到 `0..0` 而且**不发 `Change`**，所以插入之后要手工重算一次面板——选中 `/name` 之后参数面板紧接着打开就靠这一步。
- 参数补全防抖 250ms（与前端同值）；过期响应靠**换掉任务句柄**丢弃（前端用自增序号丢弃），每换一个触发上下文即换一次任务，上一次请求随之取消。`cursor` 传参数字符数——服务端 `complete_command` 的默认值就是这么算的。
- 提交分流：`Api::submit_prompt` 从 `post_empty` 改成 `post_json`，读回 `PromptSubmitResponse`。`Handled` 且文本是 host 的 `compact_session` 时重新拉一次会话快照——压缩会换掉整篇转录，而服务端只回一句 `compact completed; N messages removed`。其余 `Handled` 不写任何东西：前端会给非命令的 handled 消息插一条 system note，桌面端的会话状态层没有「本地插块」这条路，而能走到这里的只有「忙时被服务端排队」，当前 `can_submit` 在忙时本就是假。
- 命令列表在切会话、面板每次打开、以及收到 `ExtensionRegistryChanged` 增量时重取（对应前端 `refreshCommands` 的三处调用点）；慢响应跨了会话就丢掉。
- 刻意没跟的：`args_schema` 派生出的参数表单（前端也只把 `needs_argument` 当提示用）、`SlashCommandListResponseDto` 里的快捷键绑定与状态栏项（一并回来了，但那属另一处界面）、`uppercase tracking-wider`。
- 验证：`cargo fmt --all --check` 干净，`cargo clippy -p astrcode-ui --all-targets --no-deps -- -D warnings` 干净，`cargo test -p astrcode-ui --lib` 84 项通过（新增 14 项），`cargo check -p astrcode-gui -p astrcode-webui --all-targets` 通过（webui 那三条 unused dependency 警告是既有的）。**浏览器验收 8/8 通过**，脚本在 `.consult/webui-spike/e2e/commands-panel.mjs`（不入库，驱动脚本 `/tmp/astrcode-commands-e2e/run.sh` 同样不入库）：隔离 home 起一个 `astrcode server` + 一个代理（静态托管 `crates/astrcode-webui/www`、`/api` 反代、把命令列表与参数补全两个端点换成一纸桩数据）——键入 `/` 面板展开（输入区上方 8 条文字带，基线 0 条）；Escape 收起（回到 0 条）；Tab 选中第一条并插入（服务端收到 `/commands/alpha/complete`）；面板随即换成候选列表；方向键 + 回车插入的是第二条（服务端收到 `/commands/beta-long/complete`）；重装页面后不带斜杠的回车照旧提交（收到 `/prompt` 并 accepted）；全程无 panic。
- **这一批验收测到哪一层必须说清**：面板画在 canvas 上，读不到 DOM 文本，所以「插进去的是哪条命令」是用服务端收到的补全请求路径反推的，画出来的字形本身没有逐像素断言（只到「那一段有没有文字」这一层）。`/compact` 的提交分流（重拉快照那一步）也没进浏览器验收——桩掉的命令列表里那条 host 命令不会真的压缩，只有单测守着 `is_compact_command` 的判据。

已落地（第十批：输入区的工具权限模式开关）：

- 推导层（新模块 `src/composer_config.rs`，纯函数、无 gpui）：`approval_label` / `approval_hint`（按钮上写什么、悬停说什么，口径照搬前端 `InputBar` 的 `approvalLabel` 与 `title`）、`toggled_approval`（yolo ↔ manual）、`selection_request`（只换指定字段、其余照当前配置原样带上）。4 项单测钉住。
- 端点（`src/api.rs`）：`GET /api/config` 与 `POST /api/config/active-selection`。后者的响应体只有硬写的 `success: true`（`routes/config.rs`），没有可用信息，所以只往外交成功与否。**服务端一次收下整套选区**，换权限模式那一路必须把当前模型与小模型原样带上，否则会把 active selection 打回默认。
- 开关本体（`views::chat`）：gpui-kit 的 `Toggle`（两态按钮，选中态自带强调色底），未选中画「请求批准」、选中画「完全访问」，悬停文案说的是「点了会变成什么」。提交中置灰。**颜色用 `primary` 一族**：本主题的 `accent` 是浅色底，当文字色会糊。
- 配置是全局的（不跟会话走），建视图时取一次、每次写入成功后再取一次；后一次调用换掉前一次的任务，于是慢响应盖不住新结果。`open_session` 不清它——它跟的是宿主进程不是会话。
- 失败会走 `fail`（输入区下面那行红字），不只写日志：这是用户主动点的动作，静默失败等于按钮坏了。
- ⚠️ **模型选择面板这一批没成，代码已撤回**。三种铺法在 wasm 宿主上都画不出来：（1）组件的 `component::popover::Popover`——点触发器后浮层从未出现，`Button` 会把容器上的 mouse down 吃掉（它自己要处理点击），把开合挪到按钮自己的 `on_click` 上仍然不画；（2）照命令面板自绘 `absolute` 浮层——同一个容器、同一种锚法，标志位为真时也不画；（3）排进列里——同样不画。**同一轮里用「标志位为真才渲染的方块」证明了状态是对的**（点一下出现、再点消失、点别处不变），所以问题在渲染不在状态。撤回时一并删掉了随它进来的纯推导（分组/过滤/当前项判定）与 `/api/models*` 两个端点，不留死代码。
- ✅ **上面那条的归因已被推翻（同日，探针页）**。`crates/astrcode-webui/www/probe.html` + wasm 里的 `run_probe` 入口是一个不连服务端的探针页，五种铺法各占一块固定几何的纯色，验收脚本（`.consult/webui-spike/e2e/overlay-probe.mjs`，不入库）按颜色找区域判定，**9/9 通过**：普通绝对定位、`deferred()` 叠在绝对定位之上、`deferred()` 直接排在流里、组件 `Popover`（默认打开）、以及「点一下才出现的自绘绝对定位浮层」**全部画得出来**。也就是说延迟绘制与组件浮层在 Web 宿主上是好的，第十批的失败不是它们造成的。
- **探针顺带复现出一个真陷阱**：浮层默认开着时，它的遮挡面正好压住下面那个按钮，第一次点击落在浮层里、连 `on_click` 都没执行（`console` 里一条处理器日志都没有）——与第十批记的「`Button` 会把容器上的 mouse down 吃掉」「点触发器后浮层从未出现」是同一类现象。查浮层问题时，**先证明点击真的到了处理器上**（本次的判据是处理器里的 `log::warn!` 有没有出现在页面 console），再谈画不画得出来。
- 刻意没跟的：模型选择、附件（`ComposerAttachments`）、hero 呈现、Queue/Inject 投递模式。
- 验证：`cargo fmt --all --check` 干净，`cargo clippy -p astrcode-ui --all-targets --no-deps -- -D warnings` 干净，`cargo test -p astrcode-ui --lib` 86 项通过，`cargo check -p astrcode-gui -p astrcode-webui --all-targets` 通过。**浏览器验收 5/5 通过**，脚本 `.consult/webui-spike/e2e/composer-config.mjs`（不入库；驱动 `/tmp/astrcode-composer-e2e/run.sh` 同样不入库）：隔离 home 里放一份真实配置（拷进去的，服务端真的改它），起真 server + `tools/dev_server.py` 静态代理——页面装载无 panic；输入区那一行右侧找到权限模式与发送两段控件；点一下服务端收到 `POST /api/config/active-selection` 且 `GET /api/config` 从 manual 翻成 yolo；再点一下翻回 manual（同一个按钮的两态都能写回）；两次点击后工具条那一行确实重画过（与基线差 2982 个像素）。
- **这一批验收测到哪一层必须说清**：验收用的是隔离 home 里的配置副本，改的是副本不是用户真实配置（用户那份的 activeSelection 全程没动）。按钮的像素只断言到「点一下画面变了」，没有逐字形断言。

已落地（第十一批：输入区的模型选择面板）：

- 推导层（`src/composer_config.rs` 扩写）：`wire_format_label`（照搬前端 `providerWireFormatLabel`）、`current_model_label`（取到写模型 id，没取到按加载态给「加载中…」/「未选择」）、`model_groups`（按 profile 分组，段内保持原顺序、段间按首次出现顺序，过滤口径同前端：profile 名或模型 id 命中即留）、`is_current_model`（profile 与模型 id 要同时对得上——同名模型在另一个 profile 下不算选中）、`empty_model_note`（「无结果」/「未配置模型」）。3 项新单测。
- 端点（`src/api.rs`）：`GET /api/models`（平铺全部可选模型）与 `GET /api/models/current`。
- 面板（`views::chat`）：触发器写当前模型名（上限 140px 截断）+ 展开箭头，展开时铺底色、悬停时换文字色；浮层贴着触发器**左上方**（`relative` + `left_0` + `bottom_full`，宽 240，列表高度上限 240），上半是搜索框、下半是按 profile 分组的行（段名写 `profile · 线缆格式`，当前项打勾且用 `primary` 文字）。开面板时清空搜索词、重取一次清单、把焦点交给搜索框；Escape 与点面板外收起。
- ⚠️ **`on_mouse_down_out` 看的是元素自己的边界，不是子树** —— 这一点不写下来下次还会踩。最初把收起监听挂在外层那个 `relative` 容器（触发器所在的那层）上，结果**点面板自己的搜索框也会把面板收起来**：面板是绝对定位的，画在那层边界之外，于是「点面板里面」被判成「点外面」。改为挂在面板本体上，并且触发器那一下要防重复：`on_mouse_down_out` 在**捕获阶段**，早于触发器的 `on_click`，所以点触发器时面板已经被收起了，`on_click` 再无条件翻面就会又收又开；照 gpui-base `Popover` 的做法，只在当前状态与本次渲染时看到的那个值一致时才翻（`if this.model_panel_open == open`）。
- 写回同样走 `composer_config::selection_request`：把当前权限模式与小模型原样带上。成功后**配置与模型清单都要重取**——只重取清单会让权限开关手里那份选区变陈旧（下一次切权限就会把模型改回去）。
- 清单取数是后台行为：失败只 `tracing::warn!` 并维持上一次的值（与 `load_config` / `load_commands` 同口径），不往输入区下面那行红字上写。
- 刻意没跟的：前端的 `modelRefreshKey`（用任务句柄替换表达同一件事）、面板展开动画。
- 验证：`cargo fmt --all --check` 干净，`cargo clippy -p astrcode-ui --all-targets --no-deps -- -D warnings` 干净，`cargo test -p astrcode-ui --lib` 89 项通过。**浏览器验收 15/15 通过**（模型选择 10/10 + 权限模式 5/5），脚本 `.consult/webui-spike/e2e/model-selector.mjs` 与 `composer-config.mjs`（均不入库，共用 `composer-row.mjs`；驱动 `/tmp/astrcode-model-e2e/run.sh` 同样不入库）：隔离 home 里一份真实配置，起真 server + 静态代理——点触发器后面板真的画出来了（输入区上方多出一块 232x283 的浮层，变化像素 19705）；点自己的搜索框面板**不收**；点面板里最下面那一行写回的 `{activeProfile, activeModel}` **在 `/api/models` 里真实存在**，写回后 `GET /api/config` 的 activeModel 跟着变；再点一次触发器收起；点面板之外也收起。
- **这一批验收测到哪一层必须说清**：面板里点的是「扫出来的最下面一条横带」，不是按名字点某一行，所以「点中的是哪一行」没有逐字断言；守住的只是「POST 里带的 profile+model 是真实存在的组合」这一条。改的仍是隔离 home 里的配置副本（用户那份全程没动）。
- **验收脚本本身踩了两个坑，都值得记**（都是扫描口径，不是被测代码的问题）：（1）控件的底色不能取「角落那个像素」——浮层四角是描边与圆角，拿边框色当底色会让整块内部都显得「有内容」，八条文字行并成一条；（2）横带扫描要横向内缩两个像素，否则描边那一列会让每一行都算有内容。修完后横带才正确地分成「搜索框 / 段名 / 各行」。
- 探针入口（`run_probe` + `www/probe.html`）**保留**：它是 Web 宿主上唯一能直接回答「这类浮层画不画得出来」的手段，而 `Dialog`/`Menu`/`Tooltip` 这些同样走延迟层的组件还在后面。字体与主题设置已抽成两个入口共用的 `configure`，不留重复。
已落地（第十二批：输入区状态行与投递模式）：

- 状态行（`views::chat::render_status_row`）：输入区上方一行——项目名（工作目录末段，由外壳按会话注入）、`本地`、分支（插件状态栏项里 `git-branch`/`branch`/`gitBranch` 三个 id 之一）、瞬态重试状态（`control.retry_status`：远端状态码或「连接中断」+ 次数与退避秒数）、其余非空状态栏项，以及会话指标。宽度上限逐项照搬前端（项目 220px / 分支 180px / 状态项 160px）。
- `src/metrics.rs`（纯推导、无 gpui）：照搬前端 `ConversationMetricsBar`——读数取**最近一次模型请求**（输入、输出、缓存命中率）加上下文占用与生成速度，顺序与守卫逐条对齐；没有样本的指标不出项（尚无请求、输入读数为 0、上下文缺窗口或窗口为 0）。token 数按量级缩写（`1.0K` / `1.00M`）。
- 投递模式与待发队列（`src/composer_queue.rs`，同样纯推导）：忙时按下的发送按投递模式走——`Queue` 排进本地队列，`Inject` 直接 `POST /api/sessions/{id}/inject`；`/compact` 与已注册的斜杠命令例外，忙时照旧直接提交（前端 `isRegisteredSlashCommand` 同判据，这条判据落在 `slash_command::is_registered_command`）。可注入与否认 `activeTurnId` 且压缩期间为假（前端 `canInjectMidTurn`）。
- 队列面板（`views::chat::render_queue_panel`）：`N queued` 一行可折叠，展开后每条一行「正文 + 编辑 / 重发 / Inject / 删除」，四个动作都按前端 `PendingMessagesPanel` 的语义接上（编辑取回正文写进输入区、重发失败回队列、Inject 需要活跃 turn、删除）。队列与投递模式在切会话时清空（前端 `resetSessionView` 把两者都算进会话态）。
- 冲刷时机：`after_state_change` 里判「不在执行阶段且队列非空」就依次提交，失败的一条回队列并留提示、其余继续。**一处刻意不同**：前端那边队列长度一变就会再冲一次，失败时会一直重试；这里一趟冲刷在飞时不再起第二趟（句柄被换掉会取消上一趟，而它手里的条目已经不在队列里），失败留下的条目等下一个 turn 结束或用户手动重发。
- 提示位：inject 打不进去时回落队列、排队提交失败、重发失败都写进输入区下方的一行弱色提示（下一次提交时清掉）。前端的 `TransientHintDialog` 是一枚弹窗，这里没有照搬弹窗形态。
- 刻意没跟的：指标项的悬停说明（前端 `title` 里的精确 token 数与累计值）——这套 gpui-kit 的浮层只对自带组件（Button/Switch/Checkbox/Radio/Clipboard/input Group）开放，给任意元素挂悬停说明要另接一层覆盖物；`uppercase tracking-wider`、行内动作的 `opacity-60 → hover:100`、动画。
- 验证：`cargo fmt --all --check` 干净，`cargo clippy -p astrcode-ui --all-targets --no-deps -- -D warnings` 干净，`cargo test -p astrcode-ui --lib` 158 项通过（新增 14 项：队列 8、指标 5、已注册命令 1）；`cargo check -p astrcode-gui --all-targets`、`-p astrcode-webui --all-targets` 通过（webui 那三条 unused dependency 警告是既有的）。**浏览器验收 9/9 通过**（wasm 重建后在无头 chromium 上跑，脚本 `.consult/webui-spike/e2e/composer-queue.mjs` 与桩后端 `/tmp/astrcode-queue-e2e/{stub_api.py,run.sh}`，都不入库）：桩把控制态做成 streaming（有活跃 turn），实测——turn 跑着时回车**不发** `/prompt`；队列面板让输入区的内容带 2 → 4 条；桩把控制态翻成 idle 之后 `/prompt` 真的收到那条文本、内容带落回 2 条；点投递模式开关只改了那一行、没有顺带敲开旁边的模型面板（`/api/models` 计数不变）；Inject 档下回车走 `/inject` 且没有新增 `/prompt`；状态行实测 8 段文字（项目、本地、分支、输入、缓存、输出、上下文、速度），与设计逐项对上；全程无 panic。
- **这一批验收测到哪一层必须说清**：桩替换了整个后端，所以验的是「客户端在给定控制态下把输入送去哪里」，不涉及真实服务端的注入窗口与队列语义；真实模型回合（含 `/compact` 与服务端拒绝注入那两条分支）仍未端到端跑过。

已落地（第十三批：侧边栏打平分）：

- 会话列表分组与排序（`src/session_list.rs`，纯推导、无 gpui）：照搬前端 `Sidebar.tsx` 与 `projectFolderOrder.ts`——项目顺序首次按各组最早会话的 `createdAt` 定序，之后只做「删掉的移除、新的追加」；组内按最近使用降序（`updatedAt`，相同则 `createdAt`）；会话行取「首条用户消息 → 标题 → 新对话」；折叠集合按当前项目剪枝（要落盘的那一份不能一直留着已删项目的目录）。项目名沿用「路径末段」那个取法，从 `views::chat` 挪进来共用。10 项新单测。
- 界面偏好（`preferences.rs` 扩写 + `src/api.rs`）：`UiPreferences` 是界面侧那份当前值——`PUT /api/preferences` 是**整份替换**，写回时必须把不属于自己的字段（看板那两项）原样带上；宽度在读写两侧都夹进 240–380，服务端只存不解释。新增 `DELETE /api/sessions/{id}` 与 `DELETE /api/projects?workingDir=…`（工作目录经 `url::form_urlencoded` 转义）。3 项新单测。
- 侧边栏（`views/sidebar.rs`）：项目分组（组头 = 文件夹图标 + 项目名 + 折叠箭头；整行点选组内最近会话，箭头只折叠）+ 组内会话行；页头与「新对话」「看板」导航；「会话」标题行带刷新；会话行与项目组头右键出菜单（Fork 会话 / 删除会话 / 删除项目），删除要先确认一次。
- 宽度拖拽（`views/shell.rs`）：侧边栏右边缘 4px 把手，拖拽期间只改界面宽度（夹取实时生效），松手才写回偏好。偏好取回**之前**用户已改过的话，等服务端那份回来再补一次写回——否则本地 `defaults()` 里那份空的看板路径会把服务端已存的内容抹掉。
- 删除与 Fork 之后的选中语义：删会话时优先留住当前会话，它没了就退到第一条；删项目时跳过被删目录下的会话（跨了一次 HTTP 的列表不值得信，宁可退到「没有会话」也不去选一个已经删掉的）；一个都不剩就清空会话面板（`reset_session_view` 从 `open_session` 里抽出来两边共用）。
- **两处踩坑，写下来**：（1）`on_mouse_down_out` 在**绝对定位且高度由内容决定**的浮层上判「外面」不可靠——菜单画得对，点菜单里那一下却被判成「点外面」，于是菜单一碰就收、菜单项永远点不中；（2）改成「满尺寸的层 + 流内菜单」后点击才生效：层铺满侧边栏并接住外面的点击，菜单是层的流内子元素、只靠外边距挪到点击处，点菜单自己由 `stop_propagation` 拦下。第十一批记过 `on_mouse_down_out` 的边界是「元素自己而不是子树」，这一批补的是「绝对定位 + 内容高度」这一种情形。
- 刻意没跟的：新建项目弹窗（`NewProjectModal`，要接一层目录选择器）、插件入口、鼠标悬停光标（`cursor-col-resize`）、页脚的 AS 头像与设置入口（设置页还没做）。前端的 `SessionItem.tsx` / `ProjectGroup.tsx` 是死文件（`Sidebar.tsx` 把行内联了），没有照搬。批量选择删除当时也没跟，后来单独补上了（第十四批）；新建项目弹窗与页脚的 AS 头像也补上了（第十五批，插件与设置入口仍等各自的页面）。
- 一处**主动偏离**（第十五批已消除）：前端「新对话」在一个项目都没有时打开新建项目弹窗；当时这里退回宿主注入的工作目录（弹窗还没做）。
- 验证：`cargo fmt --all --check` 干净，`cargo clippy -p astrcode-ui --all-targets --no-deps -- -D warnings` 干净，`cargo test -p astrcode-ui --lib` 171 项通过（新增 13 项：分组/排序/折叠剪枝 8、删除后选中 2、偏好 3）；`cargo check -p astrcode-gui --all-targets`、`-p astrcode-webui --all-targets` 通过。**浏览器验收 15/15 通过**（wasm 重建后在无头 chromium 上跑，脚本 `.consult/webui-spike/e2e/sidebar-parity.mjs` 与桩后端 `/tmp/astrcode-sidebar-e2e/{stub_api.py,run.sh}`，都不入库）：桩给两个项目三条会话——侧边栏画出 7 条内容带（活跃项目头与活跃会话行底色相连会并成一条）；点箭头折叠后组内行消失、`PUT /api/preferences` 收到 `collapsedProjectDirs:["…/alpha"]` 且看板那两项原样带着；再点一次展开回到原状；拖右边缘 40px 后聊天区左边界从 304 移到 344，松手写回 `sidebarWidth:340`；右键会话行出菜单、点「删除会话」进确认态、确认后 `DELETE /api/sessions/s-alpha-new` 且该行消失；右键项目组头 → 删除项目 → 确认后 `DELETE /api/projects?workingDir=%2Ftmp%2F…%2Fbeta`（工作目录确实被转义）；全程无 panic。
- **这一批验收测到哪一层必须说清**：桩替换了整个后端，「删除之后服务端真的把会话删了」没验（桩自己维护列表，删了就不再返回，只证明客户端按预期打了端点并随之重取）；偏好写回只断言请求体内容与像素位移，没验服务端对越界宽度的处理（服务端不解释，只存）。
- 验收脚本自己踩的两个坑也记一下（扫描口径，不是被测代码的问题）：（1）活跃项目头与活跃会话行的底色相连时，横带扫描会把它们并成一条，断言得按这一点写；（2）确认态的「删除」与「取消」两个按钮相邻，笔画间隔放宽会把它们并成一段，而那一段的中心正好落在两格之间的缝里——点下去什么都不会发生。

已落地（第十四批：侧边栏的批量选择删除）：

- 选择态（`views/sidebar.rs` + `session_list.rs` 的四个纯函数）：表头那行「会话 / 刷新 / 选择」整块换成一张卡片——上行「已选 N 项 + 取消」，下行「全选 / 取消全选 + 删除」；删除按钮在 `count == 0` 时禁用（同一颗按钮，禁用态只是标签不着红）。每行左边多出一个 16px 勾选框，**整行都是它的点击区**：选择态里点行只切勾选、不切会话（`is_active` 也一并关掉——那一刻整份列表在讲另一件事），项目组头在选择态里只切折叠，右键菜单一概不给。勾选集合按当前列表剪枝（`prune_selected`），「全选」要求列表非空且两端可翻（`toggle_select_all` / `all_selected`）。3 项新单测。
- 删除要先确认：第一次点删除只把卡片换成确认态（问句 + 取消 / 删除），确认后才发事件。外壳收到 `DeleteSessions` 后**逐条**发 `DELETE /api/sessions/{id}`（一条失败不挡其余，失败只报一句），再重取列表并按 `pick_active_after_delete` 决定选中谁。
- 验证：`cargo fmt --all --check` 干净，`cargo clippy -p astrcode-ui --all-targets --no-deps -- -D warnings` 干净，`cargo test -p astrcode-ui --lib` 174 项通过（第十三批是 171 项，多出的 3 项就是 `selection_*` 那三条）。**浏览器验收 15/15 通过**（本轮补的，脚本 `.consult/webui-spike/e2e/sidebar-batch-delete.mjs` 与驱动 `/tmp/astrcode-sidebar-e2e/probe.sh`，都不入库）：桩给两个项目三条会话——点「选择」后表头那 16px 的一行变成 90px 的卡片（115~204），卡片上行右端量得到「取消」的字形、下行的两个底块是「全选 / 删除」；一个都没勾时卡片内红字像素 0，勾上一行后变 206（按钮转可用）；点行只勾选——全程 `GET /api/sessions/{id}/conversation` 请求 1 → 1，没有多出来；点全选后三行的勾选框近白像素 `[211,211,211]`；第一次点删除后卡片缩到 115~191、右上角那块「取消」字形消失（上行换成问句）、`DELETE` 条数仍为 0；点取消回到选择态、三个勾选还在、仍然 0 条 DELETE；再点删除并确认后 `DELETE` 打的是 `s-alpha-new / s-beta-one / s-alpha-old` 三条（一次一条），`DELETE /api/projects` 0 条；删完侧边栏只剩三条横带、会话行 0 条、表头那两个按钮也不再画；全程无 panic。
- **这一批验收测到哪一层必须说清**：桩替换了整个后端，会话表由桩自己维护（删掉就不再返回），所以「服务端真的删了」没验，验的是客户端按预期逐条打端点、并随之重取列表；确认态那条断言判的是「布局换了 + 还没有 DELETE」，问句文字本身没有逐字断言（画在 canvas 上读不到文本）。
- 验收脚本自己踩的三个坑记一下（都是扫描口径，不是被测代码的问题）：（1）把横带 `[top, bottom]` 当区域对象传进按 `{left, right, top, bottom}` 取值的计数函数，`box.top` 是 `undefined`、循环一次都不进，计数恒为 0——一条断言因此假通过、另一条假失败，两侧都骗人；（2）勾上之后行内第一簇近白像素是勾选框（x≈40）而不是标签（x≈65），按「第一簇」找行的判定会全部落空，得跳过 60 之前的那一簇；（3）「全选」勾满后按钮标签变成「取消全选」（四字），删除按钮因此右移 32px，沿用进选择态时量的坐标点不中，得在点之前重新量。

待打平（优先级从高到低）：

1. **输入区其余能力**：`InputBar.tsx` 810 行里还没搬的部分——附件（`ComposerAttachments`，图片选择与粘贴）、hero 呈现（空会话时输入区居中）。
   - ~~先查清延迟浮层在 wasm 上能不能画出来~~ **已结清**：能。见第十批那条 ✅ 记录与探针页 `www/probe.html`。查浮层失败时先证「点击到了处理器」，再谈渲染。
   - 模型选择已落地（第十一批）；Queue/Inject 投递模式与待发队列、输入区上方的状态行（含会话指标行）已落地（第十二批）。
2. **转录的流式渲染策略**：第 28–31 轮结论要求按块切分 + 只渲可见尾部；当前整篇交给一个 `TextView`，长会话会撞 16ms 帧预算。
3. **会话历史窗口**：第 15 轮（向后分页、`detachedFromLatest`、「回到最新」）。
4. **其余 Chat 附属面**：`CompactSummaryCard` 的 token 明细、`useKeybindings` 的键位、工具卡的运行中耗时（需要每帧滴答的任务，前端把命令行的「已持续 Ns」和回合总耗时都算在这上面）、`TopBar` 的「N 个子会话」汇总。（`ConversationMetricsBar` 已落地，见第十二批。）
5. **侧边栏打平**：项目分组（`ProjectGroup`）、折叠状态、宽度拖拽（`useSidebarResize`）、会话与项目的删除 / Fork（第十三批）、批量选择删除（第十四批）、新建项目弹窗与页头「新项目 / 收起边栏」两枚小按钮、页脚身份块（第十五批）已落地；还差插件入口与页脚设置入口（各自等页面落地）。
   - **「会话重命名」这一项已核销**（2026-10-03）：前端 `Sidebar.tsx` 的会话右键菜单只有「Fork 会话」与「删除会话」（`Sidebar.tsx:729-750`），服务端也没有改标题的路由，待打平清单里一直写着的这一项是空头条目，已从表中去掉。
   - 「新对话」在一个项目都没有时打开新建项目弹窗（第十五批对齐前端的 `handleCreateConversation`，第十三批的主动偏离已消除）。

### 1.2 看板打平批次（2026-10）

数据层先落地，视图才能只负责画。

已落地（第三批 A：数据层，`crates/astrcode-ui/src/kanban/`）：

- 线缆镜像（`wire.rs`）：`GET /board`、`POST /cards`、`PATCH /cards/{id}`、`DELETE /cards/{id}`、`POST /directories` 的请求与响应形状。**刻意不加 `deny_unknown_fields`、可选字段一律给默认值**——线缆形状归扩展所有，扩展加字段不该让宿主解码失败（用扩展里真实存在的 `errorRetries` 写了一条测试固化）。宿主也不复刻扩展的默认值（`column=backlog`、`workingDir=配置默认`、`date=创建当天`）：省略的字段一律不出现在请求体里，两边才不会漂移。
- 日历分桶（`calendar.rs`）：四种刻度的桶列表与覆盖范围（日看当月、周看覆盖当月的整周、月看当年、年看当前十年）、翻页、锚点标签、归属日归一。日键必须是补零的 `YYYY-MM-DD`——`chrono` 会收下 `2026-2-3`，放过去会让月/年桶切出错标签，因此解析前先做逐字节规范性校验；不规范的输入返回空串（不编造日期），由调用方归进「未排期」。
- 路径历史（`path_history.rs`）：MRU 10 / ignored 50、`remember` 撤销忽略、`forget` 记忽略、候选并集（历史顺序优先、去重、丢空、过滤忽略）。**纯函数、不落盘**：落盘由 UI 偏好统一负责（第四批接）。
- 列/槽位/落点词汇（`mod.rs`）：两区恰好覆盖六列（公共区四格 + 日历两槽位，不新增也不隐藏）、只有 `ready`/`blocked` 接受公共区落点、只有日刻度的桶键带真日期、`DropTarget` 结构相等即同一落点、`extension_available` 门控。
- 客户端（`src/api.rs`）：`list_extensions` 与五个看板方法；`patch_json` / `delete_empty` / `send_without_body` / `require_success` 四个原语从既有的 `get_json` / `post_empty` 里抽出来复用。

已落地（第三批 B/C：视图、入口与切换，`views/kanban.rs` + `views/{sidebar,shell}.rs`）：

- 两区六列：左侧公共区四格（`待领取`/`分析中`/`实施中`/`已阻塞`，每格列头有状态色圆点、数量角标与一句说明），右侧日历（刻度条 + 翻页 + 「回到今天」），每个时间桶两个手风琴项（`待办`/`已完成`）。空桶收缩到 144px、有卡片的桶占视口四分之一；今天那一列强调。
- 手风琴三态照搬前端：两项对半 → 点开一项 → 再点它收回对半。展开态只挂在标题条上：卡片在下面，点卡片不该顺带收起这一项。年刻度只给数量不列卡片（一列要塞进整年）。
- 自绘拖拽走 gpui-kit 的框架拖拽（`on_drag` / `on_drop`）：跨容器投递与跟随指针的幽灵都由框架负责，本页只在落点上判断「落在哪一列、哪一天」；落点高亮挂 `drag_over` 的样式，不自存悬停状态——本版 gpui 的 `on_drag_move` 在这些落点上不触发（按它写高亮时浏览器验收量到 0 变化像素），而 `drag_over` 与真正生效的 `on_drop` 共用同一条命中判定。运行中的两列不给拖拽把手（扩展独占，写进去只会拿到 400），落在原处是空操作。
- 入场门控（`kanban::extension_available`）：扩展在册、已启用、已加载三条缺一不可（与前端 `kanbanExtensionAvailable` 同判据）。侧边栏的「看板」项按它出现，外壳也按它拒绝停在看板页。
- 页面切换（`views::{MainView, sidebar, shell}`）：`MainView` 是导航项与外壳共用的取值；点卡片跳对话 = 切回对话页再开那个会话。
- 轮询跟可见性走：看板只在显示时每 5 秒拉一次，隐藏即丢弃轮询句柄。扩展缺席时那个路由打不通，隐藏期间继续轮询只会每 5 秒刷出一条同样的错误（前端同样只在页面挂载期间刷新）。初始版本在 `new` 里就起了轮询，属于本批自查修掉的缺陷。
- 刻意没跟的：卡片编辑弹窗（新建/编辑/文件夹选择器）、框选与多选成组拖拽、卡片上的列下拉框与折叠操作行、槽位内按项目分组、空列槽位标签的「悬停才显形」（改为常显低透明度）、卡片没有绑定会话时点击给提示（现在无反馈）。前四项是下一个批次的主体。
- 静态验证：`cargo fmt --all --check` 干净；`cargo test -p astrcode-ui --lib` 143 项通过（看板相关 47 项）；`cargo clippy -p astrcode-ui --all-targets -- -D warnings` 在 `astrcode-ui` 自身 0 告警（唯一挡住的是 `crates/astrcode-protocol/src/http.rs:479` 的既存 `large_enum_variant`，与本批无关）；`cargo check -p astrcode-gui -p astrcode-webui --all-targets` 与 `cargo check -p astrcode-webui --lib --target wasm32-unknown-unknown` 通过。
- 浏览器验收（2026-10-03，wasm 重建后对着真实 server + 真看板扩展的隔离 home 跑无头 chromium）：**20 项全通**——三条落点解析路径都对（公共区→公共区；日历槽→公共区且归属日不变；公共区→日历槽且归属日改今天）、落回原处不发 PATCH、在没有落点的地方松手不写也不留残影、悬停落点时格子描边与整条槽高亮、离开落点后高亮清掉、点已完成卡片跳到它的对话并离开看板页、停用扩展后侧边栏入口消失、全程无 panic。**验收查出两处真缺陷，都已修**：(1) 日历区（含它那条横向滚动条）缺 `min_w_0`，gpui 把滚动内容的 min-content 当成自动最小宽度顶上来，该格被撑到一屏只放得下一个时间桶；(2) 拖拽高亮挂在 `on_drag_move` 上，而本版 gpui 在这些落点上不派发它，改成 `drag_over` 的样式。验收脚本 `.consult/webui-spike/e2e/kanban-board.mjs` 与驱动 `/tmp/astrcode-kanban-e2e/run.sh`（`.consult/` 被 gitignore，不入库）；驱动用隔离 home 且**不开** `automationEnabled`——否则验收会让自动化真的领卡片、开模型回合花用户的钱。**没覆盖**：月/年刻度、运行中的两列（扩展独占、不给落点）、批量拖拽（前端支持「拖一张就是拖一组」），以及原生 `astrcode-gui` 宿主——这三项留给后续批次与桌面端手测。

### 1.3 看板高度修补（2026-10-03）

看板页在 900px 高的窗口里只占中间一条：公共区四格落在 y 236–711、日历的时间桶落在 y 318–630，上下各空掉约 190px，窗口越高留白越多。

- 根因不是哪一处高度写死，而是 gpui 的 `h_flex` 与 `v_flex` 在交叉轴上不对称：`v_flex` 让子元素拉伸，`h_flex` 却居中（gpui-base `styled.rs` 的 `h_flex` 文档把这条写成了陷阱）。看板页把公共区与日历放进一个 `h_flex` 行里，两个子元素于是都只拿自己的内容高度并被居中，行内的 `flex_1` 与日历桶的 `h_full` 都拿不到可解析的高度。
- 修法是一行（`views/kanban.rs:1068`）：行上加 `items_stretch()`。两个区域改成按行高解析后，行内的 `flex_1` / 滚动区 / `h_full` 才真正生效。刻意没动的：公共区 20% 宽、空桶 144px、卡片折叠高度 54px 这些份额与尺寸照旧；这一行也不涉及任何数据流。
- 顺带修的是**验收脚本**（`.consult/` 被 gitignore，不入库）。它有三处跟着实现漂了，看板片的验收实际上自侧边栏默认宽度改成 300（`preferences::SIDEBAR_WIDTH_DEFAULT`）之后就再没跑到过看板页：(1) `SIDEBAR` 常量还是旧宽度时代的 260，改成侧边栏 300 + 右边缘 4px 把手；(2) 点导航项时取的是侧边栏第二条内容带，而那条带现在是「新对话」（第一项），于是验收一直在点「新对话」而不是「看板」——server 日志里多出来的那次 `POST /api/sessions` 就是它，改成取「看板」那一行的文字簇；(3) 格子描边贴着内缩带扫面板色，而公共区外壳早就是页面底色 + 1px 描边，改成扫左描边、并从量到的顶边往上找真实顶描边（左描边被圆角上下各切掉几像素）。停用扩展那条断言也从「导航簇只有 1 个」改成按导航项形状数（那条带现在是「会话」表头，单看 y 区间分不出来）。
- 验证：`cargo fmt --all --check` 干净；`cargo test -p astrcode-ui --lib` 174 项通过；`cargo clippy -p astrcode-ui --all-targets --no-deps -- -D warnings` 干净。**浏览器验收 20/20 通过**（驱动 `/tmp/astrcode-kanban-e2e/run.sh`）：公共区四格 y 62–886（每格 189px，修前 101px）、今天那一列 y 98–890（修前 312px），并顺带复验了三条拖拽落点、落点高亮、点卡片跳会话与扩展停用后入口消失。

### 1.4 侧边栏小按钮与新建项目弹窗（2026-10-03）

前端这三块是三个组件（`Sidebar.tsx` 的页头页脚、`NewProjectModal.tsx`、`ProjectPathField.tsx` + `ProjectFolderPicker.tsx`），桌面端落成两个落点：`views/new_project.rs` 一个弹窗，`views/sidebar.rs` 页头两枚按钮 + 页脚身份块。

- 弹窗形态：手绘覆盖层——铺满一层 + 流内居中卡片，遮罩点击由卡片自己 `stop_propagation` 拦下。不走组件库的 `Dialog`：那条线要两个宿主都包一层 `Root`，本轮按最小改动不动宿主。文件夹选择器开着时**替换**卡片内容，而不是叠第二层弹窗，因此不存在「一次点击关两层」的协调问题（前端用一个 `pickerOpen` 开关挡掉 Esc；这里连这一层都不需要）。
- 路径候选复用第十批的 `kanban::path_history`（MRU 10 / ignored 50、合并与忽略过滤都是既有纯函数），目录列举复用既有的 `Api::kanban_list_directories`——**与前端同一处耦合**：那两条路由属于看板扩展，扩展缺席时选择器只会报错（前端的 `ProjectFolderPicker` 同样如此）。历史与忽略集的落盘仍归外壳：`PUT /api/preferences` 是整份替换，只能有一个写者，弹窗只发 `ForgetPath` 事件，外壳调 `kanban::{remember,forget}_project_path` 后落盘。
- 候选来源与前端同口径：默认目录取当前会话的项目（没有就退宿主工作目录），额外候选是会话列表工作目录的投影（不去重不排序，去重由合并那一步做）。
- 页头两枚小按钮：`+`（新项目）与边栏图标（收起）。收起后侧边栏与那条 4px 拖拽把手一起消失，展开入口移到两个主区域的页头（`ChatEvent::ToggleSidebar` / `KanbanEvent::ToggleSidebar`）——否则收起来就回不去了。页脚是 AS 头像 + `AstrCode`，设置入口等设置页落地后挂在这里。
- 建会话失败时报在弹窗卡片里，而不是侧边栏底部的错误位：弹窗正挡着那一格，消息会被埋掉。失败后输入与按钮放开，用户能改路径重试。
- **一处刻意不同**：前端传 `defaultWorkingDir=''` 时占位符写「项目路径」、选择器由扩展回落到自己的当前目录；这里默认目录直接取宿主注入的工作目录（Web 宿主为空串，选择器便落到服务端当前目录），占位符因此会写出那个目录。语义等价，少一次「空串 → 服务端 cwd」的隐式约定。
- 自查对着前端核出来并修掉的两处：候选为空时不画面板（前端 `open={pathMenuOpen && pathCandidates.length > 0}`，否则只剩一道空描边）；建会话请求在飞时输入框禁用（前端 `disabled={loading}`）。
- 静态验证：`cargo fmt --all --check` 干净；`cargo clippy -p astrcode-ui --all-targets --no-deps -- -D warnings` 干净；`cargo test -p astrcode-ui --lib` 178 项通过（新增 4 项：提交路径的去空白与空串判据——「按钮亮不亮」与「实际建在哪个目录」共用这一个判断；以及 `PendingPreferences::merge` 的重放规则 3 项）；`cargo check -p astrcode-gui -p astrcode-webui --all-targets` 通过；`cargo check -p astrcode-webui --lib --target wasm32-unknown-unknown` 真的重编译了 `astrcode-ui` + `astrcode-webui` 并通过。
- **浏览器验收 17/17 + 5/5 通过**（本轮补的；脚本 `.consult/webui-spike/e2e/sidebar-new-project.mjs`，桩与驱动 `/tmp/astrcode-newproject-e2e/{stub_api.py,run.sh}`，都不入库）。两趟各起一次桩：一趟验弹窗与页头页脚，一趟把偏好响应压 5 秒验补写。实测：页脚 AS 头像底色的像素 377；页头悬停扫出两处 32px 方块（`[212,240]` / `[252,280]`）；点 `+` 后遮罩把远离卡片那一点从 `(10,10,10)` 压到 `(8,8,8)`、卡片 360~820 / y 288~490、内部四条内容带；点输入框后面板 y 424~488、行高 19 → 3 行候选；点第一行（历史里那条）→ `POST /api/sessions {"workingDir":"…/ws/stored"}`；选择文件夹 → 卡片高 464、`POST …/directories {"path":"…/ws/stored"}`（默认目录是**当前活跃项目**，上一步刚建的那条会话就是它）；点列表第一条 → 再列举 `…/ws/stored/inner`；选择此文件夹 → 卡片回到 460 宽、再点创建 → 建在 `…/ws/stored/inner`；键入带 `boom` 的路径 → 桩回 400 → 卡片从 203 长到 269 且弹窗不退；Esc 先收面板（输入行下方的内容带从 65 回落到 32）、再按一次关窗；收起边栏后右侧两簇归零、聊天页头在 `[24,52]` 多出一簇，点它复原；两阶段全程无 pageerror / console.error（那条刻意造的 400 除外）。
- 补写那趟（偏好延迟 5 秒）：在路上建项目、等它落地后，`PUT /api/preferences` 的体是 `{sidebarWidth:300, collapsedProjectDirs:["…/ws/proj-b"], kanbanProjectPaths:["…/ws/alpha","…/ws/stored"], kanbanIgnoredProjectPaths:["…/ws/ignored"]}`——服务端存着的折叠集合与忽略集原样带着，本地那次「记住 alpha」重放到服务端那份上（而不是把它盖掉），本地没动过的字段一个没丢。
- **这一批验收测到哪一层必须说清**：桩替换了整个后端（建会话、目录列举、偏好读写都是桩），所以「真的建出了会话」「扩展真的列了目录」没验，验的是客户端按预期打端点、并按响应更新界面；候选面板里「点中的是哪一行」是靠随后 `POST /api/sessions` 的 `workingDir` 反推的（画在 canvas 上读不到文本）。
- 验收脚本本身踩的四个坑记一下（都是扫描口径，不是被测代码的问题）：（1）这一版主题里 `overlay` 几乎全黑，遮罩只把底色从 `(10,10,10)` 压到 `(8,8,8)`，而卡片自己的 `popover` 与页面底色**同为** `(10,10,10)`——「弹窗开着」不能看卡片颜色，要看远离卡片那点是否被压暗；卡片则按「与未压暗的底色相同、且不是整屏高」那一段单色列来找。（2）候选行之间没有纵向 padding，`contentBands` 会把几行并成一条，行高与行数得改用悬停高亮量（悬停会铺满整行）。（3）选择器卡片有四条内容带（关闭按钮 / 文件夹页眉 / 列表 / 底部按钮），按序号取列表会拿到页眉——得取最高的那一条。（4）选择器的起始目录是**当前活跃项目**，上一步刚建出的会话会把它换掉，写死期望值必然失败。
- **补浏览器验收时又核出并修掉的三处**（都是这一批引入的，或第一次被点到）：
  - **候选面板压在按钮行上，却是被按钮盖住的下层**：它是 `path_field` 的绝对定位子元素，绘制排在按钮行**之前**，于是前两行被按钮盖住、点不着（验收脚本点第一行候选时按到了按钮）。改用框架的 `deferred()` 延迟绘制：布局仍在原处，绘制排到该帧最后（前端把下拉挂到 `body` 上，效果相同）。
  - **Esc 只关弹窗，面板开着也照关**：改成先收候选面板、面板没开才是关弹窗——对应前端 `Dropdown.tsx` 的 Escape `stopPropagation`（它的注释就写着「只关掉最内层的弹层」）。Esc 靠应用级 `intercept_keystrokes` 接，所以弹窗**刻意不取焦**（前端的 `Modal` 把焦点交给对话框容器，这里连那一步也不做）。
  - **偏好补写的基准问题**（本条同时修掉第十三批留下的两处）：原实现把「取回之前改过」记成一个 bool，取回时在那份本地值上补写。可那份本地值不是「值」而是 delta——基准（服务端那份）还没到，于是（a）本轮新加的看板路径被服务端的旧值盖掉，（b）宽度与折叠集合被本地初值打回（第十三批起就在，只是没被点到），（c）服务端那份宽度与折叠集合压根没装到界面上。改成 `preferences::PendingPreferences`：取回之前的改动按字段记成 delta（宽度与折叠集合取最后一次，看板路径按发生顺序记操作），取回时重放到服务端那份上再写回。3 项新单测钉住重放规则。
  - 顺带去掉 `persist_preferences` 里「回读侧边栏折叠集合」那一步：`CollapsedChanged` 已经把它同步住了，而侧边栏那份在会话列表还没到的时候会被剪成空——回读会把「还没加载」写回成「用户全展开了」。

## 2. 切片顺序

1. **第一片：最小端到端 Chat**（第 6 轮）——连自己的 server（进程内 `bootstrap_with` + `127.0.0.1:0`，第 5 轮）、会话列表、发一条提示、流式 markdown、一个只读工具卡（`diff`）、一次审批往返。刻意做薄：无虚拟滚动、无视觉打磨、只一个渲染器。
2. **设置片**（第 11 轮）——紧接第一片，理由是 CLI 没有 `config` 子命令，设置页是唯一的图形化 provider 配置入口。例外：`appearance` 分区不照搬，桌面端主题语义不同。
3. **看板片**（第 10 轮）——排在最后，删 `frontend/` 前必须打平；照搬现有交互、用 gpui-kit 原语自绘拖拽，预授权降级为「下拉选列 + 键盘移动」。

第 1 片之后、看板之前的顺序（会话历史窗口、流式 markdown 打磨、外壳完善）由第 13/15/16 轮的约束推导而来，**未单独盘问**，可按实现时的依赖调整。

## 3. 三个 spike 的挂钩

第 21 轮定：不设统一门禁，分三档挂钩。

| spike | 钩子 | 观察点 |
| --- | --- | --- |
| cadence | 可与第一片的 UI 骨架并行，但**必须先于状态层落地** | **已完成（第 23–33 轮；第 33 轮收尾）**：推入 delta 几乎免费（debug 0.0004 / release 0.0001 ms/块）；单帧成本随文档大小陡增且无跨帧缓存，绝对值还受内容形态影响约 6.9 倍（构建历史经第 26 轮复核无影响），阈值以第 26 轮单窗口曲线为准（32 KiB 14.3 ms、64 KiB 35.5 ms、128 KiB 160 ms）；虚拟化已证有效（单窗口下只渲染尾部 16 块 1.20 ms，仍持有全部 512 个状态）；第 27 轮补测边写边渲：与写好后渲同价（7/15/31/62 KiB 为 2.5/5.0/13.3/35.4 ms），但 62 KiB 文档上追加 60 字节的单帧仍要 35 ms——**每帧按整篇计**；第 28 轮给出出路：按定长切块 + 只渲可见尾部后，帧成本与总长无关（7→118 KiB 全程 1.33–1.38 ms；同一份 118 KiB 全渲染对照 142–146 ms）；第 29 轮补齐最后一处：末块追加无特殊成本（与「内容未变」同价，1.32–1.43 ms）；第 30 轮把切块大小这个参数结掉：它基本不影响帧成本，唯一驱动量是被渲的可见字节数（约 0.4 ms/KiB 直到 16 KB）。**第 33 轮收尾，这一档门禁已开**：状态层可以落地。遗留观察点：帧层内存上限（1024 delta / 256KiB 文本）迁移后的实取值；切点规则（「不在围栏内切」，第 31 轮）随状态层一并实现。 |
| DnD | 看板片之前 | 跨容器拖拽是否可行；现实现是 HTML5 `draggable` + `dataTransfer`（`KanbanCardItem.tsx`），并支持「拖一张就是拖一组」的多选成组拖拽 |
| wasm | **只设截止线：删除日**，不设起点 | **已完成（2026-10-03，第 34 轮）：可行**。浏览器内能流式消费 SSE 增量、中文 IME 输入可用，两者都在无头 chromium 上跑通；落地形态是共享层 `crates/astrcode-ui` + 两个宿主（`astrcode-gui` / `astrcode-webui`），见 ADR 第 34 轮。半截围栏的渲染形态与长回答尾段的每块重解析成本已在第 23–31 轮以 cadence spike 名义测掉。产物已由 server 内嵌托管在 `/app`（`COOP`/`COEP` 齐备，无头 chromium 对真实 server 复核 7 项全通）；删除日把 `/` 换成它 |

wasm 的截止线是删除日：第 3 轮已定「spike 出结论前不引入 wasm 可移植性约束」，过早开工与之矛盾；但删除日一到，浏览器入口的存亡只能靠它的结论。**该结论已于 2026-10-03 落地（可行），浏览器入口保留**；产物内嵌托管也已完成（`webui_assets.rs`，挂 `/app`），删除日只剩把 `/` 换过来这一步。

## 4. 验收基线：`frontend/scripts/` 的分类

第 7 轮定：领域不变式类必须有一对一的 Rust 测试，Web 运行时权宜类明确宣布不重建。
第 17 轮附带修订：**按断言级而非文件级**判定。

判定值域：**领域不变式**（重建 + 一对一测试）／**部分重建**（同一文件里两类断言并存）／**不重建**（随前端删除，或属 Web 运行时权宜）。

| 脚本 | 行数 | 被测模块 | 判定 | 依据 |
| --- | --- | --- | --- | --- |
| `delta-coalesce.test.mjs` | 678 | `store/delta/{coalesce,applyDelta,effects,frameBuffer}.ts` | 部分重建 | 第 17 轮附带：`coalesceDeltas` 的相邻拼接属帧层可弃；`applyCoalescedDeltas` 的孤儿 patch 不变式（「不得在没有 start 或 durable request 时造出 block」）与 `reduceConversationDeltas` 的重试语义必须重建 |
| `protocol-contract.test.mjs` | 488 | `services/protocol.ts` + 生成物 | 不重建 | 第 19 轮：随 `frontend/` 与 ts-rs 生成链一起删除；npm 分发只发 CLI 二进制，无类型消费者 |
| `settings-model-options.test.mjs` | 367 | `Settings/settingsSupport.ts` | 领域不变式 | 本轮新读：`deriveThinkingFormValue` / `thinkingFormToRequest` / `effortOptions` / `isToggleOnlyThinking` 编码的是「模型 thinking 能力 → 表单值 → 请求」的映射，不是 React 权宜；设置片复用 `/api/models*`，这层映射必须存在 |
| `kanban-calendar.test.mjs` | 336 | `Kanban/{calendar,columns,projectGroups}.ts` | 领域不变式 | 第 17 轮：本地时区换算、补零日键与字符串比较即时间先后、`unscheduled` 兜底、四种刻度的桶边界 |
| `assistant-run-model.test.mjs` | 325 | `Chat/assistantRunModel.ts`、`Chat/tools/askUser.ts` | 部分重建 | 本轮新读：`assistantVisibleText` / `assistantRunCompletedReply` / `assistantRunCopyText` / `pendingAskUserHasVisibleBlock` / `recoveredAskUserBlock` / `remainingAutoSelectSeconds` 是会话展示与审批交互的领域语义；`buildMessageListItems` / `processSummaryTitle` 属展示整形 |
| `tailwind-classes.test.mjs` | 265 | 类名字符串扫描 | 不重建 | Web 运行时权宜：Tailwind 专属，桌面端无对应物 |
| `session-stream-controller.test.mjs` | 227 | `store/{sessionStreamController,pendingAskUserPoller}.ts` | 领域不变式 | 第 13 轮：流生命周期是领域逻辑 |
| `kanban-selection.test.mjs` | 152 | `Kanban/selection.ts` | 领域不变式 | 本轮新读：`rectFromPoints` / `rectsIntersect` / `idsInRect` 是矩形几何，`rangeSelection` / `toggleSelection` / `movesForDrop` / `isSelectableCard` 是选择集与落点语义，与第 10 轮自绘拖拽同批落地 |
| `streaming-cache.test.mjs` | 142 | `Chat/markdownStreaming.ts`、`Chat/thinkingExtraction.ts`、`Chat/tools/helpers.ts` | 部分重建（待逐项确认） | 第 16 轮已定 `markdownStreaming` 的安全提交点算法不重建；`thinkingExtraction` 与 `tools/helpers` 未读过，判定暂时缺失 |
| `project-path-history.test.mjs` | 134 | `Kanban/projectPathHistory.ts` | 领域不变式 | 第 18 轮：归入第 12 轮的服务端偏好，语义 1:1 上移（MRU 10 / ignored 50 / `remember` 撤销忽略 / 并集合并规则） |
| `conversation-history.test.mjs` | 49 | `store/conversationHistory.ts`（114 行） | 领域不变式 | 第 15 轮：游标按数值比较（测试守 `'9' < '10'`）、`mergeById` 按 id 覆盖、越界丢最旧页 |
| `tool-renderers.test.mjs` | 108 | `Chat/tools/builtinRenderers.tsx`、`Chat/toolRendererRegistry.tsx` | 部分重建 | 第 8 轮：双层注册表的选择顺序（intent 层读 `ToolResult.metadata["presentation"]`，名字回退层其次）要照搬；测试用的 `renderToStaticMarkup` 断言方式不重建 |
| `conversation-performance.mjs` | 76 | `store/delta/applyDelta.ts` | 非测试 | 性能画像脚本（`npm run profile:conversation`），不属验收基线 |

关于这张表的完备度，必须说清，别让它看起来比实际更有底气：

- 只有四个存疑脚本（`delta-coalesce`、`streaming-cache`、`conversation-history`、`project-path-history`）被读到断言级；其余是**文件级**判定，依据行与模块而非逐条断言。
- `assistant-run-model`、`settings-model-options`、`kanban-selection` 三条是写本文时才读的，此前未在盘问中过目。它们的名字与导入列表足以支撑上表的判定，但不等于逐条断言已核。
- 第 7 轮承诺「逐项分类的最终判定权在用户」尚未兑现。要兑现，需要过的是上表「领域不变式」与「部分重建」两列的名单，而不是全部 3,347 行脚本。

## 5. 删除日作业清单

删除 `frontend/` 不是单纯 `rm -rf`，以下耦合点都要一起处理：

- 删 `frontend/` 整目录，含 12 个 npm 测试脚本、`check` 链（15 道关卡）、`package.json` 与 lockfile。
- 删 `crates/astrcode-server/src/http/static_assets.rs` 整个模块。
- 删 `crates/astrcode-server/assets/frontend-placeholder.html`，以及 `crates/astrcode-server/build.rs` 里写占位页与 `cargo:rerun-if-changed` 的那段逻辑。
- 处理 `crates/astrcode-server/src/http/server.rs:210` 的 `is_placeholder()` 启动告警。
- **重定向 `crates/astrcode-server/src/http/routes/extensions.rs:115`**：扩展公开路由分发今天会兜底到内嵌前端（`static_assets::serve(uri.path())`），删除后需另定兜底（404 或结构化错误）。
- 移除 `rust-embed` 依赖。
- 删 ts-rs 生成链（第 19 轮清单：`typescript` feature、`generate-typescript` example、4 个文件的 `ts_rs::TS` derive、workspace `ts-rs` 依赖、生成物目录、`generate:protocol` / `check:protocol` 脚本）。
- 修 `scripts/bump-release-version.sh`：去掉 `frontend/package.json` 与 lockfile 的版本同步。
- 修 `docs/release.md` 的「桌面包」漂移；`release.yml:109` 对 `frontend/package.json` 的版本校验一并去掉。
- 更新 `astrcode-core` 里 `PRESENTATION_METADATA_KEY` 的注释（原文写死「前端按此键拾取 intent」，第 8 轮记录的副作用）。
- 清理 `ci.yml` 的 4 个 frontend job（`frontend-lint` / `frontend-typecheck` / `frontend-format` / `contract-test`，最后一个还含 `npm run build`）。
- wasm 结论为**可行**（2026-10-03），浏览器入口保留，故「发布说明声明入口消失」不再需要。产物内嵌托管**已落地**：`crates/astrcode-server/src/http/webui_assets.rs` 把 `crates/astrcode-webui/www` 编进二进制挂在 `/app`（与 `/` 上的旧前端并存），响应带 `COOP: same-origin` + `COEP: require-corp`——`SharedArrayBuffer` 只在 cross-origin isolated 文档里可用。**删除日仍要做的是**：把 `/` 与扩展路由的兜底从 `static_assets::serve` 换成它，然后按本清单前几条删掉 `static_assets`。

## 6. 未决项

以下五项都还没有决定。记在这里是为了不让它们散落在轮次记录里；每项缺的是你的决定，不是更多资料。

1. `check:protocol` 是否进 CI（第 20 轮附带项，需明确点头，可撤回）。加进 `ci.yml` 的 `contract-test` job 是一行 `run:`，但属本次迁移范围外。
2. 旧 localStorage 键是否作一次性种子迁移（第 12 轮遗留 (4)）。涉及的 5 个键：`astrcode-sidebar-width`、`astrcode:collapsedProjectDirs`、`astrocode-theme`、`astrcode:kanbanProjectPathHistory`、`astrcode:kanbanIgnoredProjectPaths`；注意主题键的前缀拼写与其余 4 个不一致。
3. 「今天」的求值时机（第 17 轮遗留）：原生 App 长期驻留、跨时区或跨夏令时运行时，看板日历的「今天」是每次渲染重算还是缓存。
4. Plugins 视图的归属：`MainView` 四个视图之一，路由 `/api/extensions`、`/api/extensions/set-enabled` 已存在，但 21 轮盘问未覆盖它；可并入设置片，或独立成片。
5. 长文档的渲染策略（第 23 轮 cadence spike 引出，第 24 轮收窄，第 28–32 轮定型）：Chat 视图不能每帧重排整篇文档——单帧成本随文档大小陡增且无跨帧缓存，16 ms 预算约在 32–40 KiB 被突破，绝对值还受内容形态影响约 6.9 倍（第 25–26 轮；构建历史经第 26 轮复核无影响）。方向是**按可见块渲染**：成本取决于被渲染的内容量，与持有的历史量无关（第 24 轮），且**正在流式写入的那条消息同样可以虚拟化**（第 28 轮）——按定长切块、每帧只渲染可见尾部后，帧成本与总长无关（7→118 KiB 全程 1.35 ms，同份 118 KiB 全渲染 142–146 ms）。切块大小不必按性能标定（第 30 轮），可按实体数与解析/提交粒度选。帧预算由「每帧渲多少字节」决定：900×700 视口实测可容约 1.5 KiB（第 32 轮），对应约 0.6 ms（约 0.4 ms/KiB）。两条硬约束：其一，末块不得无上限增长——虚拟化粒度就是块，单块无法只渲一半；其二，切点必须避开围栏内部（第 31 轮），否则后半块会把代码行渲成标题与列表。完整数据见 ADR「cadence spike 结论」。