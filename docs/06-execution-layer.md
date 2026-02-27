# 执行层：订单生命周期与链上操作

> 策略引擎告诉你"该怎么做"，执行层负责把这个意图可靠地转化为实际操作。
> 执行层的核心挑战：网络不可靠，API 有限流，链上操作不可逆。

---

## 1. 订单的完整生命周期

### 1.1 从策略决策到链上确认

```
策略引擎                           执行层                        Polymarket
    │                                 │                              │
    │  OrderSpec                       │                              │
    │  { price: 0.49,                  │                              │
    │    size: 200,                    │                              │
    │    side: BUY,                    │                              │
    │    market: "abc" }               │                              │
    │                                  │                              │
    ├──────────────────────────────────▶ 1. EIP-712 签名             │
    │                                  │    生成 order_signature     │
    │                                  │                              │
    │                                  ├──────────────────────────────▶ POST /order
    │                                  │                              │
    │                                  │                              │  撮合引擎
    │                                  │                              │  检查合法性
    │                                  │                              │  加入订单簿
    │                                  │                              │
    │                                  │                    order_id ◀─┤
    │                                  │                              │
    │                                  ├─ 记录 order_id 到本地状态    │
    │                                  │                              │
    │              WebSocket user channel                             │
    │                    order_update: LIVE ◀─────────────────────────┤
    │                                  │                              │
    │                                  │      （等待成交...）           │
    │                                  │                              │
    │                    order_update: MATCHED ◀──────────────────────┤
    │                                  │  （链下撮合完成，等链上确认）   │
    │                                  │                              │
    │                    order_update: CONFIRMED ◀────────────────────┤
    │                                  │  （Polygon 链上确认完成）      │
    │                                  │                              │
    │                                  ├─ 更新本地持仓记录              │
    │                                  ├─ 触发持仓检查（IIR 更新）      │
    │                                  │                              │
```

### 1.2 订单状态机

```
              创建请求
                 │
                 ▼
            PENDING_SUBMIT
            （本地创建，等待提交）
                 │
        POST /order 成功
                 │
                 ▼
              LIVE
         （在订单簿中挂着）
         ╱              ╲
    被成交              主动取消/被强制取消
        │                     │
        ▼                     ▼
    MATCHED                CANCELED
（链下撮合成功，等链上）      （订单移出订单簿）
        │
        ▼
   CONFIRMED
  （链上确认，最终状态）

特殊分支：
  PARTIALLY_FILLED → 部分成交，剩余在 LIVE 状态
  提交失败（HTTP 错误）→ FAILED → 重试或放弃
```

---

## 2. "撤旧立新"模式：为什么没有"改单"

### 2.1 为什么 Polymarket 不提供改单接口

```
理论上"改单"= 取消旧单 + 下新单
Polymarket 直接不提供改单接口，你必须自己做两步操作

原因（推测）：
  简化链下撮合引擎的逻辑
  避免"改单瞬间"存在订单簿空缺

对做市商的影响：
  每次价格更新都需要两步操作（取消 + 下新单）
  两步之间有时间窗口（通常 100-500ms）
  在这个窗口内，你在这个市场没有任何挂单（Q-Score 短暂为零）
  → 频繁更新会累积大量"空窗期"，损失 Q-Score

优化策略：
  不要为每个细小的价格变动都更新
  只在价格移动超过阈值（0.5 cents）时更新
  这样平衡了"价格准确性"和"Q-Score 损失"
```

### 2.2 完整的更新流程

```
def update_market_orders(market_id, new_orders_spec):

    # Step 1: 取消旧单（批量操作，快）
    cancel_response = api.delete_market_orders(market_id)
    if not cancel_response.ok:
        log.error(f"取消失败: {cancel_response.status_code}")
        return  # 不继续，避免双重下单

    # Step 2: 记录正在取消的订单
    for order_id in my_active_orders[market_id]:
        my_cancel_requests.add(order_id)

    # Step 3: 等待取消确认（通过 WebSocket 或超时）
    wait_for_cancel_confirmation(market_id, timeout=5)

    # Step 4: 计算新挂单
    new_orders = generate_ladder_orders(
        midpoint=market_states[market_id].midpoint,
        vaf=compute_vaf(market_id),
        iir=compute_iir(market_id),
        skew_factor=0.02
    )

    # Step 5: 批量提交新挂单（每批最多 15 个）
    all_order_ids = []
    for batch in chunks(new_orders, 15):
        signed_orders = [sign_order(o) for o in batch]
        response = api.post_orders(signed_orders)
        if response.ok:
            all_order_ids.extend(response.order_ids)
        else:
            log.error(f"下单失败: {response.status_code}")
            # 部分失败的处理：记录哪些失败了，下次补充

    # Step 6: 更新本地订单状态
    my_active_orders[market_id] = all_order_ids
    last_update_time[market_id] = now()
```

