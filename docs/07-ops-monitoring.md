# 07 - 监控运维层

> 系统运行时的监控、指标、日志和运维操作指南。

---

## 1. 日志体系

### 1.1 日志框架

使用 `tracing` + `tracing-subscriber` 提供结构化日志：

```rust
// 在 main.rs 中初始化
tracing_subscriber::fmt()
    .with_env_filter(
        EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| EnvFilter::new("info"))
    )
    .init();
```

通过环境变量 `RUST_LOG` 控制日志级别：

```bash
# 全局 INFO
RUST_LOG=info cargo run

# 模块级精细控制
RUST_LOG=polymarket_mm=debug,polymarket_mm::data::ws=trace cargo run

# 只看警告和错误
RUST_LOG=warn cargo run
```

### 1.2 日志级别规范

| 级别 | 用途 | 示例 |
|------|------|------|
| ERROR | 需要立即处理的故障 | WS 任务崩溃、API 调用失败、L3 紧急状态 |
| WARN | 潜在问题或风险事件 | L2 触发、Ghost Fill、取消超时、IIR 偏高 |
| INFO | 关键业务事件 | 订单提交/取消、风险等级变化、启动/关闭、定期指标 |
| DEBUG | 调试信息 | 每条 WS 消息、订单簿更新、定价计算详情 |
| TRACE | 极细粒度 | 原始 WS 帧数据（通常不使用） |

### 1.3 关键日志模式

**启动日志：**
```
INFO  Initializing CLOB client at https://clob.polymarket.com
INFO  CLOB API authenticated OK
INFO  Loaded initial position: market=Example, YES shares=150.00
INFO  Fetched settlement times for 2/2 markets
INFO  Market 0xABC: settles at 2026-04-01T00:00:00Z, 623.5h remaining
INFO  Loaded 4 existing open orders from API
INFO  Main loop started. Press Ctrl+C to stop gracefully.
```

**策略执行日志：**
```
INFO  Market 0xABC: mid=0.52, IIR=0.15, VAF=1.2, TF=1.0, orders=6, est_Q=42.3
INFO  Submitted 6/6 orders successfully
```

**风险事件日志：**
```
WARN  RISK LEVEL: L1-Normal → L2-Warning | Trigger: IIR exceeded: market=0xABC, IIR=0.55
WARN  Ghost fill detected! Order abc123 cancelled without our request
WARN  RISK LEVEL: L2-Warning → L3-Emergency | Trigger: Ghost fills detected: 3
```

---

## 2. 定期指标

### 2.1 指标输出 (每 60 秒)

系统每分钟输出一次聚合指标：

```
INFO  METRICS | Risk=L1-Normal | ActiveOrders=12 | Markets=2 | DailyPnL=$3.42 | Fills=8
INFO    Market ExampleA | mid=0.52 | spread=0.01 | IIR=0.15 | YES=150.00 NO=0.00
INFO    Market ExampleB | mid=0.71 | spread=0.02 | IIR=-0.08 | YES=30.00 NO=50.00
```

### 2.2 关键指标定义

| 指标 | 计算方式 | 含义 |
|------|----------|------|
| Risk | RiskController.level() | 当前风控等级 L1/L2/L3 |
| ActiveOrders | my_orders 中 status=Live 的数量 | 当前活跃挂单 |
| Markets | config.markets.len() | 配置的市场数量 |
| DailyPnL | PnlTracker.realized_pnl | 当日已实现盈亏 (USDC) |
| Fills | PnlTracker.fill_count | 当日成交笔数 |
| mid | MarketState.midpoint | 市场中间价 |
| spread | MarketState.spread (ask - bid) | 当前买卖价差 |
| IIR | PositionRecord.iir() | 库存失衡比率 [-1, +1] |
| YES/NO | PositionRecord.yes_shares/no_shares | 持仓数量 |

---

## 3. 循环定时器

系统使用 `tokio::select!` 驱动多个并行定时器：

| 定时器 | 间隔 | 功能 |
|--------|------|------|
| strategy_tick | 10s | 风险评估 + 每市场策略执行 |
| position_tick | 60s | 持仓价值更新 + IIR 评估 + Merge 检测 |
| metrics_tick | 60s | 输出聚合指标日志 |
| settlement_tick | 1800s (30min) | 从 Gamma API 刷新结算时间 |
| cleanup_tick | 300s (5min) | 清理过期订单和取消请求 |

---

## 4. 状态清理

### 4.1 订单清理 (prune_stale_state)

每 5 分钟执行一次：
- **已终结订单** (Canceled/Matched)：更新时间 >5min → 移除
- **疑似卡住订单** (Live/Pending)：更新时间 >1h → 移除
- **取消请求**：清理 RiskController 中过期的 cancel request 记录

