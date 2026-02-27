# 数据层：信息从哪来，怎么用

> 数据层是整个系统的"感觉器官"。
> 策略再好，如果拿到的是过时的价格，挂出的单就是错的。

---

## 1. 两类数据源的本质区别

### 1.1 为什么要用 WebSocket 而不是 REST？

```
REST（轮询）的工作方式：
  你发请求 → 服务器返回当前状态 → 你处理 → 等一会儿 → 你发下一个请求
  问题：
    每次请求有网络往返延迟（50-200ms）
    两次请求之间可能错过多次价格变化
    频繁轮询会消耗 API 配额

WebSocket（长连接推送）的工作方式：
  建立连接 → 服务器有更新就立刻推送 → 你持续接收
  优势：
    延迟极低（推送 vs 主动拉取）
    不遗漏任何价格变化
    一个连接处理所有市场数据
    API 配额消耗极少

结论：实时数据必须用 WebSocket。REST 只用于不经常变化的数据。
```

### 1.2 两类数据的使用场景

```
WebSocket（实时推送）：
  ├─ 订单簿快照（book）
  ├─ 价格变化（price_change）
  ├─ 成交记录（trade）
  └─ 我的订单状态（order_update）

REST（定时轮询）：
  ├─ 市场列表和元数据（每 5 分钟）
  ├─ 奖励配置（每小时）
  ├─ 账户持仓（每分钟，用于对账）
  └─ 历史成交（每日统计）
```

---

## 2. WebSocket 连接详解

### 2.1 两个独立的 WebSocket 频道

```
Polymarket 有两个 WebSocket 端点，必须同时维护：

频道1：公开市场数据
  URL: wss://ws-subscriptions-clob.polymarket.com/ws/market

  订阅消息格式：
    { "type": "subscribe",
      "channel": "market",
      "markets": ["YES_token_id_1", "YES_token_id_2", ...] }

  接收的事件类型：
    book        → 订单簿快照（连接时或重连时发送）
    price_change → 中间价变化
    trade       → 成交事件

频道2：私有用户数据（需要认证）
  URL: wss://ws-subscriptions-clob.polymarket.com/ws/user

  认证方式：
    连接时在请求头带上 API Key + L2 Auth（EIP-712 签名）

  订阅消息格式：
    { "type": "subscribe",
      "channel": "user",
      "markets": ["YES_token_id_1", ...],
      "auth": { "apiKey": "...", "secret": "...", "passphrase": "..." } }

  接收的事件类型：
    order_update → 我的订单创建、成交、取消等状态变化
```

### 2.2 WebSocket 保活机制

```
Polymarket WebSocket 连接如果 10 秒没有消息，会被断开

保活方法：主动发送 PING
  每 8 秒发送一次：{ "type": "PING" }
  服务器回复：{ "type": "PONG" }

如果收不到 PONG（超过 15 秒）：
  说明连接可能已断开
  触发重连逻辑

保活代码示意：
  def start_keepalive():
      while connected:
          send({ "type": "PING" })
          sleep(8)
          if last_pong_time < now() - 15:
              reconnect()
```

### 2.3 断线重连策略（指数退避）

```
原因：网络不可避免地会有波动，必须能自动重连

指数退避算法：
  attempt 1：等 1 秒再重连
  attempt 2：等 2 秒再重连
  attempt 3：等 4 秒再重连
  attempt 4：等 8 秒再重连
  ...
  最多等待 30 秒（不再延长）

为什么用指数退避？
  立即重连：如果服务器在重启，你的大量重连请求会加重负担
  等太久：错过太多价格更新，挂单失效

重连后必须做的事：
  Step 1: 重新发送订阅消息（订阅信息不会被保留）
  Step 2: 拉取最新的订单簿快照（GET /book）补齐断线期间的数据
  Step 3: 检查是否有订单状态变化（GET /orders/active）
  Step 4: 触发一次完整的挂单更新（断线期间可能有价格变化）

代码结构：
  def reconnect():
      delay = 1
      while not connected:
          try:
              connect_websocket()
              resubscribe_all_markets()
              sync_state_after_reconnect()
              break
          except:
              sleep(min(delay, 30))
              delay *= 2
```

