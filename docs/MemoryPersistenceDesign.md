# Codex 外挂记忆持久化 — 需求与设计文档

> 本文档评估在 GT AI Gateway 网关层为 Codex 请求增加"外挂记忆持久化"功能的可行性、设计方案及对用户的全面影响。

---

## 1. 原始需求

### 1.1 需求描述

为 Codex 发起的 LLM 请求在网关处增加**外挂记忆持久化**能力，实现以下目标：

1. **Session 识别**：在请求处理管道中识别会话（Session），优先读取 `X-Session-Id` header，否则用 `conversation_id` 或 Token 生成 Session ID。
2. **记忆存储层**：在现有 SQLite 基础上新增 `memories` 表，存储每个 Session 的记忆摘要、关键洞察等。
3. **记忆注入**：请求进入时，加载该 Session 的记忆，注入到 system prompt 中。
4. **记忆提取**：响应返回后，提取关键思考内容，保存到记忆库。
5. **上下文压缩**：检测上下文长度，提前总结（类似 Claude Code 的 PreCompact 逻辑）。

### 1.2 需求背景

Codex（OpenAI 的命令行 AI 编程助手）使用 Responses API 与大模型交互。当对话变长时，上下文窗口会被填满，Codex 会丢失早期对话的关键信息。如果在网关层持久化记忆，可以在新一轮请求中自动注入历史关键信息，让 Codex"记住"之前的工作内容。

---

## 2. 可行性评估

### 2.1 结论：可以实现，但需谨慎处理缓存冲突

本项目**非常适合**作为实现该功能的基础，原因如下：

| 现有能力 | 如何支撑记忆功能 |
|----------|------------------|
| **请求体改写管道** | `senderService.sendRequest()` 中已有一系列顺序执行的请求体改写步骤（CCH 改写、协议转换、prompt_cache_key 注入），插入记忆注入只需在管道中增加一步 |
| **全量请求/响应记录** | `recordService` 已记录所有请求体和响应体，记忆提取可直接复用已有数据 |
| **SQLite 存储** | 新增 `memories` 表非常简单，已有 20 个迁移文件的管理机制 |
| **键值配置存储** | `configService` 提供了 `config` 表的键值存储，可用于记忆功能的开关和参数配置 |
| **后台异步处理** | `runInBackground()` 机制已在响应完成后异步执行记录更新和余额扣除，记忆提取可复用此机制 |
| **Responses API 协议理解** | 网关已完整实现 Responses API 的请求/响应解析和协议转换，理解 `instructions`、`input`、`previous_response_id` 等字段 |

### 2.2 核心挑战

| 挑战 | 难度 | 说明 |
|------|------|------|
| **缓存命中率冲突** | ⚠️ 高 | 记忆注入会改变 system prompt 内容，破坏上游 Prompt Cache 的前缀稳定性。详见第 4 节影响评估 |
| **Session 识别** | 中 | Codex 不发送 Session ID，`previous_response_id` 是 OpenAI 服务端状态，网关无法直接获取完整对话链 |
| **记忆提取成本** | 中 | 需要额外调用 LLM 进行摘要，增加 API 调用和 Token 消耗 |
| **协议适配** | 低 | 需针对 Responses（`instructions`）、OpenAI（`messages[0]`）、Anthropic（`system`）三种格式分别注入 |
| **流式响应解析** | 低 | 已有 `sseAccumulator` 完整累积响应，提取文本无技术障碍 |

---

## 3. 设计实现思路

### 3.1 整体架构

```
Codex 请求
    │
    ▼
gatewayController.responsesApi()
    │
    ▼
senderService.sendRequest()
    │
    ├── [新增] memoryService.loadMemory(sessionId)
    │       └── 从 memories 表加载该 Session 的记忆摘要
    │
    ├── [新增] memoryService.injectMemory(upstreamBody, memory, format)
    │       └── 将记忆注入到 instructions / system / messages[0]
    │
    ├── 现有改写管道（CCH、协议转换、cache_key 等）
    │
    ├── 发送到上游
    │
    ▼
响应处理（流式/非流式）
    │
    ├── 转发给客户端
    │
    └── [runInBackground] 响应完成后
            │
            ├── 现有：更新 record、扣余额
            │
            └── [新增] memoryService.extractAndSaveMemory(sessionId, response)
                    ├── 提取助手回复文本
                    ├── [可选] 调用 LLM 生成摘要
                    └── 保存/更新到 memories 表
```

### 3.2 数据库设计

