# FustAPI Control Plane UI Redesign — Glassmorphism Dashboard

**Date**: 2026-05-10  
**Status**: Approved  
**Scope**: Visual design + layout + interaction polish. Single-file HTML.

## 1. Color System

| Role | Current | New |
|------|---------|-----|
| Page background | `#020617` solid | Deep gradient + 3-4 colored glow orbs (CSS radial-gradient, subtle slow movement) |
| Card surface | `#0F172A` opaque | `rgba(255,255,255,0.03)` + `backdrop-filter: blur(24px)` + `border: 1px solid rgba(255,255,255,0.08)` |
| Accent | `#22C55E` green | `#818CF8` indigo (matches cool glass aesthetic) |
| Success | `#22C55E` | `#34D399` emerald |
| Warning | `#EAB308` | `#FBBF24` amber |
| Danger | `#EF4444` | Keep red, lower saturation |
| Text primary | `#F8FAFC` | Keep |
| Text muted | `#A1A1AA` | `#94A3B8` slate-400 |

## 2. Layout

```
Topbar (frosted glass float)   Brand + Gateway pill + Balance summary
├── Sidebar (vertical tabs, glass)    Dashboard / Providers / Routes / Health
│   └── Each tab with small icon, active = bright left border
└── Main content
    ├── Dashboard (default landing)
    │   ├── Balance overview card + Health card (2-column frosted)
    │   ├── Key metrics: QPS | Latency | Success Rate | In-Flight (4-column)
    │   ├── API Endpoints — compact quick-reference (beside metrics or below)
    │   ├── Charts: QPS + Latency (2-column)
    │   └── Provider performance table (full-width frosted card)
    ├── Providers (tab)
    │   └── Summary metrics + table with balance column
    ├── Routes (tab)
    │   └── Summary metrics + routing table
    └── Health (tab)
        └── Gateway health card
```

Key layout changes:
- Tabs move from horizontal to vertical left sidebar
- Topbar gains balance summary (balance dot per provider, click to expand)
- Dashboard metrics reduced from 6 to 4: QPS, Avg Latency, Success Rate, In-Flight
- Total Requests and Uptime move to secondary position below charts
- API Endpoints card becomes compact quick-reference near metrics

## 3. Component Design

### 3.1 Metric Cards
- Frosted glass with `scale(1.02)` on hover, border glow intensifies
- `font-variant-numeric: tabular-nums` for stable digit widths
- Auto-refresh every 5s with smooth value transition (no pointer jump)
- Loading state: skeleton shimmer (not spinner)

### 3.2 Balance Overview
- Topbar right side: provider dots (green=balance available, grey=no key, red=query error)
- Click dot → dropdown with formatted balance strings
- Dashboard balance card: full-width glass card with all provider balances listed

### 3.3 Provider Performance Table
- Row hover lifts glass brightness
- Empty state: illustration + "Send your first request to see provider stats"
- Zero/null values show `—` to distinguish "no data" from actual zero
- New columns: Gen Speed, Tokens (P/C) stay; Balance status dot added

### 3.4 Toast Notifications
- Slide in from top-right, glassmorphism background
- Three variants: success (emerald), error (red), warning (amber)
- Auto-dismiss after 4s

### 3.5 Modals
- Glass panel + blurred backdrop overlay
- Input focus: border accent glows
- Submit button shows spinner during request

### 3.6 Mobile
- Tab nav collapses to bottom fixed bar
- Tables become card lists
- Charts hidden on small screens

## 4. Implementation Strategy

Single-file `ui/index.html`. All changes in-place:
1. CSS rewrite (variables, base styles, components, animations, responsive)
2. HTML restructure (sidebar, topbar balance, dashboard sections)
3. JS adjustments (balance integration in topbar, mobile tab nav, skeleton loading)

## 5. Verification

- Open dashboard, verify glassmorphism rendering in Chrome/Firefox/Safari
- Verify balance dots in topbar and dashboard card
- Verify mobile layout at 375px width
- Verify all CRUD operations (provider add/edit/delete, route add/edit/delete)
- Verify tab switching works
- Verify toast notifications appear correctly
- Dark theme only (no light mode toggle)