---

## 3. 关键数据结构

### 3.1 订单簿快照（book 事件）

```
收到的 book 事件格式：
{
  "type": "book",
  "market": "condition_id",
  "asset_id": "YES_token_id",
  "hash": "abc123",
  "timestamp": "2024-01-01T00:00:00Z",
  "bids": [
    { "price": "0.64", "size": "800.00" },
    { "price": "0.62", "size": "1500.00" },
    { "price": "0.60", "size": "3000.00" }
  ],
  "asks": [
    { "price": "0.68", "size": "500.00" },
    { "price": "0.70", "size": "1200.00" },
    { "price": "0.72", "size": "2000.00" }
  ]
}

解析后的关键数据：
  best_bid = max(bid.price for bid in bids) = 0.64
  best_ask = min(ask.price for ask in asks) = 0.68
  adjusted_midpoint ≈ (0.64 + 0.68) / 2 = 0.66
  spread = 0.68 - 0.64 = 0.04（4 cents）
```

### 3.2 价格变化事件（price_change 事件）

```
{
  "type": "price_change",
  "asset_id": "YES_token_id",
  "market": "condition_id",
  "price": "0.67",
  "side": "BUY",
  "size": "50.00",
  "timestamp": "..."
}

这个事件比 book 快照更轻量，表示"刚发生了一笔成交，导致价格变化"
  price = 成交价格
  side = BUY/SELL（成交方向）
  size = 成交量

收到 price_change 后：
  更新本地的 adjusted_midpoint 估计
  检查是否触发挂单更新阈值（|新中间价 - 上次更新时中间价| > 0.005）
```

### 3.3 我的订单更新（order_update 事件）

```
{
  "type": "order_update",
  "id": "order_uuid",
  "market": "condition_id",
  "asset_id": "YES_token_id",
  "side": "BUY",
  "price": "0.49",
  "original_size": "200.00",
  "size_matched": "100.00",      ← 已成交数量
  "size_remaining": "100.00",    ← 剩余未成交
  "status": "PARTIALLY_FILLED",  ← 状态
  "type": "GTC",
  "timestamp": "..."
}

订单状态类型：
  LIVE         → 挂单中，等待成交
  PARTIALLY_FILLED → 部分成交，还有剩余
  MATCHED      → 完全成交（等待链上确认）
  CONFIRMED    → 链上确认完成
  CANCELED     → 已取消

注意 CANCELED 的两种情况：
  1. 你主动发了取消请求 → 正常
  2. 你没有取消但收到 CANCELED → Ghost Fill 攻击！（见风控文档）
```

---

## 4. REST API 接口汇总

### 4.1 Gamma API（元数据）

```
基础 URL：https://gamma-api.polymarket.com

GET /markets
  获取所有市场列表
  关键字段：
    id                      → 市场唯一标识（condition_id）
    question                → 市场问题文本
    endDate                 → 结算时间
    active                  → 是否仍然活跃
    volume                  → 历史总成交量
    volume24hr              → 24小时成交量
    rewards.minSize         → 最小合格挂单规模
    rewards.maxSpread       → max_incentive_spread
    rewards.dailyRewardUsdAmount → 日奖励 USD 金额
    clobTokenIds            → [YES_token_id, NO_token_id]

使用方法：
  每 5 分钟调用一次，更新市场元数据缓存
  重点关注 rewards 字段变化（奖励调整可能影响密度计算）
```

### 4.2 CLOB API（交易操作）

```
基础 URL：https://clob.polymarket.com

【无需认证的接口】
GET /book?token_id={YES_token_id}
  获取订单簿快照
  返回格式同 WebSocket book 事件

GET /trades?token_id={YES_token_id}&limit=100
  获取最近成交记录

GET /markets/{condition_id}
  获取单个市场详情

【需要认证的接口】
GET /positions
  获取当前所有持仓
  用于系统启动时恢复状态，或定期对账

POST /order
  下单接口
  支持批量下单（最多15个）
  请求体：
    { "orders": [
        { "token_id": "...",
          "price": "0.49",
          "side": "BUY",
          "size": "100",
          "type": "GTC",           ← Good Till Canceled
          "feeRateBps": "0",
          "nonce": 12345,
          "expiration": 0,
          "signature": "0x..." }   ← EIP-712 签名（必须）
      ]
    }

DELETE /cancel-market-orders
  取消某市场所有挂单
  请求体：{ "market": "condition_id" }

DELETE /cancel-all
  取消所有市场所有挂单（L3 紧急操作使用）
```