### 4.2 价格历史清理

在 `SharedState::record_price` 中持续清理：
- 保留最近 60 分钟的数据
- 硬上限 10,000 条（防止高频数据溢出）

### 4.3 Ghost Fill 记录清理

在 `RiskController::record_ghost_fill` 中：
- 清理窗口外 (默认 30min) 的旧记录
- 硬上限 1,000 条

---

## 5. WebSocket 健康检测

### 5.1 连接状态

两个 WebSocket 连接独立跟踪：

| 连接 | 状态标志 | 心跳追踪 |
|------|----------|----------|
| Market WS | market_ws_connected (AtomicBool) | ws_last_message (RwLock<DateTime>) |
| User WS | user_ws_connected (AtomicBool) | user_ws_last_message (RwLock<DateTime>) |

### 5.2 断连检测逻辑

```
ws_disconnect_secs = now - ws_last_message
max_ws_disconnect_secs = max(market_secs, user_secs)

触发条件：
  max_ws_disconnect_secs >= 30s → L2 风控
  策略循环中 max_ws_disconnect_secs > 10s → 跳过下单
  WS 任务退出 (JoinHandle 完成) → 强制 L3 + 停机
```

### 5.3 重连策略

两个 WS 都使用相同的指数退避重连：
- 成功连接后 backoff 重置为 1s
- 失败时 backoff ×2，上限 30s
- 断连时标志位置为 false (Release ordering)
- 收到首条消息后标志位置为 true (Release ordering)

---

## 6. 启动前检查清单

1. **环境变量就绪**：
   - `POLYMARKET_PRIVATE_KEY` - 钱包私钥
   - `POLYMARKET_API_KEY` - API Key (UUID)
   - `POLYMARKET_API_SECRET` - API Secret
   - `POLYMARKET_API_PASSPHRASE` - API Passphrase

2. **配置文件就绪**：
   - `config.toml` 存在且通过验证
   - 至少配置 1 个市场

3. **API 连接验证**：
   - CLOB API 认证成功（check_connection）
   - USDC 余额 > 0

4. **网络就绪**：
   - WS 端点可达
   - Gamma API 可达

---

## 7. 优雅关机流程

```
Ctrl+C (SIGINT)
  │
  ▼
warn!("Ctrl+C received, initiating graceful shutdown...")
  │
  ▼
executor.cancel_all_orders()  ← 取消所有活跃订单
  │
  ├── 成功 → info!("All orders cancelled. Shutdown complete.")
  │
  └── 失败 → error!("Failed to cancel orders during shutdown: {e:#}")
                    （仍然退出，但订单可能残留在交易所）
```

**注意**：残留订单会在交易所端保持 Live 状态，需要手动通过 API 或 UI 取消。

---

## 8. 运维操作

### 8.1 手动 L3 恢复

当系统进入 L3 紧急状态后，只能通过手动干预恢复：

1. 确认触发原因（查看日志中的 `RISK LEVEL: → L3-Emergency | Trigger:` 行）
2. 评估市场状况是否已恢复正常
3. 通过代码调用 `risk_controller.manual_recover()` 恢复到 L1
4. 系统将在下一个策略循环 (10s) 自动恢复做市

> MVP 阶段需要重启进程来恢复。未来可通过 HTTP API 端点实现运行时恢复。

### 8.2 紧急停机

```bash
# 方式 1: Ctrl+C (推荐，会自动取消所有订单)
# 方式 2: kill -SIGINT <pid>
# 方式 3: kill -SIGTERM <pid> (不会触发优雅关机!)

# 注意: 使用 SIGKILL (-9) 会导致订单残留！
```

### 8.3 日志分析常用命令

```bash
# 查看最近的风险事件
grep "RISK LEVEL" bot.log | tail -20

# 查看 Ghost Fill 事件
grep "Ghost fill" bot.log

# 查看每分钟指标
grep "METRICS" bot.log

# 查看订单提交/取消
grep -E "Submitted|Cancelling" bot.log

# 查看 PnL 变化
grep "PnL update" bot.log

# 查看 WS 断连
grep "WS" bot.log | grep -E "error|disconnect|reconnect"
```

---

## 9. 已知限制

| 限制 | 影响 | 计划 |
|------|------|------|
| 无持久化存储 | 重启后丢失 PnL 历史 | 未来添加 SQLite |
| 无 Telegram 告警 | 需要手动查看日志 | P2 延后 |
| L3 恢复需重启 | 运维不便 | 未来添加 HTTP API |
| 无 API 限流追踪 | 理论上可能超限 | 未来添加滑动窗口计数 |
| PnL 成本基础近似 | 启动时用 0.50 作为已有持仓的成本 | 文档化接受 |
