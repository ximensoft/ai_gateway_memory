# GT AI Gateway 项目总览

> 本文档由代码纵览自动生成，面向新接手项目的开发者，快速了解项目作用、架构、本地启动方式、Codex 接入方式以及管理界面访问方式。

---

## 1. 项目是做什么的

GT AI Gateway 是一个**轻量级 AI 网关（API Gateway）**，核心作用是在客户端（如 Codex、Claude Code、Chatbox、Cursor 等工具）与上游大模型供应商（如 OpenAI、Anthropic）之间充当一个"中间人"，提供以下能力：

| 能力 | 说明 |
|------|------|
| **统一 API 入口** | 对外暴露 OpenAI、Anthropic、Responses API 三种协议端点，客户端无需关心后端模型来自哪家供应商 |
| **协议自动转换** | 客户端用 OpenAI 格式发请求，后端模型是 Anthropic 的，网关自动做双向转换；反之亦然。支持 OpenAI ↔ Anthropic ↔ Responses 三种协议两两转换 |
| **请求拦截与改写** | 在网关层深度解析并改写请求体，例如清理 Claude Code 注入的随机 `cch` 标记以提升缓存命中率、为 Responses API 注入 `prompt_cache_key` 以实现粘性路由 |
| **用户管理与多租户** | 把单个上游 API Key 分发给多个用户使用，每个用户有独立 Token 和额度，防止上游 Key 泄漏 |
| **额度与计费** | 基于模型的输入/输出/缓存 Token 价格精确计费，支持用户余额管理、充值记录 |
| **全量请求记录** | 像抓包工具一样透明记录所有请求与响应（包括 SSE 流式），可对任意请求进行深度排查（耗时、Token、缓存命中率、原始 JSON） |
| **可视化管理界面** | 内置 Web 管理后台，管理渠道、模型、用户、查看请求记录和统计面板 |

### 一句话总结

> **GT AI Gateway 是一个自托管的大模型 API 网关，让你可以用统一的接口调用各家大模型，同时实现用户管理、计费、请求记录和协议转换。**

---

## 2. 技术栈与架构

### 2.1 技术栈