新增 `memories` 表（迁移文件 `migrate_0021.sql`）：

```sql
CREATE TABLE IF NOT EXISTS memories (
    id              INTEGER   NOT NULL PRIMARY KEY AUTOINCREMENT,
    session_id      TEXT      NOT NULL,           -- 会话标识
    user_id         INTEGER   NOT NULL,           -- 用户 ID
    model_id        INTEGER   NULL,               -- 模型 ID
    summary         TEXT      NOT NULL,           -- 记忆摘要（注入用）
    raw_exchanges   TEXT      NULL,               -- 原始交互记录（JSON）
    turn_count      INTEGER   DEFAULT 0,          -- 对话轮次
    token_count     INTEGER   DEFAULT 0,          -- 累计 Token 消耗
    last_updated    TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
    created_at      TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE INDEX IF NOT EXISTS idx_memories_session ON memories (session_id);
CREATE INDEX IF NOT EXISTS idx_memories_user ON memories (user_id);
CREATE UNIQUE INDEX IF NOT EXISTS idx_memories_session_unique ON memories (session_id, user_id);
```

新增配置项（写入 `config` 表）：

| 配置键 | 默认值 | 说明 |
|--------|--------|------|
| `memory_enabled` | `false` | 记忆功能总开关 |
| `memory_max_summary_length` | `2000` | 记忆摘要最大字符数 |
| `memory_summary_model` | （空） | 用于生成摘要的模型名（空则使用当前请求的模型） |
| `memory_summary_interval` | `5` | 每隔几轮对话触发一次摘要更新 |
| `memory_inject_position` | `instructions` | 注入位置（instructions / system / first_message） |

### 3.3 Session 识别策略

由于 Codex 不原生支持自定义 Header，采用**多级降级策略**：

```
Session ID 生成优先级：
1. X-Session-Id header（如果客户端支持自定义 header）
2. 从请求体中提取 previous_response_id（Responses API 的对话链标识）
3. user_id + model_name + 时间窗口（如 30 分钟内无请求则新建 Session）
4. user_id + model_name（最粗粒度，同一用户同一模型共享记忆）
```

**推荐方案**：默认使用策略 3（user_id + model + 时间窗口），同时支持通过 `X-Session-Id` header 精确指定。

对于 Codex 的特殊处理：
- Codex 使用 Responses API，每个请求会带 `instructions`（系统提示词）
- 当 `previous_response_id` 存在时，说明是同一对话的延续
- 当 `previous_response_id` 不存在时，说明是新对话的开始
- 网关可以维护一个 `previous_response_id → session_id` 的映射表

### 3.4 记忆注入实现

在 `senderService.sendRequest()` 的改写管道中，**在协议转换之后、发送上游之前**插入记忆注入：

```typescript
// 新增步骤：记忆注入（在协议转换和 cache_key 注入之后）
if ((await configService.getConfig(ConfigKey.MEMORY_ENABLED, "false")).getBoolean()) {
    const sessionId = await memoryService.resolveSessionId(c, user, modelConfig, bodyDict);
    const memory = await memoryService.loadMemory(sessionId);
    if (memory) {
        upstreamBody = memoryService.injectMemory(upstreamBody, memory, upstreamFormat);
    }
}
```

**注入逻辑按协议格式区分**：

| 上游格式 | 注入位置 | 注入方式 |
|----------|----------|----------|
| Responses | `instructions` 字段 | 在原始 instructions 末尾追加 `\n\n## Session Memory\n{memory.summary}` |
| OpenAI | `messages` 数组 | 在第一个 system 消息后插入新的 system 消息 |
| Anthropic | `system` 字段 | 在 system 文本末尾追加记忆内容 |

**注入内容示例**：

```
## Session Memory

以下是之前对话的关键信息，请参考：

- 用户正在开发一个 Vue 3 项目，使用 TypeScript
- 项目的数据库使用 better-sqlite3，ORM 是 sutando
- 上次讨论的核心问题是：协议转换器的设计
- 用户偏好：代码使用 4 空格缩进，使用默认导出
- 当前任务：实现记忆持久化功能

（记忆会在对话过程中自动更新）
```

### 3.5 记忆提取与更新

在响应完成后的 `runInBackground` 回调中执行：

```typescript
runInBackground(c, async () => {
    // 现有逻辑：更新 record、扣余额...

    // 新增：记忆提取
    if (memoryEnabled) {
        await memoryService.extractAndSaveMemory(
            sessionId,
            record.request_data,
            fullResponse,
            modelConfig
        );
    }
});
```

