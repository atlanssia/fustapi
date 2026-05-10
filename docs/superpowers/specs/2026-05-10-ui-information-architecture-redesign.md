# FustAPI UI 信息架构重新设计

**Date**: 2026-05-10
**Status**: Approved
**Scope**: Dashboard 布局重设计 + 子页面精简 + 信息去冗余。单文件 `ui/index.html`。

## 1. 设计决策

### Dashboard 定位：网关总控台
流量 + 余额 + 路由 + 健康一目了然。

### 已确认方案
- **Dashboard**: 方案 A（紧凑状态行）— 层次分明，自上而下
- **Providers/Routes**: 移除 summary metrics，全宽表格，聚焦核心 CRUD

## 2. Dashboard 新布局（方案 A）

```
┌─────────────────────────────────────────────────────────┐
│ Topbar: FustAPI │ [balance dots] [Gateway OK pill]      │
├────────┬────────────────────────────────────────────────┤
│        │ Overview                                         │
│  Dash  │                                                  │
│  bard  │ ┌──────────┐ ┌──────────────┐ ┌──────────────┐ │
│        │ │ Gateway   │ │ Total Reqs   │ │ Base URL     │ │
│  Prov  │ │ ● OK      │ │ 1,247        │ │ localhost:   │ │
│  ider  │ │ Uptime 2h │ │ since start  │ │ 8800/v1      │ │
│        │ └──────────┘ └──────────────┘ └──────────────┘ │
│  Rout  │                                                  │
│  es    │ ┌──────┐ ┌────────┐ ┌──────────┐ ┌──────────┐  │
│        │ │ QPS  │ │Latency │ │Success % │ │In-Flight │  │
│        │ │ 0.0  │ │ 0ms    │ │ 100%     │ │ 0        │  │
│        │ └──────┘ └────────┘ └──────────┘ └──────────┘  │
│        │                                                  │
│        │ ┌──────────┐ ┌──────────────────────────────┐   │
│        │ │Providers │ │ Requests / sec               │   │
│        │ │          │ │                              │   │
│        │ │ds  CNY26 │ │ [chart]                     │   │
│        │ │glm No   │ │                              │   │
│        │ │loc No   │ ├──────────────────────────────┤   │
│        │ │szyj No  │ │ Latency (ms)                 │   │
│        │ │         │ │                              │   │
│        │ │         │ │ [chart]                     │   │
│        │ └──────────┘ └──────────────────────────────┘   │
│        │                                                  │
│        │ Provider Performance                              │
│        │ ┌────────────────────────────────────────────┐  │
│        │ │ Provider|Model|Reqs|Success|Latency|TTFT|..│  │
│        │ └────────────────────────────────────────────┘  │
└────────┴────────────────────────────────────────────────┘
```

### 2.1 顶部状态行（3 列）

替换当前的 "Balance 表 + Gateway 卡片" 2 列布局：

| 卡片 | 内容 |
|------|------|
| Gateway | 状态点 + "OK" / "Error" + Uptime 时长 |
| Total Requests | 请求总数 + "Since startup" |
| Base URL | `localhost:8800/v1` + click to copy |

**删除**：当前 Balance 表格（Provider / Type / Status 三列）
**删除**：当前 Gateway 独立卡片
**删除**：底部 Total Requests + Uptime 两个独立指标卡

### 2.2 Provider 余额 + 图表并排

Provider 余额列表（左侧 1/3 宽）和两个图表（右侧 2/3 宽）并排显示：

```
┌──────────────┐ ┌──────────────────────────┐
│ Providers    │ │ Requests / sec           │
│              │ │                          │
│ ds  CNY 26   │ │ [chart]                 │
│ glm No data  │ │                          │
│ loc No data  │ ├──────────────────────────┤
│ szyj No data │ │ Latency (ms)            │
│              │ │                          │
│              │ │ [chart]                 │
└──────────────┘ └──────────────────────────┘
```

- 左侧卡片：标题 "Providers"，然后每个 provider 一行（name + balance）
- 无表头，无 Type 列（Type 列数据为空）
- Provider name 左对齐粗体，Balance 右对齐带颜色
- 右侧两个图表上下堆叠，填满剩余宽度
- 移动端：Provider 列表和图表改为上下排列

**高度对齐规则**：
- 左右两侧等高（`align-items: stretch`）
- 左侧 Provider 列表：`max-height` 固定，内容超出时 `overflow-y: auto` 显示内部滚动条
- 右侧图表区域：高度由图表 canvas 撑满，与左侧对齐
- 滚动条样式：`scrollbar-width: thin`，与 glassmorphism 风格一致

### 2.3 4 个关键指标（不变）

QPS / Avg Latency / Success Rate / In-Flight — 保持当前 4 列布局

### 2.4 图表（与 Providers 并排）

Requests/sec + Latency 上下堆叠在右侧 2/3 区域，与左侧 Provider 列表并排

### 2.5 Provider Performance 表（不变）

保持当前表格结构

### 2.6 删除的内容

- API Endpoints 行（3 个 POST/GET 按钮）— 标准接口无需展示
- 底部 Total Requests + Uptime — 移到顶部状态行
- Balance 表格 — 改为紧凑列表
- Gateway 独立卡片 — 合并到顶部状态行

## 3. Providers 页面

### 移除
- Summary metrics 行（Total providers / With API key / Local providers）

### 保留
- 标题 + 描述 + Add provider 按钮
- 全宽表格：Name / Type / Endpoint / API Key / Actions
- Mobile card list

## 4. Routes 页面

### 移除
- Summary metrics 行（Model routes / Routed providers / Fallback routes）

### 新增
- 表格底部一行小字统计："5 model routes · Refresh"
- Provider Chain 列用 pill badge 显示优先级序号

### 保留
- 标题 + 描述 + Add route 按钮
- 全宽表格：Model / Provider Chain / Actions
- Mobile card list

## 5. 信息层级总结

| 信息 | 展示位置 | 形式 |
|------|---------|------|
| Gateway 健康 | Topbar pill + Dashboard 状态行 | 色点 + 文字 |
| 余额 | Topbar 圆点 + Dashboard Provider 列表（与图表并排） | 色点 + key-value |
| Base URL | Dashboard 状态行 | 可点击复制 |
| Total Requests / Uptime | Dashboard 状态行 | 数字 |
| 流量指标 | Dashboard | 4 列数字 |
| 图表 | Dashboard（与 Provider 列表并排） | Canvas |
| Provider 配置 | Providers 页面 | 全宽表格 |
| Provider 性能 | Dashboard | 全宽表格 |
| 路由配置 | Routes 页面 | 全宽表格 |

## 6. 实施范围

仅修改 `ui/index.html`：
1. CSS: 新增 `.status-row`、`.balance-list`、`.balance-entry` 样式
2. HTML: 重构 Dashboard 布局，删除 API endpoints 行
3. JS: 更新 `renderDashboardBalance()` 为紧凑列表，移除 summary metrics 渲染

## 7. 验证

- 编译通过：`cargo build`
- 测试通过：`cargo test`
- 浏览器验证：
  - Dashboard 顶部 3 列状态行正确显示
  - Provider 余额列表紧凑、无空 Type 列
  - 无 API endpoints 按钮
  - Providers 页面无 summary metrics，全宽表格
  - Routes 页面无 summary metrics，全宽表格，底部统计行
  - Mobile 布局正常
  - 所有 CRUD 操作正常
