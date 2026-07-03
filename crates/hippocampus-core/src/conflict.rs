//! # 记忆冲突检测（v2.6 批次 8）
//!
//! 在记忆迭代更新（`update_memory`）时检测新旧事实之间的冲突，
//! 让 Agent 能识别「用户立场反转」「事实矛盾」等情况，而非盲目追加。
//!
//! ## 设计参考
//!
//! - **BeliefShift 基准**：衡量 Agent 识别跨会话矛盾立场的能力
//! - **Kumiho / 信念修正（Belief Revision）**：形式化语义，修正过去判断而不丢失历史
//!
//! ## 架构（可插拔 trait，类比 [`crate::score::Scorer`]）
//!
//! ```text
//! update 请求 → ConflictDetector.detect(update, &existing_memory) → ConflictReport
//!                                                                   ↓
//! MemoryUpdateRecord.conflicts ← Vec<ConflictRecord> ← 持久化到记忆文件
//! ```
//!
//! - [`HeuristicDetector`](crate::heuristic::HeuristicDetector)：默认纯算法实现（无 LLM 依赖）
//! - [`NoopDetector`]：空实现，不做任何检测
//!
//! ## 冲突维度（三维度）
//!
//! 1. **自我矛盾（SelfContradict）**：同一批 update 内 added 与 deprecated 包含相同/相似事实
//! 2. **直接矛盾（DirectContradict）**：added_facts 与现有 key_facts 语义相反（反义词匹配）
//! 3. **立场反转（StanceReversal）**：added_facts 与历史 updates 的 added_facts 直接冲突

use crate::model::{MemoryFile, MemoryUpdate};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

// ============================================================================
// 数据结构
// ============================================================================

/// 冲突类型
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConflictKind {
    /// 自我矛盾：同一批 update 内 added 与 deprecated 包含相同/相似事实
    SelfContradict,
    /// 直接矛盾：added_facts 与现有 key_facts 语义相反（反义词匹配）
    DirectContradict,
    /// 立场反转：added_facts 与历史 updates 的 added_facts 直接冲突
    StanceReversal,
}

/// 冲突严重级别
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    /// 信息性（如无效 deprecate，留待未来扩展）
    Info,
    /// 警告（可能矛盾，如立场反转）
    Warning,
    /// 严重（明确矛盾，如自我矛盾、直接反义）
    Critical,
}

/// 单条冲突记录
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConflictRecord {
    /// 冲突类型
    pub kind: ConflictKind,
    /// 严重级别
    pub severity: Severity,
    /// 中文描述（人类可读）
    pub description: String,
    /// 冲突的已有事实（DirectContradict / StanceReversal 时有值）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub existing_fact: Option<String>,
    /// 新事实（触发冲突的 update 中的事实）
    pub new_fact: String,
}

/// 冲突检测报告
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ConflictReport {
    /// 检测到的所有冲突记录
    pub conflicts: Vec<ConflictRecord>,
}

impl ConflictReport {
    /// 创建空报告
    pub fn empty() -> Self {
        Self::default()
    }

    /// 是否无冲突
    pub fn is_clean(&self) -> bool {
        self.conflicts.is_empty()
    }

    /// 冲突总数
    pub fn count(&self) -> usize {
        self.conflicts.len()
    }

    /// 是否存在 Critical 级别冲突
    pub fn has_critical(&self) -> bool {
        self.conflicts
            .iter()
            .any(|c| c.severity == Severity::Critical)
    }

    /// 按严重级别筛选
    pub fn by_severity(&self, severity: Severity) -> Vec<&ConflictRecord> {
        self.conflicts
            .iter()
            .filter(|c| c.severity == severity)
            .collect()
    }

    /// 追加一条冲突记录
    pub fn push(&mut self, record: ConflictRecord) {
        self.conflicts.push(record);
    }
}

// ============================================================================
// ConflictDetector trait
// ============================================================================

/// 记忆冲突检测器 trait（可插拔）
///
/// 实现方提供具体的冲突检测算法：
/// - [`HeuristicDetector`](crate::heuristic::HeuristicDetector)：启发式纯算法（默认）
/// - [`NoopDetector`]：空实现（不检测）
///
/// ## 调用时机
///
/// 在 `Storage::update_memory` **之前**同步调用：
///
/// ```text,ignore
/// let memory = storage.read_memory(&memory_id).await?;
/// let report = detector.detect(&update, &memory).await;
/// storage.update_memory_with_conflicts(&memory_id, update, report.conflicts).await?;
/// ```
///
/// ## 设计原则
///
/// - **仅记录不阻止**：即使检测到 Critical 冲突，也不阻止更新（保留历史，交由上层 LLM 决策）
/// - **无副作用**：detect 方法不修改输入数据
/// - **可插拔**：通过 trait 注入，Storage 层不感知具体实现
#[async_trait]
pub trait ConflictDetector: Send + Sync {
    /// 检测 `update` 与 `existing_memory` 之间的冲突
    ///
    /// ## 参数
    /// - `update`：待应用的更新（added/revised/deprecated facts）
    /// - `existing_memory`：现有的记忆文件（包含 turns + 历史 updates）
    ///
    /// ## 返回
    /// 冲突检测报告（即使无冲突也返回空报告，不返回错误）
    async fn detect(
        &self,
        update: &MemoryUpdate,
        existing_memory: &MemoryFile,
    ) -> ConflictReport;
}