**记忆提取策略（分两级）**：

**Level 1 — 轻量提取（默认，无额外 LLM 调用）**：
- 从响应中提取助手回复的纯文本
- 截取关键段落（如最后 N 个字符）
- 累加到 `raw_exchanges` 字段
- 每隔 `memory_summary_interval` 轮触发一次 Level 2

**Level 2 — 摘要更新（每隔 N 轮触发）**：
- 将累积的 `raw_exchanges` 发送给 LLM 生成摘要
- 摘要 Prompt 示例：
  ```
  请总结以下对话的关键信息，包括：
  1. 用户的项目背景和技术栈
  2. 正在解决的核心问题
  3. 已经做出的决策和原因
  4. 用户的偏好和习惯
  5. 待完成的任务
  
  对话内容：
  {raw_exchanges}
  
  请用简洁的要点形式输出，不超过 {max_length} 字符。
  ```
- 将生成的摘要保存到 `summary` 字段
- 清空 `raw_exchanges`，开始新一轮累积

### 3.6 上下文压缩（PreCompact）

可选的高级功能：检测请求的 Token 数接近上下文窗口限制时，主动触发摘要：

```typescript
// 在记忆注入前检查
if (memoryService.shouldCompact(sessionId, estimatedTokens, modelConfig)) {
    await memoryService.triggerCompaction(sessionId);
}
```

**触发条件**：
- 请求体估算 Token 数 > 模型上下文窗口的 80%
- 或 `turn_count` 超过阈值（如 20 轮）

**压缩动作**：
- 触发 Level 2 摘要
- 在注入的记忆中添加提示："上下文即将满，已自动压缩历史对话"

### 3.7 新增文件清单

```
src/
├── service/
│   └── memoryService.ts          # 记忆服务（加载、注入、提取、摘要）
├── model/
│   └── sgMemory.ts               # 记忆数据模型
├── util/
│   └── memoryInjector.ts         # 记忆注入工具（按协议格式注入）
├── controller/
│   └── memoryController.ts       # 记忆管理 API（查看/编辑/删除记忆）
resource/migrate/
└── migrate_0021.sql              # memories 表迁移
frontend/src/views/
└── Memory/                       # 记忆管理页面（可选）
```

### 3.8 修改现有文件

| 文件 | 修改内容 |
|------|----------|
| `src/constants.ts` | 新增 `ConfigKey.MEMORY_ENABLED` 等配置键枚举 |
| `src/service/senderService.ts` | 在 `sendRequest()` 管道中插入记忆加载和注入步骤；在 `runInBackground` 中插入记忆提取 |
| `src/service/configService.ts` | 新增记忆相关配置键 |
| `src/routes.ts` | 新增记忆管理 API 路由 |

---

## 4. 对用户的全面影响评估

### 4.1 好处

| 好处 | 说明 |
|------|------|
| **跨会话记忆保持** | Codex 重启或新开会话时，网关自动注入上次工作的关键信息，无需用户手动复述背景 |
| **上下文压缩降本** | 通过摘要替代完整历史，减少长对话中的 Token 消耗（在 `previous_response_id` 不可用时尤其有价值） |
| **调试透明性** | 记忆存储在 SQLite 中，管理员可在后台查看每个 Session 的记忆内容，了解 AI 的工作上下文 |
| **可定制性** | 通过配置开关控制记忆功能，用户可按需启用/禁用，不影响默认行为 |
| **协议无关** | 记忆注入逻辑覆盖三种协议格式，无论上游是 OpenAI、Anthropic 还是 Responses API，都能正常工作 |
| **渐进式实现** | Level 1 轻量提取不需要额外 LLM 调用，零额外成本；Level 2 摘要可配置触发频率 |

### 4.2 坏处与风险

#### ⚠️ 风险 1：Token 消耗增加（核心风险）

**直接影响**：每次请求的 system prompt 中会追加记忆摘要文本（约 500-2000 字符，约 100-500 Token），这意味着：

| 指标 | 不启用记忆 | 启用记忆 | 增幅 |
|------|-----------|---------|------|
| 每次请求输入 Token | 基准 | +100~500 Token | +2%~10% |
| 每次请求费用 | 基准 | 略增 | 与输入 Token 增幅成正比 |

**间接影响（更严重）— 缓存命中率下降**：

本项目的一大卖点是通过 CCH 改写和 `prompt_cache_key` 注入来**最大化上游 Prompt Cache 命中率**。README 中明确指出：