| 层面 | 技术 |
|------|------|
| **后端框架** | [Hono](https://hono.dev/)（轻量 Web 框架，同时支持 Cloudflare Workers 和 Node.js） |
| **语言** | TypeScript |
| **ORM** | [sutando](https://sutando.org/)（支持 SQLite / D1） |
| **数据库** | Node 模式使用 `better-sqlite3`（内嵌 SQLite）；Cloudflare 模式使用 D1 |
| **前端框架** | Vue 3 + Ant Design Vue + Vite |
| **桌面端** | Tauri（跨平台桌面应用，打包后端为 sidecar） |

### 2.2 项目结构概览

```
ai_gateway_memory/
├── src/                          # 后端源码
│   ├── index.ts                  # Cloudflare Workers 入口
│   ├── local.ts                  # Node.js 本地入口
│   ├── routes.ts                 # 路由定义（Hono app）
│   ├── constants.ts              # 常量定义（枚举）
│   ├── controller/               # 控制器层（接收请求，调用 service）
│   │   └── gatewayController.ts  #   AI 请求入口（3 个 LLM 端点）
│   ├── service/                  # 业务逻辑层
│   │   ├── senderService.ts      #   核心请求处理管道（改写、转发、记录）
│   │   ├── recordService.ts      #   请求记录管理
│   │   ├── userService.ts        #   用户与 Token 管理
│   │   ├── configService.ts      #   键值配置存储
│   │   ├── hostService.ts        #   主机标识与端口管理
│   │   └── clientConfigService/  #   客户端配置写入（Codex/Claude等）
│   ├── model/                    # 数据模型层（sutando Model）
│   │   ├── sgRecord.ts           #   请求记录模型
│   │   ├── sgUser.ts             #   用户模型
│   │   ├── sgVendor.ts           #   供应商模型
│   │   ├── sgModel.ts            #   模型路由模型
│   │   └── ...
│   ├── middleware/               # 中间件
│   │   ├── authMiddleware.ts     #   管理员鉴权
│   │   └── corsMiddleware.ts     #   CORS
│   └── util/                     # 工具层
│       ├── cchRewriter.ts        #   CCH 缓存标记改写
│       ├── responsesPromptCacheKeyRewriter.ts  # Responses 缓存键注入
│       ├── protocolConverter/    #   协议转换引擎
│       └── ...
├── frontend/                     # 前端源码（Vue 3）
│   └── src/
│       ├── views/                #   页面组件
│       │   ├── Dashboard.vue     #     仪表盘
│       │   ├── Record/           #     请求记录查看
│       │   ├── Vendor/           #     供应商管理
│       │   ├── Model/            #     模型管理
│       │   ├── User/             #     用户管理
│       │   ├── Integration/      #     接入配置
│       │       └── ...
│       └── ...
├── resource/migrate/             # 数据库迁移文件（SQL）
├── tauri/                        # Tauri 桌面应用配置
├── doc/                          # 项目原有文档目录
├── docs/                         # 本文档所在目录
└── package.json                  # 依赖与脚本
```

### 2.3 请求处理流程

以 Codex 发起请求为例，一个请求经过网关的完整流程：

```
Codex 客户端
    │
    │  POST /llm/v1/responses (Responses API)
    │  Authorization: Bearer <用户Token>
    │  Body: { model, input, instructions, previous_response_id, ... }
    │
    ▼
gatewayController.responsesApi()
    │  1. 从 Authorization header 提取 Token
    │  2. 通过 Token 查找用户（root token 直接放行）
    │  3. 解析请求体，获取 model 名称
    │  4. 通过 model 名称查找模型配置（SgModel）
    │  5. 通过模型配置的 vendor_id 查找供应商（SgVendor）
    │
    ▼
senderService.sendRequest()
    │  6. 解析上游格式（resolveUpstreamFormat）
    │     - 根据客户端请求格式和供应商支持的格式决定是否需要协议转换
    │  7. 创建请求记录（recordService.create）→ 写入 record 表
    │  8. 构建上游请求 headers（过滤敏感 header，注入供应商 API Key）
    │  9. 请求体改写：
    │     a. 替换上游模型名（vendor_model_id 映射）
    │     b. CCH 改写（rewriteCchInSystemPrompt）
    │     c. 协议转换（ConverterFactory，如 Responses → Anthropic）
    │     d. 注入 stream_options（OpenAI 流式）
    │     e. 注入 prompt_cache_key（Responses API 粘性路由）
    │  10. 发起上游 fetch 请求
    │
    ▼
上游供应商 (OpenAI / Anthropic)
    │  返回 SSE 流式响应 或 JSON 非流式响应
    │
    ▼
senderService（响应处理）
    │  11. 流式响应：逐块读取 SSE，实时转换协议，转发给客户端
    │      - 同时用 sseAccumulator 累积完整响应
    │  12. 流结束后（runInBackground）：
    │      - 更新 record（写入完整响应、usage、耗时、费用）
    │      - 扣除用户余额
    │
    ▼
Codex 客户端（收到流式响应）
```

### 2.4 数据库表结构

| 表名 | 作用 |
|------|------|
| `user` | 用户信息（name, token, type, balance, status） |
| `vendor` | 供应商渠道（type, name, token, urls） |
| `model` | 模型路由（name, vendor_id, vendor_model_id, enable, prices） |
| `vendor_model` | 供应商模型映射（vendor_id, model_id, allowed_formats） |
| `record` | 请求记录（user_id, model_id, request_data, response_data, usage, cost, status, ...） |
| `config` | 键值配置存储（name, value） |
| `recharge_records` | 充值记录（user_id, amount, type, remark） |
| `client_config` | 客户端配置备份（client, name, configContent, enabled） |

---

## 3. Node.js 本地启动

### 3.1 环境准备

- **Node.js** v20 或以上
- **Git**

### 3.2 拉取代码与安装依赖

```bash
git clone <项目仓库地址>
cd ai_gateway_memory

# 安装后端依赖
npm install

# 安装前端依赖
cd frontend && npm install && cd ..
```

### 3.3 配置环境变量

在项目根目录创建 `.dev.vars` 文件（或 `.env` 文件），配置以下变量：

```env
# 超级管理员登录密码（必填）
ROOT_TOKEN=your-secret-root-token

# 服务监听端口（默认 8720）
PORT=8720

# SQLite 数据库路径（默认项目根目录 local.db）
DB_PATH=local.db
```

> 完整的环境变量模板见 `.env.template`。

### 3.4 开发模式启动（推荐）

开发模式支持代码热重载，需要开两个终端：

**终端 1：启动后端**

```bash
npm run backend:dev:local
```

这会使用 `tsx watch` 监听文件变化并自动重启，后端监听 `127.0.0.1:8720`。

**终端 2：启动前端**

```bash
npm run frontend:dev
```

这会启动 Vite 开发服务器，监听 `http://localhost:8721`，支持热更新。

浏览器访问 `http://localhost:8721` 即可打开管理后台。

### 3.5 生产模式启动（前后端同域）

```bash
# 1. 编译前端静态资源
npm run frontend:build

# 2. 启动后端（同时托管前端静态文件）
npm run backend:start
```

浏览器访问 `http://localhost:8720` 即可打开管理后台。后端会自动将非 API 路由回退到前端 SPA 的 `index.html`。

### 3.6 一键启动（编译+启动）

```bash
npm run backend:start:integrated
```

此命令会先编译前端，再启动后端。

### 3.7 数据库管理

```bash
# 初始化数据库
npm run db:init:node

# 执行迁移
npm run db:migrate:node

# 查看迁移状态
npm run db:status:node

# 清空数据库
npm run db:clear:node
```

> Node 模式下，服务首次启动时会自动执行数据库迁移，无需手动操作。

---

## 4. Codex 配置访问网关

Codex 是 OpenAI 的命令行 AI 编程助手，使用 **OpenAI Responses API** 协议。以下是配置 Codex 通过网关访问大模型的步骤。

### 4.1 前置条件

1. 网关已启动并可访问（如 `http://127.0.0.1:8720`）
2. 已在网关管理后台完成以下配置：
   - 添加了供应商（Vendor），填入上游 API Key
   - 配置了模型路由（Model），记住模型名称
   - 创建了用户并获取到用户 Token（格式如 `sk-xxxxxx`），或直接使用 `ROOT_TOKEN`

### 4.2 方式一：通过管理界面自动配置（推荐）

网关管理后台内置了**客户端配置管理**功能，可以自动写入 Codex 配置文件：

1. 打开管理后台 → 左侧菜单 **"接入配置"** 或 **"客户端管理"**
2. 选择 Codex 客户端
3. 填入网关地址、用户 Token、模型名称
4. 点击应用，系统会自动修改 `~/.codex/config.toml` 和 `~/.codex/auth.json`

### 4.3 方式二：手动配置 Codex

Codex 的配置文件位于 `~/.codex/` 目录（可通过 `CODEX_HOME` 环境变量覆盖）。

**1. 编辑 `~/.codex/config.toml`**

```toml
model = "your-model-name"
model_provider = "gt_ai_gateway"

[model_providers.gt_ai_gateway]
name = "GT AI Gateway"
base_url = "http://127.0.0.1:8720/llm/v1"
wire_api = "responses"
experimental_bearer_token = "your-user-token-or-root-token"
```

- `model`：在网关后台配置的模型名称
- `base_url`：网关地址 + `/llm/v1`（Responses API 端点为 `/llm/v1/responses`）
- `wire_api`：固定为 `"responses"`（Codex 使用 Responses API）
- `experimental_bearer_token`：网关用户 Token 或 ROOT_TOKEN

**2. 编辑 `~/.codex/auth.json`**

```json
{
    "OPENAI_API_KEY": "your-user-token-or-root-token"
}
```

### 4.4 验证配置

配置完成后，在终端运行 Codex，如果网关管理后台的"请求记录"页面出现了对应的请求记录，说明配置成功。

### 4.5 网关对 Codex 请求的特殊处理

网关对 Codex 发起的 Responses API 请求有以下增强处理：

| 处理项 | 作用 | 配置项 |
|--------|------|--------|
| **prompt_cache_key 注入** | 为每个用户+主机生成稳定的缓存键，最大化 OpenAI 上游的缓存命中率 | `responses_prompt_cache_key_enabled`（默认开启） |
| **Responses → Anthropic 协议转换** | 如果后端供应商只支持 Anthropic Messages 格式，网关自动将 Codex 的 Responses 请求转为 Anthropic 格式 | 自动触发，无需配置 |

#### Codex 请求的协议匹配规则详解

Codex 使用 Responses API（`/llm/v1/responses`）发请求，网关会根据后端供应商实际支持的协议格式来决定是透传还是转换。具体的匹配逻辑在 `resolveUpstreamFormat()` 中实现：

| 后端供应商支持的格式 | 网关行为 | 说明 |
|---------------------|----------|------|
| 支持 **Responses** | ✅ 透传 | 直接以 Responses 格式发送给上游，无转换开销 |
| 不支持 Responses，支持 **Anthropic** | ✅ 自动转换为 Anthropic | 使用 `ResponsesToAnthropicConverter` 转换请求和响应 |
| 不支持 Responses，支持 **OpenAI Chat Completions** | ⚠️ **不转换**，尝试透传 | 见下方说明 |
| 支持 **Responses + Anthropic** | ✅ 透传 | 优先使用与客户端一致的格式 |

> **重要提示：Responses → OpenAI Chat Completions 的转换目前不支持。**
>
> `resolveUpstreamFormat` 中 Responses 格式的备选格式只有 Anthropic，不包含 OpenAI Chat Completions。`ConverterFactory` 中也明确注释了这一点：
> ```typescript
> // Responses ↔ OpenAI（Responses 和 Chat Completions 都是 OpenAI 体系，
> // 但格式差异大，目前暂不支持互转，后续可扩展）
> ```
>
> 当后端供应商只配置了 OpenAI Chat Completions URL（没有 Responses URL）时，网关会：
> 1. 判断不需要转换（`upstreamFormat = RESPONSES`）
> 2. 自动将 OpenAI URL 的 `/chat/completions` 路径替换为 `/responses`
> 3. 以 Responses 格式直接发送到该 URL
>
> - 如果供应商是 **OpenAI 官方**，`/v1/responses` 端点有效，能正常工作
> - 如果供应商是**只支持 Chat Completions 的第三方**（如部分国内中转站），`/responses` 端点可能不存在，请求会失败
>
> **建议**：使用 Codex 时，后端供应商最好配置 Responses API URL，或使用支持 Anthropic 格式的供应商（网关会自动转换）。

---

## 5. 访问管理界面

### 5.1 访问地址

- **开发模式**：`http://localhost:8721`（前端独立端口）
- **生产模式**：`http://localhost:8720`（前后端同域）

### 5.2 登录

1. 打开浏览器访问上述地址
2. 页面提示输入密码，输入你在环境变量中配置的 `ROOT_TOKEN` 值
3. 登录成功后进入主控制台

### 5.3 管理界面功能

| 页面 | 功能说明 |
|------|----------|
| **仪表盘 (Dashboard)** | 总览统计：请求量、Token 消耗、费用、最近请求 |
| **渠道管理 (Vendor)** | 添加/编辑/删除上游供应商，配置 API Key 和接口地址，测试连通性 |
| **模型管理 (Model)** | 配置模型路由，将模型名映射到供应商，设置价格 |
| **供应商模型** | 管理供应商支持的模型列表和协议格式 |
| **用户管理 (User)** | 创建用户、分配额度、查看 Token、调整余额 |
| **请求记录 (Record)** | 查看所有请求的详细信息：请求体、响应体、Token 消耗、耗时、费用、缓存命中 |
| **额度管理 (Balance)** | 查看用户余额和充值记录 |
| **接入配置 (Integration)** | 自动生成各协议的接入端点和示例代码，支持一键写入客户端配置 |
| **高级设置** | 开关 CCH 改写、Responses 缓存键注入等功能 |

### 5.4 快速配置流程

首次使用的管理员配置流程：

1. **登录** → 输入 `ROOT_TOKEN`
2. **添加渠道** → 填入上游供应商的 API Key 和接口地址
3. **配置模型** → 创建模型名称并关联到渠道
4. **创建用户** → 创建普通用户并获取 Token（或直接用 ROOT_TOKEN）
5. **接入客户端** → 在"接入配置"页面获取端点和示例，或一键写入 Codex/Claude 配置

---

## 6. API 端点汇总

### 6.1 AI 请求端点（用户 Token 鉴权）

| 端点 | 方法 | 协议 | 鉴权方式 |
|------|------|------|----------|
| `/llm/v1/chat/completions` | POST | OpenAI Chat Completions | `Authorization: Bearer <token>` |
| `/llm/v1/messages` | POST | Anthropic Messages | `x-api-key: <token>` 或 `Authorization: Bearer <token>` |
| `/llm/v1/responses` | POST | OpenAI Responses API | `Authorization: Bearer <token>` |

### 6.2 管理接口（管理员 Token 鉴权）

所有管理接口 URL 以 `.json` 结尾，需要 `Authorization: Bearer <root_token>` 或管理员用户 Token。

主要管理接口包括：渠道管理、模型管理、用户管理、请求记录查看、统计、配置管理等。详见 `src/routes.ts` 中的路由定义。

---

## 7. 关键设计决策

1. **双运行模式**：同一份代码同时支持 Cloudflare Workers（Serverless）和 Node.js，通过 `ormService.mode` 区分。Workers 模式使用 D1 数据库，Node 模式使用 better-sqlite3。

2. **请求体改写管道**：`senderService.sendRequest()` 中有一系列顺序执行的改写步骤（模型名替换 → CCH 改写 → 协议转换 → stream_options 注入 → prompt_cache_key 注入），每步都是幂等的字符串替换，新增改写逻辑只需在管道中插入新步骤。

3. **流式响应处理**：使用 Hono 的 `streamSSE` 逐块读取上游 SSE，实时转发给客户端的同时用 `sseAccumulator` 累积完整响应，流结束后在后台异步写入数据库。

4. **无状态代理**：网关本身是无状态的，每个请求独立处理，不维护会话状态。多轮对话的上下文由客户端（如 Codex）通过 `previous_response_id` 或完整的 messages 历史自行维护。

---

## 参考文档

- [源码部署文档](../doc/deploy/SourceCodeDeployment.md)
- [系统配置指南](../doc/usage/ConfigurationGuide.md)
- [LLM API 使用指南](../doc/usage/LlmApiUsage.md)
- [自动协议转换说明](../doc/usage/ProtocolConversion.md)
- [Codex 客户端配置](../doc/client_config/codex.md)
- [后端开发手册](../doc/dev/BackendDevManual.md)
- [前端开发手册](../doc/dev/FrontendDevManual.md)
- [测试手册](../doc/dev/TestManual.md)
