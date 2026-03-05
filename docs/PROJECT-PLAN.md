# 项目计划: Polymarket LP 做市机器人

> 版本: 1.1 | 更新日期: 2026-03-05

## 1. 项目阶段概览

```
Phase 1: 基础架构    [✅ 完成]
Phase 2: 核心策略    [✅ 完成]
Phase 3: 风控系统    [✅ 完成]
Phase 4: 执行层      [✅ 完成]
Phase 5: 功能完善    [✅ 完成] (API 限流延后到 v1.1)
Phase 6: 测试强化    [✅ 完成] (66 tests, 全部通过)
Phase 7: 代码审查    [✅ 完成] (3 CRITICAL 已修复)
```

---

## 2. Phase 1: 基础架构 ✅

### 目标
建立项目骨架、配置系统、数据层连接。

### 交付物
| 任务 | 文件 | 状态 |
|------|------|------|
| 项目初始化 (Cargo.toml, 依赖) | Cargo.toml | ✅ |
| 配置模块 (TOML + .env) | src/config/mod.rs | ✅ |
| 共享状态 (DashMap, RwLock) | src/data/state.rs | ✅ |
| REST 客户端 (CLOB API) | src/data/rest.rs | ✅ |
| Gamma 客户端 | src/data/gamma.rs | ✅ |
| Market WebSocket | src/data/ws.rs (run_market_ws) | ✅ |
| User WebSocket | src/data/ws.rs (run_user_ws) | ✅ |
| 主入口 | src/main.rs | ✅ |

---

## 3. Phase 2: 核心策略 ✅

### 目标
实现定价引擎和持仓管理。

### 交付物
| 任务 | 文件 | 状态 |
|------|------|------|
| 阶梯挂单生成 | src/pricing/mod.rs (generate_quotes) | ✅ |
| Q-Score 估算 | src/pricing/mod.rs (estimate_qscore) | ✅ |
| VAF 波动率因子 | src/pricing/mod.rs (compute_vaf) | ✅ |
| TF 时间因子 | src/pricing/mod.rs (compute_tf) | ✅ |
| Quote Skewing | src/pricing/mod.rs (compute_skew) | ✅ |
| IIR 计算 | src/data/state.rs (PositionRecord::iir) | ✅ |
| 持仓评估 | src/position/mod.rs (evaluate) | ✅ |
| Merge 检测 | src/position/mod.rs (mergeable_amount) | ✅ |

---

## 4. Phase 3: 风控系统 ✅

### 目标
实现 L1/L2/L3 三级风控状态机。

### 交付物
| 任务 | 文件 | 状态 |
|------|------|------|
| 三级状态机 | src/risk/mod.rs | ✅ |
| L2 触发条件 | src/risk/mod.rs (evaluate) | ✅ |
| L3 触发条件 | src/risk/mod.rs (evaluate) | ✅ |
| L2 自动恢复 | src/risk/mod.rs (check_l2_recovery) | ✅ |
| Ghost Fill 检测 | src/risk/mod.rs (record_ghost_fill) | ✅ |
| 手动恢复 | src/risk/mod.rs (manual_recover) | ✅ |

---

## 5. Phase 4: 执行层 ✅

### 目标
实现订单管理和主循环编排。

### 交付物
| 任务 | 文件 | 状态 |
|------|------|------|
| 批量下单 | src/execution/mod.rs (submit_orders) | ✅ |
| Cancel-Replace | src/execution/mod.rs (cancel_market_orders) | ✅ |
| 紧急全取消 | src/execution/mod.rs (cancel_all_orders) | ✅ |
| 重试机制 | src/execution/mod.rs (cancel_market_with_retry) | ✅ |
| 主循环编排 | src/monitor/mod.rs (run_orchestrator) | ✅ |
| 策略执行 | src/monitor/strategy.rs (run_market_strategy) | ✅ |
| 风险评估 | src/monitor/strategy.rs (evaluate_risk) | ✅ |
| 状态清理 | src/monitor/mod.rs (prune_stale_state) | ✅ |

---

## 6. Phase 5: 功能完善 ✅

### 目标
补充缺失功能、强化健壮性。

### 任务清单
| # | 任务 | 优先级 | 状态 | 说明 |
|---|------|--------|------|------|
| 5.1 | CTF Merge 合约调用 | P1 | ✅ | src/data/ctf.rs, alloy + SDK |
| 5.2 | Config 验证强化 | P1 | ✅ | 30+ 条验证规则 |
| 5.3 | API 限流管理 | P2 | ⬜ | 延后到 v1.1 |
| 5.4 | PnlTracker 单元测试 | P1 | ✅ | tests/state_tests.rs (12 tests) |
| 5.5 | 初始持仓加载健壮性 | P1 | ✅ | 错误回退 + 成本基础初始化 |

---

## 7. Phase 6: 测试强化 ✅

### 目标
达到 80%+ 代码覆盖率。

### 测试统计
| 文件 | 测试数 | 模块 |
|------|--------|------|
| src/data/ctf.rs (unit) | 4 | CTF decimal_to_u256 |
| tests/config_tests.rs | 14 | AppConfig 解析+验证 |
| tests/position_tests.rs | 8 | PositionManager |
| tests/pricing_tests.rs | 12 | PricingEngine+边界 |
| tests/risk_tests.rs | 16 | RiskController+补充 |
| tests/state_tests.rs | 12 | PnlTracker+PositionRecord |
| **合计** | **66** | **全部通过** |

---

## 8. Phase 7: 代码审查 ✅

### 目标
通过代码审查确保质量和安全性。

### 检查清单
- [x] 安全审查：密钥处理、输入验证、注入防护
- [x] 并发安全：锁顺序、死锁风险、内存序
- [x] 错误处理：panic 路径、unwrap 使用、错误传播
- [x] 性能：不必要的 clone、锁持有时间、内存泄漏
- [x] 代码风格：命名一致性、注释准确性、模块边界

### CRITICAL 修复
| # | 问题 | 修复 |
|---|------|------|
| CR-1 | Config 测试 env var 竞态 | `AppConfig::from_toml_str()` |
| CR-2 | Merge 后未更新本地状态 | 更新 yes/no_shares + values |
| CR-3 | settlement_tick 不刷新 condition_ids | 改用 `fetch_all_metadata` |

---

## 9. 风险和缓解

| 风险 | 影响 | 缓解措施 |
|------|------|----------|
| SDK API 不兼容 | Merge 功能无法实现 | 降级为手动 Merge 提示 |
| 测试中 mock 困难 | 无法测试 WS/REST 层 | 集中测试纯逻辑模块 |
| Gas 成本过高 | Merge 操作不经济 | min_merge_size 设置为 $100+ |
| WS 频繁断连 | 策略无法持续运行 | 指数退避 + L2 自动触发 |

---

## 10. 依赖关系

```
Phase 1 (基础) → Phase 2 (策略) → Phase 4 (执行)
                ↘ Phase 3 (风控) ↗
                                 ↘ Phase 5 (完善) → Phase 6 (测试) → Phase 7 (审查)
```
