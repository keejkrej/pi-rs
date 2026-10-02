//! Port of packages/codemode/src/index.ts

// @generated-mods begin (scaffold-owned, do not edit)
pub mod declarations;
pub mod identifier;
pub mod runtime;
pub mod source;
pub mod types;
pub mod vendor;
pub mod wasm;
// @generated-mods end
pub use pi_js::{Error, Result};

pub use declarations::{
    DEFAULT_INPUT_SCHEMA_MAX_CHARS, MCP_TYPESCRIPT_PREAMBLE, RenderDeclarationsOptions, RenderToolSignatureOptions,
    SchemaToTypeOptions, mcp_structured_content_schema, render_declarations, render_tool_output_type,
    render_tool_sample, render_tool_signature, schema_to_type,
};
pub use identifier::to_codemode_identifier;
pub use runtime::host::CodemodeSandbox;
pub use runtime::prelude_source::{MAX_STORE_TOTAL_CHARS, MAX_STORE_VALUE_CHARS};
pub use source::{
    CODEMODE_OPTIONS_PREFIX, CODEMODE_SOURCE_GRAMMAR, CodemodeSourceError, CodemodeSourceOptions, ParsedCodemodeSource,
    parse_codemode_source,
};
pub use types::{
    CodemodeCall, CodemodeCallStatus, CodemodeError, CodemodeErrorKind, CodemodeExecuteOptions, CodemodeJsonSchema,
    CodemodeOutputItem, CodemodeResult, CodemodeSandboxOptions, CodemodeStoreWrites, CodemodeTool, CodemodeToolContext,
    CodemodeWasmInput,
};
pub use wasm::{CodemodeWasmModule, load_quick_js_wasm};
