# Providers UI 重设计

**日期**: 2026-05-10
**状态**: Draft

## 背景

当前 Providers 展示存在三个核心问题：

1. **扩展性差** — balance cards 区域无法优雅地展示 10+ 个 providers
2. **数据格式不统一** — DeepSeek 返回纯文本余额，GLM 返回复杂 JSON，前端靠 `try/catch JSON.parse` 猜测类型
3. **视觉风格需更新** — 需要全新设计，不限于现有 glassmorphism 风格

## 设计决策汇总

| 决策项 | 选择 |
|--------|------|
| 设计风格 | Modern SaaS Dashboard（Linear/Stripe 风格） |
| 重设计范围 | Dashboard 余额展示区 + Providers 配置面板 |
| Dashboard 布局 | 左右分栏：左侧 provider 列表 + 右侧详情面板 |
| 右侧默认状态 | 汇总概览（在线/告警/离线统计 + 告警列表） |
| 列表项样式 | 两行紧凑卡片（第一行名称+标签，第二行缩进指标） |
| 配置面板操作 | 右上角图标按钮组（Edit/Test/Delete） |
| 数据模型 | 后端统一结构化格式 |
| 移动端 | 暂不考虑，仅桌面端 |

## 一、后端统一数据模型

### ProviderBalance 结构体

每个 provider 的 `balance()` 方法在内部完成转换，返回统一的 JSON 结构：

```json
{
  "provider_name": "glm",
  "status": "online",
  "plan": "plus",
  "plan_type": "coding",
  "alerts": [
    { "level": "warn", "message": "Token quota 72% used" }
  ],
  "metrics": [
    {
      "label": "Tokens",
      "kind": "percentage",
      "value": 72,
      "total": 100,
      "unit": "%",
      "percentage": 72,
      "status": "ok"
    },
    {
      "label": "Time",
      "kind": "percentage",
      "value": 45,
      "total": 100,
      "unit": "%",
      "percentage": 45,
      "status": "ok"
    }
  ],
  "breakdown": [
    { "label": "glm-4", "value": 1240, "unit": "requests" },
    { "label": "coder-1", "value": 580, "unit": "requests" }
  ],
  "resets": [
    { "label": "Token quota", "resets_at_ms": 1778499600000 }
  ],
  "config_summary": {
    "provider_type": "cloud",
    "endpoint": "open.bigmodel.cn",
    "has_key": true,
    "model": "glm-4-plus"
  }
}
```

### 各 Provider 转换规则

| Provider | metrics | breakdown | resets | plan_type |
|----------|---------|-----------|--------|-----------|
| GLM | Tokens(%), Time(%) | 按模型用量 | 有重置时间 | coding / token |
| DeepSeek | Balance(absolute, CNY) | 无 | 无 | credit |
| OpenAI | 无（无 balance API） | 无 | 无 | null |
| Local (omlx/lmstudio/sglang) | 无 | 无 | 无 | null |

### 扩展点

- **metrics** 是核心扩展点：新 provider 按实际情况填充不同数量的 Metric
- **plan_type** 用枚举（coding/token/credit），避免前端解析字符串歧义
- **alerts** 由后端生成（用量>80%、余额不足、离线等），前端只负责展示
- **breakdown** 是可选的，只有少数 provider 有明细数据

### status 字段

```
"online"  — 有 balance 数据且正常
"offline" — 连接失败
"error"   — API 返回错误
"no_data" — 无 balance API 或未配置 key
```

### metric.status 规则

```
"ok"       — percentage < 80%
"warn"     — percentage >= 80%
"critical" — percentage >= 95% 或 balance <= 0
```

## 二、Dashboard Providers 区域

### 整体布局

替代现有 balance cards 区域，改为左右分栏：

```
┌──────────────────────────────────────────────────┐
│  Providers (7 configured)     5 online  1 warning │
├────────────────────┬─────────────────────────────┤
│ 🔍 Search...       │                             │
│                    │  [Detail Panel]              │
│ ┌────────────────┐ │                             │
│ │ ● GLM  PLUS 72%│ │  未选中时：汇总概览          │
│ └────────────────┘ │  选中时：Provider 详情       │
│ ┌────────────────┐ │                             │
│ │ ● DeepSeek ¥1.6│ │                             │
│ └────────────────┘ │                             │
│ ...                │                             │
├────────────────────┴─────────────────────────────┤
│  Charts & Performance Table                      │
└──────────────────────────────────────────────────┘
```

- 左侧列表：约 42% 宽度，可滚动
- 右侧详情：约 58% 宽度，可滚动
- 两者通过 `border-right` 明确分隔

### 左侧列表：两行紧凑卡片

每个 provider 占两行：

```
┌─────────────────────────┐
│ ● GLM     [PLUS] 72% ▓▓ │  ← 第一行：状态灯 + 名称 + 标签 + 迷你进度/数值
│   Tokens 72% · Time 45% │  ← 第二行：缩进的关键指标摘要
└─────────────────────────┘
```