### 4.3 认证机制

```
Polymarket 使用双重认证：

L1 认证（API 请求认证）：
  每个请求头带上：
    POLY_ADDRESS   → 你的钱包地址
    POLY_SIGNATURE → 请求的签名
    POLY_TIMESTAMP → 当前时间戳

L2 认证（EIP-712 签名）：
  每笔订单必须用私钥签名（证明是你的订单）
  签名算法：EIP-712（以太坊标准签名方式）
  签名的内容：订单的所有关键字段（价格、数量、方向等）

  为什么需要签名？
  Polymarket 是去中心化的，链上结算需要验证订单真实性
  签名后的订单不能被他人伪造或篡改

工具：
  py-clob-client（Python SDK）自动处理签名
  或者使用 web3.py 手动实现 EIP-712 签名
```

---

## 5. 本地状态缓存（内存）

### 5.1 三类核心状态

```
状态1：市场状态（market_states）
  {
    "YES_token_id": {
      "midpoint": 0.50,          ← 当前中间价
      "best_bid": 0.49,          ← 最优买价
      "best_ask": 0.51,          ← 最优卖价
      "spread": 0.02,            ← 当前价差
      "volume_24h": 125000,      ← 24小时成交量
      "last_updated": timestamp, ← 最后更新时间
      "orderbook": {             ← 完整订单簿快照
        "bids": [...],
        "asks": [...]
      }
    }
  }

状态2：我的订单（my_orders）
  {
    "order_uuid": {
      "market": "condition_id",
      "asset_id": "YES_token_id",
      "price": 0.49,
      "size": 200,
      "remaining": 100,
      "side": "BUY",
      "status": "PARTIALLY_FILLED",
      "created_at": timestamp
    }
  }

状态3：持仓（positions）
  {
    "condition_id": {
      "yes_shares": 300,         ← 持有 YES 代币数量
      "no_shares": 200,          ← 持有 NO 代币数量
      "yes_avg_cost": 0.48,      ← YES 平均成本
      "no_avg_cost": 0.54,       ← NO 平均成本
      "iir": 0.053               ← 当前持仓失衡比率（实时计算）
    }
  }
```

### 5.2 状态更新逻辑

```
WebSocket 事件 → 状态更新规则：

book 事件：
  market_states[asset_id].orderbook = event.orderbook
  market_states[asset_id].best_bid = max(event.bids, key=price)
  market_states[asset_id].best_ask = min(event.asks, key=price)
  market_states[asset_id].midpoint = (best_bid + best_ask) / 2

price_change 事件：
  # 轻量更新，只更新中间价估计
  market_states[asset_id].midpoint = (event.price + market_states[asset_id].midpoint) / 2
  # 注意：这只是估计，以 book 快照为准

order_update 事件：
  my_orders[event.id].status = event.status
  my_orders[event.id].remaining = event.size_remaining

  if event.status == "MATCHED" or event.status == "CONFIRMED":
      # 有订单成交，更新持仓
      update_position(event.market, event.side, event.size_matched, event.price)

  if event.status == "CANCELED":
      # 检查是否是我自己发的取消
      if event.id not in my_cancel_requests:
          # Ghost Fill！
          trigger_ghost_fill_alert(event.id)
```

---

## 6. 历史数据存储（数据库）

### 6.1 需要持久化的数据