// ============================================================================
// NoopDetector（默认空实现）
// ============================================================================

/// 空实现（不做任何冲突检测）
///
/// 用于未配置检测器时的默认行为，或测试中需要跳过检测的场景。
#[derive(Debug, Default, Clone)]
pub struct NoopDetector;

impl NoopDetector {
    /// 创建空检测器
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl ConflictDetector for NoopDetector {
    async fn detect(
        &self,
        _update: &MemoryUpdate,
        _existing_memory: &MemoryFile,
    ) -> ConflictReport {
        ConflictReport::empty()
    }
}

// ============================================================================
// 单元测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ArchivePeriod, MessageContent, MessageTurn};
    use chrono::Utc;
    use uuid::Uuid;

    /// 构造测试用 MemoryFile
    fn make_test_memory() -> MemoryFile {
        let turn = MessageTurn {
            id: Uuid::new_v4(),
            user_message: MessageContent {
                text: Some("用户消息".to_string()),
                attachments: vec![],
                tool_calls: vec![],
                thinking: None,
            },
            llm_message: MessageContent {
                text: Some("助手回复".to_string()),
                attachments: vec![],
                tool_calls: vec![],
                thinking: None,
            },
            tags: vec![],
            timestamp: Utc::now(),
            token_count: 100,
        };

        MemoryFile {
            id: Uuid::new_v4(),
            schema_version: 1,
            archived_at: Utc::now(),
            session_id: "test-sess".to_string(),
            project_id: None,
            turns: vec![turn],
            tags: vec![],
            total_tokens: 100,
            truncated: false,
            period: ArchivePeriod::Daily,
            access_count: 0,
            importance: 0,
            updates: vec![],
        }
    }

    #[test]
    fn test_conflict_report_empty() {
        let report = ConflictReport::empty();
        assert!(report.is_clean());
        assert_eq!(report.count(), 0);
        assert!(!report.has_critical());
    }

    #[test]
    fn test_conflict_report_push_and_query() {
        let mut report = ConflictReport::empty();
        report.push(ConflictRecord {
            kind: ConflictKind::SelfContradict,
            severity: Severity::Critical,
            description: "测试冲突".to_string(),
            existing_fact: None,
            new_fact: "fact A".to_string(),
        });
        report.push(ConflictRecord {
            kind: ConflictKind::StanceReversal,
            severity: Severity::Warning,
            description: "立场反转".to_string(),
            existing_fact: Some("旧立场".to_string()),
            new_fact: "新立场".to_string(),
        });

        assert!(!report.is_clean());
        assert_eq!(report.count(), 2);
        assert!(report.has_critical());
        assert_eq!(report.by_severity(Severity::Critical).len(), 1);
        assert_eq!(report.by_severity(Severity::Warning).len(), 1);
        assert_eq!(report.by_severity(Severity::Info).len(), 0);
    }

    #[test]
    fn test_severity_ordering() {
        assert!(Severity::Critical > Severity::Warning);
        assert!(Severity::Warning > Severity::Info);
        assert!(Severity::Critical > Severity::Info);
    }

    #[tokio::test]
    async fn test_noop_detector_returns_empty() {
        let detector = NoopDetector::new();
        let memory = make_test_memory();
        let update = MemoryUpdate::new().add_fact("新事实");
        let report = detector.detect(&update, &memory).await;
        assert!(report.is_clean());
    }

    #[test]
    fn test_conflict_record_serialization() {
        let record = ConflictRecord {
            kind: ConflictKind::DirectContradict,
            severity: Severity::Critical,
            description: "用户先说喜欢，后说不喜欢".to_string(),
            existing_fact: Some("用户喜欢咖啡".to_string()),
            new_fact: "用户不喜欢咖啡".to_string(),
        };
        let json = serde_json::to_string(&record).unwrap();
        assert!(json.contains("direct_contradict"));
        assert!(json.contains("critical"));
        assert!(json.contains("用户喜欢咖啡"));

        // 反序列化往返
        let restored: ConflictRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.kind, ConflictKind::DirectContradict);
        assert_eq!(restored.severity, Severity::Critical);
        assert_eq!(restored.new_fact, "用户不喜欢咖啡");
    }

    #[test]
    fn test_conflict_report_serialization_skip_none() {
        let record = ConflictRecord {
            kind: ConflictKind::SelfContradict,
            severity: Severity::Critical,
            description: "自我矛盾".to_string(),
            existing_fact: None,
            new_fact: "fact".to_string(),
        };
        let json = serde_json::to_string(&record).unwrap();
        // existing_fact 为 None 时应被跳过
        assert!(!json.contains("existing_fact"));
    }
}