---

## 3. 批量操作：提高效率，节省 API 配额

### 3.1 批量下单的上限和处理

```
Polymarket 限制：
  每次 POST /order 最多 15 个订单

多市场同时更新的例子：
  6 个市场，每市场 6 个挂单（双侧 3 层 × 2）
  总订单数 = 36 个
  需要 ceil(36/15) = 3 次 POST 请求

批量处理策略：

  方案A：顺序批量（简单）
    发送 batch1 → 等待响应 → 发送 batch2 → ...
    慢，但易于处理失败

  方案B：并行批量（快）
    同时发送所有 batch
    快，但需要处理部分失败的情况

  推荐：方案B + 失败重试
    并行发送，任何批次失败就放入重试队列
    重试队列在下一个 10 秒周期处理

批量取消：
  DELETE /cancel-market-orders：一次性取消整个市场（不受 15 限制）
  DELETE /cancel-all：一次性取消所有市场所有订单（紧急操作）
```

### 3.2 API 限流的处理

```
Polymarket 限制：3000 次 / 10 分钟滑动窗口

监控和控制：
  维护一个本地计数器：
    request_times = deque()  # 最近的请求时间队列

  每次发送请求前检查：
    # 清理超过 10 分钟的记录
    cutoff = now() - 600
    while request_times and request_times[0] < cutoff:
        request_times.popleft()

    # 检查是否超限
    if len(request_times) >= 2800:  # 留 200 的缓冲
        wait_time = request_times[0] + 600 - now()
        sleep(wait_time)

    # 发送请求并记录
    request_times.append(now())
    return api.send_request(...)

正常使用情况下（6 市场，每 30 秒更新一次）：
  每次更新：取消 6 次 + 下单 ceil(36/15) = 3 次 = 9 次请求
  每分钟：9 × 2 = 18 次请求
  10 分钟：180 次请求（3000 限制的 6%）
  → 安全，有大量余量应对突发更新
```

---

## 4. EIP-712 签名：证明订单是你的

### 4.1 为什么需要签名

```
Polymarket 是去中心化交易所，链上结算需要验证订单真实性：
  没有签名：任何人都可以以你的名义下单（安全风险）
  有签名：私钥签名证明这个订单确实来自你

EIP-712 是以太坊标准签名方式（不同于简单的 keccak256 签名）：
  结构化数据签名：对订单的具体字段进行签名，而不是原始字节
  更安全：防止签名被复用到其他场景

签名需要的内容：
  私钥（存在 .env 文件中，永远不要泄露）
  待签名的订单字段（价格、数量、方向、nonce 等）
```

### 4.2 订单签名流程

```
订单结构（以 Python 为例，使用 py-clob-client）：

from py_clob_client.order_builder.constants import BUY, SELL

order_args = MarketOrderArgs(
    token_id = "YES_token_id",    # 要交易的代币
    amount   = 200,               # 规模（USDC）
)

# 或者限价单
order_args = LimitOrderArgs(
    token_id = "YES_token_id",
    price    = 0.49,              # 挂单价格（0-1之间）
    side     = BUY,               # BUY 或 SELL
    size     = 200,               # USDC 规模
)

# 创建并签名（SDK 自动处理 EIP-712 签名）
signed_order = client.create_order(order_args)

# 提交
resp = client.post_order(signed_order)
```

### 4.3 签名的安全注意事项

