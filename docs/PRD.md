# PRD: Polymarket LP 做市机器人 MVP

> 版本: 1.0 | 更新日期: 2026-03-05

## 1. Executive Summary

基于 Q-Score 奖励机制的 Polymarket 预测市场自动化做市系统 MVP。使用 Rust 开发，支持 1-3 个市场同时做市，实盘交易模式。核心目标是通过智能的阶梯挂单策略最大化 LP 奖励收入，同时通过三级风控系统控制风险。

## 2. Tech Stack

| 组件 | 技术选型 | 用途 |
|------|----------|------|
| 语言 | Rust 2021 edition | 高性能、内存安全 |
| 异步运行时 | tokio | 异步 I/O |
| SDK | polymarket-client-sdk | CLOB/Gamma API + WebSocket |
| 签名 | alloy (EIP-712) | 订单签名 |
| 精度 | rust_decimal | 金融精度计算 |
| 配置 | toml + dotenvy | 策略参数 + 密钥管理 |
| 日志 | tracing + tracing-subscriber | 结构化日志 |
| 序列化 | serde + serde_json | 数据序列化 |
| 并发 | dashmap | 无锁并发 HashMap |
| 安全 | zeroize | 密钥清理 |

## 3. 需求规格

### R1: 配置模块 (config)

| ID | 需求 | 优先级 | 状态 |
|----|------|--------|------|
| R1-1 | 从 `.env` 加载敏感信息（私钥、API Key/Secret/Passphrase） | P0 | ✅ 完成 |
| R1-2 | 从 `config.toml` 加载策略参数（价差、阶梯、风控阈值） | P0 | ✅ 完成 |
| R1-3 | 支持多市场配置（market_id, token_id, name, max_incentive_spread, min_size） | P0 | ✅ 完成 |
| R1-4 | 参数验证（价格范围、资金比例、非负数检查） | P1 | ✅ 完成 |
| R1-5 | 资金分配计算 per_market_capital = total / max(markets, 1/fraction) | P0 | ✅ 完成 |

### R2: 数据层 (data)

| ID | 需求 | 优先级 | 状态 |
|----|------|--------|------|
| R2-1 | Market WebSocket: 订阅 orderbook 更新，获取 bid/ask/midpoint | P0 | ✅ 完成 |
| R2-2 | User WebSocket: 认证后订阅订单状态更新 | P0 | ✅ 完成 |
| R2-3 | WS 断线重连：指数退避 (1s → 2s → ... → 30s max) | P0 | ✅ 完成 |
| R2-4 | REST 客户端: 下单、取消、批量下单、余额查询 | P0 | ✅ 完成 |
| R2-5 | Gamma API: 市场元数据、结算时间查询 | P0 | ✅ 完成 |
| R2-6 | 内存状态缓存: market_states, my_orders, positions | P0 | ✅ 完成 |
| R2-7 | 价格历史记录 (60min 滑动窗口，上限 10,000 条) | P0 | ✅ 完成 |
| R2-8 | token_id → market_id 映射 | P0 | ✅ 完成 |
| R2-9 | WS 连接状态跟踪 (AtomicBool + Ordering) | P0 | ✅ 完成 |
| R2-10 | 每日 PnL 跟踪器 (加权平均成本基础) | P0 | ✅ 完成 |
| R2-11 | WS 价格范围验证 (binary market: 0 < p < 1) | P1 | ✅ 完成 |
| R2-12 | API 限流管理: 3000 次/10分钟滑动窗口 | P2 | ⬜ 未实现 |

### R3: 定价引擎 (pricing)

| ID | 需求 | 优先级 | 状态 |
|----|------|--------|------|
| R3-1 | Q-Score 估算: ((max_spread - distance) / max_spread)² × size | P0 | ✅ 完成 |
| R3-2 | VAF (波动率调整因子): clamp(recent_vol / baseline_vol, min, max) | P0 | ✅ 完成 |
| R3-3 | TF (时间因子): 基于结算剩余时间的阶梯倍数 | P0 | ✅ 完成 |
| R3-4 | 阶梯挂单 (3 层): 可配置 distance + capital_fraction | P0 | ✅ 完成 |
| R3-5 | Quote Skewing: bid/ask 同向偏移 skew = -IIR × skew_factor | P0 | ✅ 完成 |
| R3-6 | 最小距离保护 (≥0.005 防止零价差) | P1 | ✅ 完成 |
| R3-7 | 价格边界保护 [0.01, 0.99] + bid < ask 检查 | P0 | ✅ 完成 |
| R3-8 | 最小订单量过滤 (min_size) | P0 | ✅ 完成 |
| R3-9 | Ask 侧库存上限 (不超过持有的 YES shares) | P0 | ✅ 完成 |

### R4: 持仓管理 (position)

| ID | 需求 | 优先级 | 状态 |
|----|------|--------|------|
| R4-1 | IIR 计算: (yes_value - no_value) / allocated_capital | P0 | ✅ 完成 |
| R4-2 | IIR 响应: |IIR|≥0.5 → L2, |IIR|≥0.75 → L3 | P0 | ✅ 完成 |
| R4-3 | Merge 检测: min(YES, NO) ≥ min_merge_size | P0 | ✅ 完成 |
| R4-4 | Merge 冷却: 每个市场独立的合并冷却时间 | P1 | ✅ 完成 |
| R4-5 | CTF 合约 mergePositions 调用 | P1 | 🔲 TODO |
| R4-6 | 持仓价值实时更新 (基于最新 midpoint) | P0 | ✅ 完成 |

### R5: 风控模块 (risk)

