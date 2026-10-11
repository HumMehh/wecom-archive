# 实施状态

## 0.0.8 验证范围

更新日期：2026-10-08。自动化验证使用合成数据和模拟接口；需单独授权的真实数据测试保持忽略。

前端 63 项测试、Rust 全工作区 117 项测试通过（4 项真实数据测试按原配置忽略）；TypeScript、Rust 格式检查、client/server 构建和 Windows x64 Release 构建通过。

| 范围 | 自动化覆盖 |
| --- | --- |
| 存储目录发现 | 注册表设置、跨根去重排序、当前数据优先、媒体目录合并、目录边界校验 |
| 手动目录重试 | 采集与快速导出参数透传、失败保留输入、成功记忆、损坏配置回退、并发保存 |
| 采集计划 | 间隔及每日计划启用后立即执行、忙碌时保留首采、首次空结果上传、普通周期去重 |
| 成功状态与界面 | 尝试与成功分别记录、管理页仅显示上传成功时间、心跳事件更新计划、成功后清除旧错误、状态按行显示 |
| 既有功能回归 | 签名控制配置、加密传输、幂等合并、消息分页、媒体、脱敏和单文件导出 |
| 语音采集 | 媒体开关开启时语音字节随批次落盘；关闭时保留消息记录并标记 `voice_collection_disabled`，不产生悬空哈希 |
| 沟通记录 | 姓名 × 月份分桶、同月多会话合并、跨月拆分、按方向识别对方、群聊过滤、月份按本地时区过滤、路径穿越与非法扩展名拦截、要点抽取、工作簿原子写入、总结回填忽略未知行 |
| 联系人备注 | 备注列优先于任何昵称；空备注回退昵称；带备注列的表即使表名不含 contact/user 也会被读取 |
| 外部客户识别 | 读取企业微信自带的 `external_user_ids` 名单，采集时写入 `participantKind = external`，报表据此过滤掉同事与官方账号 |

### 已修复：备注名被昵称覆盖

采集阶段解析联系人姓名时，优先级原本只按**表**分配（`user_table` 取 `name` 列记 100 分），通用表扫描即使找到备注列也只记 20 分。结果只要账号库里存在 `user_table`，昵称就无条件压过备注。

现在优先级按**列的语义**分配：备注列固定 200 分，压过所有昵称；昵称列仍按原规则。同表的 nickname 与 remark 两列都会读取，所以没设备注的人仍有昵称可用（空备注会被 `clean_display_name` 跳过）。

### 实测更正：企业微信 5.x 的备注列是 `real_remarks`

拿一个真实测试账号解密 `user.db` 后确认，备注既不在 `remark_name` 也不在 `remark`：

| 表.列 | 值 |
| --- | --- |
| `external_user_relation_v3.user_id` | `7881299717332511` |
| `external_user_relation_v3.real_remarks` | **备注** |
| `external_user_relation_v3.remarks` / `corp_remark` / `recommend_remark` | 空 |
| `user_table.id` | `7881299717332511`（同一个 ID） |
| `user_table.name` | 昵称 |

`real_remarks` 原先不在白名单里，所以备注从未被读取。已加入 `REMARK_NAME_COLUMNS` 并排在首位；`corp_remark` / `remarks` / `recommend_remark` 归入弱白名单，只在已识别为联系人目录的表中读取，避免无关表里的 `remarks` 覆盖正确姓名。修正后用生产解析函数跑该真实库，目标联系人正确解析为备注。

### 客户判定：同昵称的两条记录

真实库里「昵称相同」的两人分属不同主体，`corp_id` 也不同：

| user_id | 昵称 | 在 `external_user_ids` |
| --- | --- | --- |
| `1688856681886908` | 本人 | 否 |
| `1688857240854005` | 与客户同名 | 否（同事） |
| `7881299717332511` | 与同事同名 | **是（客户）** |

因此「客户」不能靠昵称或 `corp_id` 判断，必须用企业微信自己的 `external_user_ids` 名单。采集时据此写入 `participantKind`，导出时按对方是否在该名单内过滤。

### 本地改动新增能力

在 0.0.8 基线上追加了语音落盘与沟通记录导出（含可选外部大模型总结）。这些能力不改动任何版本化契约，`crates/domain`、`crates/parser`、`crates/export`、`crates/transfer`、`crates/archive-store`、`crates/media-store` 保持 0.0.8 原样；新增 `crates/communication-report` 和 `apps/server/src/summary.rs`。

### 已知验证缺口

`wecom-archive-desktop` 的 lib 测试二进制在本机无法加载：`tauri::generate_context!` 为 app 可执行文件嵌入了 `Microsoft.Windows.Common-Controls` v6 的 manifest，但 lib test 目标没有该 manifest，`comctl32.dll` 因此解析到不含 `TaskDialogIndirect` 的 v5 垫片，加载器报 `STATUS_ENTRYPOINT_NOT_FOUND`。这是上游测试基础设施缺陷，与本次改动无关；0.0.7 因链接路径不同而恰好未触发。该 crate 的测试可编译（`cargo test -p wecom-archive-desktop --no-run` 通过），其中语音采集用例与 0.0.7 上实际跑通的版本逐字节一致。排查过程中还修掉了 7 处上游在新版 clippy 下的告警，`cargo clippy --workspace --all-targets -- -D warnings` 现已全绿。

验证命令见 README 的“开发”。生产前端已通过模拟浏览器流程检查；真实企业微信采集上传、托盘交互和升级后的采集端仍需人工验收。升级工作台后，应重新生成采集端 EXE。

## 尚未完成

- OIDC、RBAC、自动保留策略和 legal hold 执行器。
- 50 万消息规模的性能基准测试。
