# Polymarket LP 做市系统

Polymarket 预测市场的自动化做市（Market Making）系统，基于 Q-Score 奖励机制设计，兼顾持仓安全与奖励最大化。

---

## 文档目录

> 先读 `00-system-overview`，再按序阅读各模块文档。

| 文档 | 内容概要 |
|------|---------|
| [00-system-overview](./docs/00-system-overview.md) | 系统架构、完整数据流图、启动顺序、技术约束 |
| [01-data-layer](./docs/01-data-layer.md) | WebSocket 实时推送、REST 轮询、断线重连、本地状态缓存 |
| [02-qscore-rewards](./docs/02-qscore-rewards.md) | Q-Score 公式推导、二次衰减、双边 3 倍效应、激励密度 |
| [03-pricing-engine](./docs/03-pricing-engine.md) | 基础价差计算、VAF/IIF/TF 三因子动态调整、阶梯挂单设计 |
| [04-position-management](./docs/04-position-management.md) | IIR 持仓失衡度、Quote Skewing、Merge 完整流程与决策树 |
| [05-risk-control](./docs/05-risk-control.md) | L1/L2/L3 三级状态机、Ghost Fills 攻击检测、恢复流程 |
| [06-execution-layer](./docs/06-execution-layer.md) | 订单生命周期、EIP-712 签名、批量操作、链上 Merge 调用 |

---

## 核心设计亮点

### 1. Q-Score 二次衰减：内层小单才是核心资产

```
距中间价     得分效率
0.0 cents   100%
1.0 cents    44%   ← 移动 1 cent，损失 56%
2.0 cents    11%   ← 再移动 1 cent，损失 75%
3.0 cents     0%   （超出激励范围）
```

奖励随距离平方级下降，主力资金必须集中在最靠近中间价的位置。

### 2. 双边挂单 = 3 倍奖励

平台对单边挂单执行 ÷3 惩罚：

```
$200 全部买单（单边）→ Q-Score × 1/3 ≈ 26.7 分
$100 买单 + $100 卖单（双边）→ Q-Score × 1  ≈ 80.2 分
```

相同资金，双边策略奖励是单边的 **3 倍**。

### 3. 阶梯挂单：奖励与保护兼顾

```
内层（0.5 cent）$100 × 2  →  贡献 75% 的 Q-Score
中层（1.5 cents）$200 × 2  →  贡献 20%
外层（2.5 cents）$200 × 2  →  贡献  5%（主要目的是缓冲极端行情）
```

外层大单不是为了奖励，而是给风控系统争取**撤单反应时间**。

### 4. Quote Skewing：让市场帮你调仓

持仓失衡时，不直接市价卖出，而是通过调整报价引导市场自然平衡：

```
IIR = +0.6（持有过多 YES）
正常报价：bid@0.59，ask@0.61
偏斜后：  bid@0.578，ask@0.598
→ 卖出 YES 的概率增大，买入概率减小，零滑点调仓
```

### 5. Merge 优先于市价卖出

当同时持有 YES 和 NO 时，1 YES + 1 NO = 1 USDC（链上确定性，无滑点）：

```
持有 YES=300，NO=200
→ Merge 200 对 → 收回 $200 USDC，剩余 YES=100
→ 比市价卖出 200 YES 零损耗
```

### 6. 三级风控状态机

```
L1（正常）→ 全自动，持续做市
    ↓ IIR > 0.5 / 5分钟价格跳动 > 10% / 日亏 > 3%
L2（预警）→ 规模收缩 50%，通知人工，可自动恢复
    ↓ IIR > 0.75 / 价格跳动 > 20% / 日亏 > 8%
L3（紧急）→ cancel-all，停止做市，必须人工恢复
```

L3 → 恢复**必须人工确认**，防止系统在不安全状态下自动重启。

### 7. Ghost Fills 攻击防护

监控订单取消事件，区分"我主动取消"和"被外部取消"：

```python
if event.type == "CANCELED" and order_id not in my_cancel_requests:
    # 我没有发取消指令，但订单被取消 → Ghost Fill 攻击
    trigger_l3_emergency()
```

---

## 盈利逻辑

```
盈利来源              典型占比    风险等级
────────────────────────────────────────
LP 奖励（Q-Score）    60–70%      低
做市价差（Spread）    20–30%      中
方向性持仓             5–15%      高
```

**LP 奖励是支柱**：只要合规挂单，每分钟采样一次，每日结算——是相对稳定的收入来源。

---

## 技术约束

| 约束 | 数值 |
|------|------|
| 批量下单上限 | 15 个/批 |
| API 限流 | 3000 次/10 分钟 |
| Q-Score 采样频率 | 每分钟 1 次（1440 次/天） |
| WebSocket 保活 | 每 8 秒 PING |
| Merge 最小规模 | 建议 $100（Gas 成本考量） |
| 市场价格范围 | [0.01, 0.99] |
| 奖励结算 | 每日 UTC 00:00 |
