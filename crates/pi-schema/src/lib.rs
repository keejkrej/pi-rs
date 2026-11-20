//! Eval-free JSON Schema drafts 2020-12 and 7 validator, coercer, and type builders (`@earendil-works/pi-schema` v1.5.2).

mod annotations;
mod arena;
pub mod builders;
mod checks;
mod coerce;
mod compiler;
mod explain;
pub mod formats;
pub mod resolver;
pub mod shared;
pub mod typebox_static;
mod types;
mod validator;

pub use builders::{
    Annotations, ArrOptions, BigintOptions, BytesOptions, IntoArg, NumOptions, ObjOptions, Schema, StrOptions,
    StringEnumOptions, arr, arr_with, bigint, bigint_with, bool, bool_with, bytes, bytes_with, int, int_with, lit,
    lit_with, map, map_with, nul, nul_with, nullable, nullable_with, num, num_with, obj, obj_with, one_of, one_of_with,
    open_obj, open_obj_with, opt, recursive, str, str_with, string_enum, t, tuple, tuple_with, union, union_with,
    unknown, unknown_with, unsafe_schema,
};
pub use checks::{code_points, equal};
pub use shared::{shared_regex, shared_regex_count};
pub use types::{
    AsSchema, CompileOptions, Draft, FormatCheck, ParseResult, SchemaError, UnresolvedRefs, ValidationError,
};
pub use validator::{CoercingValidator, Validator, compile, compile_coercing};