```
私钥安全：
  永远不要把私钥写入代码
  使用 .env 文件存储：PRIVATE_KEY=0x...
  确保 .env 不被提交到 git（.gitignore 中排除）

Nonce 管理：
  EIP-712 签名包含 nonce，防止重放攻击
  每笔订单的 nonce 必须唯一
  通常用时间戳或自增序号

签名验证：
  提交后，CLOB API 会验证签名
  验证失败返回 400 错误
  原因可能是：私钥错误、时间戳过期、nonce 重复
```

---

## 5. Merge 操作：链上 CTF 合约调用

### 5.1 CTF 合约简介

```
CTF = Conditional Token Framework（条件代币框架）
Polymarket 的 YES/NO 代币都是 CTF 合约生成的

Merge 操作调用 CTF 合约的 mergePositions 函数：
  将 1 YES + 1 NO → 换回 1 USDC
  这是链上操作（写入 Polygon 区块链）

合约地址（Polygon 主网）：
  CTF合约：0x4D97DCd97eC945f40cF65F87097ACe5EA0476045
  USDC合约：0x2791Bca1f2de4661ED88A30C99A7a9449Aa84174

Gas 费用：
  Polygon 的 Gas 非常便宜（< $0.01 per 交易）
  Merge 是写入操作，Gas 比读取贵，但仍然很低
  建议只在 merge 规模 > $100 时执行（避免 Gas 成本占比过高）
```

### 5.2 Merge 的调用方式

```
方法A：使用 poly_merger 工具（官方推荐）
  这是 Polymarket 官方提供的 Node.js 命令行工具
  适合手动操作或脚本调用

  安装：npm install -g poly-merger
  使用：
    poly-merger merge \
      --condition-id "0x..." \
      --amount 200 \
      --private-key "0x..." \
      --rpc-url "https://polygon-rpc.com"

方法B：使用 web3.py 直接调用合约
  适合集成到 Python 后端

  from web3 import Web3

  w3 = Web3(Web3.HTTPProvider("https://polygon-rpc.com"))
  ctf = w3.eth.contract(address=CTF_ADDRESS, abi=CTF_ABI)

  # 构建 mergePositions 调用
  tx = ctf.functions.mergePositions(
      collateral    = USDC_ADDRESS,
      parentCollection = bytes(32),       # 0x000...
      conditionId   = bytes.fromhex(condition_id),
      partition     = [1, 2],             # YES=1, NO=2
      amount        = Web3.to_wei(200, 'ether')  # 200 个代币（18位小数）
  ).build_transaction({
      'from': my_address,
      'nonce': w3.eth.get_transaction_count(my_address),
      'gas': 200000,
      'gasPrice': w3.eth.gas_price
  })

  # 签名并发送
  signed_tx = w3.eth.account.sign_transaction(tx, private_key)
  tx_hash = w3.eth.send_raw_transaction(signed_tx.rawTransaction)

  # 等待确认（Polygon 通常 2-3 秒）
  receipt = w3.eth.wait_for_transaction_receipt(tx_hash, timeout=30)
  if receipt.status == 1:
      log.info(f"Merge 成功: {tx_hash.hex()}")
  else:
      log.error(f"Merge 失败: {tx_hash.hex()}")
```

### 5.3 Merge 操作的幂等性

```
问题：如果 Merge 交易发送后，等待超时，不知道是否成功，该怎么办？

正确处理：
  Step 1: 发送交易，记录 tx_hash
  Step 2: 等待 30 秒
  Step 3: 如果超时，查询 tx_hash 的状态（不是重发）
    w3.eth.get_transaction_receipt(tx_hash)
    如果返回 None：交易还在 mempool，继续等
    如果返回 status=1：成功，更新本地状态
    如果返回 status=0：失败，分析原因

  永远不要因为超时就重发相同的交易！
  重发会导致：
    如果第一笔成功了 → 两笔都执行，Double Merge
    导致账户 YES/NO 变成负数（合约会 revert）

  正确做法：始终先查询 tx_hash 状态，再决定是否重试
```

---

## 6. 重试机制：应对不可避免的失败

### 6.1 指数退避重试

