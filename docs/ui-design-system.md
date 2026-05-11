## Design System: FustAPI

### Pattern
- **Name:** Real-Time / Operations Dashboard
- **Style:** Modern SaaS Dashboard (Linear/Stripe aesthetic)
- **Mode:** Dark only
- **Scope:** Single-file HTML (`ui/index.html`), no build tools

### Colors
| Role | Hex | CSS Variable |
|------|-----|--------------|
| Background | `#020617` | `--bg` |
| Card surface | `#0F172A` | `--card` |
| Card elevated | `#1E293B` | `--card-elevated` |
| Text primary | `#F8FAFC` | `--fg` |
| Text muted | `#94A3B8` | `--muted` |
| Text subtle | `#64748B` | `--subtle` |
| Success | `#34D399` | `--success` |
| Warning | `#FBBF24` | `--warning` |
| Danger | `#EF4444` | `--danger` |
| Border | `#334155` | `--border` |

### Provider Brand Gradients
| Provider | Gradient |
|----------|----------|
| GLM | `#6366f1 → #8b5cf6` (indigo → purple) |
| DeepSeek | `#10b981 → #34d399` (green) |
| OpenAI | `#059669 → #34d399` (emerald) |
| z.ai | `#3b82f6 → #06b6d4` (blue → cyan) |
| omlx | `#f59e0b → #fbbf24` (amber → yellow) |
| lmstudio | `#f43f5e → #fb7185` (rose → pink) |
| sglang | `#f97316 → #ef4444` (orange → red) |

### Typography
- **Font:** System font stack (no external fonts)
- **Heading:** font-weight 700
- **Pill/label:** 0.5-0.55rem, uppercase, letter-spacing
- **Body:** 0.65-0.72rem
- **Auxiliary:** 0.55-0.6rem, color: var(--muted)

### Component Patterns

**Split Panel (Dashboard):** 42% list + 58% detail, flex layout
**List Items:** Two-row compact cards — row1: status dot + name + pills + mini value; row2: metrics summary
**Detail Panel:** Metric cards (grid auto-fit), alerts, breakdown table, reset schedule, config summary
**Config Cards:** Brand icon + name + type pill + action buttons (Edit/Test/Delete)
**Status Dots:** 6px circle — green (online), yellow (warn), gray (no_data), red (offline)
**Pills:** border-radius 2px, font-size 0.5rem, uppercase
**Action Buttons:** 26px square, border-radius 5px, icon content; Delete has red border

### Balance Data Model

Backend returns unified `ProviderBalance` JSON:
```
{ provider_name, status, plan, plan_type, alerts[], metrics[], breakdown[], resets[], config_summary }
```

- `metrics[].kind`: "percentage" or "absolute"
- `metrics[].status`: "ok" (<80%), "warn" (>=80%), "critical" (>=95%)
- `plan_type`: "coding" | "token" | "credit"

### Conventions
- No inline event handlers (`onclick=`) — use `addEventListener`
- No build tools — vanilla HTML/CSS/JS in single file
- Use `escapeHtml()` for all dynamic text
- Desktop-only (no responsive breakpoints)