| ID | 需求 | 优先级 | 状态 |
|----|------|--------|------|
| R5-1 | L1/L2/L3 三级状态机 | P0 | ✅ 完成 |
| R5-2 | L2 触发: IIR≥0.5, 价变≥5¢, 日亏≥3%, WS断连≥30s | P0 | ✅ 完成 |
| R5-3 | L3 触发: IIR≥0.75, 价变≥10¢, 日亏≥8%, Ghost Fills≥3 | P0 | ✅ 完成 |
| R5-4 | L2 → L1 自动恢复 (条件满足 + hold 期) | P0 | ✅ 完成 |
| R5-5 | L3 仅手动恢复 | P0 | ✅ 完成 |
| R5-6 | L2 超时自动升级 L3 (默认 2h) | P0 | ✅ 完成 |
| R5-7 | Ghost Fill 检测 (our_cancel_requests vs WS CANCELED) | P0 | ✅ 完成 |
| R5-8 | L2 参数调节 (size×0.5, spread×1.5, 可配置) | P0 | ✅ 完成 |
| R5-9 | 时钟偏移防护 (.max(0) 防止负数持续时间) | P1 | ✅ 完成 |

### R6: 执行层 (execution)

| ID | 需求 | 优先级 | 状态 |
|----|------|--------|------|
| R6-1 | 批量下单 (每批最多 15 单) | P0 | ✅ 完成 |
| R6-2 | Cancel-Replace 模式 (先取消旧单，再提交新单) | P0 | ✅ 完成 |
| R6-3 | 指数退避重试 (可配置 base/max delay) | P0 | ✅ 完成 |
| R6-4 | WS 取消确认等待 (带超时) | P0 | ✅ 完成 |
| R6-5 | 未确认取消检测 (跳过本轮新单提交) | P1 | ✅ 完成 |
| R6-6 | 订单跟踪容量保护 (MAX_TRACKED_ORDERS=5000) | P1 | ✅ 完成 |
| R6-7 | 预提交安全检查 (总价值上限 1M) | P1 | ✅ 完成 |
| R6-8 | L3 紧急全市场取消 + 强制标记 | P0 | ✅ 完成 |

### R7: 主循环编排 (monitor)

| ID | 需求 | 优先级 | 状态 |
|----|------|--------|------|
| R7-1 | 策略循环 (每 10s) | P0 | ✅ 完成 |
| R7-2 | 持仓检查循环 (每 60s) | P0 | ✅ 完成 |
| R7-3 | 指标日志循环 (每 60s) | P0 | ✅ 完成 |
| R7-4 | 结算时间刷新 (每 30min) | P0 | ✅ 完成 |
| R7-5 | 状态清理循环 (每 5min) | P0 | ✅ 完成 |
| R7-6 | WS 任务崩溃检测 → 自动 L3 | P0 | ✅ 完成 |
| R7-7 | 优雅关机 (Ctrl+C → cancel all) | P0 | ✅ 完成 |
| R7-8 | 启动时加载初始持仓和订单 | P0 | ✅ 完成 |
| R7-9 | 重报价条件: 价格变动 > threshold || 定时 interval | P0 | ✅ 完成 |
| R7-10 | WS 断连保护 (>10s 跳过下单) | P0 | ✅ 完成 |
| R7-11 | WS 连接前等待 (不使用默认 midpoint) | P0 | ✅ 完成 |

### R8: 监控告警 (monitoring)

| ID | 需求 | 优先级 | 状态 |
|----|------|--------|------|
| R8-1 | 结构化日志 (tracing, DEBUG/INFO/WARN/ERROR) | P0 | ✅ 完成 |
| R8-2 | 关键指标周期日志 (风险等级/活跃订单/PnL/填充数) | P0 | ✅ 完成 |
| R8-3 | 每市场指标日志 (midpoint/spread/IIR/shares) | P0 | ✅ 完成 |
| R8-4 | Telegram Bot 告警 | P2 | ⬜ 延后 |

## 4. 非功能需求

| ID | 需求 | 状态 |
|----|------|------|
| NF-1 | 24h 连续运行不崩溃 | ✅ 架构支持 |
| NF-2 | 密钥安全 (zeroize, 不在日志中打印) | ✅ 完成 |
| NF-3 | ARM 内存序正确性 (Ordering::Acquire/Release) | ✅ 完成 |
| NF-4 | 无死锁 (锁获取顺序文档化) | ✅ 完成 |
| NF-5 | 无 unbounded 内存增长 (所有集合有容量限制) | ✅ 完成 |

## 5. 约束条件

| 约束 | 值 |
|------|-----|
| 批量下单上限 | 15 单/批 |
| API 限流 | 3000 次/10分钟 |
| Q-Score 采样 | 每分钟 1 次 |
| WS Keepalive | 8-10s PING (SDK 内部处理) |
| 最小 Merge 规模 | $100 |
| 价格范围 | [0.01, 0.99] |
| 奖励结算 | 每日 UTC 00:00 |

## 6. MVP 范围外 (延后)

- Web UI / Dashboard
- 自动市场发现/评分
- 多策略支持
- 分布式部署
- 数据库持久化 (SQLite/PostgreSQL)
- Telegram 告警集成
- API 速率限制精确追踪

## 7. 成功标准

1. 能连接 Polymarket WebSocket 并维持稳定连接
2. 能在 1-3 个市场同时维护阶梯挂单
3. Q-Score 估算值持续大于 0
4. L1/L2/L3 风控状态机正确响应各类触发条件
5. Ghost Fills 检测能识别异常取消事件
6. 系统在 24h 连续运行中不崩溃
7. 所有模块测试覆盖率 ≥ 80%