**视觉规则：**
- 每个卡片有独立的圆角背景（`border-radius: 6px`），卡片间距 2-3px
- 选中状态：浅色 accent 背景 + 左边框高亮
- 状态灯颜色：绿色(online)、黄色(warn)、灰色(no_data)、红色(offline)
- 标签使用 pill 样式：`border-radius: 2px` + 对应颜色背景

**不同 provider 的第二行展示：**

| 场景 | 第二行内容 |
|------|-----------|
| GLM (coding plan) | `Tokens 72% · Time 45%` |
| DeepSeek (余额) | `Balance ¥1.60 · Available` |
| z.ai (用量高) | `██████░░░░ 95%` |
| OpenAI (无数据) | `No balance data` |
| omlx (本地) | `Local · Online` |
| sglang (离线) | `Connection refused` (红色) |

### 搜索/筛选栏

位于列表顶部，支持：
- 按名称搜索
- 按状态筛选（全部/Online/Warning/Offline）

### 右侧默认：汇总概览

未选中任何 provider 时显示：

```
┌─────────────────────────────┐
│  ✦ 全局概览                  │
│  点击左侧 Provider 查看详情   │
│                              │
│  ┌────┐ ┌────┐ ┌────┐      │
│  │  5 │ │  1 │ │  1 │      │
│  │ On │ │Warn│ │Off │      │
│  └────┘ └────┘ └────┘      │
│                              │
│  Alerts                      │
│  ⚠ z.ai usage 95%           │
│  ⚠ sglang offline           │
│                              │
│  Top Usage                   │
│  1. z.ai       95%          │
│  2. GLM        72%          │
│  3. DeepSeek   ¥1.60        │
└─────────────────────────────┘
```

### 右侧选中：Provider 详情面板

分区展示，自上而下：

1. **Provider Header** — 图标 + 名称 + plan 标签 + provider 类型
2. **Quota Metrics** — 1x2 或 1x3 网格卡片，每个指标一张卡（进度条 + 数值 + 说明）
3. **Reset Schedule** — 倒计时列表（如有）
4. **Model Breakdown** — 按模型用量表格（如有）
5. **Configuration** — 只读配置摘要（type, endpoint, key, model）

## 三、Providers 配置面板

### 卡片设计

每个 provider 一张全宽卡片，使用与 Dashboard 一致的视觉语言：

```
┌──────────────────────────────────────────────────┐
│ [G] GLM  [PLUS] [Coding]              ✏️ 🔌 🗑️  │
│     open.bigmodel.cn · glm-4-plus · ••••a3f2      │
└──────────────────────────────────────────────────┘
```

- **第一行**：图标(首字母+渐变色) + 名称 + plan 标签 + 右上角图标按钮组
- **第二行**：缩进的配置摘要（endpoint · model · key mask）
- **右上角操作**：三个图标按钮（Edit ✏️ / Test 🔌 / Delete 🗑️），有明确边框和背景

### 视觉状态

- 离线 provider：整体 `opacity: 0.7`，状态灯红色
- Delete 按钮：红色边框，与其他按钮视觉区分

### 品牌色图标

每个 provider 类型有默认渐变色：

| Provider | 渐变色 |
|----------|--------|
| GLM | indigo → purple (#6366f1 → #8b5cf6) |
| DeepSeek | green (#10b981 → #34d399) |
| OpenAI | emerald (#059669 → #34d399) |
| z.ai | blue → cyan (#3b82f6 → #06b6d4) |
| omlx | amber → yellow (#f59e0b → #fbbf24) |
| lmstudio | rose → pink (#f43f5e → #fb7185) |
| sglang | orange → red (#f97316 → #ef4444) |
| 其他 | gray (#6b7280 → #9ca3af) |

图标内容：provider 名称首字母大写。

## 四、视觉风格

### 设计语言

Modern SaaS Dashboard，类似 Linear/Stripe 的设计特征：
- 精致的卡片设计（细边框 + 微阴影）
- 柔和的色彩过渡（渐变 progress bars）
- 圆角一致（卡片 8px, 标签 2-4px, 按钮 4-6px）
- 清晰的信息层次（标题 > 标签 > 正文 > 辅助文字）
- 紧凑但不拥挤的间距

### 色彩体系

保持深色主题基础，但改进对比度和层次感：

```
背景层级：  --bg → --card → --card-elevated
文字层级：  --fg → --muted → --subtle
状态色：    --success (#10b981) / --warning (#f59e0b) / --danger (#ef4444)
主色调：    --accent (indigo #6366f1 系列)
```

### 排版

- 标题：font-weight 700
- 标签/pill：font-size 0.5-0.55rem, uppercase letter-spacing
- 正文：font-size 0.65-0.72rem
- 辅助：font-size 0.55-0.6rem, color: var(--muted)
