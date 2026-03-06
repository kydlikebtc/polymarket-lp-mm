# 08 - 动态策略管理

> 运行时动态管理市场和策略，无需重启服务。

---

## 1. 架构概述

### 1.1 设计动机

原始架构中，市场列表和策略参数在 `config.toml` 中静态配置，修改后需重启服务。动态策略管理系统在不改变 `AppConfig`（不可变）的前提下，新增 `StrategyRegistry`（可变）作为运行时策略状态的权威来源。

### 1.2 核心组件

```
AppConfig (启动时不可变)
  ├── markets: Vec<MarketConfig>       ← 初始市场列表
  ├── pricing: PricingConfig           ← 全局默认定价参数
  └── risk: RiskConfig                 ← 全局风控参数

StrategyRegistry (Arc<RwLock<T>> — 运行时可变)
  ├── profiles: HashMap<String, StrategyProfile>
  │   ├── "default"      → PricingConfig (来自 config.toml)
  │   ├── "conservative" → PricingConfig (来自 configs/strategy-conservative.toml)
  │   ├── "balanced"     → PricingConfig (来自 configs/strategy-balanced.toml)
  │   └── "aggressive"   → PricingConfig (来自 configs/strategy-aggressive.toml)
  │
  └── instances: Vec<StrategyInstance>
      ├── market: MarketConfig        ← 市场配置
      ├── profile_name: String        ← 使用哪个 Profile
      ├── enabled: bool               ← 是否启用
      ├── capital_allocation: Decimal  ← 分配资金 (USDC)
      └── overrides: PricingOverrides  ← 覆盖 Profile 的参数
```

### 1.3 数据流

```
TUI 用户操作
     │
     ▼
TuiCommand (mpsc channel)
     │
     ▼
Orchestrator (run_orchestrator)
     │
     ├── ToggleMarket → registry.toggle_market()
     ├── UpdateStrategy → registry.update_strategy()
     ├── AddMarket → registry.add_market() + state.register_market() + WS 重连
     └── RemoveMarket → registry.remove_market() + state.unregister_market() + WS 重连
     │
     ▼
strategy_tick (每 10s)
     │
     └── for instance in registry.active_instances():
             effective_pricing = registry.effective_pricing(instance)
             run_market_strategy(instance, effective_pricing)
```

---

## 2. StrategyProfile

### 2.1 定义

```rust
pub struct StrategyProfile {
    pub name: String,
    pub pricing: PricingConfig,
}
```

Profile 是一组命名的定价参数模板。每个市场通过 `profile_name` 引用一个 Profile，然后可通过 `PricingOverrides` 微调。

### 2.2 Profile 来源

- **"default"** — 始终存在，来自 `config.toml` 的 `[pricing]` 部分
- **预设 Profile** — 从 `configs/strategy-*.toml` 加载（`load_profiles_from_dir`）
- **运行时添加** — 通过 `add_profile()` API

### 2.3 有效参数计算

```rust
effective_pricing(instance) = Profile.pricing + PricingOverrides
```

每个字段独立合并：如果 Override 有值则使用 Override，否则用 Profile 默认值。

---

## 3. StrategyInstance

### 3.1 定义

```rust
pub struct StrategyInstance {
    pub market: MarketConfig,        // 市场标识 + 元数据
    pub profile_name: String,        // 使用的 Profile 名
    pub enabled: bool,               // 是否参与策略循环
    pub capital_allocation: Decimal,  // 分配资金 (USDC)
    pub overrides: PricingOverrides,  // 覆盖参数
}
```

### 3.2 生命周期

```
from_config()      启动时从 AppConfig 创建初始实例
     │
     ▼
toggle_market()    TUI 按 'e' → enabled 切换
     │
     ▼
update_strategy()  TUI 按 Enter → 修改参数/Profile/资金
     │
     ▼
remove_market()    TUI 按 'd' → 从 instances 移除
```

### 3.3 启用/禁用

禁用的市场（`enabled = false`）会被 `active_instances()` 过滤掉，不参与 strategy_tick 循环。已有的挂单不会被自动取消，需等下一次策略循环自然过期或手动取消。

---

## 4. PricingOverrides

```rust
pub struct PricingOverrides {
    pub base_half_spread: Option<Decimal>,
    pub skew_factor: Option<Decimal>,
    pub layers: Option<Vec<LayerConfig>>,
    pub baseline_volatility: Option<Decimal>,
    pub vaf_min: Option<Decimal>,
    pub vaf_max: Option<Decimal>,
    pub requote_threshold: Option<Decimal>,
    pub requote_interval_secs: Option<u64>,
}
```

所有字段均为 `Option`：`None` 表示使用 Profile 默认值，`Some(v)` 表示覆盖。

---

## 5. 动态市场管理

### 5.1 添加市场流程