```
为什么需要数据库？
  内存状态重启后丢失 → 需要恢复持仓和订单
  PnL 计算需要历史价格数据
  奖励统计需要历史记录
  风控分析需要价格历史

建议使用 SQLite（单文件，无需服务器，够用）：

表结构1：价格历史
  CREATE TABLE price_history (
      id          INTEGER PRIMARY KEY,
      market_id   TEXT,
      token_id    TEXT,
      midpoint    REAL,
      best_bid    REAL,
      best_ask    REAL,
      spread      REAL,
      recorded_at TIMESTAMP
  );

表结构2：我的成交记录
  CREATE TABLE trades (
      id          INTEGER PRIMARY KEY,
      order_id    TEXT,
      market_id   TEXT,
      side        TEXT,         -- BUY/SELL
      price       REAL,
      size        REAL,
      fee         REAL,
      executed_at TIMESTAMP
  );

表结构3：PnL 记录（每小时快照）
  CREATE TABLE pnl_snapshots (
      id                INTEGER PRIMARY KEY,
      snapshot_time     TIMESTAMP,
      realized_pnl      REAL,    -- 已实现 PnL
      unrealized_pnl    REAL,    -- 未实现 PnL
      total_rewards     REAL,    -- 累计奖励
      gas_costs         REAL,    -- Gas 成本
      net_pnl           REAL     -- 净 PnL
  );

表结构4：市场元数据缓存
  CREATE TABLE markets (
      condition_id        TEXT PRIMARY KEY,
      question            TEXT,
      yes_token_id        TEXT,
      end_date            TIMESTAMP,
      max_spread          REAL,
      min_size            REAL,
      daily_reward_usd    REAL,
      last_fetched        TIMESTAMP
  );
```

### 6.2 系统启动时的状态恢复

```
系统重启后，需要恢复到上次的状态：

Step 1: 从数据库读取最新的持仓记录
  positions = db.query("SELECT * FROM positions ORDER BY id DESC LIMIT 1")

Step 2: 从 CLOB API 获取当前实际持仓
  api_positions = clob_api.get_positions()

Step 3: 比较本地记录和 API 实际值
  for market in api_positions:
      local = positions.get(market.condition_id)
      if local is None or local != api_positions:
          log.warning(f"持仓不匹配，使用 API 值: {market}")
          positions[market.condition_id] = api_positions[market]

Step 4: 从 CLOB API 获取当前活跃订单
  active_orders = clob_api.get_active_orders()
  my_orders = {order.id: order for order in active_orders}

Step 5: 恢复市场状态（从 WebSocket 或 REST 初始化）
  for market in active_markets:
      orderbook = clob_api.get_book(market.yes_token_id)
      market_states[market.yes_token_id] = parse_orderbook(orderbook)

完成后系统处于一致状态，可以开始正常做市
```

---

## 7. 数据质量校验

### 7.1 常见数据异常及处理

```
异常1：订单簿两侧都为空
  情况：market_states[id].best_bid is None
  处理：使用最近一次有效中间价，标记该市场为"数据缺失"
        暂停该市场做市，等待数据恢复

异常2：中间价跳跃超过 20%（可能是错误数据）
  检测：|new_midpoint - old_midpoint| > 0.2
  处理：
    丢弃这条数据，保持旧值
    记录告警日志
    如果连续 3 次跳跃，升级到 L2 风控处理

异常3：WebSocket 消息时间戳远远落后
  检测：event.timestamp < now() - 60 秒
  处理：可能是 WebSocket 积压，请求重新同步
        发送订阅消息，服务器会重新推送最新快照

异常4：API 请求连续失败
  HTTP 429（限流）：等待 60 秒后重试
  HTTP 5xx（服务器错误）：指数退避，最多重试 5 次
  连续 5 次失败：触发 L2 预警（API 不可用）
```

---

## 8. 小结

| 数据类型 | 来源 | 更新频率 | 用途 |
|---------|------|---------|------|
| 实时价格 | WebSocket market | 事件驱动（毫秒级） | 触发挂单更新 |
| 订单状态 | WebSocket user | 事件驱动 | 持仓跟踪、Ghost Fill 检测 |
| 市场元数据 | Gamma REST | 每 5 分钟 | 激励密度计算、市场筛选 |
| 账户持仓 | CLOB REST | 每分钟（对账） | 启动恢复、对账 |
| 价格历史 | SQLite 本地 | 每分钟存储 | PnL 计算、波动率估算 |
