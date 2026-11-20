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
    render_tool_sample, render_tool_signature, resolve_schema_ref, schema_to_type,
};
pub use identifier::to_codemode_identifier;
pub use runtime::execution::CodemodeSandboxBase;
pub use runtime::host::CodemodeSandbox;
pub use runtime::inline::{
    DEFAULT_INTERRUPT_BUDGET, INLINE_MAX_STACK_SIZE, InlineCodemodeSandbox, InlineCodemodeSandboxOptions,
};
pub use runtime::prelude_source::{MAX_OUTPUT_CHARS, MAX_OUTPUT_ITEMS, MAX_STORE_TOTAL_CHARS, MAX_STORE_VALUE_CHARS};
pub use runtime::remote::{
    CodemodeRemote, CodemodeRemoteExchange, CodemodeRemoteMessage, CodemodeRemoteReply, CodemodeRemoteRequest,
    CodemodeServeOptions, RemoteCodemodeSandbox, RemoteCodemodeSandboxOptions, serve_codemode_remote,
};
pub use source::{
    CODEMODE_OPTIONS_PREFIX, CODEMODE_SOURCE_GRAMMAR, CodemodeSourceError, CodemodeSourceOptions, ParsedCodemodeSource,
    parse_codemode_source,
};
pub use types::{
    CodemodeCall, CodemodeCallStatus, CodemodeCallTarget, CodemodeDeclaredGlobal, CodemodeDeclaredTool, CodemodeDuring,
    CodemodeError, CodemodeErrorKind, CodemodeExecuteOptions, CodemodeHostCall, CodemodeJsonSchema, CodemodeOutcome,
    CodemodeOutputItem, CodemodeResult, CodemodeRun, CodemodeRunOptions, CodemodeSandboxBaseOptions,
    CodemodeSandboxOptions, CodemodeStoreWrites, CodemodeTool, CodemodeToolContext, CodemodeWasmInput,
    CodemodeWorkerHooks,
};
pub use vendor::quickjs_wasi::{
    CompileFlags, EvalFlags, HandleScope, HostFunction, Intrinsics, JSDeferred, JSException, JSOwnPropertyDescriptor,
    JSPropertyDescriptor, JSValueHandle, MAX_STACK_SIZE, ModuleLoader, PromiseRejectionHandler, PromiseResult,
    PromiseState, QuickJS, QuickJSOptions, TimezoneOffset, WasiClockTimeGet, WasiFdWrite, WasiMemory, WasiOptions,
    WasiOverrides, WasiRandomGet,
};
pub use wasm::{CodemodeWasmModule, load_quick_js_wasm};