```
用户按 'a' → 打开搜索 Modal
     │
     ▼
输入关键词 → 通过 Gamma API 搜索
     │
     ▼
选择市场 → 发送 TuiCommand::AddMarket
     │
     ▼
Orchestrator 处理:
  1. registry.add_market()     ← 验证 Profile 有效、无重复、未超限
  2. state.register_market()   ← 初始化 market_states/positions/mappings
  3. ws_tokens 添加新 token    ← WS 动态订阅
  4. ws_reconnect_needed = true ← 触发 WS 重连
  5. fetch_metadata()          ← 异步获取结算时间
```

### 5.2 删除市场流程

```
用户按 'd' → 确认弹窗
     │
     ▼
TuiCommand::RemoveMarket
     │
     ▼
Orchestrator 处理:
  1. registry.remove_market()    ← 从 instances 移除
  2. state.unregister_market()   ← 清理所有状态
  3. ws_tokens 移除 token        ← 停止 WS 订阅
  4. ws_reconnect_needed = true  ← 触发 WS 重连
```

### 5.3 Gamma API 市场搜索

```rust
pub async fn search_markets(&self, query: &str, limit: i32)
    -> Result<Vec<GammaSearchResult>>
```

搜索条件：
- `slug_contains` 关键词匹配
- `active = true`（只搜索活跃市场）
- `closed = false`（排除已结算市场）
- 必须有有效的 `condition_id` 和 CLOB token IDs

### 5.4 WS 动态订阅

WebSocket 连接支持动态 token 订阅：
- `ws_tokens: Arc<RwLock<Vec<String>>>` 存储当前订阅的 token 列表
- `ws_reconnect_needed: Arc<AtomicBool>` 信号标志
- `run_market_ws` 在每次重连时读取最新 token 列表
- `run_market_ws_inner` 在循环中检测 `ws_reconnect_needed`，触发优雅断开重连

---

## 6. TUI 交互

### 6.1 Strategy Tab

Strategy Tab（按 `5` 切换到）显示所有市场的策略状态：

```
┌─ Strategy Management ──────────────────────────────────────────┐
│ Market              Profile      Capital  Status   Mid   Sprd  │
│ ► Trump 2028        aggressive   $500     ACTIVE   0.35  0.01  │
│   Fed Rate Cut      conservative $200     PAUSED   0.62  0.02  │
│   ETH > 5K          balanced     $300     ACTIVE   0.48  0.01  │
├─────────────────────────────────────────────────────────────────┤
│ Selected: Trump 2028 | Profile: aggressive                     │
│ base_half_spread: 0.005 | skew: 0.020 | capital: $500          │
├─────────────────────────────────────────────────────────────────┤
│ [e]nable/disable [a]dd [d]elete [Enter]edit [p]rofile          │
└─────────────────────────────────────────────────────────────────┘
```

### 6.2 Modal 弹窗

系统提供 4 种 Modal 弹窗：

| Modal | 触发 | 功能 |
| ------ | ------ | ------ |
| SearchMarket | `a` 键 | 搜索 Gamma API，选择并添加市场 |
| EditParams | `Enter` 键 | 编辑 base_half_spread、skew_factor、capital |
| SelectProfile | `p` 键 | 从可用 Profile 列表中选择 |
| Confirm | `d` 键 | 删除市场前的确认弹窗 |

### 6.3 键盘路由

```
handle_key()
     │
     ├── Modal 活跃? → modal.handle_key()  (Modal 捕获所有输入)
     │
     └── 正常模式:
         ├── Tab::Strategy → handle_strategy_key()
         │    ├── 'e' → toggle_market
         │    ├── 'a' → open_search_modal
         │    ├── 'd' → open_confirm_modal
         │    ├── Enter → open_edit_params_modal
         │    ├── 'p' → open_profile_modal
         │    └── j/k → navigate market list
         └── 其他 Tab → 原有逻辑
```

---

## 7. 线程安全

### 7.1 锁策略

- `StrategyRegistry` 使用 `Arc<RwLock<T>>`
- strategy_tick 读取时持 read lock（多个并发读）
- TUI 命令修改时持 write lock（独占写）
- 锁持有时间最小化：读取后立即释放，避免跨 await 持锁

### 7.2 数据一致性

- `AddMarket` 操作先验证 registry（可回滚），成功后再修改 state（不可回滚）
- `RemoveMarket` 操作顺序：registry → state → WS tokens
- 状态通过 `DashboardSnapshot` 传递给 TUI，TUI 永远不直接访问锁

---

## 8. 配置限制

| 限制 | 数值 | 说明 |
| ------ | ------ | ------ |
| 最大市场数 | 10 | 超过时 `add_market()` 返回错误 |
| Profile 名长度 | 不限 | 建议 3-20 字符 |
| 最小资金分配 | $0 | 允许零资金（用于观察模式） |
| 最大资金分配 | 无硬限制 | 受 `total_capital` 约束 |
