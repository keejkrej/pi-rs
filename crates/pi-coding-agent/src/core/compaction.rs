//! Compaction and summarization utilities.
//!
//! Port of `packages/coding-agent/src/core/compaction/index.ts`.

// @generated-mods begin (scaffold-owned, do not edit)
pub mod branch_summarization;
pub mod compaction;
pub mod utils;
// @generated-mods end

pub use branch_summarization::{
    BranchPreparation, BranchSummaryDetails, BranchSummaryResult, CollectEntriesResult, GenerateBranchSummaryOptions,
    ReadonlySessionManager, collect_entries_for_branch_summary, generate_branch_summary, prepare_branch_entries,
};
pub use compaction::{
    CompactOptions, CompactionDetails, CompactionPreparation, CompactionResult, CompactionSettings,
    ContextUsageEstimate, DEFAULT_COMPACTION_SETTINGS, GenerateSummaryOptions, SummarizationContext, SummaryOutput,
    calculate_context_tokens, compact, complete_summarization, estimate_context_tokens,
    estimate_projected_context_tokens, estimate_tokens, find_cut_point, find_turn_start_index, generate_summary,
    generate_summary_with_usage, get_last_assistant_usage, get_summarization_failure, prepare_compaction,
    should_compact,
};
pub use utils::{
    FileLists, FileOperations, SUMMARIZATION_SYSTEM_PROMPT, compute_file_lists, create_file_ops,
    extract_file_ops_from_message, format_file_operations, serialize_conversation,
};