> Claude Code 直接使用 OpenAI API 缓存命中 0%；启用改写之后命中 97%，节省成本超过 10 倍

记忆注入会**破坏缓存前缀稳定性**：

- 上游 Prompt Cache 的工作原理：相同的 prompt 前缀可以命中缓存，缓存读取的价格通常是正常输入的 1/10
- 记忆注入位置在 `instructions`（system prompt），这是 prompt 的**最前面**
- 每次记忆更新后，system prompt 内容变化 → 前缀变化 → **缓存全部失效**
- 即使记忆内容不变，注入行为本身也改变了 system prompt 的长度和内容

**量化影响估算**：

| 场景 | 缓存命中率 | 等效输入 Token 成本倍数 |
|------|-----------|----------------------|
| 不启用记忆（现状） | ~97% | ~1.03x |
| 启用记忆，记忆固定不变 | ~97%（记忆内容作为稳定前缀的一部分） | ~1.03x + 记忆 Token |
| 启用记忆，每 5 轮更新一次 | 命中率波动，更新轮次 ~0%，其他轮次 ~97% | 平均 ~1.2x + 记忆 Token |
| 启用记忆，每轮都更新 | ~0% | ~10x + 记忆 Token |

**缓解方案**：
1. 将记忆注入到 `instructions` 的**末尾**而非开头，使前面的系统指令保持稳定（但 OpenAI 的缓存机制可能按整体匹配，此方案效果待验证）
2. 降低摘要更新频率（如每 10 轮更新一次），减少缓存失效次数
3. 在记忆内容不变时，确保注入文本完全一致（包括换行和空格），以最大化缓存命中
4. 提供 `memory_inject_position` 配置项，允许用户选择注入到 system 末尾还是作为独立消息

#### ⚠️ 风险 2：额外 LLM 调用成本

Level 2 摘要更新需要调用 LLM：

| 配置 | 每次摘要成本 |
|------|-------------|
| 使用当前请求模型（如 GPT-4o） | 每次约 500-2000 输入 Token + 200-500 输出 Token |
| 使用低成本模型（如 GPT-4o-mini） | 每次约 $0.001-$0.005 |
| 每 5 轮触发一次 | 100 轮对话 = 20 次摘要调用 |

**缓解方案**：默认使用低成本模型进行摘要，并提供配置项让用户选择。

#### ⚠️ 风险 3：延迟增加

| 环节 | 延迟 | 是否阻塞用户 |
|------|------|-------------|
| 记忆加载（SQLite 查询） | <1ms | 是（但可忽略） |
| 记忆注入（字符串拼接） | <1ms | 是（但可忽略） |
| 记忆提取（解析响应） | <5ms | 否（异步） |
| LLM 摘要调用 | 1-5 秒 | 否（异步） |

**结论**：对用户感知延迟的影响可忽略不计（<2ms），摘要操作在后台异步执行。

#### ⚠️ 风险 4：Session 识别不准确

由于 Codex 不原生支持 Session ID，基于时间窗口的识别可能出错：

- 用户同时开两个 Codex 窗口操作同一项目 → 两个 Session 被合并为一个
- 用户长时间思考后继续对话 → 可能被误判为新 Session

**缓解方案**：
1. 提供 `X-Session-Id` header 支持，高级用户可手动指定
2. 时间窗口设置为较大值（如 30 分钟）以减少误判
3. 在管理界面显示 Session 列表，允许用户手动合并/拆分

#### ⚠️ 风险 5：记忆内容质量

- LLM 生成的摘要可能遗漏关键信息或产生幻觉
- 过时的记忆可能误导后续对话
- 记忆内容可能包含敏感信息（如代码片段、API Key）

**缓解方案**：
1. 在管理界面提供记忆查看和编辑功能
2. 记忆摘要中过滤敏感信息（如 API Key 格式的字符串）
3. 设置记忆过期时间（如 7 天未更新自动归档）
4. 提供"清除记忆"的 API 和界面操作

#### ⚠️ 风险 6：与 `previous_response_id` 的冲突

Codex 使用 Responses API 时，`previous_response_id` 让 OpenAI 服务端维护对话历史。当网关注入记忆后：

- 网关注入的记忆 + OpenAI 服务端维护的完整历史 = **信息重复**
- 如果记忆说"用户正在开发 Vue 项目"，而完整历史中也包含这个信息，模型会收到冗余上下文