```
网络请求失败时的重试逻辑：

def api_request_with_retry(request_func, max_retries=3):
    delay = 1  # 初始等待 1 秒

    for attempt in range(max_retries):
        try:
            response = request_func()

            if response.status_code == 200:
                return response

            elif response.status_code == 429:
                # 限流，等更长时间
                retry_after = int(response.headers.get("Retry-After", 60))
                log.warning(f"API限流，等待 {retry_after} 秒")
                sleep(retry_after)

            elif response.status_code in [500, 502, 503]:
                # 服务器错误，可以重试
                log.warning(f"服务器错误 {response.status_code}，重试 {attempt+1}/{max_retries}")
                sleep(delay)
                delay = min(delay * 2, 30)  # 指数退避，最多 30 秒

            else:
                # 其他错误（4xx），不重试
                log.error(f"请求失败 {response.status_code}: {response.text}")
                return response  # 返回错误响应，由调用方处理

        except Exception as e:
            log.error(f"网络异常: {e}")
            sleep(delay)
            delay = min(delay * 2, 30)

    log.error(f"请求失败，已重试 {max_retries} 次")
    return None
```

### 6.2 哪些操作应该重试，哪些不应该

```
【应该重试】：
  GET 请求（获取数据）：完全幂等，任何错误都可以重试
  下单请求（返回 5xx）：服务器问题，可以重试
  取消请求（返回 5xx）：服务器问题，可以重试

【谨慎重试】：
  下单请求（返回 4xx）：可能是订单参数问题
    如果是 400 Bad Request：检查参数，不要盲目重试
    如果是 401 Unauthorized：认证问题，重新认证后再试

【不应该重试（需要人工确认）】：
  链上 Merge 交易：先查 tx_hash 状态，不要盲目重发
  批量取消（cancel-all）：在 L3 场景下，要确认是否已取消

【需要放弃的情况】：
  下单：价格已经超出 max_spread → 不重试，重新计算价格
  Merge：余额不足 → 不重试，先解决余额问题
```

---

## 7. 执行层的监控指标

### 7.1 关键指标

```
每分钟监控：
  active_orders_count      → 当前活跃挂单数（应该和预期一致）
  order_update_lag         → 订单更新到 WebSocket 通知的延迟
  api_call_count_10min     → 最近 10 分钟 API 调用次数（接近 3000 要告警）
  cancel_fail_count        → 取消失败次数（不应该频繁失败）
  submit_fail_count        → 下单失败次数
  merge_success_count      → 当天 Merge 成功次数

告警阈值：
  active_orders_count < 预期 * 0.8 → 有订单丢失
  api_call_count_10min > 2700     → 接近限流
  submit_fail_count > 5/分钟      → API 异常
  WebSocket 事件延迟 > 5 秒        → 连接可能有问题
```

### 7.2 订单一致性检查

```
每 5 分钟运行一次：

def verify_order_consistency():
    # 从 API 获取实际活跃订单
    api_orders = clob_api.get_active_orders()
    api_order_set = {o.id for o in api_orders}

    # 本地记录的活跃订单
    local_order_set = set(my_active_orders.values())

    # 检查差异
    only_in_api = api_order_set - local_order_set
    only_in_local = local_order_set - api_order_set

    if only_in_api:
        # API 有，本地没有 → 可能是上次重连后漏记了
        log.warning(f"发现未知订单: {only_in_api}")
        # 取消这些未知订单（保险起见）

    if only_in_local:
        # 本地有，API 没有 → 订单已经被取消但没收到 WebSocket 通知
        log.warning(f"本地订单不存在于API: {only_in_local}")
        # 从本地状态中移除这些订单

一致性检查是数据质量的最后防线
```

---

## 8. 小结：执行层的设计原则

| 原则 | 说明 |
|------|------|
| **撤旧立新** | 没有改单接口，每次价格更新都是取消+重建 |
| **批量优先** | 最多 15 个订单一批，减少 API 调用次数 |
| **签名安全** | 私钥只在本地，永远不上传；每笔订单单独签名 |
| **重试有度** | 只重试可重试的操作；链上操作先查状态再决定 |
| **一致性验证** | 定期对比本地状态和 API 状态，发现并修复差异 |
| **限流感知** | 监控 API 使用量，在接近限制时自动降频 |
| **幂等设计** | 所有操作要考虑"如果执行两次会怎样"，设计幂等保护 |
