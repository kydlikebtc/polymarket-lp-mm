# Polymarket LP 做市系统

Polymarket 预测市场的自动化做市（Market Making）系统，基于 Q-Score 奖励机制设计，兼顾持仓安全与奖励最大化。

## 快速开始

### 环境要求

- Rust 1.88+ (Edition 2024)
- Polygon 网络 USDC 余额
- Polymarket CLOB API 凭证（[申请入口](https://docs.polymarket.com/)）

### 1. 克隆与编译

```bash
git clone https://github.com/kydlikebtc/polymarket-lp-mm.git
cd polymarket-lp-mm

# Headless 模式（纯日志输出）
cargo build --release

# TUI 仪表盘模式（终端实时面板）
cargo build --release --features tui
```

### 2. 配置 API 密钥

```bash
cp .env.example .env
```

编辑 `.env`，填入 4 个凭证（获取方式见下文）：

```env
POLYMARKET_PRIVATE_KEY=0xyour_private_key_here
POLYMARKET_API_KEY=your_api_key_here
POLYMARKET_API_SECRET=your_api_secret_here
POLYMARKET_API_PASSPHRASE=your_passphrase_here
```

**如何获取这 4 个凭证：**

1. **导出私钥** — 登录 [Polymarket](https://polymarket.com)，进入 Settings → [Export Private Key](https://polymarket.com/settings?tab=export-private-key)。邮箱注册用户也可通过 [Magic Link](https://reveal.magic.link/polymarket) 导出。

2. **派生 API 凭证** — 用 Polymarket 官方 Python 工具一次性生成 API Key / Secret / Passphrase（只需执行一次）：

   ```bash
   pip install py-clob-client
   python -c "
   from py_clob_client.client import ClobClient
   client = ClobClient('https://clob.polymarket.com', key='你的私钥', chain_id=137)
   # 邮箱注册用户改用:
   # client = ClobClient('https://clob.polymarket.com', key='你的私钥', chain_id=137, signature_type=1, funder='你的充值地址')
   creds = client.create_or_derive_api_creds()
   print(f'POLYMARKET_API_KEY={creds.api_key}')
   print(f'POLYMARKET_API_SECRET={creds.api_secret}')
   print(f'POLYMARKET_API_PASSPHRASE={creds.api_passphrase}')
   "
   ```

3. 将输出的 3 个值连同私钥一起填入 `.env` 文件。

> **安全提示**：私钥在启动时读取后立即从环境变量中清除（`remove_var`），运行期内存中不保留原始私钥。凭证派生是确定性的，同一私钥每次派生结果相同。

### 3. 配置策略

项目提供 3 个预设策略模板，按风险偏好选择：

```bash
# 方式一：使用预设策略（推荐新手从保守型开始）
cp configs/strategy-conservative.toml config.toml

# 方式二：从空白模板开始
cp config.example.toml config.toml
```

编辑 `config.toml`，**必须替换**以下占位符：

```toml
[[markets]]
market_id = "REPLACE_WITH_MARKET_ID"   # ← 替换为实际 condition_id
token_id  = "REPLACE_WITH_TOKEN_ID"    # ← 替换为实际 YES token ID
name      = "My First Market"
```

**获取 market_id 和 token_id**：

```bash
# 方式一：通过市场 slug 查询
curl -s "https://gamma-api.polymarket.com/markets?slug=will-trump-win-2028" | jq '.[0] | {condition_id, tokens}'

# 方式二：直接通过 CLOB API
curl -s "https://clob.polymarket.com/markets/YOUR_CONDITION_ID"
```

### 4. 启动

```bash
# Headless 模式（日志输出到 stderr）
cargo run --release

# TUI 仪表盘模式（日志写入 bot.log）
cargo run --release --features tui

# 调整日志级别
RUST_LOG=polymarket_mm=debug cargo run --release
```

### 5. 运行测试

```bash
cargo test           # 运行全部 75 个测试
cargo test -- -q     # 静默模式
```

---

## 预设策略

`configs/` 目录包含 3 个策略模板，覆盖不同风险偏好：

| 策略 | 文件 | 资金 | 市场数 | 基础价差 | 内层距离 | 年化预期 |
| ------ | ------ | ------ | -------- | --------- | --------- | --------- |
| 保守型 | `strategy-conservative.toml` | $1,000 | 1 | 1.0c | 0.8c | 5-15% |
| 平衡型 | `strategy-balanced.toml` | $3,000 | 2-3 | 0.8c | 0.5c | 10-25% |
| 激进型 | `strategy-aggressive.toml` | $5,000+ | 3-5 | 0.5c | 0.3c | 20-40% |

### 策略选择建议

- **首次运行** → 保守型。宽价差 + 低风控阈值，适合熟悉系统行为
- **稳定运行 1-2 周后** → 平衡型。多市场分散，适度提升 Q-Score 效率
- **有做市经验** → 激进型。窄价差集中内层，最大化 Q-Score，但逆向选择风险显著增加

### 关键参数说明

```toml
[pricing]
base_half_spread = 0.008    # 基础半价差（越小越激进，成交更多但逆向选择风险更大）
skew_factor      = 0.020    # 偏斜因子（IIR=1 时报价偏移量，越大调仓越快）

[[pricing.layers]]
distance         = 0.005    # 距中间价距离（越近 Q-Score 越高，风险也越大）
capital_fraction = 0.40     # 该层资金占比

[risk]
l2_iir_threshold     = 0.45 # 持仓失衡达到 45% 触发 L2 预警
l2_daily_loss_pct    = 0.03 # 日亏损 3% 触发 L2 预警
l3_daily_loss_pct    = 0.06 # 日亏损 6% 触发 L3 紧急停止
```

---

## TUI 仪表盘

使用 `--features tui` 编译后启动，终端内实时展示完整运营数据：

```
┌─ Polymarket MM v0.1.0 ── L1-Normal ── WS:OK ── Up: 2h 15m ──────────┐
│ ┌─ Account ─────────────────┐ ┌─ Strategy ──────────────────────────┐│
│ │ USDC:   $196.49           │ │ Capital: $190  Deployed: $95 (50%)  ││
│ │ Tokens: Y=$0.00  N=$0.00  │ │ PnL:  $0.52  Fills: 3              ││
│ │ Total:  $196.49           │ │ Q-Score: 17.3  LP: DUAL             ││
│ └───────────────────────────┘ └─────────────────────────────────────┘│
│ ┌─ Markets ─────────────────────────────────────────────────────────┐│
│ │ Market            Mid    Sprd   Bid    Ask    IIR   YES NO  Q  LP ││
│ │ Iranian regime.. 0.3850 0.0100 0.3800 0.3900 0.000  0   0 17  DL ││
│ └───────────────────────────────────────────────────────────────────┘│
│ ┌─ Pricing Factors ─────────┐ ┌─ Execution ─────────────────────────┐│
│ │ Iranian reg.. VAF:0.80    │ │ Placed: 12  Cancelled: 9           ││
│ │   TF:1.0 Skew:0.000      │ │ Ghost: 1  Cancel%: 75%  Ghost%: 8% ││
│ │   Stl: 2792h             │ │ Fills: 3                            ││
│ └───────────────────────────┘ └─────────────────────────────────────┘│
│ PnL: +$0.52 | Fills: 3 | [1-4] Tab  [q] Quit                       │
└──────────────────────────────────────────────────────────────────────┘
```

**4 个 Tab**：Overview（战情室）/ Orders（订单明细）/ Risk（风控状态）/ Charts（价格图表）

**键盘操作**：

| 按键 | 作用 |
|------|------|
| `1-4` / `Tab` | 切换面板 |
| `j/k` / `↑↓` | 滚动订单列表 |
| `f` | 切换订单筛选（全部/活跃） |
| `r` | 手动 L3 恢复 |
| `←→` | 切换图表市场 |
| `q` / `Ctrl+C` | 退出 |

> TUI 模式下，tracing 日志自动重定向到 `bot.log`，不干扰终端显示。

---

## 项目结构

```
polymarket-lp-mm/
├── src/
│   ├── main.rs              # 入口：密钥读取、组件初始化、TUI 启动
│   ├── lib.rs               # 模块声明
│   ├── config/mod.rs        # TOML 配置解析与校验
│   ├── data/
│   │   ├── mod.rs           # SharedState 共享状态（DashMap + RwLock）
│   │   ├── rest.rs          # CLOB REST API 客户端
│   │   ├── ws.rs            # WebSocket 实时行情/用户事件
│   │   ├── gamma.rs         # Gamma API（结算时间、市场元数据）
│   │   ├── state.rs         # 状态管理辅助
│   │   └── ctf.rs           # CTF 链上 Merge 操作
│   ├── pricing/mod.rs       # 定价引擎：VAF/IIF/TF 三因子 + 阶梯挂单
│   ├── position/mod.rs      # 持仓管理：IIR 计算、Skewing、Merge 决策
│   ├── risk/mod.rs          # 三级风控状态机（L1→L2→L3）
│   ├── execution/mod.rs     # 订单执行：批量下单、EIP-712 签名
│   ├── monitor/
│   │   ├── mod.rs           # 主循环 Orchestrator（tokio::select!）
│   │   └── strategy.rs      # 策略逻辑：报价生成、风控联动
│   └── tui/                 # TUI 仪表盘（feature-gated）
│       ├── mod.rs           # TUI 入口 + 终端管理
│       ├── app.rs           # 应用状态 + 键盘处理
│       ├── event.rs         # 事件循环（crossterm + tick + snapshot）
│       ├── snapshot.rs      # DashboardSnapshot 只读快照
│       ├── ui.rs            # 顶层渲染调度
│       └── tabs/            # 4 个 Tab 渲染模块
├── tests/                   # 75 个单元/集成测试
├── configs/                 # 预设策略模板
├── docs/                    # 设计文档
├── .env.example             # API 密钥模板
└── config.example.toml      # 配置文件模板
```

---

## 核心设计

### Q-Score 二次衰减

```
Q = ((max_spread - distance) / max_spread)² × size

距中间价     得分效率
0.0 cents   100%
0.5 cents    69%
1.0 cents    44%   ← 移动 1 cent，损失 56%
2.0 cents    11%   ← 再移动 1 cent，损失 75%
3.0 cents     0%   （超出激励范围）
```

奖励随距离平方级下降，主力资金必须集中在最靠近中间价的位置。

### 双边挂单 = 3 倍奖励

平台对单边挂单执行 ÷3 惩罚：

```
$200 全部买单（单边）→ Q-Score × 1/3 ≈ 26.7 分
$100 买单 + $100 卖单（双边）→ Q-Score × 1  ≈ 80.2 分
```

相同资金，双边策略奖励是单边的 **3 倍**。

### 阶梯挂单

```
内层（0.5c）→  贡献 ~75% Q-Score（高效但高风险）
中层（1.5c）→  贡献 ~20%
外层（2.5c）→  贡献 ~5%（缓冲极端行情，争取撤单时间）
```

### Quote Skewing

持仓失衡时，通过调整报价引导市场自然平衡，零滑点调仓：

```
IIR = +0.6（持有过多 YES）
正常报价：bid@0.59，ask@0.61
偏斜后：  bid@0.578，ask@0.598
→ 卖出 YES 概率增大，被动调仓
```

### CTF Merge

同时持有 YES 和 NO 时，链上 Merge 比市价卖出零损耗：

```
YES=300, NO=200 → Merge 200 对 → 收回 $200 USDC，剩余 YES=100
```

### 三级风控状态机

```
L1（正常）→ 全自动做市
    ↓ IIR/价格跳动/日亏超阈值
L2（预警）→ 规模收缩 + 价差扩大，可自动恢复
    ↓ 持续恶化或超时
L3（紧急）→ 全部撤单，必须人工确认恢复（TUI 按 r 键）
```

---

## 技术约束

| 约束 | 数值 |
|------|------|
| 批量下单上限 | 15 个/批 |
| API 限流 | 3000 次/10 分钟 |
| Q-Score 采样频率 | 每分钟 1 次 |
| WebSocket 保活 | 每 8 秒 PING |
| Merge 最小规模 | 建议 $100（Gas 成本） |
| 市场价格范围 | [0.01, 0.99] |
| 奖励结算 | 每日 UTC 00:00 |

---

## 文档目录

| 文档 | 内容 |
|------|------|
| [00-system-overview](./docs/00-system-overview.md) | 系统架构、数据流、启动顺序 |
| [01-data-layer](./docs/01-data-layer.md) | WebSocket、REST、断线重连、状态缓存 |
| [02-qscore-rewards](./docs/02-qscore-rewards.md) | Q-Score 公式、二次衰减、激励密度 |
| [03-pricing-engine](./docs/03-pricing-engine.md) | 价差计算、VAF/IIF/TF 动态调整 |
| [04-position-management](./docs/04-position-management.md) | IIR、Skewing、Merge 决策树 |
| [05-risk-control](./docs/05-risk-control.md) | L1/L2/L3 状态机、Ghost Fills 检测 |
| [06-execution-layer](./docs/06-execution-layer.md) | 订单生命周期、EIP-712、批量操作 |
| [07-ops-monitoring](./docs/07-ops-monitoring.md) | 运维监控、告警、日志分析 |

---

## License

MIT