**缓解方案**：
1. 检测 `previous_response_id` 是否存在，如果存在则减少记忆注入量（因为 OpenAI 已有完整上下文）
2. 或在 `previous_response_id` 存在时完全跳过记忆注入（只在新建对话时注入）
3. 这也意味着记忆功能在 `previous_response_id` 不可用时（如使用 Anthropic 后端、或 Codex 重启后）最有价值

### 4.3 影响总结矩阵

| 维度 | 不启用记忆（现状） | 启用记忆后 | 严重程度 |
|------|-------------------|-----------|----------|
| **Token 消耗** | 基准 | +100~500 Token/请求 | 🟡 中 |
| **缓存命中率** | ~97% | 降至 0%~97%（取决于更新频率） | 🔴 高 |
| **整体费用** | 基准 | +10%~200%（取决于缓存影响） | 🔴 高 |
| **首字延迟** | 基准 | +<2ms | 🟢 低 |
| **记忆摘要成本** | 无 | 每 N 轮一次 LLM 调用 | 🟡 中 |
| **跨会话连续性** | 无（每次新会话从零开始） | ✅ 有（自动注入历史关键信息） | ✅ 好处 |
| **上下文压缩** | 无（客户端自管理） | ✅ 有（网关层主动压缩） | ✅ 好处 |
| **可观测性** | 仅请求记录 | ✅ 记忆内容可视化 | ✅ 好处 |
| **复杂度** | 低 | 中（新增表、服务、注入逻辑） | 🟡 中 |

---

## 5. 推荐实施路径

### 阶段一：最小可行版本（MVP）

**目标**：验证记忆注入的基本效果，不引入 LLM 摘要调用。

1. 新增 `memories` 表
2. 实现 `memoryService.loadMemory()` 和 `memoryService.injectMemory()`
3. Session 识别使用策略 3（user_id + model + 时间窗口）
4. 记忆提取仅做 Level 1（截取助手回复文本，不做摘要）
5. 在 `senderService` 管道中插入注入逻辑
6. 添加 `memory_enabled` 配置开关
7. 添加管理 API 查看记忆内容

### 阶段二：智能摘要

1. 实现 Level 2 摘要更新（调用 LLM 生成摘要）
2. 添加摘要模型配置项
3. 实现摘要触发频率控制
4. 添加前端记忆管理页面

### 阶段三：高级优化

1. 实现 `X-Session-Id` header 支持
2. 实现上下文压缩（PreCompact）逻辑
3. 优化缓存命中策略（记忆内容固定化、注入位置优化）
4. 实现 `previous_response_id` 感知（检测到时减少注入量）
5. 记忆过期和归档机制

---

## 6. 配置示例

启用记忆功能后的用户配置（在管理后台"高级设置"中）：

```
memory_enabled = true
memory_max_summary_length = 2000
memory_summary_model = gpt-4o-mini
memory_summary_interval = 5
memory_inject_position = instructions
memory_session_timeout = 30  (分钟)
```

Codex 侧无需任何额外配置，网关会自动处理。

---

## 7. 测试策略

| 测试类型 | 覆盖内容 |
|----------|----------|
| **单元测试** | `memoryService` 的加载、注入、提取逻辑；`memoryInjector` 的三种协议格式注入 |
| **集成测试** | 完整请求流程：Codex 请求 → 记忆注入 → 上游响应 → 记忆提取 → 存储 |
| **缓存影响测试** | 对比启用/禁用记忆时的缓存命中率变化 |
| **并发测试** | 多个并发请求的 Session 隔离正确性 |
| **边界测试** | 空记忆、超长记忆、记忆表不存在等异常场景 |

---

## 8. 结论

**该功能技术上完全可行**，项目的请求改写管道、全量记录能力和 SQLite 存储为实现提供了坚实基础。

**但核心矛盾在于记忆注入与缓存优化的冲突**。本项目的一个重要价值主张是通过 CCH 改写和 `prompt_cache_key` 注入实现 97% 的缓存命中率，节省 10 倍成本。记忆注入会破坏缓存前缀稳定性，可能导致缓存命中率大幅下降，反而增加成本。

**建议**：
1. 将记忆功能设为**默认关闭**，用户按需启用
2. 优先实现 MVP（Level 1 轻量提取），验证实际效果后再决定是否引入 LLM 摘要
3. 重点优化缓存友好的注入策略（如记忆内容固定化、降低更新频率、注入位置选择）
4. 在 `previous_response_id` 存在时智能减少注入量，避免与 OpenAI 服务端对话历史重复
5. 在管理界面清晰展示记忆功能的成本影响，让用户知情决策
